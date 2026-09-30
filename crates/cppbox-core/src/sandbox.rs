//! Isolated compile/run via podman (preferred) or docker, plus host-side
//! clang-format and clang++ syntax check. Mirrors backend/sandbox.py.
//!
//! `compile_and_run` and `make_and_run` now prefer the `wasi_exec`
//! (wasmtime) path for non-threaded, non-sanitizer code when that toolchain
//! is ready, falling back to podman below otherwise - see
//! docs/WASM_PLAN.md. `compile_debug` prefers running natively (no
//! container) when the host has clang++ + lldb-dap - see
//! `native_debug_available`/`debug.rs`.
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;

/// Canonical registry image (public on ghcr; published by release CI).
pub const GHCR_IMAGE: &str = "ghcr.io/leafyoung/cppbox-sandbox:latest";

/// Resolve which sandbox image to use:
///   1. $CPPBOX_SANDBOX_IMAGE (explicit override)
///   2. a locally-built unqualified tag `cpp-sandbox:latest` (dev machines)
///   3. ghcr.io/leafyoung/cppbox-sandbox:latest (fresh installs pull from ghcr)
/// Podman is off: wasm is the only execution backend.
///
/// Nothing sets the image up at startup and no request routes to the podman
/// path, so there is no fallback - code wasm cannot serve (threads,
/// `<execution>`, sanitizers) now reports why instead of quietly running
/// somewhere else. The podman implementation below is left intact and
/// compiling; re-enabling it means flipping this to `true` and restoring the
/// `ensure_sandbox_image()` calls in `bin/cppbox-server.rs` and
/// `src-tauri/src/main.rs`.
///
/// Debugging is unaffected only where the host has `clang++` + `lldb-dap`
/// (`native_debug_available()`); the containerised debug path is disabled with
/// everything else.
pub const PODMAN_ENABLED: bool = false;

pub fn sandbox_image() -> String {
    if let Ok(v) = std::env::var("CPPBOX_SANDBOX_IMAGE") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    if image_exists("cpp-sandbox:latest") {
        return "cpp-sandbox:latest".into();
    }
    GHCR_IMAGE.into()
}

fn image_exists(reference: &str) -> bool {
    std::process::Command::new(runtime())
        .args(["image", "inspect", reference])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Sandbox readiness shared with the status endpoint: 0 unknown, 1 pulling,
/// 2 ready, 3 failed.
static SANDBOX_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static SANDBOX_MSG: OnceLock<String> = OnceLock::new();

pub fn sandbox_state() -> (u8, String) {
    (
        SANDBOX_STATE.load(std::sync::atomic::Ordering::Relaxed),
        SANDBOX_MSG.get().cloned().unwrap_or_default(),
    )
}

/// Initialization: make sure the sandbox image is present (pull from ghcr on a
/// fresh install) and prove it works by compiling+running a sample program.
/// Blocking; call from a background thread at startup.
pub fn ensure_sandbox_image() {
    if !PODMAN_ENABLED {
        // Nothing to pull or smoke-test: wasm is the only backend. Left as a
        // guard rather than deleting the callers so re-enabling is one const.
        SANDBOX_MSG.get_or_init(|| "podman backend disabled (wasm only)".to_string());
        return;
    }
    let rt = runtime();
    let set = |code: u8, msg: String| {
        SANDBOX_STATE.store(code, std::sync::atomic::Ordering::Relaxed);
        let _ = SANDBOX_MSG.set(msg.clone());
        tracing::info!("sandbox init: {msg}");
        eprintln!("sandbox init: {msg}");
    };
    let image = sandbox_image();
    if image_exists(&image) {
        set(1, format!("image {image} present"));
    } else {
        set(1, format!("image {image} missing, pulling from ghcr…"));
        let out = std::process::Command::new(rt)
            .args(["pull", &image])
            .output();
        match out {
            Ok(o) if o.status.success() => set(1, format!("pulled {image}")),
            Ok(o) => {
                let tail = String::from_utf8_lossy(&o.stderr);
                let tail = tail.lines().last().unwrap_or("").to_string();
                set(3, format!("pull {image} failed: {tail}"));
                return;
            }
            Err(e) => {
                set(3, format!("pull {image} failed: {e}"));
                return;
            }
        }
    }
    // smoke test: compile and run a sample program inside the container
    let test = "printf 'int main(){return 0;}' > /home/sandbox/t.cpp && clang++ -std=c++17 /home/sandbox/t.cpp -o /home/sandbox/t && /home/sandbox/t";
    let out = std::process::Command::new(rt)
        .args([
            "run",
            "--rm",
            "--network",
            "none",
            "--memory",
            "512m",
            "--security-opt",
            "label=disable",
            &image,
            "sh",
            "-c",
            test,
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => set(2, format!("sandbox ready ({image} smoke test ok)")),
        Ok(o) => {
            let tail = String::from_utf8_lossy(&o.stderr);
            let tail = tail.lines().last().unwrap_or("").to_string();
            set(3, format!("smoke test failed on {image}: {tail}"));
        }
        Err(e) => set(3, format!("smoke test failed on {image}: {e}")),
    }
}

/// Host clang++ + lldb-dap present, so `debug.rs` can run a debug session
/// natively instead of in podman's ptrace-enabled container. See
/// docs/WASM_PLAN.md's Phase 3: a student debugging their own process
/// isn't an adversarial scenario, so podman's ptrace container wasn't
/// buying real security here, just packaging - but bundling a *native*
/// debugger the way Phase 2 bundled the wasm toolchain would need a full
/// native clang++ (wasi-sdk's is wasm32-only) and lldb-dap has no slim
/// standalone release the way clangd does, so this still requires a host
/// install, falling back to podman when it's missing.
pub fn native_debug_available() -> bool {
    let ok = |bin: &str, arg: &str| {
        std::process::Command::new(bin)
            .arg(arg)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    };
    ok("clang++", "--version") && ok("lldb-dap", "--help")
}

/// `make` + run on the host, for `Backend::Native` in the Makefile flow. The
/// generated Makefile builds `./app`, same as the container path expects.
async fn make_and_run_native(
    dir: &Path,
    stdin: &str,
    clean: bool,
    timeout: Option<Duration>,
) -> Value {
    if clean {
        // best-effort, same as the other paths
        let _ = Command::new("make")
            .arg("clean")
            .current_dir(dir)
            .output()
            .await;
    }
    let build = match tokio::time::timeout(
        Duration::from_secs(60),
        Command::new("make")
            .current_dir(dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return json!({ "ok": false, "stage": "compile", "compile_output": format!("make error: {e}"), "run_output": "", "backend": "native" })
        }
        Err(_) => {
            return json!({ "ok": false, "stage": "compile", "compile_output": "Build timed out (60s)", "run_output": "", "backend": "native" })
        }
    };
    let mut text = String::from_utf8_lossy(&build.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&build.stderr));
    let binary = dir.join("app");
    if !build.status.success() || !binary.exists() {
        if text.trim().is_empty() {
            text = "Build failed".into();
        }
        return json!({ "ok": false, "stage": "compile", "compile_output": text, "run_output": "", "backend": "native" });
    }
    let rr = run_native_binary(&binary, stdin, timeout).await;
    json!({
        "ok": rr.exit_code == 0 && !rr.timed_out,
        "stage": "run",
        "compile_output": text,
        "run_output": rr.output,
        "exit_code": rr.exit_code,
        "timed_out": rr.timed_out,
        "backend": "native",
    })
}

/// Can we build and run natively? Only needs a host C++ compiler - unlike
/// `native_debug_available()` this does not require `lldb-dap`, because running
/// is not debugging.
///
/// Native runs the student's program as an ordinary host process: no container,
/// no seccomp, no memory cap. That is a deliberate trade for the backend the
/// user picks explicitly when they need threads, `<execution>` or sanitizers -
/// the same reasoning as the native debug path (a student running their own
/// code is not an adversary), and the reason it is never selected implicitly.
pub fn native_available() -> bool {
    std::process::Command::new("clang++")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Compile on the host with the project's standard and extra flags. Mirrors
/// `compile_debug_native` but honours `extra` (so `-O2`, `-fsanitize=...` and
/// friends work) instead of forcing a debug profile.
pub async fn compile_native(
    root: &Path,
    files: &[File],
    std: &str,
    extra: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let dir = job_dir(root);
    write_sources(&dir, files);
    let sources = source_list(files);
    let inc = include_flags(files);
    let mut c = Command::new("clang++");
    c.current_dir(&dir)
        .arg(format!("-std={std}"))
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-fcolor-diagnostics")
        .args(inc.split_whitespace())
        .args(extra.split_whitespace())
        .args(&sources)
        .arg("-o")
        .arg("a.out")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = match tokio::time::timeout(Duration::from_secs(60), c.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("compile error: {e}")),
        Err(_) => return Err("Compilation timed out (60s)".into()),
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let binary = dir.join("a.out");
    if out.status.success() && binary.exists() {
        Ok((binary, dir))
    } else {
        Err(if text.trim().is_empty() {
            "Compilation failed".into()
        } else {
            text
        })
    }
}

/// Run a host binary with an optional wall-clock timeout (`None` = run to
/// completion, however long that takes - the "Infinite" UI choice), merging
/// stdout and stderr the way the podman and wasm paths do. Killed on timeout
/// rather than left behind.
async fn run_native_binary(binary: &Path, stdin: &str, timeout: Option<Duration>) -> RunResult {
    let dir = binary.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut c = Command::new(binary);
    c.current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            return RunResult {
                output: format!("Execution error: {e}"),
                exit_code: -1,
                timed_out: false,
            }
        }
    };
    if let Some(mut si) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = si.write_all(stdin.as_bytes()).await;
    }
    let outcome = match timeout {
        Some(d) => tokio::time::timeout(d, child.wait_with_output())
            .await
            .map_err(|_| ()),
        None => Ok(child.wait_with_output().await),
    };
    match outcome {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            RunResult {
                output: text,
                exit_code: out.status.code().unwrap_or(-1),
                timed_out: false,
            }
        }
        Ok(Err(e)) => RunResult {
            output: format!("Execution error: {e}"),
            exit_code: -1,
            timed_out: false,
        },
        Err(()) => RunResult {
            output: format!("Timed out after {}s", timeout.unwrap().as_secs()),
            exit_code: -1,
            timed_out: true,
        },
    }
}

/// Compile and run natively: the `Backend::Native` half of both entry points.
async fn compile_and_run_native(
    root: &Path,
    files: &[File],
    stdin: &str,
    std: &str,
    extra: &str,
    timeout: Option<Duration>,
) -> Value {
    if !native_available() {
        return json!({ "ok": false, "stage": "compile", "compile_output": "The Native backend needs clang++ on this machine, and it isn't on PATH. Install it, or switch Sandbox to WASM.", "run_output": "", "backend": "native" });
    }
    let (binary, dir) = match compile_native(root, files, std, extra).await {
        Ok(v) => v,
        Err(text) => {
            return json!({ "ok": false, "stage": "compile", "compile_output": text, "run_output": "", "backend": "native" })
        }
    };
    let rr = run_native_binary(&binary, stdin, timeout).await;
    cleanup(&dir);
    json!({
        "ok": rr.exit_code == 0 && !rr.timed_out,
        "stage": "run",
        "compile_output": "",
        "run_output": rr.output,
        "exit_code": rr.exit_code,
        "timed_out": rr.timed_out,
        "backend": "native",
    })
}

/// Compile with debug symbols (-g -O0) directly on the host, no container -
/// used when `native_debug_available()`. Returns (binary, job_dir), same
/// contract as `compile_debug`.
pub async fn compile_debug_native(
    root: &Path,
    files: &[File],
    std: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let dir = job_dir(root);
    write_sources(&dir, files);
    let sources = source_list(files);
    let inc = include_flags(files);
    let mut c = Command::new("clang++");
    c.current_dir(&dir)
        .arg("-g")
        .arg("-O0")
        .arg("-fno-omit-frame-pointer")
        .arg(format!("-std={std}"))
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-fcolor-diagnostics")
        .args(inc.split_whitespace())
        .args(&sources)
        .arg("-o")
        .arg("a.out")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = match tokio::time::timeout(Duration::from_secs(60), c.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("compile error: {e}")),
        Err(_) => return Err("Compilation timed out (60s)".into()),
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let binary = dir.join("a.out");
    if out.status.success() && binary.exists() {
        Ok((binary, dir))
    } else {
        Err(if text.trim().is_empty() {
            "Compilation failed".into()
        } else {
            text
        })
    }
}

/// Compile with debug symbols (-g -O0) for gdb. Returns (binary, job_dir).
pub async fn compile_debug(
    root: &Path,
    files: &[File],
    std: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let dir = job_dir(root);
    write_sources(&dir, files);
    let sources = source_list(files);
    let inc = include_flags(files);
    let cmd_str = format!(
        "clang++ -g -O0 -fno-omit-frame-pointer -std={std} -Wall -Wextra -fcolor-diagnostics {inc} {src} -o a.out 2>&1",
        src = sources.join(" ")
    );
    let abs = dir.canonicalize().map_err(|e| e.to_string())?;
    let mut c = Command::new(runtime());
    c.args([
        "run",
        "--rm",
        "--network",
        "none",
        "--memory",
        "512m",
        "--cpus",
        "2",
        "--pids-limit",
        "50",
        "--read-only",
        "--security-opt",
        "label=disable",
    ])
    .arg("-v")
    .arg(format!("{}:/home/sandbox/work:rw", abs.display()))
    .args(["-w", "/home/sandbox/work"])
    .arg(sandbox_image())
    .args(["sh", "-c", &cmd_str])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let out = match tokio::time::timeout(Duration::from_secs(60), c.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("compile error: {e}")),
        Err(_) => return Err("Compilation timed out (60s)".into()),
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let binary = dir.join("a.out");
    if out.status.success() && binary.exists() {
        Ok((binary, dir))
    } else {
        Err(if text.trim().is_empty() {
            "Compilation failed".into()
        } else {
            text
        })
    }
}

/// User-selectable sandbox backend preference (a project's `flags.backend`
/// field, parsed by `from_flags` - see `storage::flags_to_extra` for the
/// sibling `werror`/`sanitize`/`opt` fields living in the same JSON blob).
/// `Auto` (the default when absent) keeps today's opportunistic
/// wasm-if-ready-and-compatible/else-podman behavior unchanged; `Wasm`/
/// `Podman` force one or the other. Threading/sanitizer use are hard gates
/// independent of this preference - wasm32-wasip1-threads is confirmed
/// non-functional (see docs/WASM_PLAN.md) and sanitizer support on
/// wasm32-wasip1 is unverified, so a `Wasm`-forced request for such code
/// returns a clear compile-stage error naming why, rather than silently
/// switching to podman - silently ignoring an explicit override would
/// defeat the point of asking for one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backend {
    /// wasm32-wasip1 under wasmtime. Fast, sandboxed, no threads.
    Wasm,
    /// Host clang++, host process. Slower to start, no sandbox, but supports
    /// everything the host does - threads, `<execution>`, sanitizers.
    Native,
    /// Retained but never selected: `from_flags` cannot produce it and
    /// `PODMAN_ENABLED` is false. Keeps the container path compiling (and one
    /// const away from returning) rather than deleting it.
    Podman,
}

impl Backend {
    pub fn from_flags(flags_json: Option<&str>) -> Backend {
        let v: Option<Value> = flags_json.and_then(|s| serde_json::from_str(s).ok());
        match v
            .as_ref()
            .and_then(|v| v.get("backend"))
            .and_then(|b| b.as_str())
        {
            Some("native") => Backend::Native,
            // wasm is the default, and the only other selectable backend.
            // Legacy values ("auto", "podman") land here rather than mapping to
            // Backend::Podman: podman is off (see PODMAN_ENABLED) and nothing
            // sets its image up, so a saved "podman" preference keeps working
            // by running on wasm instead of failing on a backend that is gone.
            _ => Backend::Wasm,
        }
    }
}

/// Default run timeout when a project has no explicit `flags.timeout` -
/// raised from the original 15s (found too tight for anything beyond a
/// single quick check: a real multi-question regression-test harness ran
/// ~50s legitimately, with no bug and no hang, just more work to do).
const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// `"30"`/`"60"` -> that many seconds, `"inf"` -> `None` (no timeout - the
/// run is awaited to completion, however long that takes). Absent or
/// unrecognised -> `DEFAULT_RUN_TIMEOUT`. Shared by `run_timeout` (reads a
/// project's saved `flags.timeout`) and the ad-hoc `/api/run` endpoint
/// (which has no project/flags, just this same raw string straight off the
/// request).
pub fn parse_run_timeout(raw: Option<&str>) -> Option<Duration> {
    match raw {
        Some("inf") => None,
        Some("30") => Some(Duration::from_secs(30)),
        Some("60") => Some(Duration::from_secs(60)),
        _ => Some(DEFAULT_RUN_TIMEOUT),
    }
}

/// Per-project run timeout from `flags.timeout` (same JSON blob as
/// `backend`/`werror`/`sanitize`/`opt`). This bounds the *run* stage only;
/// compilation keeps its own separate, fixed 60s timeout.
pub fn run_timeout(flags_json: Option<&str>) -> Option<Duration> {
    let v: Option<Value> = flags_json.and_then(|s| serde_json::from_str(s).ok());
    parse_run_timeout(
        v.as_ref()
            .and_then(|v| v.get("timeout"))
            .and_then(|t| t.as_str()),
    )
}

/// The per-run knobs `make_and_run` needs, bundled to keep its own arg count
/// (and every call site) manageable now that `backend` joined `std`/`extra`/
/// `clean`.
pub struct RunOpts<'a> {
    pub stdin: &'a str,
    pub std: &'a str,
    pub extra: &'a str,
    pub clean: bool,
    pub backend: Backend,
    pub timeout: Option<Duration>,
}

/// Build with make (incremental) then run ./app. Ensures a Makefile exists.
pub async fn make_and_run(
    root: &Path,
    pid: &str,
    local_path: Option<&str>,
    opts: RunOpts<'_>,
) -> Value {
    let RunOpts {
        stdin,
        std,
        extra,
        clean,
        backend,
        timeout,
    } = opts;
    let dir = crate::storage::project_root(root, pid, local_path);
    // regenerate the Makefile when absent or when it's still our own
    // auto-generated one (e.g. missing the $(info ...) command echo) —
    // user-authored Makefiles are left untouched (needed regardless of which
    // backend ends up building it)
    if crate::storage::makefile_is_ours_or_absent(root, pid, local_path) {
        crate::storage::write_makefile(root, pid, local_path, std, extra);
    }

    // The backend is whatever the user picked - there is no fallback in either
    // direction. Wasm reports a specific reason when it cannot serve the code
    // (and names Native as the answer); Native reports when clang++ is missing.
    let use_wasm = match backend {
        Backend::Native => {
            let src = crate::storage::collect_source_files(root, pid, local_path);
            if src.is_empty() {
                return json!({ "ok": false, "stage": "compile", "compile_output": "No source files found.", "run_output": "", "backend": "native" });
            }
            if !native_available() {
                return json!({ "ok": false, "stage": "compile", "compile_output": "The Native backend needs clang++ on this machine, and it isn't on PATH. Install it, or switch Sandbox to WASM.", "run_output": "", "backend": "native" });
            }
            return make_and_run_native(&dir, stdin, clean, timeout).await;
        }
        Backend::Podman => false,
        Backend::Wasm => {
            let src = crate::storage::collect_source_files(root, pid, local_path);
            if src.is_empty() {
                return json!({ "ok": false, "stage": "compile", "compile_output": "No source files found.", "run_output": "", "backend": "wasm" });
            }
            if crate::wasi_exec::wants_sanitizer(extra) {
                return json!({ "ok": false, "stage": "compile", "compile_output": "Sanitizers (-fsanitize=...) aren't supported by the WASM sandbox, which is the only backend in this build.", "run_output": "", "backend": "wasm" });
            }
            let files: Vec<File> = src
                .into_iter()
                .map(|(name, content)| File { name, content })
                .collect();
            if let Some(reason) = crate::wasi_exec::wasm_unsupported_reason(&files) {
                return json!({ "ok": false, "stage": "compile", "compile_output": format!("{reason}. The WASM sandbox is the only backend in this build."), "run_output": "", "backend": "wasm" });
            }
            if !crate::wasi_exec::is_ready() {
                let (_, msg) = crate::wasi_exec::wasi_state();
                return json!({ "ok": false, "stage": "compile", "compile_output": format!("WASM toolchain isn't ready yet: {msg}. Wait for it to finish downloading and try again."), "run_output": "", "backend": "wasm" });
            }
            true
        }
    };

    if use_wasm {
        return crate::wasi_exec::make_and_run_via_makefile(root, &dir, stdin, clean, timeout)
            .await;
    }

    // ── podman path ──
    let abs = match dir.canonicalize() {
        Ok(a) => a,
        Err(e) => {
            return json!({ "ok": false, "stage": "compile", "compile_output": format!("project dir error: {e}"), "run_output": "", "backend": "podman" })
        }
    };
    // 1) make (incremental; with clean, 'make clean' first, best-effort)
    let build_cmd = if clean {
        "make clean >/dev/null 2>&1 || true; make 2>&1"
    } else {
        "make 2>&1"
    };
    let mut c = Command::new(runtime());
    c.args([
        "run",
        "--rm",
        "--user",
        "0",
        "--network",
        "none",
        "--memory",
        "512m",
        "--cpus",
        "2",
        "--pids-limit",
        "50",
        "--security-opt",
        "label=disable",
    ])
    .arg("-v")
    .arg(format!("{}:/home/sandbox/work:rw", abs.display()))
    .args(["-w", "/home/sandbox/work"])
    .arg(sandbox_image())
    .args(["sh", "-c", build_cmd])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let out = match tokio::time::timeout(Duration::from_secs(60), c.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return json!({ "ok": false, "stage": "compile", "compile_output": format!("make error: {e}"), "run_output": "", "backend": "podman" })
        }
        Err(_) => {
            return json!({ "ok": false, "stage": "compile", "compile_output": "Build timed out (60s)", "run_output": "", "backend": "podman" })
        }
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        return json!({ "ok": false, "stage": "compile", "compile_output": text, "run_output": "", "backend": "podman" });
    }
    // 2) run the built executable
    let mut r = Command::new(runtime());
    r.args([
        "run",
        "--rm",
        "-i",
        "--network",
        "none",
        "--memory",
        "256m",
        "--cpus",
        "1",
        "--pids-limit",
        "30",
        "--read-only",
        "--security-opt",
        "label=disable",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
    ])
    .arg("-v")
    .arg(format!("{}:/home/sandbox/work:ro", abs.display()))
    .args(["-w", "/home/sandbox/work"])
    .arg(sandbox_image())
    .args(["sh", "-c", "timeout 15 ./app 2>&1"])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = match r.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            return json!({ "ok": false, "stage": "run", "compile_output": text, "run_output": format!("run error: {e}"), "backend": "podman" })
        }
    };
    if let Some(mut sin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = sin.write_all(stdin.as_bytes()).await;
    }
    match tokio::time::timeout(Duration::from_secs(25), child.wait_with_output()).await {
        Ok(Ok(o)) => {
            let mut rtext = String::from_utf8_lossy(&o.stdout).into_owned();
            rtext.push_str(&String::from_utf8_lossy(&o.stderr));
            let code = o.status.code().unwrap_or(-1);
            json!({ "ok": code == 0, "stage": "run", "compile_output": text, "run_output": rtext, "exit_code": code, "timed_out": code == 124, "backend": "podman" })
        }
        Ok(Err(e)) => {
            json!({ "ok": false, "stage": "run", "compile_output": text, "run_output": format!("run error: {e}"), "backend": "podman" })
        }
        Err(_) => {
            json!({ "ok": false, "stage": "run", "compile_output": text, "run_output": "Execution timed out.", "timed_out": true, "backend": "podman" })
        }
    }
}

/// Pick the container CLI once: podman (rootless) preferred, docker fallback.
pub fn runtime() -> &'static str {
    static RT: OnceLock<&'static str> = OnceLock::new();
    RT.get_or_init(|| {
        for c in ["podman", "docker"] {
            if std::process::Command::new(c)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok()
            {
                return c;
            }
        }
        "podman"
    })
}

pub struct File {
    pub name: String,
    pub content: String,
}

struct CompileResult {
    success: bool,
    output: String,
    binary: Option<PathBuf>,
}

struct RunResult {
    output: String,
    exit_code: i32,
    timed_out: bool,
}

fn job_dir(root: &Path) -> PathBuf {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let d = root.join("workdir").join(&id[..12]);
    std::fs::create_dir_all(&d).ok();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o777)).ok();
    }
    d
}

fn write_sources(dir: &Path, files: &[File]) {
    for f in files {
        let p = dir.join(&f.name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&p, &f.content).ok();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o666)).ok();
        }
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
        let parent = Path::new(&f.name).parent();
        let mut p = parent.map(|x| x.to_path_buf());
        while let Some(pp) = p {
            let s = pp.to_string_lossy().into_owned();
            if !s.is_empty() && !dirs.contains(&s) {
                dirs.push(s);
            }
            p = pp.parent().map(|x| x.to_path_buf());
        }
    }
    dirs.iter()
        .map(|d| format!("-I{}", d))
        .collect::<Vec<_>>()
        .join(" ")
}

async fn compile_files(root: &Path, files: &[File], std: &str) -> CompileResult {
    let dir = job_dir(root);
    write_sources(&dir, files);
    let sources = source_list(files);
    let inc = include_flags(files);
    let cmd_str = format!(
        "clang++ -std={} -O2 -Wall -Wextra -pedantic -fcolor-diagnostics {} {} -o a.out 2>&1",
        std,
        inc,
        sources.join(" ")
    );
    let abs = match dir.canonicalize() {
        Ok(a) => a,
        Err(e) => {
            return CompileResult {
                success: false,
                output: format!("job dir error: {e}"),
                binary: None,
            }
        }
    };

    let mut c = Command::new(runtime());
    c.args([
        "run",
        "--rm",
        "--network",
        "none",
        "--memory",
        "512m",
        "--cpus",
        "2",
        "--pids-limit",
        "50",
        "--read-only",
        "--security-opt",
        "label=disable",
    ])
    .arg("-v")
    .arg(format!("{}:/home/sandbox/work:rw", abs.display()))
    .args(["-w", "/home/sandbox/work"])
    .arg(sandbox_image())
    .args(["sh", "-c", &cmd_str])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    let output = match tokio::time::timeout(Duration::from_secs(60), c.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return CompileResult {
                success: false,
                output: format!("Compilation error: {e}"),
                binary: None,
            }
        }
        Err(_) => {
            return CompileResult {
                success: false,
                output: "Compilation timed out (60s)".into(),
                binary: None,
            }
        }
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let binary = dir.join("a.out");
    let success = output.status.success() && binary.exists();
    let binary_path = if success { Some(binary) } else { None };
    let output_text = if text.trim().is_empty() {
        "Compilation failed (no output)".into()
    } else {
        text
    };
    CompileResult {
        success,
        output: output_text,
        binary: binary_path,
    }
}

async fn run_binary(binary: &Path, stdin: &str, timeout: u64) -> RunResult {
    let dir = match binary.parent() {
        Some(d) => d.to_path_buf(),
        None => {
            return RunResult {
                output: "invalid binary path".into(),
                exit_code: -1,
                timed_out: false,
            }
        }
    };
    let name = binary
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("a.out");
    let abs = match dir.canonicalize() {
        Ok(a) => a,
        Err(e) => {
            return RunResult {
                output: format!("Execution error: {e}"),
                exit_code: -1,
                timed_out: false,
            }
        }
    };
    let cmd_str = format!("timeout {timeout} ./{name} 2>&1");

    let mut c = Command::new(runtime());
    c.args([
        "run",
        "--rm",
        "-i",
        "--network",
        "none",
        "--memory",
        "256m",
        "--cpus",
        "1",
        "--pids-limit",
        "30",
        "--read-only",
        "--security-opt",
        "label=disable",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
    ])
    .arg("-v")
    .arg(format!("{}:/home/sandbox/work:ro", abs.display()))
    .args(["-w", "/home/sandbox/work"])
    .arg(sandbox_image())
    .args(["sh", "-c", &cmd_str])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            return RunResult {
                output: format!("Execution error: {e}"),
                exit_code: -1,
                timed_out: false,
            }
        }
    };
    if let Some(mut sin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = sin.write_all(stdin.as_bytes()).await;
    }

    match tokio::time::timeout(Duration::from_secs(timeout + 10), child.wait_with_output()).await {
        Ok(Ok(o)) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            let code = o.status.code().unwrap_or(-1);
            let timed_out = code == 124;
            RunResult {
                output: text,
                exit_code: code,
                timed_out,
            }
        }
        Ok(Err(e)) => RunResult {
            output: format!("Execution error: {e}"),
            exit_code: -1,
            timed_out: false,
        },
        Err(_) => RunResult {
            output: "Execution timed out.".into(),
            exit_code: -1,
            timed_out: true,
        },
    }
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// Compile then run. Returns the JSON shape the frontend expects.
///
/// Dispatches to the backend the caller asked for - wasm (wasmtime) or native
/// (host clang++ + host process) - and never silently to the other one: wasm
/// code it cannot serve gets an error naming Native as the answer, and Native
/// without clang++ gets an error naming WASM. The podman path below is
/// unreachable unless `PODMAN_ENABLED` is turned back on - unready wasi toolchain, or code that
/// `wasi_exec::wasm_unsupported_reason` flags (threads, whose
/// wasm32-wasip1-threads support is confirmed non-functional and is being
/// removed upstream; or `<execution>`, which wasi-sdk's libc++ cannot build -
/// see docs/WASM_PLAN.md). This keeps podman as a zero-regression fallback
/// rather than a hard requirement everywhere.
pub async fn compile_and_run(
    root: &Path,
    files: &[File],
    stdin: &str,
    std: &str,
    backend: Backend,
    timeout: Option<Duration>,
) -> Value {
    if matches!(backend, Backend::Native) {
        // Match the wasm ad-hoc path below, which always hard-codes -O2 in
        // wasi_exec::compile() - without this, unoptimized (-O0, clang's
        // default) CPU-heavy code can genuinely blow the 15s run timeout on
        // Native while the same code finishes comfortably under Wasm, which
        // looks like a Native-specific bug in the student's code but is
        // really just this backend forgetting to optimize (found by running
        // a real quiz question's O(2^n) subset-sum timing comparison: 17.4s
        // unoptimized vs 4.8s at -O2, straddling the timeout either way).
        return compile_and_run_native(root, files, stdin, std, "-O2", timeout).await;
    }
    if !matches!(backend, Backend::Podman) {
        // Wasm, explicitly. No fallback: report which of the two reasons
        // applies and name the backend that can run it.
        if let Some(reason) = crate::wasi_exec::wasm_unsupported_reason(files) {
            let why = format!("{reason}. Switch Sandbox to Native to run it.");
            return json!({ "ok": false, "stage": "compile", "compile_output": why, "run_output": "", "backend": "wasm" });
        }
        if !crate::wasi_exec::is_ready() {
            let (_, msg) = crate::wasi_exec::wasi_state();
            let why = format!("WASM toolchain isn't ready yet: {msg}. Wait for it to finish downloading, or switch Sandbox to Native.");
            return json!({ "ok": false, "stage": "compile", "compile_output": why, "run_output": "", "backend": "wasm" });
        }
        return crate::wasi_exec::compile_and_run(root, files, stdin, std, timeout).await;
    }
    let cr = compile_files(root, files, std).await;
    if !cr.success {
        if let Some(b) = &cr.binary {
            if let Some(p) = b.parent() {
                cleanup(p);
            }
        }
        return json!({ "ok": false, "stage": "compile", "compile_output": cr.output, "run_output": "", "backend": "podman" });
    }
    let binary = cr.binary.clone().unwrap();
    let rr = run_binary(&binary, stdin, 15).await;
    if let Some(p) = binary.parent() {
        cleanup(p);
    }
    json!({
        "ok": rr.exit_code == 0,
        "stage": "run",
        "compile_output": cr.output,
        "run_output": rr.output,
        "exit_code": rr.exit_code,
        "timed_out": rr.timed_out,
        "backend": "podman",
    })
}

/// clang-format — the bundled one from the wasi-sdk archive
/// (`wasi_exec::bundled_clang_format`) when ready, else whatever's on the
/// host PATH. Returns original on failure either way.
pub async fn format_code(root: &Path, code: &str, style: &str) -> String {
    let bin = crate::wasi_exec::bundled_clang_format(root)
        .map(|p| p.into_os_string())
        .unwrap_or_else(|| "clang-format".into());
    let mut c = Command::new(bin);
    c.arg(format!("--style={style}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(_) => return code.into(),
    };
    if let Some(mut sin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = sin.write_all(code.as_bytes()).await;
    }
    match tokio::time::timeout(Duration::from_secs(10), child.wait_with_output()).await {
        Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => code.into(),
    }
}

/// Host clang++ -fsyntax-only parse; returns diagnostics.
pub async fn check_syntax(
    root: &Path,
    files: &[File],
    std: &str,
    entry: Option<&str>,
) -> Vec<Value> {
    let dir = job_dir(root);
    write_sources(&dir, files);

    let entry = entry.map(str::to_string).unwrap_or_else(|| {
        let cpp: Vec<_> = files
            .iter()
            .filter(|f| [".cpp", ".cc", ".cxx"].iter().any(|e| f.name.ends_with(e)))
            .collect();
        cpp.first()
            .map(|f| f.name.clone())
            .or_else(|| files.first().map(|f| f.name.clone()))
            .unwrap_or_default()
    });
    if entry.is_empty() {
        cleanup(&dir);
        return vec![];
    }

    // include dirs (absolute) so headers in subdirs resolve
    let mut inc: Vec<String> = vec![dir.display().to_string()];
    for f in files {
        let mut p = Path::new(&f.name).parent().map(|x| dir.join(x));
        while let Some(pp) = p {
            let s = pp.display().to_string();
            if !inc.contains(&s) {
                inc.push(s);
            }
            p = pp.parent().map(|x| dir.join(x));
        }
    }
    let mut args: Vec<String> = vec![
        format!("-std={std}"),
        "-fsyntax-only".into(),
        "-fno-color-diagnostics".into(),
        "-Wall".into(),
        "-Wextra".into(),
    ];
    for d in &inc {
        args.push("-I".into());
        args.push(d.clone());
    }

    // Prefer the bundled clang++ (from the wasi-sdk archive) over a host
    // install; -fsyntax-only never links or runs anything, so it doesn't
    // matter that it's a wasm32-wasip1 cross-compiler for this purpose.
    let bin = match crate::wasi_exec::bundled_clangxx(root) {
        Some(p) => {
            args.push("--target=wasm32-wasip1".into());
            args.push(format!(
                "--sysroot={}",
                crate::wasi_exec::sysroot_dir(root).display()
            ));
            p.into_os_string()
        }
        None => "clang++".into(),
    };
    args.push(dir.join(&entry).display().to_string());

    let mut c = Command::new(bin);
    c.args(&args).stdout(Stdio::piped()).stderr(Stdio::piped());
    let text = match tokio::time::timeout(Duration::from_secs(20), c.output()).await {
        Ok(Ok(o)) => {
            let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
            t.push_str(&String::from_utf8_lossy(&o.stderr));
            t
        }
        _ => {
            cleanup(&dir);
            return vec![];
        }
    };

    let re = regex::Regex::new(
        r"^(?P<file>[^:]+):(?P<line>\d+):(?P<col>\d+):\s*(?P<sev>error|warning|note|fatal error):\s*(?P<msg>.*)$",
    ).unwrap();
    let diags: Vec<Value> = text.lines().filter_map(|line| {
        let c = re.captures(line)?;
        let sev = c.name("sev")?.as_str();
        let severity = if sev.contains("error") { "error" }
            else if sev.contains("warning") { "warning" } else { "info" };
        Some(json!({
            "file": Path::new(c.name("file")?.as_str()).file_name().and_then(|n| n.to_str()).unwrap_or(""),
            "line": c.name("line")?.as_str().parse::<i64>().unwrap_or(0),
            "col": c.name("col")?.as_str().parse::<i64>().unwrap_or(0),
            "severity": severity,
            "message": c.name("msg")?.as_str().trim(),
        }))
    }).collect();
    cleanup(&dir);
    diags
}
