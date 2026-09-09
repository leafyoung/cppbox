//! Compile + run non-threaded student C++ by targeting `wasm32-wasip1` with
//! the host's own native `clang++`, then executing the result sandboxed
//! under `wasmtime` - replacing podman for the common case. See
//! `docs/WASM_PLAN.md` in the repo root for the full design and the
//! Phase 0 spike this was ported from (`src_v2/`).
//!
//! Thread-using code (`uses_threading`) still routes to `sandbox.rs`'s
//! podman path: `wasm32-wasip1-threads` is confirmed non-functional with
//! today's wasi-sdk/wasmtime pairing (spawned threads trap on
//! `uninitialized element`), not something this module works around.
//!
//! If the host is missing `wasm-ld` (this project doesn't bundle it yet -
//! see Phase 2), the toolchain is marked unready and `sandbox.rs` falls
//! back to podman for *everything*, unchanged from today - this module is
//! purely opportunistic, never a regression.
use std::path::{Path, PathBuf};
use std::process::Stdio;
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
const BASE_URL: &str = "https://github.com/WebAssembly/wasi-sdk/releases/download";

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

fn sysroot_dir(root: &Path) -> PathBuf {
    toolchain_dir(root).join(format!("wasi-sysroot-{WASI_SDK_VERSION}"))
}

fn resource_dir(root: &Path) -> PathBuf {
    toolchain_dir(root).join("resource-dir")
}

fn wasm_ld_present() -> bool {
    std::process::Command::new("wasm-ld")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Download/assemble the wasm32-wasip1 sysroot + resource-dir overlay.
/// Blocking; call from a background thread at startup (mirrors
/// `sandbox::ensure_sandbox_image()`).
pub fn ensure_wasi_toolchain(root: &Path) {
    if std::process::Command::new("clang++")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        set_state(3, "no native clang++ on PATH".into());
        return;
    }
    if !wasm_ld_present() {
        set_state(
            3,
            "wasm-ld not found (install LLVM's lld) - falling back to podman for all compile/run"
                .into(),
        );
        return;
    }

    let dir = toolchain_dir(root);
    let sysroot = sysroot_dir(root);
    let resdir = resource_dir(root);
    if sysroot.join("include").exists() && resdir.join("include").exists() {
        set_state(2, format!("wasi toolchain ready ({})", sysroot.display()));
        return;
    }
    set_state(1, "assembling wasi toolchain...".into());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        set_state(3, format!("creating {}: {e}", dir.display()));
        return;
    }

    for (asset, dest_name) in [
        (
            format!("wasi-sysroot-{WASI_SDK_VERSION}.tar.gz"),
            "wasi-sysroot.tar.gz",
        ),
        (
            format!("libclang_rt-{WASI_SDK_VERSION}.tar.gz"),
            "libclang_rt.tar.gz",
        ),
    ] {
        let dest = dir.join(dest_name);
        if dest.exists() {
            continue;
        }
        let url = format!("{BASE_URL}/{WASI_SDK_TAG}/{asset}");
        set_state(1, format!("downloading {asset}..."));
        if let Err(e) = download_blocking(&url, &dest) {
            set_state(3, format!("downloading {asset}: {e}"));
            return;
        }
    }
    for (archive, _label) in [
        ("wasi-sysroot.tar.gz", "sysroot"),
        ("libclang_rt.tar.gz", "builtins"),
    ] {
        let out = std::process::Command::new("tar")
            .args(["xzf", archive])
            .current_dir(&dir)
            .output();
        match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                set_state(
                    3,
                    format!(
                        "extracting {archive}: {}",
                        String::from_utf8_lossy(&o.stderr)
                    ),
                );
                return;
            }
            Err(e) => {
                set_state(3, format!("extracting {archive}: {e}"));
                return;
            }
        }
    }
    let _ = std::fs::remove_file(dir.join("wasi-sysroot.tar.gz"));
    let _ = std::fs::remove_file(dir.join("libclang_rt.tar.gz"));

    if let Err(e) = build_resource_dir_overlay(&dir, &resdir) {
        set_state(3, format!("building resource-dir overlay: {e}"));
        return;
    }

    set_state(2, format!("wasi toolchain ready ({})", sysroot.display()));
}

fn download_blocking(url: &str, dest: &Path) -> Result<(), String> {
    let resp = reqwest::blocking::get(url).map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().map_err(|e| e.to_string())?;
    std::fs::write(dest, &bytes).map_err(|e| e.to_string())
}

/// Symlink the host clang's own resource dir (includes, native-arch libs)
/// and add the wasm32 compiler-rt builtins from `libclang_rt-*.tar.gz` -
/// see `src_v2/toolchain/setup.sh` for the reasoning (clang needs both its
/// native builtins and the wasm32 ones, and copying 86MB of headers is
/// wasteful when a symlink works).
fn build_resource_dir_overlay(toolchain_dir: &Path, resdir: &Path) -> Result<(), String> {
    let host_resdir = std::process::Command::new("clang++")
        .arg("-print-resource-dir")
        .output()
        .map_err(|e| e.to_string())?;
    let host_resdir = String::from_utf8_lossy(&host_resdir.stdout)
        .trim()
        .to_string();
    let host_resdir = PathBuf::from(host_resdir);

    std::fs::create_dir_all(resdir.join("lib")).map_err(|e| e.to_string())?;
    symlink_force(&host_resdir.join("include"), &resdir.join("include"))?;
    for entry in std::fs::read_dir(host_resdir.join("lib")).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        symlink_force(&entry.path(), &resdir.join("lib").join(entry.file_name()))?;
    }

    let libclang_rt = toolchain_dir.join(format!("libclang_rt-{WASI_SDK_VERSION}"));
    let target = "wasm32-unknown-wasip1";
    let src = libclang_rt.join(target).join("libclang_rt.builtins.a");
    let dest_dir = resdir.join("lib").join(target);
    std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
    std::fs::copy(&src, dest_dir.join("libclang_rt.builtins.a")).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(unix)]
fn symlink_force(target: &Path, link: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(link);
    std::os::unix::fs::symlink(target, link).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn symlink_force(target: &Path, link: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(link);
    let _ = std::fs::remove_file(link);
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link).map_err(|e| e.to_string())
    } else {
        std::os::windows::fs::symlink_file(target, link).map_err(|e| e.to_string())
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
) -> std::io::Result<std::process::Output> {
    let sysroot = sysroot_dir(root);
    let resdir = resource_dir(root);
    std::process::Command::new("clang++")
        .current_dir(dir)
        .arg("--target=wasm32-wasip1")
        .arg(format!("--sysroot={}", sysroot.display()))
        .arg(format!("-resource-dir={}", resdir.display()))
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
