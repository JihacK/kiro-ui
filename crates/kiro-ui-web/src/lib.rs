//! Browser entry point: the `kiro-ui-app` interface on a canvas, talking to
//! `kiro-ui --server` over a WebSocket. Empty on native targets.
#![cfg(target_arch = "wasm32")]

use kiro_ui_app::{allowed_url, Backend, KiroApp};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{CloseEvent, HtmlCanvasElement, MessageEvent, WebSocket};

const RETRY_MS_FIRST: i32 = 2_000;
const RETRY_MS_MAX: i32 = 10_000;

struct Shared {
    socket: Option<WebSocket>,
    /// Sent before the socket opened.
    outgoing: Vec<String>,
    incoming: VecDeque<Value>,
    ctx: Option<eframe::egui::Context>,
    /// A probe socket is retrying; reload the page when one opens.
    lost: bool,
}

type Link = Rc<RefCell<Shared>>;

struct WebBackend(Link);

fn window() -> web_sys::Window {
    web_sys::window().expect("window")
}

fn ws_url(token: &str) -> String {
    let loc = window().location();
    let scheme = if loc.protocol().as_deref() == Ok("https:") { "wss" } else { "ws" };
    let host = loc.host().unwrap_or_default();
    format!("{scheme}://{host}/ws?token={token}")
}

fn push_event(shared: &Link, ev: Value) {
    let ctx = {
        let mut s = shared.borrow_mut();
        s.incoming.push_back(ev);
        s.ctx.clone()
    };
    if let Some(ctx) = ctx {
        ctx.request_repaint();
    }
}

fn connect(shared: &Link, token: &str) {
    let Ok(ws) = WebSocket::new(&ws_url(token)) else { return };

    let s = shared.clone();
    let onopen = Closure::<dyn FnMut()>::new(move || {
        let (socket, pending) = {
            let mut s = s.borrow_mut();
            (s.socket.clone(), std::mem::take(&mut s.outgoing))
        };
        if let Some(socket) = socket {
            for text in pending {
                let _ = socket.send_with_str(&text);
            }
        }
    });
    ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
    onopen.forget();

    let s = shared.clone();
    let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
        if let Some(text) = e.data().as_string() {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                push_event(&s, v);
            }
        }
    });
    ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();

    let s = shared.clone();
    let token = token.to_string();
    let onclose = Closure::<dyn FnMut(CloseEvent)>::new(move |_| {
        let first = {
            let mut s = s.borrow_mut();
            s.socket = None;
            !std::mem::replace(&mut s.lost, true)
        };
        if first {
            push_event(
                &s,
                json!({"type": "error", "tabId": "tab-1", "message": "Lost connection to the Kiro UI server. Reconnecting\u{2026}"}),
            );
            probe(RETRY_MS_FIRST, token.clone());
        }
    });
    ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
    onclose.forget();

    shared.borrow_mut().socket = Some(ws);
}

/// Retry with a throwaway socket; once one opens the server is back, and a
/// reload gives a fresh hub (and a fresh token).
fn probe(delay_ms: i32, token: String) {
    let cb = Closure::once_into_js(move || {
        let Ok(ws) = WebSocket::new(&ws_url(&token)) else { return probe(next_delay(delay_ms), token) };
        let onopen = Closure::once_into_js(|| {
            let _ = window().location().reload();
        });
        ws.set_onopen(Some(onopen.unchecked_ref()));
        let retry = Closure::once_into_js({
            let token = token.clone();
            move || probe(next_delay(delay_ms), token)
        });
        ws.set_onclose(Some(retry.unchecked_ref()));
    });
    let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), delay_ms);
}

fn next_delay(ms: i32) -> i32 {
    (ms * 2).min(RETRY_MS_MAX)
}

impl Backend for WebBackend {
    fn send(&self, msg: Value) {
        let text = msg.to_string();
        let mut s = self.0.borrow_mut();
        match &s.socket {
            Some(ws) if ws.ready_state() == WebSocket::OPEN => {
                let _ = ws.send_with_str(&text);
            }
            // Connecting: flushed in onopen. Closed: the page is about to reload.
            Some(_) => s.outgoing.push(text),
            None => {}
        }
    }

    fn poll(&self) -> Vec<Value> {
        self.0.borrow_mut().incoming.drain(..).collect()
    }

    fn open_url(&self, url: &str) {
        if allowed_url(url) {
            let _ = window().open_with_url_and_target_and_features(url, "_blank", "noopener,noreferrer");
        }
    }
}

#[wasm_bindgen(start)]
pub fn start() {
    let document = window().document().expect("document");
    let Some(canvas) = document.get_element_by_id("kiro").and_then(|e| e.dyn_into::<HtmlCanvasElement>().ok()) else {
        return;
    };
    let token = document
        .query_selector("meta[name=kiro-token]")
        .ok()
        .flatten()
        .and_then(|m| m.get_attribute("content"))
        .unwrap_or_default();

    let shared: Link = Rc::new(RefCell::new(Shared {
        socket: None,
        outgoing: Vec::new(),
        incoming: VecDeque::new(),
        ctx: None,
        lost: false,
    }));
    connect(&shared, &token);

    wasm_bindgen_futures::spawn_local(async move {
        let result = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(move |cc| {
                    shared.borrow_mut().ctx = Some(cc.egui_ctx.clone());
                    Ok(Box::new(KiroApp::new(cc, Box::new(WebBackend(shared)))))
                }),
            )
            .await;
        if let Err(e) = result {
            web_sys::console::error_1(&e);
        }
    });
}
