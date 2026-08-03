//! Isolated compile/run via podman (preferred) or docker, plus host-side
//! clang-format and clang++ syntax check. Mirrors backend/sandbox.py.
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;

pub fn sandbox_image() -> String {
    std::env::var("CPPBOX_SANDBOX_IMAGE").unwrap_or_else(|_| "cpp-sandbox:latest".into())
}

/// Compile with debug symbols (-g -O0) for gdb. Returns (binary, job_dir).
pub async fn compile_debug(root: &Path, files: &[File], std: &str) -> Result<(PathBuf, PathBuf), String> {
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
    c.args(["run", "--rm", "--network", "none", "--memory", "512m", "--cpus", "2",
            "--pids-limit", "50", "--read-only", "--security-opt", "label=disable"])
        .arg("-v").arg(format!("{}:/home/sandbox/work:rw", abs.display()))
        .args(["-w", "/home/sandbox/work"])
        .arg(sandbox_image())
        .args(["sh", "-c", &cmd_str])
        .stdout(Stdio::piped()).stderr(Stdio::piped());
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
        Err(if text.trim().is_empty() { "Compilation failed".into() } else { text })
    }
}

/// Pick the container CLI once: podman (rootless) preferred, docker fallback.
pub fn runtime() -> &'static str {
    static RT: OnceLock<&'static str> = OnceLock::new();
    *RT.get_or_init(|| {
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
    dirs.iter().map(|d| format!("-I{}", d)).collect::<Vec<_>>().join(" ")
}

async fn compile_files(root: &Path, files: &[File], std: &str) -> CompileResult {
    let dir = job_dir(root);
    write_sources(&dir, files);
    let sources = source_list(files);
    let inc = include_flags(files);
    let cmd_str = format!(
        "clang++ -std={} -O2 -Wall -Wextra -pedantic -fcolor-diagnostics {} {} -o a.out 2>&1",
        std, inc, sources.join(" ")
    );
    let abs = match dir.canonicalize() {
        Ok(a) => a,
        Err(e) => return CompileResult { success: false, output: format!("job dir error: {e}"), binary: None },
    };

    let mut c = Command::new(runtime());
    c.args(["run", "--rm", "--network", "none", "--memory", "512m", "--cpus", "2",
            "--pids-limit", "50", "--read-only", "--security-opt", "label=disable"])
        .arg("-v").arg(format!("{}:/home/sandbox/work:rw", abs.display()))
        .args(["-w", "/home/sandbox/work"])
        .arg(sandbox_image())
        .args(["sh", "-c", &cmd_str])
        .stdout(Stdio::piped()).stderr(Stdio::piped());

    let output = match tokio::time::timeout(Duration::from_secs(60), c.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return CompileResult { success: false, output: format!("Compilation error: {e}"), binary: None },
        Err(_) => return CompileResult { success: false, output: "Compilation timed out (60s)".into(), binary: None },
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let binary = dir.join("a.out");
    let success = output.status.success() && binary.exists();
    let binary_path = if success { Some(binary) } else { None };
    let output_text = if text.trim().is_empty() { "Compilation failed (no output)".into() } else { text };
    CompileResult { success, output: output_text, binary: binary_path }
}

async fn run_binary(binary: &Path, stdin: &str, timeout: u64) -> RunResult {
    let dir = match binary.parent() {
        Some(d) => d.to_path_buf(),
        None => return RunResult { output: "invalid binary path".into(), exit_code: -1, timed_out: false },
    };
    let name = binary.file_name().and_then(|n| n.to_str()).unwrap_or("a.out");
    let abs = match dir.canonicalize() {
        Ok(a) => a,
        Err(e) => return RunResult { output: format!("Execution error: {e}"), exit_code: -1, timed_out: false },
    };
    let cmd_str = format!("timeout {timeout} ./{name} 2>&1");

    let mut c = Command::new(runtime());
    c.args(["run", "--rm", "-i", "--network", "none", "--memory", "256m", "--cpus", "1",
            "--pids-limit", "30", "--read-only", "--security-opt", "label=disable",
            "--cap-drop", "ALL", "--security-opt", "no-new-privileges"])
        .arg("-v").arg(format!("{}:/home/sandbox/work:ro", abs.display()))
        .args(["-w", "/home/sandbox/work"])
        .arg(sandbox_image())
        .args(["sh", "-c", &cmd_str])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return RunResult { output: format!("Execution error: {e}"), exit_code: -1, timed_out: false },
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
            RunResult { output: text, exit_code: code, timed_out }
        }
        Ok(Err(e)) => RunResult { output: format!("Execution error: {e}"), exit_code: -1, timed_out: false },
        Err(_) => RunResult { output: "Execution timed out.".into(), exit_code: -1, timed_out: true },
    }
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// Compile then run. Returns the JSON shape the frontend expects.
pub async fn compile_and_run(root: &Path, files: &[File], stdin: &str, std: &str) -> Value {
    let cr = compile_files(root, files, std).await;
    if !cr.success {
        if let Some(b) = &cr.binary { if let Some(p) = b.parent() { cleanup(p); } }
        return json!({ "ok": false, "stage": "compile", "compile_output": cr.output, "run_output": "" });
    }
    let binary = cr.binary.clone().unwrap();
    let rr = run_binary(&binary, stdin, 15).await;
    if let Some(p) = binary.parent() { cleanup(p); }
    json!({
        "ok": rr.exit_code == 0,
        "stage": "run",
        "compile_output": cr.output,
        "run_output": rr.output,
        "exit_code": rr.exit_code,
        "timed_out": rr.timed_out,
    })
}

/// Host clang-format (no container) — returns original on failure.
pub async fn format_code(code: &str, style: &str) -> String {
    let mut c = Command::new("clang-format");
    c.arg(format!("--style={style}"))
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
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
pub async fn check_syntax(root: &Path, files: &[File], std: &str, entry: Option<&str>) -> Vec<Value> {
    let dir = job_dir(root);
    write_sources(&dir, files);

    let entry = entry.map(str::to_string).unwrap_or_else(|| {
        let cpp: Vec<_> = files.iter().filter(|f| {
            [".cpp", ".cc", ".cxx"].iter().any(|e| f.name.ends_with(e))
        }).collect();
        cpp.first().map(|f| f.name.clone()).or_else(|| files.first().map(|f| f.name.clone())).unwrap_or_default()
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
            if !inc.contains(&s) { inc.push(s); }
            p = pp.parent().map(|x| dir.join(x));
        }
    }
    let mut args: Vec<String> = vec![format!("-std={std}"), "-fsyntax-only".into(), "-fno-color-diagnostics".into(), "-Wall".into(), "-Wextra".into()];
    for d in &inc { args.push("-I".into()); args.push(d.clone()); }
    args.push(dir.join(&entry).display().to_string());

    let mut c = Command::new("clang++");
    c.args(&args).stdout(Stdio::piped()).stderr(Stdio::piped());
    let text = match tokio::time::timeout(Duration::from_secs(20), c.output()).await {
        Ok(Ok(o)) => {
            let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
            t.push_str(&String::from_utf8_lossy(&o.stderr));
            t
        }
        _ => { cleanup(&dir); return vec![]; }
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
