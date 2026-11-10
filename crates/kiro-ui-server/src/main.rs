//! Optional browser mode: serves the web frontend at http://127.0.0.1:3000 and
//! bridges each WebSocket to a kiro-ui-core client.

mod http;

use std::time::Duration;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let port: u16 = std::env::var("KIRO_UI_PORT")
        .or_else(|_| std::env::var("PORT"))
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let state = kiro_ui_core::init_state();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("Kiro UI running at http://{}", listener.local_addr()?);
    let app = http::router(state.clone());
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            println!("Shutting down...");
            // Agents first: open WebSockets would otherwise keep graceful shutdown waiting.
            let _ = tokio::time::timeout(Duration::from_secs(4), state.shutdown()).await;
        })
        .await
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
