//! Browser mode: serves the wasm build of the UI and bridges each WebSocket to a
//! `kiro_ui_core::Client`. Binds 127.0.0.1 only.

use axum::body::Body;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use kiro_ui_core::{AppState, Client};
use regex::Regex;
use rust_embed::RustEmbed;
use serde_json::Value;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(RustEmbed)]
#[folder = "../../dist/web"]
struct Assets;

const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self' ws://127.0.0.1:* ws://localhost:*; frame-ancestors 'none'";
const TOKEN_PLACEHOLDER: &str = "__KIRO_TOKEN__";

static ORIGIN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^https?://(localhost|127\.0\.0\.1)(:\d+)?$").unwrap());
static HOST_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(localhost|127\.0\.0\.1)(:\d+)?$").unwrap());

pub fn run(port: u16, open: bool) -> std::io::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    let state = {
        let _guard = rt.enter();
        kiro_ui_core::init_state()
    };
    let result = rt.block_on(serve(state, port, open));
    rt.shutdown_timeout(Duration::from_secs(1));
    result
}

async fn serve(state: Arc<AppState>, port: u16, open: bool) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let url = format!("http://{}", listener.local_addr()?);
    println!("Kiro UI running at {url}");
    if open {
        kiro_ui_core::picker::open_url(&url);
    }
    let app = router(state.clone());
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

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/ws", get(ws))
        .fallback(static_file)
        .layer(middleware::from_fn(guard))
        .with_state(state)
}

/// DNS-rebinding guard plus security headers on every response.
async fn guard(req: Request, next: Next) -> Response {
    let host_ok = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).is_some_and(|h| HOST_RE.is_match(h));
    let mut res = if host_ok { next.run(req).await } else { StatusCode::FORBIDDEN.into_response() };
    let h = res.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    res
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `GET /`: the page, with this run's token injected.
async fn index(State(state): State<Arc<AppState>>) -> Response {
    let Some(file) = Assets::get("index.html") else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Browser UI not built. Run: cargo xtask web").into_response();
    };
    let html = String::from_utf8_lossy(&file.data).replace(TOKEN_PLACEHOLDER, &state.token);
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        // The token changes on every start, so the page must never be cached.
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(html))
        .unwrap()
}

/// `GET /ws`: origin and token checked before the upgrade.
async fn ws(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or("");
    let origin_ok = origin.is_empty() || ORIGIN_RE.is_match(origin);
    let token_ok = uri
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")))
        .is_some_and(|t| constant_time_eq(t.as_bytes(), state.token.as_bytes()));
    if !origin_ok || !token_ok {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(upgrade) = upgrade else { return StatusCode::BAD_REQUEST.into_response() };
    upgrade
        .max_message_size(64 << 20)
        .on_upgrade(move |socket| serve_socket(state, socket))
}

/// Bridge one WebSocket to a core client: JSON text frames both ways.
async fn serve_socket(state: Arc<AppState>, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(text) = out_rx.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });
    let client = Client::start(state, move |ev| {
        let _ = out_tx.send(ev.to_string());
    });
    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(text) => {
                if let Ok(v) = serde_json::from_str::<Value>(text.as_str()) {
                    client.send(v);
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    client.close();
    writer.abort();
}

async fn static_file(headers: HeaderMap, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    // Pre-compressed assets are embedded as `<name>.gz`.
    let (file, gzip) = match Assets::get(&format!("{path}.gz")) {
        Some(f) if !path.is_empty() => (f, true),
        _ => match Assets::get(path) {
            Some(f) if !path.is_empty() && path != "index.html" => (f, false),
            _ => return (StatusCode::NOT_FOUND, "Not found").into_response(),
        },
    };
    let etag = format!("\"{}\"", file.metadata.sha256_hash().iter().map(|b| format!("{b:02x}")).collect::<String>());
    let mut res = if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let mut res = Response::new(Body::from(file.data.into_owned()));
        res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_str(mime.as_ref()).unwrap());
        if gzip {
            res.headers_mut().insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        }
        res
    };
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res.headers_mut().insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
    res
}
