//! Application shell: owns the core client, routes events into the model,
//! executes effects, and implements the user actions (port of `App.tsx`).

use crate::model::{self, CmdOption, Effect, Goal, GoalStatus, Model, Role, ToastKind};
use crate::theme;
use base64::Engine;
use eframe::egui;
use egui_commonmark::CommonMarkCache;
use kiro_ui_core::{picker, suggestions, AppState, Client, JsonMap};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Results of background work, delivered back to the UI thread.
pub enum Bg {
    Suggestions { tab: String, list: Vec<String>, error: Option<String> },
    Workspace { tab: String, path: Option<String> },
    Attached { tab: String, items: Vec<Attachment>, errors: Vec<String> },
    Exported(Result<String, String>),
    Models(Vec<(String, String, String)>),
    TestResult(bool, String),
}

const EXPORT_CANCELLED: &str = "cancelled";

/// Marks a native dialog as open; dropping it re-enables the dialog buttons.
pub struct DialogGuard {
    flag: Arc<AtomicBool>,
    ctx: egui::Context,
}

impl Drop for DialogGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
        self.ctx.request_repaint();
    }
}

fn try_begin_dialog(flag: &Arc<AtomicBool>, ctx: &egui::Context) -> Option<DialogGuard> {
    if flag.swap(true, Ordering::SeqCst) {
        return None;
    }
    Some(DialogGuard { flag: flag.clone(), ctx: ctx.clone() })
}

#[derive(Clone)]
pub enum Attachment {
    Image { name: String, mime: String, data: String },
    File { name: String, content: String },
}

impl Attachment {
    pub fn name(&self) -> &str {
        match self {
            Attachment::Image { name, .. } | Attachment::File { name, .. } => name,
        }
    }
}

/// Per-tab input state.
#[derive(Default)]
pub struct TabUi {
    pub input: String,
    pub attachments: Vec<Attachment>,
    pub popup: Option<Vec<CmdOption>>,
    pub popup_idx: usize,
    pub cmd_rev_seen: u64,
    /// Debounced `command_options` request: (due, command, partial).
    pub pending_options: Option<(Instant, String, String)>,
}

/// Editable copy of the persisted settings for the settings page.
#[derive(Default)]
pub struct SettingsForm {
    pub loaded: bool,
    pub workspace: String,
    pub transport: String,
    pub acp_command: String,
    pub tcp_host: String,
    pub tcp_port: String,
    pub share: bool,
    pub max_tabs: String,
    pub max_mem: String,
    pub max_msgs: String,
    pub sugg_enabled: bool,
    pub sugg_region: String,
    pub sugg_profile: String,
    pub sugg_model: String,
    pub sugg_count: String,
    pub testing: bool,
    pub test_result: Option<(bool, String)>,
    pub models: Vec<(String, String, String)>,
    pub models_loading: bool,
    pub trust: JsonMap,
    pub kiro_edits: HashMap<String, String>,
    pub kiro_pending: HashMap<String, Instant>,
    pub saved_flash: Option<(&'static str, Instant)>,
}

pub struct KiroApp {
    pub rt: tokio::runtime::Handle,
    pub state: Arc<AppState>,
    pub client: Arc<Client>,
    events: Receiver<Value>,
    pub bg_tx: Sender<Bg>,
    bg_rx: Receiver<Bg>,
    pub ctx: egui::Context,
    pub model: Model,
    pub md: CommonMarkCache,
    pub tab_ui: HashMap<String, TabUi>,
    pub dark: bool,
    pub sidebar_open: bool,
    pub show_settings: bool,
    pub show_rewind: bool,
    pub editor: String,
    pub debug: bool,
    pub session_filter: String,
    pub skill_filter: String,
    pub settings: SettingsForm,
    /// Collapsed state of tool groups, by first message uid.
    pub collapsed_groups: HashMap<u64, bool>,
    pub copied: Option<(u64, Instant)>,
    paste_counter: u64,
    dialog_open: Arc<AtomicBool>,
}

const PERSIST_KEY: &str = "kiro-ui";

#[derive(serde::Serialize, serde::Deserialize)]
struct Persisted {
    dark: bool,
    sidebar_open: bool,
}

impl KiroApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Handle, state: Arc<AppState>) -> KiroApp {
        let persisted: Option<Persisted> = cc.storage.and_then(|s| eframe::get_value(s, PERSIST_KEY));
        let (dark, sidebar_open) = persisted.map_or((true, true), |p| (p.dark, p.sidebar_open));
        theme::apply(&cc.egui_ctx, dark);

        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<Value>();
        let repaint = cc.egui_ctx.clone();
        let client = {
            let _guard = rt.enter();
            Client::start(state.clone(), move |ev| {
                let _ = ev_tx.send(ev);
                repaint.request_repaint();
            })
        };
        let (bg_tx, bg_rx) = std::sync::mpsc::channel();
        let mut app = KiroApp {
            rt,
            state: state.clone(),
            client: Arc::new(client),
            events: ev_rx,
            bg_tx,
            bg_rx,
            ctx: cc.egui_ctx.clone(),
            model: Model::new(),
            md: CommonMarkCache::default(),
            tab_ui: HashMap::new(),
            dark,
            sidebar_open,
            show_settings: false,
            show_rewind: false,
            editor: state.setting("editor").unwrap_or_else(|| "vscode".into()),
            debug: state.setting("debugEnabled").as_deref() == Some("true"),
            session_filter: String::new(),
            skill_filter: String::new(),
            settings: SettingsForm::default(),
            collapsed_groups: HashMap::new(),
            copied: None,
            paste_counter: 0,
            dialog_open: Arc::new(AtomicBool::new(false)),
        };
        if let Some(p) = state.setting("permPolicy") {
            app.model.active_tab_mut().perm_policy = p;
        }
        if app.debug {
            app.send(json!({"action": "set_debug", "enabled": true}));
        }
        app
    }

    // ------------------------------------------------------------- plumbing

    pub fn send(&self, msg: Value) {
        self.client.send(msg);
    }

    pub fn send_later(&self, msg: Value, ms: u64) {
        if ms == 0 {
            return self.send(msg);
        }
        let client = self.client.clone();
        self.rt.spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            client.send(msg);
        });
    }

    /// Run `fut` on the runtime and deliver its result to the UI thread.
    pub fn spawn_bg<F>(&self, fut: F)
    where
        F: std::future::Future<Output = Bg> + Send + 'static,
    {
        let tx = self.bg_tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            let _ = tx.send(fut.await);
            ctx.request_repaint();
        });
    }

    pub fn save_setting(&self, key: &str, value: Value) {
        let state = self.state.clone();
        let mut patch = JsonMap::new();
        patch.insert(key.to_string(), value);
        self.rt.spawn(async move {
            state.merge_settings(patch).await;
        });
    }

    fn run_effects(&mut self, fx: Vec<Effect>) {
        for e in fx {
            match e {
                Effect::Send(v, ms) => self.send_later(v, ms),
                Effect::Suggest { tab, context } => {
                    let state = self.state.clone();
                    let ctx_text: String = String::from_utf16_lossy(&context.encode_utf16().take(2000).collect::<Vec<_>>());
                    self.spawn_bg(async move {
                        let r = suggestions::suggest(&state, &ctx_text).await;
                        let list = r.get("suggestions").and_then(Value::as_array).into_iter().flatten().filter_map(|s| s.as_str().map(str::to_owned)).collect();
                        let error = r.get("error").and_then(Value::as_str).map(str::to_owned);
                        Bg::Suggestions { tab, list, error }
                    });
                }
                Effect::Notify(body) => {
                    if self.ctx.input(|i| i.viewport().focused) == Some(false) {
                        picker::notify("Kiro", &body);
                    }
                }
            }
        }
    }

    fn drain(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            let fx = self.model.handle(&ev);
            self.run_effects(fx);
        }
        // Debug builds only: send one prompt once connected (screenshot tests without input injection).
        #[cfg(debug_assertions)]
        if self.model.active_tab().modes.is_some() {
            if let Some(list) = std::env::var("KIRO_UI_DEMO_SKILLS").ok().filter(|s| !s.is_empty()) {
                std::env::remove_var("KIRO_UI_DEMO_SKILLS");
                self.model.active_tab_mut().skills = list.split(',').map(|s| s.trim().to_string()).collect();
            }
            if let Some(p) = std::env::var("KIRO_UI_DEMO_PROMPT").ok().filter(|p| !p.is_empty()) {
                std::env::remove_var("KIRO_UI_DEMO_PROMPT");
                self.handle_send(p, Vec::new());
            }
            if std::env::var_os("KIRO_UI_DEMO_SETTINGS").is_some() {
                std::env::remove_var("KIRO_UI_DEMO_SETTINGS");
                self.show_settings = true;
                self.open_settings();
            }
        }
        while let Ok(bg) = self.bg_rx.try_recv() {
            match bg {
                Bg::Suggestions { tab, list, error } => {
                    if let Some(t) = self.model.tab_mut(&tab) {
                        if !list.is_empty() {
                            t.suggestions = list;
                        }
                    }
                    if let Some(e) = error {
                        self.model.toast(format!("Suggestions: {e}"), ToastKind::Warning);
                    }
                }
                Bg::Workspace { tab, path } => {
                    if let Some(dir) = path {
                        if let Some(t) = self.model.tab_mut(&tab) {
                            t.cwd = Some(dir.clone());
                            t.connecting = true;
                            t.messages.clear();
                        }
                        self.send(json!({"action": "new_chat", "tabId": tab, "cwd": dir}));
                    }
                }
                Bg::Attached { tab, items, errors } => {
                    self.tab_ui.entry(tab).or_default().attachments.extend(items);
                    for e in errors {
                        self.model.toast(e, ToastKind::Warning);
                    }
                }
                Bg::Exported(r) => match r {
                    Ok(p) => self.model.toast(format!("Exported to {p}"), ToastKind::Info),
                    Err(e) if e == EXPORT_CANCELLED => {}
                    Err(e) => self.model.toast(format!("Export failed: {e}"), ToastKind::Error),
                },
                Bg::Models(list) => {
                    self.settings.models = list;
                    self.settings.models_loading = false;
                }
                Bg::TestResult(ok, msg) => {
                    self.settings.testing = false;
                    self.settings.test_result = Some((ok, msg));
                }
            }
        }
        // Toasts disappear after 5s.
        let now = Instant::now();
        self.model.toasts.retain(|t| now.duration_since(t.created) < Duration::from_secs(5));
    }

    // -------------------------------------------------------------- actions

    pub fn active_id(&self) -> String {
        self.model.active.clone()
    }

    pub fn new_tab(&mut self) {
        let cwd = self.model.active_tab().cwd.clone();
        let id = self.model.add_tab("New Chat", cwd.clone());
        self.send(json!({"action": "new_tab", "tabId": id, "cwd": cwd}));
    }

    pub fn close_tab(&mut self, id: &str) {
        if self.model.tabs.len() <= 1 {
            return;
        }
        self.send(json!({"action": "close_tab", "tabId": id}));
        self.model.close_tab(id);
        self.tab_ui.remove(id);
    }

    pub fn load_session(&mut self, id: &str, title: &str) {
        if let Some(t) = self.model.tabs.iter().find(|t| t.session_id.as_deref() == Some(id)) {
            self.model.active = t.id.clone();
            return;
        }
        let cwd = self.model.active_tab().cwd.clone();
        let name = if title.is_empty() { "Loading..." } else { title };
        let tab = self.model.add_tab(name, cwd);
        self.send(json!({"action": "load_session", "tabId": tab, "sessionId": id}));
    }

    pub fn list_sessions(&self) {
        self.send(json!({"action": "list_sessions", "tabId": self.model.active, "cwd": self.model.active_tab().cwd}));
    }

    pub fn pick_workspace(&self) {
        let Some(guard) = self.begin_dialog() else { return };
        let tab = self.active_id();
        let start = self.model.active_tab().cwd.clone().unwrap_or_default();
        self.spawn_bg(async move {
            let path = picker::pick_folder(&start).await;
            drop(guard);
            Bg::Workspace { tab, path }
        });
    }

    pub fn attach_files(&self) {
        let Some(guard) = self.begin_dialog() else { return };
        let tab = self.active_id();
        let start = self.model.active_tab().cwd.clone().unwrap_or_default();
        self.spawn_bg(async move {
            let paths = picker::pick_files(&start).await;
            drop(guard);
            let (items, errors) = read_attachments(paths).await;
            Bg::Attached { tab, items, errors }
        });
    }

    pub fn attach_paths(&self, paths: Vec<String>) {
        let tab = self.active_id();
        self.spawn_bg(async move {
            let (items, errors) = read_attachments(paths).await;
            Bg::Attached { tab, items, errors }
        });
    }

    /// Plain text send (suggestion buttons, /compact): never queued, ignored while running.
    pub fn send_text(&mut self, text: &str) {
        let tab_id = self.active_id();
        let tab = self.model.active_tab();
        if text.is_empty() || tab.running {
            return;
        }
        let m = self.model.msg(Role::User, text);
        let t = self.model.active_tab_mut();
        t.messages.push(m);
        t.running = true;
        t.stream.clear();
        t.active_files.clear();
        t.suggestions.clear();
        let skills = self.model.active_tab().skills.clone();
        self.send(json!({"action": "prompt", "tabId": tab_id, "text": model::with_skills(&skills, text)}));
    }

    /// Main send path: local panels, queueing while running, /goal tracking, attachments.
    pub fn handle_send(&mut self, text: String, attachments: Vec<Attachment>) {
        if text.is_empty() && attachments.is_empty() {
            return;
        }
        let tab_id = self.active_id();
        let panel = match text.as_str() {
            "/mcp" | "/mcp list" => Some("mcp"),
            "/tools" => Some("tools"),
            "/context" | "/context show" => Some("context"),
            "/help" => Some("help"),
            _ => None,
        };
        if let Some(kind) = panel {
            let u = self.model.msg(Role::User, text.clone());
            let mut p = self.model.msg(Role::Panel, "");
            p.panel = Some(kind.into());
            let t = self.model.active_tab_mut();
            t.messages.push(u);
            t.messages.push(p);
            return;
        }
        if self.model.active_tab().running {
            self.model.active_tab_mut().queue.push(text);
            return;
        }
        let m = self.model.msg(Role::User, text.clone());
        let t = self.model.active_tab_mut();
        if text == "/goal clear" {
            t.goal = None;
        } else if let Some((goal, max)) = model::parse_goal(&text) {
            t.goal = Some(Goal { text: goal, max, current: 1, status: GoalStatus::Active });
        }
        t.messages.push(m);
        t.running = true;
        t.stream.clear();
        t.active_files.clear();
        t.suggestions.clear();
        let skills = t.skills.clone();
        let mut msg = json!({"action": "prompt", "tabId": tab_id, "text": model::with_skills(&skills, &text)});
        let images: Vec<Value> = attachments
            .iter()
            .filter_map(|a| match a {
                Attachment::Image { name, mime, data } => Some(json!({"data": data, "mimeType": mime, "name": name})),
                _ => None,
            })
            .collect();
        let files: Vec<Value> = attachments
            .iter()
            .filter_map(|a| match a {
                Attachment::File { name, content } => Some(json!({"name": name, "content": content})),
                _ => None,
            })
            .collect();
        if !images.is_empty() {
            msg["images"] = Value::Array(images);
        }
        if !files.is_empty() {
            msg["files"] = Value::Array(files);
        }
        self.send(msg);
    }

    pub fn cancel(&self) {
        self.send(json!({"action": "cancel", "tabId": self.model.active}));
    }

    pub fn retry(&mut self, idx: usize) {
        let tab_id = self.active_id();
        let Some(text) = self.model.active_tab().messages.get(idx).filter(|m| m.role == Role::User).map(|m| m.text.clone()) else { return };
        let m = self.model.msg(Role::User, text.clone());
        let t = self.model.active_tab_mut();
        t.messages.truncate(idx);
        t.messages.push(m);
        t.running = true;
        t.stream.clear();
        t.last_stop = None;
        let skills = self.model.active_tab().skills.clone();
        self.send(json!({"action": "prompt", "tabId": tab_id, "text": model::with_skills(&skills, &text)}));
    }

    /// Rewind to the user turn at message index `turn_idx` (kiro `/rewind N`).
    pub fn rewind(&mut self, turn_idx: usize) {
        self.show_rewind = false;
        let tab_id = self.active_id();
        let t = self.model.active_tab_mut();
        let turn_num = t.messages[..=turn_idx.min(t.messages.len().saturating_sub(1))].iter().filter(|m| m.role.is_user()).count();
        let end = t.messages.iter().enumerate().skip(turn_idx + 1).find(|(_, m)| m.role.is_user()).map_or(t.messages.len(), |(i, _)| i);
        t.messages.truncate(end);
        t.thinking = None;
        t.running = true;
        t.stream.clear();
        self.send(json!({"action": "prompt", "tabId": tab_id, "text": format!("/rewind {turn_num}")}));
    }

    pub fn respond_permission(&mut self, request_id: Value, option_id: &str, title: &str) {
        let tab_id = self.active_id();
        self.send(json!({"action": "permission_response", "tabId": tab_id, "requestId": request_id, "optionId": option_id, "title": title}));
        self.model.active_tab_mut().permissions.retain(|p| p.request_id != request_id);
    }

    pub fn export(&self) {
        let Some(guard) = self.begin_dialog() else { return };
        let tab = self.model.active_tab();
        let md = model::export_markdown(tab);
        let name: String = tab.name.chars().filter(|c| c.is_ascii_alphanumeric() || " -_".contains(*c)).collect();
        let file = format!("{}.md", if name.trim().is_empty() { "chat" } else { name.trim() });
        let start = tab.cwd.clone().unwrap_or_else(|| self.state.workspace());
        self.spawn_bg(async move {
            let path = picker::pick_save(&start, &file).await;
            drop(guard);
            let Some(path) = path else { return Bg::Exported(Err(EXPORT_CANCELLED.into())) };
            Bg::Exported(tokio::fs::write(&path, md).await.map(|_| path).map_err(|e| e.to_string()))
        });
    }

    /// One native dialog at a time: a double click must not open a second picker.
    pub fn begin_dialog(&self) -> Option<DialogGuard> {
        try_begin_dialog(&self.dialog_open, &self.ctx)
    }

    pub fn dialog_open(&self) -> bool {
        self.dialog_open.load(Ordering::SeqCst)
    }

    pub fn set_dark(&mut self, dark: bool) {
        self.dark = dark;
        theme::apply(&self.ctx, dark);
    }

    pub fn editor_link(&self, path: &str, line: Option<u64>) -> Option<String> {
        let scheme = match self.editor.as_str() {
            "cursor" => "cursor://file/",
            "idea" => "idea://open?file=",
            "none" => return None,
            _ => "vscode://file/",
        };
        Some(format!("{scheme}{path}{}", line.map(|l| format!(":{l}")).unwrap_or_default()))
    }

    pub fn ui_for(&mut self, tab: &str) -> &mut TabUi {
        self.tab_ui.entry(tab.to_string()).or_default()
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        let (new, clear, sidebar) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::T) | i.consume_key(Modifiers::COMMAND, Key::N),
                i.consume_key(Modifiers::COMMAND, Key::L),
                i.consume_key(Modifiers::COMMAND, Key::B),
            )
        });
        if new {
            self.new_tab();
        }
        if clear {
            let t = self.model.active_tab_mut();
            t.messages.clear();
            t.thinking = None;
        }
        if sidebar {
            self.sidebar_open = !self.sidebar_open;
        }
        // Files dropped onto the window become attachments.
        let dropped: Vec<String> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_string_lossy().into_owned()).filter(|p| !p.is_empty()).collect());
        if !dropped.is_empty() {
            self.attach_paths(dropped);
        }
        // Ctrl/⌘+V with an image (and no text) on the clipboard.
        let pasted: Vec<Arc<egui::ColorImage>> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::PasteImage(img) => Some(img.clone()),
                    _ => None,
                })
                .collect()
        });
        if !self.show_settings {
            for img in pasted {
                self.attach_pasted_image(img);
            }
        }
    }

    /// Encode a pasted clipboard image as PNG (off the UI thread) and attach it.
    pub fn attach_pasted_image(&mut self, img: Arc<egui::ColorImage>) {
        let tab = self.active_id();
        self.paste_counter += 1;
        let name = format!("pasted-image-{}.png", self.paste_counter);
        self.spawn_bg(async move {
            let encoded = tokio::task::spawn_blocking(move || encode_png(&img)).await.unwrap_or_else(|e| Err(e.to_string()));
            match encoded {
                Ok(bytes) if bytes.len() as u64 > MAX_ATTACHMENT => Bg::Attached { tab, items: vec![], errors: vec![format!("{name}: larger than 10 MB")] },
                Ok(bytes) => Bg::Attached {
                    tab,
                    items: vec![Attachment::Image { name, mime: "image/png".into(), data: base64::engine::general_purpose::STANDARD.encode(&bytes) }],
                    errors: vec![],
                },
                Err(e) => Bg::Attached { tab, items: vec![], errors: vec![format!("Pasted image: {e}")] },
            }
        });
    }

    /// Route link clicks through the scheme whitelist instead of eframe's opener.
    fn handle_links(&self, ctx: &egui::Context) {
        let urls: Vec<String> = ctx.output_mut(|o| {
            let mut urls = Vec::new();
            o.commands.retain(|c| match c {
                egui::OutputCommand::OpenUrl(u) => {
                    urls.push(u.url.clone());
                    false
                }
                _ => true,
            });
            urls
        });
        for u in urls {
            picker::open_url(&u);
        }
    }
}

const IMAGE_EXT: &[(&str, &str)] = &[("png", "image/png"), ("jpg", "image/jpeg"), ("jpeg", "image/jpeg"), ("gif", "image/gif"), ("webp", "image/webp")];
const MAX_ATTACHMENT: u64 = 10 * 1024 * 1024;

/// RGBA PNG from an egui image (egui stores premultiplied alpha).
pub fn encode_png(img: &egui::ColorImage) -> Result<Vec<u8>, String> {
    let [w, h] = img.size;
    if w == 0 || h == 0 {
        return Err("empty image".into());
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for c in &img.pixels {
        rgba.extend_from_slice(&c.to_srgba_unmultiplied());
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&rgba).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

async fn read_attachments(paths: Vec<String>) -> (Vec<Attachment>, Vec<String>) {
    let mut items = Vec::new();
    let mut errors = Vec::new();
    for p in paths {
        let path = std::path::Path::new(&p);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.clone());
        match tokio::fs::metadata(path).await {
            Ok(m) if m.len() > MAX_ATTACHMENT => {
                errors.push(format!("{name}: larger than 10 MB"));
                continue;
            }
            Err(e) => {
                errors.push(format!("{name}: {e}"));
                continue;
            }
            _ => {}
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let Ok(bytes) = tokio::fs::read(path).await else {
            errors.push(format!("{name}: cannot read"));
            continue;
        };
        if let Some((_, mime)) = IMAGE_EXT.iter().find(|(e, _)| *e == ext) {
            items.push(Attachment::Image { name, mime: (*mime).into(), data: base64::engine::general_purpose::STANDARD.encode(&bytes) });
        } else {
            match String::from_utf8(bytes) {
                Ok(content) => items.push(Attachment::File { name, content }),
                Err(_) => errors.push(format!("{name}: not a text file")),
            }
        }
    }
    (items, errors)
}

impl eframe::App for KiroApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain();
        self.shortcuts(&ctx);
        self.draw(ui);
        self.handle_links(&ctx);
        // Only animate while something is live; otherwise egui sleeps until input/events.
        let t = self.model.active_tab();
        if t.running || t.thinking.is_some() || t.connecting {
            ctx.request_repaint_after(Duration::from_millis(250));
        } else if !self.model.toasts.is_empty() || self.tab_ui.values().any(|u| u.pending_options.is_some()) || !self.settings.kiro_pending.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, PERSIST_KEY, &Persisted { dark: self.dark, sidebar_open: self.sidebar_open });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.client.close();
        let state = self.state.clone();
        let _ = self.rt.block_on(async move { tokio::time::timeout(Duration::from_secs(4), state.shutdown()).await });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_image_png_roundtrip() {
        let mut img = egui::ColorImage::filled([3, 2], egui::Color32::from_rgb(10, 20, 30));
        img.pixels[1] = egui::Color32::from_rgba_unmultiplied(200, 100, 50, 128);
        let bytes = encode_png(&img).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height, info.color_type), (3, 2, png::ColorType::Rgba));
        assert_eq!(&buf[0..4], &[10, 20, 30, 255]);
        // Half-transparent pixel comes back unpremultiplied (within rounding).
        let px = &buf[4..8];
        assert_eq!(px[3], 128);
        assert!((px[0] as i32 - 200).abs() <= 2 && (px[1] as i32 - 100).abs() <= 2, "{px:?}");
        assert!(encode_png(&egui::ColorImage::filled([0, 0], egui::Color32::BLACK)).is_err());
    }

    #[test]
    fn second_dialog_is_refused_until_first_closes() {
        let flag = Arc::new(AtomicBool::new(false));
        let ctx = egui::Context::default();
        let first = try_begin_dialog(&flag, &ctx).expect("first dialog opens");
        assert!(try_begin_dialog(&flag, &ctx).is_none(), "double click must not open a second dialog");
        drop(first);
        assert!(try_begin_dialog(&flag, &ctx).is_some(), "available again after the first closes");
    }
}
