//! Desktop backend: the core client runs in-process on the tokio runtime.

use eframe::egui;
use kiro_ui_core::{AppState, Client};
use serde_json::Value;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

pub struct NativeBackend {
    rt: tokio::runtime::Handle,
    state: Arc<AppState>,
    client: Client,
    events: Receiver<Value>,
}

impl NativeBackend {
    pub fn new(rt: tokio::runtime::Handle, state: Arc<AppState>, ctx: egui::Context) -> Self {
        let (tx, events) = channel();
        let client = {
            let _guard = rt.enter();
            Client::start(state.clone(), move |ev| {
                if tx.send(ev).is_ok() {
                    ctx.request_repaint();
                }
            })
        };
        Self { rt, state, client, events }
    }
}

impl kiro_ui_app::Backend for NativeBackend {
    fn send(&self, msg: Value) {
        self.client.send(msg);
    }

    fn poll(&self) -> Vec<Value> {
        self.events.try_iter().collect()
    }

    fn open_url(&self, url: &str) {
        kiro_ui_core::picker::open_url(url);
    }

    fn shutdown(&self) {
        self.client.close();
        let state = self.state.clone();
        let _ = self.rt.block_on(tokio::time::timeout(Duration::from_secs(4), async move { state.shutdown().await }));
    }
}
