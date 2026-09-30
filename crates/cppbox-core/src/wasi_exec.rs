//! Compile + run non-threaded student C++ against `wasm32-wasip1`, using a
//! **bundled** clang++ (from wasi-sdk's own release, not a host install),
//! then executing the result sandboxed under `wasmtime` - replacing podman
//! for the common case. See `docs/WASM_PLAN.md` for the full design and
//! the Phase 0 spike this was ported from (`src_v2/`).
//!
//! Also bundles clang-format (comes free in the same wasi-sdk archive) and
//! clangd (its own slim standalone release) - see `bundled_clang_format`/
//! `bundled_clangd`, used by `sandbox::format_code`/`lsp.rs` in preference
//! to a host install, falling back to PATH if bundling isn't ready.
//!
//! Key discovery that shapes this whole module: wasi-sdk's own clang++
//! needs **no** `-resource-dir` trick to work (unlike a generic host
//! clang++, which needs its native resource-dir manually merged with the
//! wasm32 builtins - see git history for that earlier, more complicated
//! approach). wasi-sdk ships a self-contained cross-compiler; pointing
//! `--sysroot` at its bundled sysroot is enough.
//!
//! Code this target cannot serve (`wasm_unsupported_reason`) still routes to
//! `sandbox.rs`'s podman path, and that is not a gap waiting to be closed.
//! Re-measured against wasi-sdk 34 + wasmtime 46.0.3: a
//! `wasm32-wasip1-threads` build does compile and link, and carries the right
//! ABI (a `wasi.thread-spawn` import, a `wasi_thread_start` export), but it
//! only instantiates with shared memory enabled and then `std::thread` still
//! fails with `thread constructor failed: Resource temporarily unavailable`.
//! Upstream is removing the proposal rather than fixing it: wasmtime warns that
//! `-Sthreads` becomes a hard error in 47.0.0, and Bytecode Alliance RFC 47
//! (merged 2026-05) deletes wasi-threads outright, pointing at WASIp3
//! cooperative threads near term and shared-everything-threads long term.
//!
//! Everything here is opportunistic: if a download fails or the platform
//! isn't recognized, the relevant toolchain just stays unready and callers
//! fall back to their pre-existing host-PATH/podman behavior - never a
//! regression from today.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};
use wasmtime::{Config, Engine, Linker, Module, ResourceLimiter, Store};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

use crate::sandbox::File;

const WASI_SDK_TAG: &str = "wasi-sdk-34";
const WASI_SDK_VERSION: &str = "34.0";
const WASI_SDK_BASE_URL: &str = "https://github.com/WebAssembly/wasi-sdk/releases/download";
const CLANGD_VERSION: &str = "22.1.6";
const CLANGD_BASE_URL: &str = "https://github.com/clangd/clangd/releases/download";

/// Ceiling for a run's wasm linear memory - applied uniformly to every path
/// that executes through `wasmtime` here (single-file `compile_and_run` and
/// multi-file `compile_and_run_with_flags`/`make_and_run`, whether invoked by
/// a student running their own code or by grading re-running a submission -
/// there is exactly one execution path in this module, so "uniform" falls
/// out of the architecture rather than needing separate wiring). 4 GiB is
/// wasm32's hard ceiling (a 32-bit linear-memory index tops out at 2^32
/// bytes = 65536 64KiB pages) - this is the highest a wasm32 module can ever
/// request, not an arbitrary number. Must stay in lockstep with `compile()`'s
/// `--max-memory` linker flag below: the wasmtime `ResourceLimiter` can only
/// ever be as generous as what the compiled module itself declares as its
/// own max, so raising just one without the other is a no-op.
const MAX_MEMORY_BYTES: usize = 4 * 1024 * 1024 * 1024;

/// wasm stack size. wasi-sdk defaults this to 64 KiB, which is far smaller
/// than any host default (Linux gives a thread 8 MiB) and small enough that an
/// ordinary local array blows it: `array<array<double, 512>, 512>` is 2 MiB and
/// traps with a bare "memory access out of bounds" that names neither the stack
/// nor the array. Numerical code declares big locals routinely, so this is not
/// an edge case - discovered by running a matrix-multiply teaching example that
/// works everywhere else. 8 MiB matches the usual host default; unlike
/// `MAX_MEMORY_BYTES` this is stack reserved up front, so it is charged against
/// the module's memory rather than being free.
const WASM_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;

/// Readiness, mirroring `sandbox::sandbox_state()`: 0 unknown, 1 preparing,
/// 2 ready, 3 failed (falls back to podman for everything).
static WASI_STATE: AtomicU8 = AtomicU8::new(0);
static WASI_MSG: OnceLock<String> = OnceLock::new();

pub fn wasi_state() -> (u8, String) {
    (
        WASI_STATE.load(Ordering::Relaxed),
        WASI_MSG.get().cloned().unwrap_or_default(),
    )
}

fn set_state(code: u8, msg: String) {
    WASI_STATE.store(code, Ordering::Relaxed);
    let _ = WASI_MSG.set(msg.clone());
    tracing::info!("wasi toolchain: {msg}");
}

pub fn is_ready() -> bool {
    WASI_STATE.load(Ordering::Relaxed) == 2
}

fn toolchain_dir(root: &Path) -> PathBuf {
    root.join("wasi-toolchain")
}

fn sdk_dir(root: &Path) -> PathBuf {
    toolchain_dir(root).join(format!("wasi-sdk-{WASI_SDK_VERSION}"))
}

pub fn sysroot_dir(root: &Path) -> PathBuf {
    sdk_dir(root).join("share/wasi-sysroot")
}

fn bin_dir(root: &Path) -> PathBuf {
    sdk_dir(root).join("bin")
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.into()
    }
}

/// Bundled clang++, if the wasi-sdk toolchain is ready.
pub fn bundled_clangxx(root: &Path) -> Option<PathBuf> {
    let p = bin_dir(root).join(exe("clang++"));
    p.exists().then_some(p)
}

/// Bundled clang-format (comes free in the same wasi-sdk archive), if ready.
pub fn bundled_clang_format(root: &Path) -> Option<PathBuf> {
    let p = bin_dir(root).join(exe("clang-format"));
    p.exists().then_some(p)
}

fn clangd_dir(root: &Path) -> PathBuf {
    toolchain_dir(root).join(format!("clangd_{CLANGD_VERSION}"))
}

/// Bundled clangd, if its (separate, smaller) download is ready.
pub fn bundled_clangd(root: &Path) -> Option<PathBuf> {
    let p = clangd_dir(root).join("bin").join(exe("clangd"));
    p.exists().then_some(p)
}

/// `(os, arch)` -> wasi-sdk release asset name. `None` for unsupported
/// combinations - the caller just stays on the podman/host-PATH fallback.
fn wasi_sdk_asset() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("wasi-sdk-34.0-x86_64-linux.tar.gz"),
        ("linux", "aarch64") => Some("wasi-sdk-34.0-arm64-linux.tar.gz"),
        ("macos", "x86_64") => Some("wasi-sdk-34.0-x86_64-macos.tar.gz"),
        ("macos", "aarch64") => Some("wasi-sdk-34.0-arm64-macos.tar.gz"),
        ("windows", "x86_64") => Some("wasi-sdk-34.0-x86_64-windows.tar.gz"),
        ("windows", "aarch64") => Some("wasi-sdk-34.0-arm64-windows.tar.gz"),
        _ => None,
    }
}

/// `os` -> clangd release asset name (clangd ships one build per OS, not
/// per-arch - e.g. the mac build covers both x86_64 and arm64).
fn clangd_asset() -> Option<&'static str> {
    match std::env::consts::OS {
        "linux" => Some("clangd-linux-22.1.6.zip"),
        "macos" => Some("clangd-mac-22.1.6.zip"),
        "windows" => Some("clangd-windows-22.1.6.zip"),
        _ => None,
    }
}

/// Download/assemble the bundled wasi-sdk toolchain (clang++, clang-format,
/// wasm-ld, the wasm32-wasip1 sysroot) and clangd. Blocking; call from a
/// background thread at startup (mirrors `sandbox::ensure_sandbox_image()`).
pub fn ensure_wasi_toolchain(root: &Path) {
    let Some(sdk_asset) = wasi_sdk_asset() else {
        set_state(
            3,
            format!(
                "unsupported platform {}-{} for bundled wasi-sdk - falling back to podman",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        );
        return;
    };

    let dir = toolchain_dir(root);
    let sdk = sdk_dir(root);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        set_state(3, format!("creating {}: {e}", dir.display()));
        return;
    }

    if !bin_dir(root).join(exe("clang++")).exists() {
        set_state(1, format!("downloading {sdk_asset}..."));
        let archive = dir.join("wasi-sdk.tar.gz");
        let url = format!("{WASI_SDK_BASE_URL}/{WASI_SDK_TAG}/{sdk_asset}");
        if let Err(e) = download_blocking(&url, &archive) {
            set_state(3, format!("downloading {sdk_asset}: {e}"));
            return;
        }
        set_state(1, "extracting wasi-sdk...".into());
        // Extract the whole archive, unfiltered. An earlier version tried to
        // extract only the wasm32-wasip1 subset of the sysroot (dropping
        // wasip1-threads/wasip2/wasip3, which we never target) to save
        // disk - that silently broke clang's eh/noeh header selection
        // (`#include <iostream>` failed to resolve) for reasons not worth
        // chasing further: the sysroot's directory *shape* apparently
        // matters to clang's multilib detection in a way that isn't
        // documented, and a broken toolchain is worse than an extra ~300MB
        // on disk for what's already a large one-time download.
        let top = format!(
            "wasi-sdk-{}-{}",
            WASI_SDK_VERSION,
            sdk_asset
                .strip_prefix(&format!("wasi-sdk-{WASI_SDK_VERSION}-"))
                .and_then(|s| s.strip_suffix(".tar.gz"))
                .unwrap_or("x86_64-linux")
        );
        let out = std::process::Command::new("tar")
            .arg("xzf")
            .arg(&archive)
            .current_dir(&dir)
            .output();
        match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                set_state(
                    3,
                    format!(
                        "extracting wasi-sdk: {}",
                        String::from_utf8_lossy(&o.stderr)
                    ),
                );
                return;
            }
            Err(e) => {
                set_state(3, format!("extracting wasi-sdk: {e}"));
                return;
            }
        }
        let extracted = dir.join(&top);
        if extracted != sdk {
            if let Err(e) = std::fs::rename(&extracted, &sdk) {
                set_state(
                    3,
                    format!("renaming {} -> {}: {e}", extracted.display(), sdk.display()),
                );
                return;
            }
        }
        let _ = std::fs::remove_file(&archive);
    }

    if let Some(cd_asset) = clangd_asset() {
        if !clangd_dir(root).join("bin").join(exe("clangd")).exists() {
            set_state(1, format!("downloading clangd {cd_asset}..."));
            let archive = dir.join("clangd.zip");
            let url = format!("{CLANGD_BASE_URL}/{CLANGD_VERSION}/{cd_asset}");
            if let Err(e) = download_blocking(&url, &archive) {
                // clangd is a separate, smaller nicety (LSP) - don't fail
                // the whole compile/run toolchain over it.
                tracing::warn!("clangd download failed, falling back to host PATH: {e}");
            } else {
                let ok = extract_zip(&archive, &dir);
                if let Err(e) = ok {
                    tracing::warn!("clangd extraction failed, falling back to host PATH: {e}");
                }
                let _ = std::fs::remove_file(&archive);
            }
        }
    }

    set_state(2, format!("wasi toolchain ready ({})", sdk.display()));
}

fn download_blocking(url: &str, dest: &Path) -> Result<(), String> {
    let resp = reqwest::blocking::get(url).map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().map_err(|e| e.to_string())?;
    std::fs::write(dest, &bytes).map_err(|e| e.to_string())
}

/// `unzip` on Linux/macOS; Windows' built-in `tar.exe` (bsdtar) reads zip
/// archives directly via `tar -xf`, so no separate zip tool is needed there.
fn extract_zip(archive: &Path, dest_dir: &Path) -> Result<(), String> {
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("tar");
        c.arg("-xf").arg(archive);
        c
    } else {
        let mut c = std::process::Command::new("unzip");
        c.arg("-q").arg(archive);
        c
    };
    let out = cmd
        .current_dir(dest_dir)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// Why this code cannot *build* for wasm, or `None`.
///
/// Only `<execution>` qualifies: wasi-sdk's libc++ ships no parallel
/// algorithms, so `std::execution::par` fails to compile with an error that
/// says nothing useful to a student.
///
/// Threads are deliberately *not* here. Header presence is the wrong signal:
/// `<atomic>`, `<mutex>` and `<shared_mutex>` all work fine single-threaded,
/// and merely including `<thread>` costs nothing - three teaching modules were
/// blocked from wasm while running perfectly on it, one of them because a
/// vendored header it depends on mentions `<thread>`. Only *spawning* fails, so
/// the code runs and `thread_failure_hint` explains it if it does.
pub fn wasm_unsupported_reason(files: &[File]) -> Option<&'static str> {
    const UNBUILDABLE_MARKERS: &[&str] = &["<execution>"];
    if files
        .iter()
        .any(|f| UNBUILDABLE_MARKERS.iter().any(|m| f.content.contains(m)))
    {
        return Some("This code uses <execution> (parallel algorithms), which the WASM sandbox can't build (wasi-sdk's libc++ has no parallel algorithm support)");
    }
    None
}

/// Headers that imply *spawning* a thread - the operation wasm32-wasip1 cannot
/// do. Not a blocker, only evidence for `thread_failure_hint`: `<atomic>`,
/// `<mutex>` and `<shared_mutex>` are excluded because they work without ever
/// starting a thread.
const THREAD_SPAWN_MARKERS: &[&str] = &["<thread>", "<future>", "<condition_variable>"];

/// Do these sources spawn threads? Evidence for `thread_failure_hint`.
pub fn mentions_thread_spawn(files: &[File]) -> bool {
    files
        .iter()
        .any(|f| THREAD_SPAWN_MARKERS.iter().any(|m| f.content.contains(m)))
}

/// Same question for the Makefile flow, which works from a directory rather
/// than an in-memory file list. Top level only, matching how projects are laid
/// out; a miss just means the raw error is shown unexplained.
fn dir_mentions_thread_spawn(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|e| {
        let path = e.path();
        let is_source = path
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| matches!(x, "cpp" | "cc" | "cxx" | "h" | "hpp"));
        is_source
            && std::fs::read_to_string(&path)
                .is_ok_and(|text| THREAD_SPAWN_MARKERS.iter().any(|m| text.contains(m)))
    })
}

/// Turn an opaque wasm run failure into an explanation, when the evidence
/// supports one.
///
/// A `std::thread` that cannot start throws `std::system_error`, which escapes
/// `main` and reaches the host as nothing more descriptive than "thrown Wasm
/// exception" - the same text any other uncaught exception produces (a 4 GB
/// `bad_alloc` looks identical). So this fires only when the failure looks like
/// an uncaught exception *and* the sources include a thread-spawning header,
/// and otherwise leaves the raw error alone rather than guessing.
pub fn thread_failure_hint(spawns_threads: bool, run_output: &str) -> Option<String> {
    let looks_like_uncaught = run_output.contains("thrown Wasm exception")
        || run_output.contains("uncaught exception")
        || run_output.contains("thread constructor failed");
    if !looks_like_uncaught {
        return None;
    }
    if !spawns_threads {
        return None;
    }
    Some(
        "The WASM sandbox can't start threads (wasm32-wasip1 has no thread support, and \
         Wasmtime 47 removes the wasi-threads experiment), so std::thread and std::async \
         throw as soon as they are constructed. Switch Sandbox to Native to run this."
            .to_string(),
    )
}

/// `extra` is whatever `storage::flags_to_extra` produced (e.g.
/// `-O0 -Werror`) for a project's toolchain settings - applied *after* our
/// own baseline flags so a project's `-O0` correctly overrides our default
/// `-O2` (clang uses the last `-O` flag on the command line). Callers must
/// keep `-fsanitize=...` out of `extra` here - see
/// `wants_sanitizer`/`sandbox::make_and_run`, which routes those to podman
/// instead: this project's wasm32-wasip1 sanitizer support is unverified.
fn compile(
    root: &Path,
    dir: &Path,
    sources: &[String],
    inc: &str,
    std: &str,
    extra: &str,
    out: &Path,
) -> Result<std::process::Output, String> {
    let clangxx = bundled_clangxx(root).ok_or("bundled clang++ not ready")?;
    let sysroot = sysroot_dir(root);
    std::process::Command::new(clangxx)
        .current_dir(dir)
        .arg("--target=wasm32-wasip1")
        .arg(format!("--sysroot={}", sysroot.display()))
        .arg(format!("-std={std}"))
        .arg("-O2")
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-fcolor-diagnostics")
        // See docs/WASM_PLAN.md / src_v2/README.md for why each of these is
        // needed - none of it is documented anywhere obvious upstream.
        .arg("-fwasm-exceptions")
        .arg("-mllvm")
        .arg("-wasm-enable-eh")
        .arg("-mllvm")
        .arg("-wasm-use-legacy-eh=false")
        .arg("-Wl,--initial-memory=67108864")
        .arg(format!("-Wl,--max-memory={MAX_MEMORY_BYTES}"))
        .arg(format!("-Wl,-z,stack-size={WASM_STACK_SIZE_BYTES}"))
        .arg("-lunwind")
        // wasi-libc trims long double printf/scanf support by default to
        // save size; without this, anything using `long double` (including
        // std::stold/strtold, and printf/scanf %Lf) compiles fine but traps
        // at runtime with "Support for formatting long double values is
        // currently disabled" - discovered by actually running real student
        // code that parses prices via std::stold.
        .arg("-lc-printscan-long-double")
        .args(inc.split_whitespace())
        .args(extra.split_whitespace())
        .args(sources)
        .arg("-o")
        .arg(out)
        .output()
        .map_err(|e| e.to_string())
}

/// This project's wasm32-wasip1 sanitizer support is unverified - route
/// sanitizer-enabled builds to podman instead of silently ignoring or
/// mis-running the flag.
pub fn wants_sanitizer(extra: &str) -> bool {
    extra.contains("-fsanitize")
}

/// Mirrors podman's `--memory` limit for the run stage.
struct MemLimiter {
    max_bytes: usize,
}

impl ResourceLimiter for MemLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _max: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= self.max_bytes)
    }
    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _max: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= 10_000)
    }
}

struct HostState {
    wasi: WasiP1Ctx,
    limiter: MemLimiter,
}

enum Outcome {
    Exited(i32),
    Trapped(String),
    TimedOut,
}

/// Execute a compiled wasm32-wasip1 module. Blocking (wasmtime is
/// synchronous) - call via `spawn_blocking`.
fn run_wasm_blocking(
    wasm_path: &Path,
    job_dir: &Path,
    stdin: &str,
    timeout: Option<Duration>,
    max_memory_bytes: usize,
) -> Result<(Outcome, String, String), String> {
    let mut config = Config::new();
    config.epoch_interruption(true);
    config.wasm_exceptions(true);
    let engine = Engine::new(&config).map_err(|e| e.to_string())?;
    let module = Module::from_file(&engine, wasm_path).map_err(|e| e.to_string())?;

    let mut linker: Linker<HostState> = Linker::new(&engine);
    wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |s: &mut HostState| &mut s.wasi)
        .map_err(|e| e.to_string())?;

    let stdout = wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(1 << 20);
    let stderr = wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(1 << 20);
    let stdin_pipe = wasmtime_wasi::p2::pipe::MemoryInputPipe::new(stdin.to_string());

    let wasi = WasiCtxBuilder::new()
        .stdin(stdin_pipe)
        .stdout(stdout.clone())
        .stderr(stderr.clone())
        .preopened_dir(
            job_dir,
            ".",
            wasmtime_wasi::DirPerms::all(),
            wasmtime_wasi::FilePerms::all(),
        )
        .map_err(|e| e.to_string())?
        .build_p1();

    let mut store = Store::new(
        &engine,
        HostState {
            wasi,
            limiter: MemLimiter {
                max_bytes: max_memory_bytes,
            },
        },
    );
    store.limiter(|s| &mut s.limiter);
    store.set_epoch_deadline(1);

    let engine_for_ticker = engine.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let ticker = std::thread::spawn(move || {
        // `timeout: None` is the "Infinite" run-timeout choice: block on
        // `done_rx` with no deadline instead of `recv_timeout`, so the epoch
        // is never incremented and the module runs to completion however
        // long that takes.
        let fired = match timeout {
            Some(d) => done_rx.recv_timeout(d).is_err(),
            None => done_rx.recv().is_err(),
        };
        if fired {
            engine_for_ticker.increment_epoch();
        }
    });

    let instance = linker.instantiate(&mut store, &module);
    let outcome = match instance.and_then(|i| {
        let start = i.get_typed_func::<(), ()>(&mut store, "_start")?;
        start.call(&mut store, ())
    }) {
        Ok(()) => Outcome::Exited(0),
        Err(e) => {
            if let Some(exit) = e.downcast_ref::<wasmtime_wasi::I32Exit>() {
                Outcome::Exited(exit.0)
            } else if matches!(
                e.downcast_ref::<wasmtime::Trap>(),
                Some(wasmtime::Trap::Interrupt)
            ) {
                Outcome::TimedOut
            } else {
                // The interesting part of a wasm failure is usually in the
                // cause chain, not the top-level message: an uncaught C++
                // exception surfaces as "error while executing at wasm
                // backtrace: ..." with "thrown Wasm exception" only appearing
                // as a cause. Flatten the chain so the student sees it - and so
                // `thread_failure_hint` can recognise it.
                let mut msg = e.to_string();
                for cause in e.chain().skip(1) {
                    msg.push_str(&format!("\n  caused by: {cause}"));
                }
                Outcome::Trapped(msg)
            }
        }
    };
    let _ = done_tx.send(());
    let _ = ticker.join();

    let out_text = String::from_utf8_lossy(&stdout.contents()).into_owned();
    let err_text = String::from_utf8_lossy(&stderr.contents()).into_owned();
    Ok((outcome, out_text, err_text))
}

fn job_dir(root: &Path) -> PathBuf {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let d = root.join("workdir").join(&id[..12]);
    std::fs::create_dir_all(&d).ok();
    d
}

fn write_sources(dir: &Path, files: &[File]) {
    for f in files {
        let p = dir.join(&f.name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&p, &f.content).ok();
    }
}

fn source_list(files: &[File]) -> Vec<String> {
    let exts = ["cpp", "cc", "cxx", "c"];
    let mut s: Vec<_> = files
        .iter()
        .filter(|f| exts.iter().any(|e| f.name.ends_with(e)))
        .map(|f| f.name.clone())
        .collect();
    if s.is_empty() {
        s = files.iter().map(|f| f.name.clone()).collect();
    }
    s
}

fn include_flags(files: &[File]) -> String {
    let mut dirs = vec![".".to_string()];
    for f in files {
        let mut p = Path::new(&f.name).parent().map(|x| x.to_path_buf());
        while let Some(pp) = p {
            let s = pp.to_string_lossy().into_owned();
            if !s.is_empty() && !dirs.contains(&s) {
                dirs.push(s);
            }
            p = pp.parent().map(|x| x.to_path_buf());
        }
    }
    dirs.iter()
        .map(|d| format!("-I{d}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// Compile then run under wasmtime. Same JSON shape as
/// `sandbox::compile_and_run` - callers can't tell the difference.
pub async fn compile_and_run(
    root: &Path,
    files: &[File],
    stdin: &str,
    std: &str,
    timeout: Option<Duration>,
) -> Value {
    compile_and_run_with_flags(root, files, stdin, std, "", timeout).await
}

/// Same as `compile_and_run`, plus a project's extra toolchain flags (e.g.
/// `-Werror`/`-O0` from `storage::flags_to_extra`) - used by
/// `sandbox::make_and_run`. Callers must not pass `-fsanitize=...` here,
/// see `wants_sanitizer`.
pub async fn compile_and_run_with_flags(
    root: &Path,
    files: &[File],
    stdin: &str,
    std: &str,
    extra: &str,
    timeout: Option<Duration>,
) -> Value {
    let dir = job_dir(root);
    write_sources(&dir, files);
    let sources = source_list(files);
    let inc = include_flags(files);
    let wasm_path = dir.join("out.wasm");

    let root_owned = root.to_path_buf();
    let dir_owned = dir.clone();
    let std_owned = std.to_string();
    let extra_owned = extra.to_string();
    let wasm_path_owned = wasm_path.clone();
    let compile_result = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::task::spawn_blocking(move || {
            compile(
                &root_owned,
                &dir_owned,
                &sources,
                &inc,
                &std_owned,
                &extra_owned,
                &wasm_path_owned,
            )
        }),
    )
    .await;

    let output = match compile_result {
        Ok(Ok(Ok(o))) => o,
        Ok(Ok(Err(e))) => {
            cleanup(&dir);
            return json!({ "ok": false, "stage": "compile", "compile_output": format!("Compilation error: {e}"), "run_output": "", "backend": "wasm" });
        }
        Ok(Err(e)) => {
            cleanup(&dir);
            return json!({ "ok": false, "stage": "compile", "compile_output": format!("Compilation task error: {e}"), "run_output": "", "backend": "wasm" });
        }
        Err(_) => {
            cleanup(&dir);
            return json!({ "ok": false, "stage": "compile", "compile_output": "Compilation timed out (60s)", "run_output": "", "backend": "wasm" });
        }
    };
    let mut compile_text = String::from_utf8_lossy(&output.stdout).into_owned();
    compile_text.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() || !wasm_path.exists() {
        cleanup(&dir);
        let text = if compile_text.trim().is_empty() {
            "Compilation failed (no output)".to_string()
        } else {
            compile_text
        };
        return json!({ "ok": false, "stage": "compile", "compile_output": text, "run_output": "", "backend": "wasm" });
    }

    let stdin_owned = stdin.to_string();
    let job_for_run = dir.clone();
    let run_result = tokio::task::spawn_blocking(move || {
        run_wasm_blocking(
            &wasm_path,
            &job_for_run,
            &stdin_owned,
            timeout,
            MAX_MEMORY_BYTES,
        )
    })
    .await;
    cleanup(&dir);

    run_outcome_to_json(run_result, &compile_text, mentions_thread_spawn(files))
}

type RunOutcome = Result<Result<(Outcome, String, String), String>, tokio::task::JoinError>;

/// Shared by every caller that's already run a wasm binary via
/// `run_wasm_blocking` and just needs the common `{ok,stage,compile_output,
/// run_output,...}` JSON shape - `compile_and_run_with_flags` and
/// `make_and_run_via_makefile` differ only in how they got to a compiled
/// `.wasm` file, not in how a run outcome becomes a response.
fn run_outcome_to_json(run_result: RunOutcome, compile_text: &str, spawns_threads: bool) -> Value {
    match run_result {
        Ok(Ok((Outcome::Exited(code), out_text, err_text))) => {
            let mut run_text = out_text;
            run_text.push_str(&err_text);
            json!({ "ok": code == 0, "stage": "run", "compile_output": compile_text, "run_output": run_text, "exit_code": code, "timed_out": false, "backend": "wasm" })
        }
        Ok(Ok((Outcome::TimedOut, out_text, err_text))) => {
            let mut run_text = out_text;
            run_text.push_str(&err_text);
            json!({ "ok": false, "stage": "run", "compile_output": compile_text, "run_output": run_text, "timed_out": true, "backend": "wasm" })
        }
        Ok(Ok((Outcome::Trapped(msg), out_text, err_text))) => {
            let mut run_text = out_text;
            run_text.push_str(&err_text);
            run_text.push_str(&format!("\n{msg}"));
            // A trap is where a failed std::thread lands, indistinguishable
            // from any other uncaught exception - explain it when the sources
            // back that reading up.
            if let Some(hint) = thread_failure_hint(spawns_threads, &run_text) {
                run_text.push_str(&format!("\n\n{hint}"));
            }
            json!({ "ok": false, "stage": "run", "compile_output": compile_text, "run_output": run_text, "exit_code": -1, "timed_out": false, "backend": "wasm" })
        }
        Ok(Err(e)) => {
            json!({ "ok": false, "stage": "run", "compile_output": compile_text, "run_output": format!("run error: {e}"), "backend": "wasm" })
        }
        Err(e) => {
            json!({ "ok": false, "stage": "run", "compile_output": compile_text, "run_output": format!("run task error: {e}"), "backend": "wasm" })
        }
    }
}

/// Build a Makefile-based project by invoking its OWN `make` - natively, on
/// the host, not sandboxed (only the resulting wasm binary's *execution* is,
/// via wasmtime below) - with `CXX`/`CXXFLAGS` overridden on the command
/// line to target wasm32-wasip1 via the bundled clang++.
///
/// Unlike `compile_and_run_with_flags` (which hand-compiles a synthetic file
/// list into a throwaway scratch dir with CPPBox's own flag set), this runs
/// in the real project directory (`dir`) using the project's actual
/// Makefile, so both a project's own CXXFLAGS/preprocessor defines *and* any
/// non-source assets it reads at runtime (data files, fixtures) are
/// respected - mirroring what `sandbox::make_and_run`'s podman path already
/// does, minus podman. `CXX=`/`CXXFLAGS=` are passed as `make` command-line
/// arguments (not environment variables) because command-line variable
/// assignments are the only thing GNU Make lets override a plain `CXX = ...`
/// assignment in the Makefile itself; an `override CXXFLAGS += ...` line
/// (as these course Makefiles use) still appends onto a command-line-
/// supplied CXXFLAGS, so the project's own extra flags/defines and this
/// function's wasm target flags combine correctly.
///
/// Still assumes the Makefile produces a binary literally named `app` - the
/// same convention `write_makefile`'s own template and the podman path rely
/// on. That's a separate, pre-existing limitation this function doesn't
/// change: a Makefile that builds something else (e.g. `main`) will compile
/// fine here but then fail to find `./app` to run.
///
/// Never deletes `dir` - unlike the scratch-job-dir functions in this file,
/// `dir` is the user's real project directory.
pub async fn make_and_run_via_makefile(
    root: &Path,
    dir: &Path,
    stdin: &str,
    clean: bool,
    timeout: Option<Duration>,
) -> Value {
    let clangxx = match bundled_clangxx(root) {
        Some(p) => p,
        None => {
            return json!({ "ok": false, "stage": "compile", "compile_output": "bundled clang++ not ready", "run_output": "", "backend": "wasm" })
        }
    };
    let sysroot = sysroot_dir(root);
    // -lc-printscan-long-double: see `compile()`'s comment - without it,
    // `long double` parsing/formatting (std::stold, printf/scanf %Lf) traps
    // at runtime instead of just failing to link if unused.
    let cxxflags = format!(
        "--target=wasm32-wasip1 --sysroot={} -fwasm-exceptions -mllvm -wasm-enable-eh -mllvm -wasm-use-legacy-eh=false -Wl,--initial-memory=67108864 -Wl,--max-memory={MAX_MEMORY_BYTES} -Wl,-z,stack-size={WASM_STACK_SIZE_BYTES} -lunwind -lc-printscan-long-double",
        sysroot.display()
    );

    if clean {
        // best-effort, same as the podman path's `make clean >/dev/null 2>&1 || true`
        let _ = tokio::process::Command::new("make")
            .arg("clean")
            .current_dir(dir)
            .output()
            .await;
    }
    let dir_owned = dir.to_path_buf();
    let clangxx_owned = clangxx.clone();
    let build = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::process::Command::new("make")
            .current_dir(&dir_owned)
            .arg(format!("CXX={}", clangxx_owned.display()))
            .arg(format!("CXXFLAGS={cxxflags}"))
            .output(),
    )
    .await;
    let out = match build {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return json!({ "ok": false, "stage": "compile", "compile_output": format!("make error: {e}"), "run_output": "", "backend": "wasm" })
        }
        Err(_) => {
            return json!({ "ok": false, "stage": "compile", "compile_output": "Build timed out (60s)", "run_output": "", "backend": "wasm" })
        }
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let wasm_path = dir.join("app");
    if !out.status.success() {
        let text = if text.trim().is_empty() {
            "Compilation failed (no output)".to_string()
        } else {
            text
        };
        return json!({ "ok": false, "stage": "compile", "compile_output": text, "run_output": "", "backend": "wasm" });
    }
    if !wasm_path.exists() {
        // `make` succeeded, but this project's Makefile doesn't build a
        // binary named `app` (CPPBox's build/run convention, matching
        // `write_makefile`'s own template and the podman path) — say so
        // explicitly rather than mislabeling a successful build as a
        // compile failure with no visible error.
        return json!({ "ok": false, "stage": "compile", "compile_output": format!("{text}\n\n⚠ `make` succeeded, but produced no `./app` binary. CPPBox expects the Makefile's build target to be named `app` — check what this Makefile actually outputs (e.g. `main`) and rename it, or add an `app` alias target."), "run_output": "", "backend": "wasm" });
    }

    let stdin_owned = stdin.to_string();
    let job_for_run = dir.to_path_buf();
    let run_result = tokio::task::spawn_blocking(move || {
        run_wasm_blocking(
            &wasm_path,
            &job_for_run,
            &stdin_owned,
            timeout,
            MAX_MEMORY_BYTES,
        )
    })
    .await;
    // NOTE: no `cleanup(dir)` here, deliberately - `dir` is the real project
    // directory, not a scratch job_dir; deleting it would delete the user's
    // project.

    run_outcome_to_json(run_result, &text, dir_mentions_thread_spawn(dir))
}
