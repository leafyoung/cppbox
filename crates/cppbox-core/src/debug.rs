//! LLDB debugger bridge over DAP (Debug Adapter Protocol), via lldb-dap running
//! inside the podman sandbox (ptrace-enabled). One lldb-dap subprocess per
//! WebSocket session. Frontend <-> backend: JSON commands/events over WS.
//! Backend <-> lldb-dap: DAP (Content-Length framed JSON), same framing as LSP.
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::routes::fetch_one;
use crate::{sandbox, storage, AppState};

pub async fn ws_handler(
    wsu: WebSocketUpgrade,
    Query(q): Query<DebugQuery>,
    State(st): State<AppState>,
) -> impl IntoResponse {
    let std = q.std.unwrap_or_else(|| "c++17".into());
    wsu.on_upgrade(move |socket| run_session(socket, st, q.pid, std))
}

#[derive(Deserialize)]
pub struct DebugQuery {
    pid: String,
    std: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "cmd")]
enum Cmd {
    Start { breakpoints: HashMap<String, Vec<u32>> },
    Bp { file: String, lines: Vec<u32> },
    Continue,
    Next,
    StepIn,
    StepOut,
    Pause,
    Expand { #[serde(rename = "ref")] vref: u64 },
    Stop,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

fn to_msg(v: Value) -> Message {
    Message::Text(v.to_string().into())
}

async fn run_session(socket: WebSocket, st: AppState, pid: String, std: String) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let s = match fetch_one(&st.db, &pid).await {
        Ok(s) => s,
        Err(e) => { let m = e.1; let _ = ws_sender.send(to_msg(json!({"type":"error","msg":m}))).await; return; }
    };
    let lp = s.local_path.as_deref().filter(|p| !p.is_empty());
    let src = storage::collect_source_files(&st.root, &pid, lp);
    if src.is_empty() {
        let _ = ws_sender.send(to_msg(json!({"type":"error","msg":"No source files"}))).await;
        return;
    }
    let files: Vec<sandbox::File> = src.into_iter().map(|(n, c)| sandbox::File { name: n, content: c }).collect();
    let _ = ws_sender.send(to_msg(json!({"type":"status","text":"Compiling (debug)…"}))).await;

    let (binary, job_dir) = match sandbox::compile_debug(&st.root, &files, &std).await {
        Ok(v) => v,
        Err(e) => { let _ = ws_sender.send(to_msg(json!({"type":"error","msg":e}))).await; return; }
    };
    let abs = match job_dir.canonicalize() { Ok(a) => a, Err(_) => { return; } };

    // spawn lldb-dap in a ptrace-enabled container
    let mut cmd = tokio::process::Command::new(sandbox::runtime());
    cmd.args(["run", "--rm", "-i", "--cap-add", "SYS_PTRACE",
              "--security-opt", "seccomp=unconfined", "--security-opt", "label=disable",
              "--user", "0", "--network", "none", "--memory", "512m", "--cpus", "1"])
        .arg("-v").arg(format!("{}:/home/sandbox/work:rw", abs.display()))
        .args(["-w", "/home/sandbox/work"])
        .arg(sandbox::sandbox_image())
        .args(["lldb-dap"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => { let _ = ws_sender.send(to_msg(json!({"type":"error","msg":format!("lldb-dap spawn: {e}")}))).await; return; }
    };
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let _stderr = child.stderr.take();
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let seq = Arc::new(AtomicU64::new(1));

    let (tx_evt, mut rx_evt) = mpsc::unbounded_channel::<Value>();
    let pending_r = pending.clone();
    let reader = tokio::spawn(read_dap_loop(stdout, tx_evt, pending_r));

    let _ = ws_sender.send(to_msg(json!({"type":"ready"}))).await;

    let mut thread_id: Option<u64> = None;
    let mut bps: HashMap<String, Vec<u32>> = HashMap::new();
    let mut configured = false;

    // initialize + launch up front
    let _ = send_request(&mut stdin, &pending, &seq, "initialize", Some(json!({
        "clientID": "cppbox", "adapterID": "lldb",
        "linesStartAt1": true, "columnsStartAt1": true, "pathFormat": "path",
    }))).await;
    let _ = send_request(&mut stdin, &pending, &seq, "launch", Some(json!({
        "program": "/home/sandbox/work/a.out", "cwd": "/home/sandbox/work", "stopOnEntry": false,
    }))).await;

    loop {
        tokio::select! {
            msg = ws_receiver.next() => {
                let txt = match msg {
                    Some(Ok(Message::Text(t))) => t.to_string(),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => continue,
                };
                let cmd: Cmd = match serde_json::from_str(&txt) { Ok(c) => c, Err(_) => continue };
                match cmd {
                    Cmd::Start { breakpoints } => { bps = breakpoints; }
                    Cmd::Bp { file, lines } => {
                        bps.insert(file.clone(), lines.clone());
                        if configured {
                            let _ = set_breakpoints(&mut stdin, &pending, &seq, &file, &lines, &mut ws_sender).await;
                        }
                    }
                    Cmd::Continue => { let _ = step_cmd(&mut stdin, &pending, &seq, "continue", thread_id).await; }
                    Cmd::Next     => { let _ = step_cmd(&mut stdin, &pending, &seq, "next", thread_id).await; }
                    Cmd::StepIn   => { let _ = step_cmd(&mut stdin, &pending, &seq, "stepIn", thread_id).await; }
                    Cmd::StepOut  => { let _ = step_cmd(&mut stdin, &pending, &seq, "stepOut", thread_id).await; }
                    Cmd::Pause    => { let _ = step_cmd(&mut stdin, &pending, &seq, "pause", thread_id).await; }
                    Cmd::Expand { vref } => {
                        if let Ok(r) = send_request(&mut stdin, &pending, &seq, "variables", Some(json!({"variablesReference": vref}))).await {
                            let raw = r.get("body").and_then(|b| b.get("variables")).and_then(|b| b.as_array()).cloned().unwrap_or_default();
                            let vars: Vec<Value> = raw.iter().map(|v| json!({
                                "name": v.get("name").cloned().unwrap_or(Value::Null),
                                "value": v.get("value").cloned().unwrap_or(Value::Null),
                                "ref": v.get("variablesReference").and_then(|r| r.as_u64()).unwrap_or(0),
                            })).collect();
                            let _ = ws_sender.send(to_msg(json!({"type":"vars","ref":vref,"vars":vars}))).await;
                        }
                    }
                    Cmd::Stop => {
                        let _ = send_request(&mut stdin, &pending, &seq, "disconnect", Some(json!({"terminateDebuggee": true}))).await;
                        let _ = ws_sender.send(to_msg(json!({"type":"ended"}))).await;
                        break;
                    }
                }
            }
            evt = rx_evt.recv() => {
                let Some(v) = evt else { break };
                let etype = v.get("event").and_then(|e| e.as_str()).unwrap_or("");
                let body = v.get("body").cloned().unwrap_or(Value::Null);
                match etype {
                    "initialized" => {
                        let files: Vec<(String, Vec<u32>)> = bps.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                        for (file, lines) in &files {
                            let _ = set_breakpoints(&mut stdin, &pending, &seq, file, lines, &mut ws_sender).await;
                        }
                        let _ = send_request(&mut stdin, &pending, &seq, "configurationDone", Some(json!({}))).await;
                        configured = true;
                        let _ = ws_sender.send(to_msg(json!({"type":"running"}))).await;
                    }
                    "stopped" => {
                        let tid = body.get("threadId").and_then(|t| t.as_u64());
                        thread_id = tid;
                        let reason = body.get("reason").and_then(|r| r.as_str()).unwrap_or("").to_string();
                        let mut file: Option<String> = None;
                        let mut line: Option<u64> = None;
                        let mut func: Option<String> = None;
                        let mut frames: Vec<Value> = Vec::new();
                        let mut vars: Vec<Value> = Vec::new();
                        if let Some(t) = tid {
                            if let Ok(r) = send_request(&mut stdin, &pending, &seq, "stackTrace", Some(json!({
                                "threadId": t, "startFrame": 0, "levels": 20,
                            }))).await {
                                let arr = r.get("body").and_then(|b| b.get("stackFrames")).and_then(|b| b.as_array()).cloned().unwrap_or_default();
                                for f in &arr {
                                    let path = f.get("source").and_then(|s| s.get("path")).and_then(|p| p.as_str()).map(str::to_string);
                                    frames.push(json!({
                                        "id": f.get("id").cloned().unwrap_or(Value::Null),
                                        "name": f.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                                        "file": path.as_deref().map(norm_path),
                                        "line": f.get("line").cloned().unwrap_or(Value::Null),
                                    }));
                                }
                                if let Some(top) = arr.first() {
                                    let path = top.get("source").and_then(|s| s.get("path")).and_then(|p| p.as_str()).map(str::to_string);
                                    file = path.as_deref().map(norm_path);
                                    line = top.get("line").and_then(|l| l.as_u64());
                                    func = top.get("name").and_then(|n| n.as_str()).map(str::to_string);
                                    if let Some(frid) = top.get("id").and_then(|i| i.as_u64()) {
                                        vars = fetch_vars(&mut stdin, &pending, &seq, frid).await.unwrap_or_default();
                                    }
                                }
                            }
                        }
                        let _ = ws_sender.send(to_msg(json!({
                            "type":"stopped","file":file,"line":line,"func":func,"reason":reason,"threadId":tid
                        }))).await;
                        let _ = ws_sender.send(to_msg(json!({"type":"debug_info","frames":frames,"vars":vars}))).await;
                    }
                    "output" => {
                        // only the inferior's own stdout/stderr; skip lldb-dap's
                        // internal console noise (e.g. its Python tracebacks)
                        let cat = body.get("category").and_then(|c| c.as_str()).unwrap_or("");
                        if cat == "stdout" || cat == "stderr" {
                            let text = body.get("output").and_then(|o| o.as_str()).unwrap_or("");
                            if !text.is_empty() {
                                let _ = ws_sender.send(to_msg(json!({"type":"output","text":text}))).await;
                            }
                        }
                    }
                    "terminated" | "exited" => {
                        let code = body.get("exitCode").cloned();
                        let _ = ws_sender.send(to_msg(json!({"type":"exited","code":code}))).await;
                    }
                    _ => {}
                }
            }
        }
    }
    let _ = child.kill().await;
    drop(reader);
    let _ = std::fs::remove_dir_all(&job_dir);
    let _ = binary;
}

// ── DAP helpers ──────────────────────────────────────────────────────────
async fn write_dap(stdin: &mut ChildStdin, msg: &Value) {
    let data = serde_json::to_vec(msg).unwrap_or_default();
    let header = format!("Content-Length: {}\r\n\r\n", data.len());
    let _ = stdin.write_all(header.as_bytes()).await;
    let _ = stdin.write_all(&data).await;
    let _ = stdin.flush().await;
}

async fn send_request(
    stdin: &mut ChildStdin, pending: &Pending, seq: &AtomicU64,
    command: &str, arguments: Option<Value>,
) -> Result<Value, String> {
    let n = seq.fetch_add(1, Ordering::Relaxed);
    let mut m = json!({ "seq": n, "type": "request", "command": command });
    if let Some(a) = arguments { m["arguments"] = a; }
    let (otx, orx) = oneshot::channel();
    pending.lock().await.insert(n, otx);
    write_dap(stdin, &m).await;
    match tokio::time::timeout(Duration::from_secs(8), orx).await {
        Ok(Ok(v)) => Ok(v),
        _ => Err("timeout".into()),
    }
}

async fn set_breakpoints(
    stdin: &mut ChildStdin, pending: &Pending, seq: &AtomicU64,
    file: &str, lines: &[u32],
    ws_sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
) -> Result<(), ()> {
    let bps_arr: Vec<Value> = lines.iter().map(|l| json!({"line": l})).collect();
    let resp = send_request(stdin, pending, seq, "setBreakpoints", Some(json!({
        "source": {"path": format!("/home/sandbox/work/{file}")},
        "breakpoints": bps_arr,
        "lines": lines,
        "sourceModified": false,
    }))).await;
    if let Ok(r) = resp {
        let results = r.get("body").and_then(|b| b.get("breakpoints")).and_then(|b| b.as_array()).cloned().unwrap_or_default();
        let out: Vec<Value> = results.iter().map(|b| json!({
            "line": b.get("line").cloned().unwrap_or(Value::Null),
            "ok": b.get("verified").and_then(|v| v.as_bool()).unwrap_or(false),
            "msg": b.get("message").cloned().unwrap_or(Value::Null),
        })).collect();
        let _ = ws_sender.send(to_msg(json!({"type":"breakpoints","file":file,"results":out}))).await;
    }
    Ok(())
}

async fn step_cmd(stdin: &mut ChildStdin, pending: &Pending, seq: &AtomicU64, cmd: &str, thread_id: Option<u64>) -> Result<(), ()> {
    let tid = match thread_id { Some(t) => t, None => return Err(()) };
    let _ = send_request(stdin, pending, seq, cmd, Some(json!({"threadId": tid}))).await;
    Ok(())
}

/// Strip the in-container workdir prefix from a DAP source path.
fn norm_path(p: &str) -> String {
    p.strip_prefix("/home/sandbox/work/").map(str::to_string).unwrap_or_else(|| p.to_string())
}

/// Fetch locals/args for a frame (all scopes' variables, one level deep).
async fn fetch_vars(stdin: &mut ChildStdin, pending: &Pending, seq: &AtomicU64, frame_id: u64) -> Option<Vec<Value>> {
    let r = send_request(stdin, pending, seq, "scopes", Some(json!({"frameId": frame_id}))).await.ok()?;
    let scopes = r.get("body").and_then(|b| b.get("scopes")).and_then(|b| b.as_array()).cloned().unwrap_or_default();
    let mut vars: Vec<Value> = Vec::new();
    for sc in &scopes {
        // keep only the meaningful locals/args scopes; drop registers etc.
        let sname = sc.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if sname != "Locals" && sname != "Arguments" {
            continue;
        }
        let vr = sc.get("variablesReference").and_then(|r| r.as_u64()).unwrap_or(0);
        if vr == 0 { continue; }
        let Ok(r2) = send_request(stdin, pending, seq, "variables", Some(json!({"variablesReference": vr}))).await else { continue };
        let raw = r2.get("body").and_then(|b| b.get("variables")).and_then(|b| b.as_array()).cloned().unwrap_or_default();
        for v in &raw {
            vars.push(json!({
                "name": v.get("name").cloned().unwrap_or(Value::Null),
                "value": v.get("value").cloned().unwrap_or(Value::Null),
                "ref": v.get("variablesReference").and_then(|r| r.as_u64()).unwrap_or(0),
            }));
        }
    }
    Some(vars)
}

/// Read DAP messages (Content-Length framed) forever: responses resolve pending
/// requests; events are forwarded to the session loop via `tx`.
async fn read_dap_loop(stdout: ChildStdout, tx: mpsc::UnboundedSender<Value>, pending: Pending) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut length: Option<usize> = None;
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if line == b"\r\n" || line == b"\n" { break; }
            let s = String::from_utf8_lossy(&line);
            if let Some(idx) = s.to_ascii_lowercase().find("content-length:") {
                let rest = s[idx + b"content-length:".len()..].trim();
                length = rest.parse::<usize>().ok();
            }
        }
        let length = match length { Some(l) => l, None => continue };
        let mut body = vec![0u8; length];
        if reader.read_exact(&mut body).await.is_err() { return; }
        let Ok(v) = serde_json::from_slice::<Value>(&body) else { continue };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("response") => {
                if let Some(req_seq) = v.get("request_seq").and_then(|s| s.as_u64()) {
                    if let Some(t) = pending.lock().await.remove(&req_seq) {
                        let _ = t.send(v);
                    }
                }
            }
            Some("event") => { let _ = tx.send(v); }
            _ => {}
        }
    }
}
