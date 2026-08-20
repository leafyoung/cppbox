//! Standalone launcher: binds 127.0.0.1:0, prints `CPPBOX_PORT=<n>` (and
//! `CPPBOX_ROOT=...`) as the first stdout line, then serves. Same handshake as
//! `python -m backend`, so a future Tauri shell can spawn either identically.
use cppbox_core::{build_app, db, root_dir, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("cppbox_core=info".parse()?),
        )
        .init();

    let root = root_dir();
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir)?;
    let pool = db::connect(&data_dir.join("cppbox.db")).await?;
    db::migrate(&pool).await?;

    let frontend = std::env::var("CPPBOX_FRONTEND")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| root.join("frontend"));
    let app = build_app(
        AppState {
            db: pool,
            root: root.clone(),
        },
        frontend,
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}");
    println!("CPPBOX_PORT={}", port);
    println!("CPPBOX_ROOT={}", root.display());
    println!("→ {url}  (Ctrl+click to open)");
    // auto-open the browser unless disabled (useful for standalone dev runs)
    if std::env::var("CPPBOX_NO_OPEN").is_err() {
        let url2 = url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
            open_browser(&url2);
        });
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Open the URL in the platform default browser (best-effort, fire-and-forget).
fn open_browser(url: &str) {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install ctrl-c handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
