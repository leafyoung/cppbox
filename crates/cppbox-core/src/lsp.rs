//! clangd LSP bridge: one clangd subprocess per WebSocket session.
//!
//! Frontend <-> backend: JSON-RPC objects as WebSocket text frames.
//! Backend <-> clangd: Content-Length framing over stdio. The custom method
//! `$/sync` {std, files} writes project files to the session workspace and
//! returns {workspace} so the frontend can build file:// URIs. Mirrors lsp.py.
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::mpsc;

use crate::storage;
use crate::AppState;

/// Locate clangd once (cache). Falls back to the bare name if detection fails.
fn clangd_path() -> &'static str {
    static P: OnceLock<&'static str> = OnceLock::new();
    *P.get_or_init(|| {
        for c in ["clangd"] {
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
        "clangd"
    })
}

pub async fn ws_handler(wsu: WebSocketUpgrade, State(st): State<AppState>) -> impl IntoResponse {
    wsu.on_upgrade(move |socket| run_session(socket, st.root.clone()))
}

async fn run_session(socket: WebSocket, root: PathBuf) {
    let id = uuid::Uuid::new_v4().simple();
    let ws_dir = root.join("workdir").join(format!("ws_{}", &id.to_string()[..12]));
    if std::fs::create_dir_all(&ws_dir).is_err() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&ws_dir, std::fs::Permissions::from_mode(0o777));
    }

    let mut cmd = tokio::process::Command::new(clangd_path());
    cmd.args(["--log=error", "--pch-storage=memory", "--clang-tidy"])
        .current_dir(&ws_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("clangd spawn failed: {e}");
            return;
        }
    };
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    // drain stderr so the pipe never fills
    tokio::spawn(drain(stderr));

    let (sync_tx, mut sync_rx) = mpsc::unbounded_channel::<String>();
    let (mut ws_sender, mut ws_receiver) = socket.split();

    let dir_for_sync = ws_dir.clone();
    let mut writer = tokio::spawn(async move {
        handle_ws_to_clangd(&mut ws_receiver, stdin, sync_tx, dir_for_sync).await;
    });

    let mut reader = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        loop {
            tokio::select! {
                biased;
                reply = sync_rx.recv() => {
                    let Some(t) = reply else { return; };
                    if ws_sender.send(Message::Text(t.into())).await.is_err() { return; }
                }
                frame = read_frame(&mut reader) => {
                    match frame {
                        Some(t) => {
                            if ws_sender.send(Message::Text(t.into())).await.is_err() { return; }
                        }
                        None => return,
                    }
                }
            }
        }
    });

    // wait for either side to finish, then tear down
    tokio::select! {
        _ = &mut writer => { reader.abort(); }
        _ = &mut reader => { writer.abort(); }
    }
    let _ = child.kill().await;
    let _ = std::fs::remove_dir_all(&ws_dir);
}

/// Read WS text frames -> forward JSON-RPC to clangd stdin (Content-Length
/// framed). The custom `$/sync` is handled locally; its reply goes via `tx`.
async fn handle_ws_to_clangd(
    ws_receiver: &mut futures_util::stream::SplitStream<WebSocket>,
    mut stdin: ChildStdin,
    tx: mpsc::UnboundedSender<String>,
    ws_dir: PathBuf,
) {
    while let Some(msg) = ws_receiver.next().await {
        let text = match msg {
            Ok(Message::Text(t)) => t.to_string(),
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("method").and_then(|m| m.as_str()) == Some("$/sync") {
            let id = v.get("id").cloned();
            let params = v.get("params").cloned().unwrap_or(json!({}));
            let std = params.get("std").and_then(|s| s.as_str()).unwrap_or("c++23").to_string();
            if let Some(files) = params.get("files").and_then(|f| f.as_array()) {
                for f in files {
                    let Some(name) = f.get("name").and_then(|n| n.as_str()) else { continue };
                    let Some(content) = f.get("content").and_then(|c| c.as_str()) else { continue };
                    let p = ws_dir.join(name);
                    if let Some(parent) = p.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(p, content);
                }
            }
            let _ = std::fs::write(ws_dir.join(".clangd"), storage::clangd_config_text(&std));
            let reply = json!({
                "jsonrpc": "2.0", "id": id,
                "result": { "workspace": ws_dir.to_string_lossy(), "std": std }
            });
            let _ = tx.send(reply.to_string());
            continue;
        }
        // forward to clangd with LSP framing
        let data = serde_json::to_vec(&v).unwrap_or_default();
        let header = format!("Content-Length: {}\r\n\r\n", data.len());
        let _ = stdin.write_all(header.as_bytes()).await;
        let _ = stdin.write_all(&data).await;
        let _ = stdin.flush().await;
    }
}

/// Read one LSP message (headers up to blank line, then `Content-Length`
/// bytes) from clangd stdout. Returns None on EOF.
async fn read_frame(reader: &mut BufReader<ChildStdout>) -> Option<String> {
    let mut length: Option<usize> = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line).await.ok()?;
        if n == 0 {
            return None;
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        if line.to_ascii_lowercase().starts_with(b"content-length:") {
            let rest = std::str::from_utf8(&line[b"content-length:".len()..]).unwrap_or("");
            length = rest.trim().parse::<usize>().ok();
        }
    }
    let length = length?;
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await.ok()?;
    Some(String::from_utf8_lossy(&body).into_owned())
}

async fn drain(stderr: tokio::process::ChildStderr) {
    let mut reader = BufReader::new(stderr);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            _ => {}
        }
    }
}
