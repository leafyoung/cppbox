// CPPBox desktop app. Embeds the Rust (axum) backend in-process on
// 127.0.0.1:<dynamic> and hosts the UI inside the app window. Single
// instance: a second launch focuses the existing window and exits. The server
// is localhost-only; no external browser is opened.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use cppbox_core::{build_app, db, AppState};
use tauri::WebviewWindowBuilder;
use tauri::{Manager, WebviewUrl};

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

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // a second instance just focuses the existing window and exits
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let data_root = data_root();
            std::fs::create_dir_all(&data_root).expect("create data dir");
            let frontend = frontend_dir(app);

            // init: pull (ghcr on fresh installs) + smoke-test the sandbox image
            std::thread::spawn(|| {
                cppbox_core::sandbox::ensure_sandbox_image();
            });

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
                let router = build_app(
                    AppState {
                        db: pool,
                        root: data_root,
                    },
                    frontend,
                );
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

            // host the app UI inside the window (no external browser)
            let url = format!("http://127.0.0.1:{port}");
            WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::External(url.parse().expect("valid localhost url")),
            )
            .title("CPPBox")
            .inner_size(1280.0, 820.0)
            .min_inner_size(900.0, 600.0)
            .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running CPPBox");
}
