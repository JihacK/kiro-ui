//! Application shell: routes backend events into the model, executes effects
//! (delayed sends, suggestions, notifications), and implements the user actions.
//! Everything outside the UI (agent, settings store, file dialogs) is reached
//! through [`Backend`] as JSON actions; replies arrive as events.

use crate::backend::{allowed_url, Backend};
use crate::model::{self, CmdOption, Effect, Goal, GoalStatus, Model, Role, ToastKind};
use crate::settings::setting_str;
use crate::theme;
use base64::Engine;
use eframe::egui;
use egui_commonmark::CommonMarkCache;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use web_time::Instant;

pub type JsonMap = serde_json::Map<String, Value>;

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

    /// From a `FilesAttached` item (`{"kind":"image",...}` or `{"kind":"file",...}`).
    fn from_item(v: &Value) -> Option<Attachment> {
        let g = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        match v.get("kind").and_then(Value::as_str)? {
            "image" => Some(Attachment::Image { name: g("name")?, mime: g("mime")?, data: g("data")? }),
            "file" => Some(Attachment::File { name: g("name")?, content: g("content")? }),
            _ => None,
        }
    }
}

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
    /// Latest `Settings` event from the backend (`settings`, `trust`, `workspace`).
    pub snapshot: Option<Value>,
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
    pub models_error: Option<String>,
    pub trust: JsonMap,
    pub kiro_edits: HashMap<String, String>,
    pub kiro_pending: HashMap<String, Instant>,
    /// Settings keys saved recently, for the "✔ Saved" mark.
    pub saved_flash: HashMap<String, Instant>,
}

/// A file dropped in the browser, read asynchronously: (tab, attachment or error).
type DroppedInbox = Rc<RefCell<Vec<(String, Result<Attachment, String>)>>>;

pub struct KiroApp {
    backend: Box<dyn Backend>,
    /// Sends waiting for their due time (`Effect::Send` with a delay).
    delayed: Vec<(Instant, Value)>,
    pub ctx: egui::Context,
    pub model: Model,
    pub md: CommonMarkCache,
    pub tab_ui: HashMap<String, TabUi>,
    pub dark: bool,
    /// Theme saved on exit; only the user's toggle changes it.
    saved_dark: bool,
    pub sidebar_open: bool,
    pub show_settings: bool,
    pub show_rewind: bool,
    /// Session waiting for delete confirmation: (id, title).
    pub confirm_delete: Option<(String, String)>,
    pub editor: String,
    pub debug: bool,
    /// No looping animations; egui transitions off.
    pub reduce_motion: bool,
    pub session_filter: String,
    pub skill_filter: String,
    pub settings: SettingsForm,
    /// Collapsed state of tool groups, by first message uid.
    pub collapsed_groups: HashMap<u64, bool>,
    pub copied: Option<(u64, Instant)>,
    paste_counter: u64,
    /// A folder/file/save dialog is open on the backend side; its reply clears this.
    dialog_pending: Cell<bool>,
    /// `attach_paths` requests (window drops) whose `FilesAttached` reply is still due.
    drops_in_flight: Cell<u32>,
    /// Startup settings (editor, debug, motion, policy) are applied once.
    settings_applied: bool,
    dropped_inbox: DroppedInbox,
}

const PERSIST_KEY: &str = "kiro-ui";

#[derive(serde::Serialize, serde::Deserialize)]
struct Persisted {
    dark: bool,
    sidebar_open: bool,
}

impl KiroApp {
    pub fn new(cc: &eframe::CreationContext<'_>, backend: Box<dyn Backend>) -> KiroApp {
        let persisted: Option<Persisted> = cc.storage.and_then(|s| eframe::get_value(s, PERSIST_KEY));
        let (dark, sidebar_open) = persisted.map_or((true, true), |p| (p.dark, p.sidebar_open));
        KiroApp::with_parts(cc.egui_ctx.clone(), backend, dark, sidebar_open)
    }

    /// Everything `new` does that does not need eframe's creation context.
    fn with_parts(ctx: egui::Context, backend: Box<dyn Backend>, dark: bool, sidebar_open: bool) -> KiroApp {
        theme::apply(&ctx, dark);
        let mut app = KiroApp {
            backend,
            delayed: Vec::new(),
            ctx,
            model: Model::new(),
            md: CommonMarkCache::default(),
            tab_ui: HashMap::new(),
            dark,
            saved_dark: dark,
            sidebar_open,
            show_settings: false,
            show_rewind: false,
            confirm_delete: None,
            editor: "vscode".into(),
            debug: false,
            reduce_motion: false,
            session_filter: String::new(),
            skill_filter: String::new(),
            settings: SettingsForm::default(),
            collapsed_groups: HashMap::new(),
            copied: None,
            paste_counter: 0,
            dialog_pending: Cell::new(false),
            drops_in_flight: Cell::new(0),
            settings_applied: false,
            dropped_inbox: Rc::default(),
        };
        app.set_reduce_motion(false);
        app.send(json!({"action": "get_settings"}));
        app
    }

    pub fn send(&self, msg: Value) {
        self.backend.send(msg);
    }

    /// Send now (`ms == 0`) or once `ms` have passed; see `flush_delayed`.
    pub fn send_later(&mut self, msg: Value, ms: u64) {
        if ms == 0 {
            return self.send(msg);
        }
        self.delayed.push((Instant::now() + Duration::from_millis(ms), msg));
    }

    /// Send what is due at `now`; returns the wait until the next one, if any.
    fn flush_delayed(&mut self, now: Instant) -> Option<Duration> {
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.delayed).into_iter().partition(|(at, _)| *at <= now);
        self.delayed = later;
        for (_, msg) in due {
            self.send(msg);
        }
        self.delayed.iter().map(|(at, _)| at.saturating_duration_since(now)).min()
    }

    pub fn save_setting(&self, key: &str, value: Value) {
        let mut patch = JsonMap::new();
        patch.insert(key.to_string(), value);
        self.send(json!({"action": "merge_settings", "patch": patch}));
    }

    pub fn open_link(&self, url: &str) {
        if allowed_url(url) {
            self.backend.open_url(url);
        }
    }

    fn run_effects(&mut self, fx: Vec<Effect>) {
        for e in fx {
            match e {
                Effect::Send(v, ms) => self.send_later(v, ms),
                Effect::Suggest { tab, context } => {
                    let context = String::from_utf16_lossy(&context.encode_utf16().take(2000).collect::<Vec<_>>());
                    self.send(json!({"action": "suggest", "tabId": tab, "context": context}));
                }
                Effect::Notify(body) => {
                    if self.ctx.input(|i| i.viewport().focused) == Some(false) {
                        self.send(json!({"action": "notify", "body": body}));
                    }
                }
            }
        }
    }

    fn drain(&mut self) {
        for ev in self.backend.poll() {
            self.handle_event(&ev);
        }
        self.drain_dropped();
        // Debug builds only: send one prompt once connected (screenshot tests without input injection).
        #[cfg(all(debug_assertions, not(target_arch = "wasm32")))]
        self.demo_hooks();
        let now = Instant::now();
        self.model.toasts.retain(|t| now.duration_since(t.created) < Duration::from_secs(5));
    }

    /// App-level events are handled here; the rest go to the pure model.
    fn handle_event(&mut self, ev: &Value) {
        let tab = || ev.get("tabId").and_then(Value::as_str).unwrap_or_default().to_string();
        match ev.get("type").and_then(Value::as_str) {
            Some("Settings") => self.on_settings(ev),
            Some("Suggestions") => {
                let list: Vec<String> = ev.get("suggestions").and_then(Value::as_array).into_iter().flatten().filter_map(|s| s.as_str().map(str::to_owned)).collect();
                if let Some(t) = self.model.tab_mut(&tab()) {
                    if !list.is_empty() {
                        t.suggestions = list;
                    }
                }
                if let Some(e) = ev.get("error").and_then(Value::as_str) {
                    self.model.toast(format!("Suggestions: {e}"), ToastKind::Warning);
                }
            }
            Some("SuggestionModels") => {
                self.settings.models = ev
                    .get("models")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|m| {
                        let g = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                        (g("id"), g("name"), g("group"))
                    })
                    .collect();
                self.settings.models_error = ev.get("error").and_then(Value::as_str).map(str::to_owned);
                self.settings.models_loading = false;
            }
            Some("SuggestionTest") => {
                let ok = ev.get("ok").and_then(Value::as_bool).unwrap_or(false);
                let msg = if ok { ev.get("model") } else { ev.get("error") }.and_then(Value::as_str).unwrap_or_default().to_string();
                self.settings.testing = false;
                self.settings.test_result = Some((ok, msg));
            }
            Some("FolderPicked") => {
                self.dialog_pending.set(false);
                if let Some(dir) = ev.get("path").and_then(Value::as_str) {
                    let tab = tab();
                    if let Some(t) = self.model.tab_mut(&tab) {
                        t.cwd = Some(dir.to_string());
                        t.connecting = true;
                        t.messages.clear();
                    }
                    self.send(json!({"action": "new_chat", "tabId": tab, "cwd": dir}));
                }
            }
            Some("FilesAttached") => {
                let drops = self.drops_in_flight.get();
                if drops > 0 {
                    self.drops_in_flight.set(drops - 1);
                } else {
                    self.dialog_pending.set(false);
                }
                let items = ev.get("items").and_then(Value::as_array).into_iter().flatten().filter_map(Attachment::from_item);
                self.tab_ui.entry(tab()).or_default().attachments.extend(items);
                for e in ev.get("errors").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                    self.model.toast(e, ToastKind::Warning);
                }
            }
            Some("Exported") => {
                self.dialog_pending.set(false);
                if let Some(p) = ev.get("path").and_then(Value::as_str) {
                    self.model.toast(format!("Exported to {p}"), ToastKind::Info);
                } else if let Some(e) = ev.get("error").and_then(Value::as_str) {
                    self.model.toast(format!("Export failed: {e}"), ToastKind::Error);
                }
            }
            _ => {
                let fx = self.model.handle(ev);
                self.run_effects(fx);
            }
        }
    }

    /// Store the snapshot; the first one also applies the startup preferences.
    fn on_settings(&mut self, ev: &Value) {
        self.settings.snapshot = Some(ev.clone());
        // Don't overwrite a form the user may be editing.
        if !self.show_settings {
            self.fill_settings_form();
        } else if let Some(t) = ev.get("trust").and_then(Value::as_object) {
            self.settings.trust = t.clone();
        }
        if self.settings_applied {
            return;
        }
        self.settings_applied = true;
        let get = |k: &str| setting_str(ev.get("settings"), k);
        if let Some(e) = get("editor") {
            self.editor = e;
        }
        self.debug = get("debugEnabled").as_deref() == Some("true");
        if self.debug {
            self.send(json!({"action": "set_debug", "enabled": true}));
        }
        self.set_reduce_motion(get("reduceMotion").as_deref() == Some("true"));
        if let Some(p) = get("permPolicy") {
            let t = self.model.active_tab_mut();
            if t.perm_policy != p {
                t.perm_policy = p.clone();
                let tab = t.id.clone();
                self.send(json!({"action": "set_permission_policy", "tabId": tab, "policy": p}));
            }
        }
    }

    fn drain_dropped(&mut self) {
        let done = std::mem::take(&mut *self.dropped_inbox.borrow_mut());
        for (tab, r) in done {
            match r {
                Ok(a) => self.tab_ui.entry(tab).or_default().attachments.push(a),
                Err(e) => self.model.toast(e, ToastKind::Warning),
            }
        }
    }

    #[cfg(all(debug_assertions, not(target_arch = "wasm32")))]
    fn demo_hooks(&mut self) {
        if self.model.active_tab().modes.is_none() {
            return;
        }
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
        if std::env::var_os("KIRO_UI_DEMO_LIGHT").is_some() {
            std::env::remove_var("KIRO_UI_DEMO_LIGHT");
            self.dark = false;
            theme::apply(&self.ctx, false);
        }
    }

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

    pub fn delete_session(&mut self, id: &str) {
        let (fx, closed) = self.model.delete_session(id);
        for tab in closed {
            self.tab_ui.remove(&tab);
        }
        self.run_effects(fx);
    }

    pub fn pick_workspace(&self) {
        if !self.begin_dialog() {
            return;
        }
        let start = self.model.active_tab().cwd.clone().unwrap_or_default();
        self.send(json!({"action": "pick_folder", "tabId": self.active_id(), "start": start}));
    }

    pub fn attach_files(&self) {
        if !self.begin_dialog() {
            return;
        }
        let start = self.model.active_tab().cwd.clone().unwrap_or_default();
        self.send(json!({"action": "pick_files", "tabId": self.active_id(), "start": start}));
    }

    pub fn attach_paths(&self, paths: Vec<String>) {
        self.drops_in_flight.set(self.drops_in_flight.get() + 1);
        self.send(json!({"action": "attach_paths", "tabId": self.active_id(), "paths": paths}));
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
        if !self.begin_dialog() {
            return;
        }
        let tab = self.model.active_tab();
        let name: String = tab.name.chars().filter(|c| c.is_ascii_alphanumeric() || " -_".contains(*c)).collect();
        let file = format!("{}.md", if name.trim().is_empty() { "chat" } else { name.trim() });
        // Empty start: the core falls back to its workspace.
        let start = tab.cwd.clone().unwrap_or_default();
        self.send(json!({"action": "export", "markdown": model::export_markdown(tab), "fileName": file, "start": start}));
    }

    /// One dialog at a time: a double click must not open a second picker.
    /// True if the caller may open one; it stays blocked until the reply event.
    fn begin_dialog(&self) -> bool {
        !self.dialog_pending.replace(true)
    }

    pub fn dialog_open(&self) -> bool {
        self.dialog_pending.get()
    }

    pub fn set_reduce_motion(&mut self, on: bool) {
        self.reduce_motion = on;
        self.ctx.all_styles_mut(|s| s.animation_time = if on { 0.0 } else { 1.0 / 12.0 });
    }

    pub fn set_dark(&mut self, dark: bool) {
        self.dark = dark;
        self.saved_dark = dark;
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
        // Tab switching: Ctrl+Tab / Ctrl+Shift+Tab cycle, Ctrl/⌘+1..9 jump.
        let n = self.model.tabs.len();
        let cur = self.model.tabs.iter().position(|t| t.id == self.model.active).unwrap_or(0);
        let target = ctx.input_mut(|i| {
            if i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab) {
                return Some((cur + n - 1) % n);
            }
            if i.consume_key(Modifiers::CTRL, Key::Tab) {
                return Some((cur + 1) % n);
            }
            const DIGITS: [Key; 9] = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
            DIGITS.iter().position(|k| i.consume_key(Modifiers::COMMAND, *k)).filter(|d| *d < n)
        });
        if let Some(t) = target {
            self.model.active = self.model.tabs[t].id.clone();
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped.is_empty() {
            self.attach_dropped(dropped);
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

    /// Native drops carry real paths for the backend to read; the browser has only bytes.
    #[cfg(not(target_arch = "wasm32"))]
    fn attach_dropped(&self, files: Vec<egui::DroppedFileHandle>) {
        let paths: Vec<String> = files.iter().map(|f| f.path().to_string_lossy().into_owned()).filter(|p| !p.is_empty()).collect();
        if !paths.is_empty() {
            self.attach_paths(paths);
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn attach_dropped(&self, files: Vec<egui::DroppedFileHandle>) {
        for f in files {
            let (inbox, ctx, tab) = (self.dropped_inbox.clone(), self.ctx.clone(), self.active_id());
            wasm_bindgen_futures::spawn_local(async move {
                let name = f.path().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let r = match f.bytes_async().await {
                    Ok(bytes) => attachment_from_bytes(&name, &bytes),
                    Err(e) => Err(format!("{name}: {e}")),
                };
                inbox.borrow_mut().push((tab, r));
                ctx.request_repaint();
            });
        }
    }

    /// Encode a pasted clipboard image as PNG and attach it.
    pub fn attach_pasted_image(&mut self, img: Arc<egui::ColorImage>) {
        self.paste_counter += 1;
        let name = format!("pasted-image-{}.png", self.paste_counter);
        let tu = self.tab_ui.entry(self.model.active.clone()).or_default();
        match encode_png(&img) {
            Ok(bytes) if bytes.len() as u64 > MAX_ATTACHMENT => self.model.toast(format!("{name}: larger than 10 MB"), ToastKind::Warning),
            Ok(bytes) => tu.attachments.push(Attachment::Image { name, mime: "image/png".into(), data: base64::engine::general_purpose::STANDARD.encode(&bytes) }),
            Err(e) => self.model.toast(format!("Pasted image: {e}"), ToastKind::Warning),
        }
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
            self.open_link(&u);
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

/// Attachment from raw bytes (browser drops): same limits as the core's `attach_paths`.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn attachment_from_bytes(name: &str, bytes: &[u8]) -> Result<Attachment, String> {
    if bytes.len() as u64 > MAX_ATTACHMENT {
        return Err(format!("{name}: larger than 10 MB"));
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    if let Some((_, mime)) = IMAGE_EXT.iter().find(|(e, _)| *e == ext) {
        return Ok(Attachment::Image { name: name.into(), mime: (*mime).into(), data: base64::engine::general_purpose::STANDARD.encode(bytes) });
    }
    match std::str::from_utf8(bytes) {
        Ok(content) => Ok(Attachment::File { name: name.into(), content: content.into() }),
        Err(_) => Err(format!("{name}: not a text file")),
    }
}

impl eframe::App for KiroApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain();
        let next_send = self.flush_delayed(Instant::now());
        self.shortcuts(&ctx);
        self.draw(ui);
        self.handle_links(&ctx);
        if let Some(wait) = next_send {
            ctx.request_repaint_after(wait);
        }
        // Only animate while something is live; otherwise egui sleeps until input/events.
        let t = self.model.active_tab();
        if t.running || t.thinking.is_some() || t.connecting {
            // Reduced motion only needs the 1s thinking timer.
            ctx.request_repaint_after(Duration::from_millis(if self.reduce_motion { 1000 } else { 250 }));
        } else if !self.model.toasts.is_empty() || self.tab_ui.values().any(|u| u.pending_options.is_some()) || !self.settings.kiro_pending.is_empty() || !self.settings.saved_flash.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, PERSIST_KEY, &Persisted { dark: self.saved_dark, sidebar_open: self.sidebar_open });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.backend.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

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

    #[derive(Clone, Default)]
    struct Fake {
        sent: Rc<RefCell<Vec<Value>>>,
        incoming: Rc<RefCell<VecDeque<Value>>>,
        opened: Rc<RefCell<Vec<String>>>,
    }

    impl Backend for Fake {
        fn send(&self, msg: Value) {
            self.sent.borrow_mut().push(msg);
        }
        fn poll(&self) -> Vec<Value> {
            self.incoming.borrow_mut().drain(..).collect()
        }
        fn open_url(&self, url: &str) {
            self.opened.borrow_mut().push(url.into());
        }
    }

    fn app() -> (KiroApp, Fake) {
        let fake = Fake::default();
        let app = KiroApp::with_parts(egui::Context::default(), Box::new(fake.clone()), true, true);
        (app, fake)
    }

    fn actions(f: &Fake) -> Vec<String> {
        f.sent.borrow().iter().filter_map(|m| m["action"].as_str().map(str::to_owned)).collect()
    }

    #[test]
    fn startup_requests_settings() {
        let (_app, f) = app();
        assert_eq!(actions(&f), ["get_settings"]);
    }

    #[test]
    fn delayed_sends_wait_for_their_due_time() {
        let (mut app, f) = app();
        f.sent.borrow_mut().clear();
        app.run_effects(vec![Effect::Send(json!({"action": "a"}), 0), Effect::Send(json!({"action": "b"}), 100), Effect::Send(json!({"action": "c"}), 300)]);
        assert_eq!(actions(&f), ["a"]);
        let t0 = Instant::now();
        assert!(app.flush_delayed(t0).is_some());
        assert_eq!(actions(&f), ["a"], "nothing due yet");
        let wait = app.flush_delayed(t0 + Duration::from_millis(150)).expect("c still pending");
        assert_eq!(actions(&f), ["a", "b"]);
        assert!(wait <= Duration::from_millis(300));
        assert!(app.flush_delayed(t0 + Duration::from_secs(1)).is_none());
        assert_eq!(actions(&f), ["a", "b", "c"]);
    }

    #[test]
    fn settings_event_applies_startup_preferences_once() {
        let (mut app, f) = app();
        f.sent.borrow_mut().clear();
        f.incoming.borrow_mut().push_back(json!({"type": "Settings", "settings": {"editor": "cursor", "debugEnabled": "true", "reduceMotion": "true", "permPolicy": "trust_all"}, "trust": {"fs_read": "allow_always"}, "workspace": "/w"}));
        app.drain();
        assert_eq!(app.editor, "cursor");
        assert!(app.debug && app.reduce_motion);
        assert_eq!(app.model.active_tab().perm_policy, "trust_all");
        assert!(actions(&f).contains(&"set_debug".to_string()));
        assert_eq!(app.settings.trust.len(), 1);
        // A later snapshot refreshes the form but not the applied preferences.
        f.incoming.borrow_mut().push_back(json!({"type": "Settings", "settings": {"editor": "idea", "tcpPort": 9100}, "trust": {}}));
        app.drain();
        assert_eq!(app.editor, "cursor");
        assert_eq!(app.settings.tcp_port, "9100");
    }

    #[test]
    fn second_dialog_is_refused_until_the_reply() {
        let (mut app, f) = app();
        f.sent.borrow_mut().clear();
        app.pick_workspace();
        app.pick_workspace();
        app.attach_files();
        assert_eq!(actions(&f), ["pick_folder"], "double click must not open a second dialog");
        assert!(app.dialog_open());
        f.incoming.borrow_mut().push_back(json!({"type": "FolderPicked", "tabId": app.active_id(), "path": null}));
        app.drain();
        assert!(!app.dialog_open());
        app.attach_files();
        assert_eq!(actions(&f), ["pick_folder", "pick_files"]);
    }

    #[test]
    fn folder_picked_restarts_the_chat_in_that_folder() {
        let (mut app, f) = app();
        app.pick_workspace();
        f.sent.borrow_mut().clear();
        let tab = app.active_id();
        f.incoming.borrow_mut().push_back(json!({"type": "FolderPicked", "tabId": tab, "path": "/proj"}));
        app.drain();
        assert_eq!(app.model.active_tab().cwd.as_deref(), Some("/proj"));
        assert_eq!(f.sent.borrow()[0], json!({"action": "new_chat", "tabId": tab, "cwd": "/proj"}));
    }

    #[test]
    fn window_drop_reply_does_not_unblock_an_open_dialog() {
        let (mut app, f) = app();
        app.attach_files();
        app.attach_paths(vec!["/a.txt".into()]);
        let tab = app.active_id();
        f.incoming.borrow_mut().push_back(json!({"type": "FilesAttached", "tabId": tab, "items": [{"kind": "file", "name": "a.txt", "content": "x"}], "errors": ["b: not a text file"]}));
        app.drain();
        assert!(app.dialog_open());
        assert_eq!(app.tab_ui[&tab].attachments.len(), 1);
        assert_eq!(app.model.toasts.len(), 1);
        f.incoming.borrow_mut().push_back(json!({"type": "FilesAttached", "tabId": tab, "items": [], "errors": []}));
        app.drain();
        assert!(!app.dialog_open());
    }

    #[test]
    fn export_reply_toasts_and_unblocks() {
        let (mut app, f) = app();
        app.model.active_tab_mut().name = "My chat!".into();
        app.export();
        let sent = f.sent.borrow().last().cloned().unwrap();
        assert_eq!((sent["action"].as_str(), sent["fileName"].as_str(), sent["start"].as_str()), (Some("export"), Some("My chat.md"), Some("")));
        f.incoming.borrow_mut().push_back(json!({"type": "Exported", "cancelled": true}));
        app.drain();
        assert!(!app.dialog_open() && app.model.toasts.is_empty());
        app.export();
        f.incoming.borrow_mut().push_back(json!({"type": "Exported", "path": "/x.md"}));
        app.drain();
        assert_eq!(app.model.toasts.len(), 1);
    }

    #[test]
    fn suggestions_event_fills_the_tab() {
        let (mut app, f) = app();
        let tab = app.active_id();
        f.incoming.borrow_mut().push_back(json!({"type": "Suggestions", "tabId": tab, "suggestions": ["a", "b"]}));
        app.drain();
        assert_eq!(app.model.active_tab().suggestions, ["a", "b"]);
        f.sent.borrow_mut().clear();
        app.run_effects(vec![Effect::Suggest { tab: tab.clone(), context: "é".repeat(3000) }]);
        let sent = f.sent.borrow()[0].clone();
        assert_eq!(sent["action"], "suggest");
        assert_eq!(sent["context"].as_str().unwrap().encode_utf16().count(), 2000);
    }

    #[test]
    fn only_allowed_links_reach_the_backend() {
        let (app, f) = app();
        app.open_link("https://kiro.dev");
        app.open_link("file:///etc/passwd");
        assert_eq!(*f.opened.borrow(), ["https://kiro.dev"]);
    }

    #[test]
    fn dropped_bytes_follow_the_attachment_rules() {
        assert!(matches!(attachment_from_bytes("a.PNG", &[1, 2, 3]), Ok(Attachment::Image { mime, data, .. }) if mime == "image/png" && data == "AQID"));
        assert!(matches!(attachment_from_bytes("a.jpg", b"x"), Ok(Attachment::Image { mime, .. }) if mime == "image/jpeg"));
        assert!(matches!(attachment_from_bytes("notes.md", "héllo".as_bytes()), Ok(Attachment::File { content, .. }) if content == "héllo"));
        assert_eq!(attachment_from_bytes("bin.dat", &[0xff, 0xfe]).err().as_deref(), Some("bin.dat: not a text file"));
        let big = vec![b'a'; MAX_ATTACHMENT as usize + 1];
        assert_eq!(attachment_from_bytes("big.txt", &big).err().as_deref(), Some("big.txt: larger than 10 MB"));
        assert_eq!(attachment_from_bytes("big.png", &big).err().as_deref(), Some("big.png: larger than 10 MB"));
    }
}
