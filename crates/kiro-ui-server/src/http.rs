//! HTTP layer: static web frontend, authenticated `/api` routes, and the
//! WebSocket that carries the kiro-ui-core client protocol.

use axum::body::Body;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use kiro_ui_core::{picker, suggestions, AppState, Client, JsonMap};
use regex::Regex;
use rust_embed::RustEmbed;
use serde_json::{json, Value};
use std::sync::{Arc, LazyLock};
use tokio::sync::mpsc;

#[derive(RustEmbed)]
#[folder = "../../dist/client"]
struct Assets;

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws://127.0.0.1:* ws://localhost:*";

static ORIGIN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^https?://(localhost|127\.0\.0\.1)(:\d+)?$").unwrap());

pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/trust", get(get_trust).put(put_trust))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/pick-folder", post(pick_folder))
        .route("/suggestions", post(suggest))
        .route("/suggestions/", post(suggest))
        .route("/suggestions/test", post(suggest_test))
        .route("/suggestions/models", get(suggest_models))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token))
        // Unauthenticated: same-origin pages fetch the token on load.
        .route("/token", get(get_token));

    Router::new()
        .route("/", get(index))
        .nest("/api", api)
        .fallback(static_file)
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    res
}

fn token_ok(state: &AppState, headers: &HeaderMap, uri: &Uri) -> bool {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.replacen("Bearer ", "", 1))
        .filter(|v| !v.is_empty());
    let query = || {
        uri.query()?.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == "token").then(|| v.to_string())
        })
    };
    bearer.or_else(query).is_some_and(|t| constant_time_eq(t.as_bytes(), state.token.as_bytes()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn require_token(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if token_ok(&state, req.headers(), req.uri()) {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response()
    }
}

/// `GET /` serves the app, or upgrades to the agent WebSocket.
async fn index(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let Ok(upgrade) = upgrade else { return asset("index.html") };
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or("");
    let origin_ok = origin.is_empty() || ORIGIN_RE.is_match(origin);
    // Query token only, like the original verifyClient.
    let token_ok = uri
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")))
        .is_some_and(|t| constant_time_eq(t.as_bytes(), state.token.as_bytes()));
    if !origin_ok || !token_ok {
        return StatusCode::UNAUTHORIZED.into_response();
    }
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

async fn static_file(uri: Uri) -> Response {
    asset(uri.path().trim_start_matches('/'))
}

fn asset(path: &str) -> Response {
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
            Response::builder()
                .header(header::CONTENT_TYPE, mime.as_ref())
                .header(header::CACHE_CONTROL, cache)
                .body(Body::from(file.data.into_owned()))
                .unwrap()
        }
        None if path == "index.html" => (StatusCode::INTERNAL_SERVER_ERROR, "Server error").into_response(),
        None => (StatusCode::NOT_FOUND, format!("Cannot GET /{path}")).into_response(),
    }
}

async fn get_token(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({"token": state.token}))
}

async fn get_trust(State(state): State<Arc<AppState>>) -> Json<JsonMap> {
    Json(state.trust_rules())
}

fn body_json(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

fn object_body(body: &[u8]) -> JsonMap {
    match body_json(body) {
        Value::Object(m) => m,
        _ => JsonMap::new(),
    }
}

async fn put_trust(State(state): State<Arc<AppState>>, body: axum::body::Bytes) -> Json<JsonMap> {
    Json(state.set_trust_rules(object_body(&body)).await)
}

async fn get_settings(State(state): State<Arc<AppState>>) -> Json<JsonMap> {
    Json(state.settings_snapshot())
}

async fn put_settings(State(state): State<Arc<AppState>>, body: axum::body::Bytes) -> Json<JsonMap> {
    Json(state.merge_settings(object_body(&body)).await)
}

async fn pick_folder(body: axum::body::Bytes) -> Json<Value> {
    let start = body_json(&body).get("startPath").and_then(Value::as_str).unwrap_or_default().to_string();
    Json(json!({"path": picker::pick_folder(&start).await}))
}

async fn suggest(State(state): State<Arc<AppState>>, body: axum::body::Bytes) -> Json<Value> {
    let last = body_json(&body).get("lastAssistant").and_then(Value::as_str).unwrap_or_default().to_string();
    Json(suggestions::suggest(&state, &last).await)
}

async fn suggest_test(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(suggestions::test(&state).await)
}

async fn suggest_models(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(suggestions::models(&state).await)
}
