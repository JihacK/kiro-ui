//! Kiro UI — native desktop app (egui). No webview and no local server: the UI
//! talks to `kiro-ui-core` in-process, which talks ACP to `kiro-cli`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod chat;
mod input;
mod model;
mod settings;
mod theme;
mod view;

use eframe::egui;
use std::sync::OnceLock;
use std::time::Duration;

/// Set once the window exists, so a second launch can ask us to come to the front.
static UI_CTX: OnceLock<egui::Context> = OnceLock::new();

/// GUI launches (desktop menus, Finder) get a minimal PATH; adopt the login
/// shell's so `kiro-cli` resolves like it does in a terminal.
#[cfg(unix)]
fn fix_path() {
    use std::process::{Command, Stdio};
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let Ok(mut child) = Command::new(shell)
        .args(["-ilc", "printf '\\n__KIRO_PATH__%s\\n' \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    // Don't let a slow shell profile block startup.
    for _ in 0..30 {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return,
        }
    }
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
        return;
    }
    let Ok(out) = child.wait_with_output() else { return };
    let text = String::from_utf8_lossy(&out.stdout);
    if let Some(path) = text.lines().rev().find_map(|l| l.strip_prefix("__KIRO_PATH__")).filter(|p| !p.is_empty()) {
        std::env::set_var("PATH", path);
    }
}

fn focus_window() {
    if let Some(ctx) = UI_CTX.get() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }
}

/// Returns false if another instance is running (and has been asked to focus).
#[cfg(unix)]
fn single_instance() -> bool {
    use std::os::unix::net::{UnixListener, UnixStream};
    let dir = kiro_ui_core::home_dir().join(".kiro-ui");
    let _ = std::fs::create_dir_all(&dir);
    let sock = dir.join("desktop.sock");
    if UnixStream::connect(&sock).is_ok() {
        return false;
    }
    let _ = std::fs::remove_file(&sock); // stale socket from a crash
    match UnixListener::bind(&sock) {
        Ok(listener) => {
            std::thread::spawn(move || {
                for _ in listener.incoming().flatten() {
                    focus_window();
                }
            });
            true
        }
        Err(_) => true, // can't lock: run anyway
    }
}

#[cfg(not(unix))]
fn single_instance() -> bool {
    use std::net::{TcpListener, TcpStream};
    const PORT: u16 = 47_913;
    if TcpStream::connect(("127.0.0.1", PORT)).is_ok() {
        return false;
    }
    if let Ok(listener) = TcpListener::bind(("127.0.0.1", PORT)) {
        std::thread::spawn(move || {
            for _ in listener.incoming().flatten() {
                focus_window();
            }
        });
    }
    true
}

fn main() -> eframe::Result {
    #[cfg(unix)]
    if !cfg!(debug_assertions) {
        fix_path();
    }
    if !single_instance() {
        return Ok(());
    }

    // The core is I/O bound: two workers are plenty and keep memory down.
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime");
    let state = kiro_ui_core::init_state();

    let icon = egui::IconData { rgba: include_bytes!("../assets/icon-64.rgba").to_vec(), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kiro")
            .with_app_id("dev.kiro.ui")
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_icon(icon)
            .with_drag_and_drop(true),
        persist_window: true,
        ..Default::default()
    };

    let handle = rt.handle().clone();
    let result = eframe::run_native(
        "kiro-ui",
        options,
        Box::new(move |cc| {
            let _ = UI_CTX.set(cc.egui_ctx.clone());
            Ok(Box::new(app::KiroApp::new(cc, handle, state)))
        }),
    );
    rt.shutdown_timeout(Duration::from_secs(1));
    result
}
