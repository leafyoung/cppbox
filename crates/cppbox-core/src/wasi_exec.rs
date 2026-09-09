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
//! Thread-using code (`uses_threading`) still routes to `sandbox.rs`'s
//! podman path: `wasm32-wasip1-threads` is confirmed non-functional with
//! today's wasi-sdk/wasmtime pairing (spawned threads trap on
//! `uninitialized element`), not something this module works around.
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

/// Conservative heuristic: any of these headers textually present routes to
/// podman instead. False positives (falling back when not strictly needed)
/// are harmless; false negatives are not, so this errs inclusive.
pub fn uses_threading(files: &[File]) -> bool {
    const MARKERS: &[&str] = &[
        "<thread>",
        "<future>",
        "<mutex>",
        "<condition_variable>",
        "<atomic>",
        "<shared_mutex>",
    ];
    files
        .iter()
        .any(|f| MARKERS.iter().any(|m| f.content.contains(m)))
}

fn compile(
    root: &Path,
    dir: &Path,
    sources: &[String],
    inc: &str,
    std: &str,
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
        .arg("-Wl,--max-memory=268435456")
        .arg("-lunwind")
        .args(inc.split_whitespace())
        .args(sources)
        .arg("-o")
        .arg(out)
        .output()
        .map_err(|e| e.to_string())
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
    timeout: Duration,
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
        if done_rx.recv_timeout(timeout).is_err() {
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
                Outcome::Trapped(e.to_string())
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
pub async fn compile_and_run(root: &Path, files: &[File], stdin: &str, std: &str) -> Value {
    let dir = job_dir(root);
    write_sources(&dir, files);
    let sources = source_list(files);
    let inc = include_flags(files);
    let wasm_path = dir.join("out.wasm");

    let root_owned = root.to_path_buf();
    let dir_owned = dir.clone();
    let std_owned = std.to_string();
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
                &wasm_path_owned,
            )
        }),
    )
    .await;

    let output = match compile_result {
        Ok(Ok(Ok(o))) => o,
        Ok(Ok(Err(e))) => {
            cleanup(&dir);
            return json!({ "ok": false, "stage": "compile", "compile_output": format!("Compilation error: {e}"), "run_output": "" });
        }
        Ok(Err(e)) => {
            cleanup(&dir);
            return json!({ "ok": false, "stage": "compile", "compile_output": format!("Compilation task error: {e}"), "run_output": "" });
        }
        Err(_) => {
            cleanup(&dir);
            return json!({ "ok": false, "stage": "compile", "compile_output": "Compilation timed out (60s)", "run_output": "" });
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
        return json!({ "ok": false, "stage": "compile", "compile_output": text, "run_output": "" });
    }

    let stdin_owned = stdin.to_string();
    let job_for_run = dir.clone();
    let run_result = tokio::task::spawn_blocking(move || {
        run_wasm_blocking(
            &wasm_path,
            &job_for_run,
            &stdin_owned,
            Duration::from_secs(15),
            256 * 1024 * 1024,
        )
    })
    .await;
    cleanup(&dir);

    match run_result {
        Ok(Ok((Outcome::Exited(code), out_text, err_text))) => {
            let mut run_text = out_text;
            run_text.push_str(&err_text);
            json!({ "ok": code == 0, "stage": "run", "compile_output": compile_text, "run_output": run_text, "exit_code": code, "timed_out": false })
        }
        Ok(Ok((Outcome::TimedOut, out_text, err_text))) => {
            let mut run_text = out_text;
            run_text.push_str(&err_text);
            json!({ "ok": false, "stage": "run", "compile_output": compile_text, "run_output": run_text, "timed_out": true })
        }
        Ok(Ok((Outcome::Trapped(msg), out_text, err_text))) => {
            let mut run_text = out_text;
            run_text.push_str(&err_text);
            run_text.push_str(&format!("\n{msg}"));
            json!({ "ok": false, "stage": "run", "compile_output": compile_text, "run_output": run_text, "exit_code": -1, "timed_out": false })
        }
        Ok(Err(e)) => {
            json!({ "ok": false, "stage": "run", "compile_output": compile_text, "run_output": format!("run error: {e}") })
        }
        Err(e) => {
            json!({ "ok": false, "stage": "run", "compile_output": compile_text, "run_output": format!("run task error: {e}") })
        }
    }
}
