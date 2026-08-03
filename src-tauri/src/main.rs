// CPPBox desktop launcher. Starts the embedded Rust (axum) backend on
// 127.0.0.1:<dynamic>, then shows a small window with a clickable link that
// opens the app in the default browser. No embedded app webview.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use cppbox_core::{build_app, db, AppState};
use tauri::{Manager, WebviewUrl};
use tauri::WebviewWindowBuilder;

fn data_root() -> PathBuf {
    if let Ok(r) = std::env::var("CPPBOX_ROOT") {
        return PathBuf::from(r);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cppbox")
}

fn frontend_dir(app: &tauri::App) -> PathBuf {
    if let Ok(f) = std::env::var("CPPBOX_FRONTEND") {
        return PathBuf::from(f);
    }
    app.path()
        .resource_dir()
        .map(|d| d.join("frontend"))
        .unwrap_or_else(|_| PathBuf::from("frontend"))
}

#[tauri::command]
fn app_port(state: tauri::State<AppHandle>) -> u16 {
    state.port
}

#[tauri::command]
fn open_browser(url: String) {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", &url]).spawn();
}

struct AppHandle {
    port: u16,
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let data_root = data_root();
            std::fs::create_dir_all(&data_root).expect("create data dir");
            let frontend = frontend_dir(app);

            // start the embedded axum backend on a dynamic localhost port
            let (tx, rx) = std::sync::mpsc::channel::<u16>();
            tauri::async_runtime::spawn(async move {
                let pool = match db::connect(&data_root.join("cppbox.db")).await {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("db connect failed: {e}");
                        return;
                    }
                };
                if let Err(e) = db::migrate(&pool).await {
                    eprintln!("db migrate failed: {e}");
                    return;
                }
                let router = build_app(AppState { db: pool, root: data_root }, frontend);
                let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("bind failed: {e}");
                        return;
                    }
                };
                let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                let _ = tx.send(port);
                if let Err(e) = axum::serve(listener, router).await {
                    eprintln!("server stopped: {e}");
                }
            });

            let port = rx.recv().expect("backend did not bind a port");
            eprintln!("CPPBOX_PORT={port}");

            // small launcher window with the clickable link
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("CPPBox")
                .inner_size(460.0, 210.0)
                .resizable(false)
                .build()?;
            app.manage(AppHandle { port });

            // auto-open the default browser once (the window link can reopen it)
            let url = format!("http://127.0.0.1:{port}");
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(700));
                open_browser(url);
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![app_port, open_browser])
        .run(tauri::generate_context!())
        .expect("error while running CPPBox");
}
