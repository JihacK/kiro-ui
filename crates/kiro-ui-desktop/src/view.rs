//! Window layout: header, sessions sidebar, tab bar, per-tab config bar,
//! side panels (MCP servers, files), toasts and the rewind dialog.

use crate::app::KiroApp;
use crate::model::{self, ToastKind};
use crate::theme;
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, Margin, RichText, Sense, Stroke};
use serde_json::json;

/// Chat and input share one centred column so their edges line up.
pub const CHAT_MAX_WIDTH: f32 = 860.0;
pub const CHAT_MARGIN: i8 = 24;

/// Lay out `add` in a column of at most `max_width`, centred in the available width.
pub fn centered_column<R>(ui: &mut egui::Ui, max_width: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let avail = ui.available_width();
    let width = avail.min(max_width);
    ui.horizontal(|ui| {
        ui.add_space(((avail - width) / 2.0).max(0.0));
        ui.vertical(|ui| {
            ui.set_width(width);
            add(ui)
        })
        .inner
    })
    .inner
}

pub fn chip(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) {
    egui::Frame::new().fill(bg).corner_radius(CornerRadius::same(5)).inner_margin(Margin::symmetric(7, 2)).show(ui, |ui| {
        ui.label(RichText::new(text).size(11.0).color(fg));
    });
}

/// A clickable area whose inner widgets (close buttons, links) still get their own clicks:
/// the area's sense is registered before its children, so the children sit on top.
/// `label` is the screen-reader name; a focus ring is drawn while it has keyboard focus.
pub fn clickable<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    let r = ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
        // Selectable labels would swallow clicks meant for the area.
        ui.style_mut().interaction.selectable_labels = false;
        add(ui)
    });
    let label = label.to_string();
    r.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    focus_ring(ui, &r.response);
    r
}

fn focus_ring(ui: &egui::Ui, r: &egui::Response) {
    if r.has_focus() {
        let color = ui.visuals().selection.stroke.color;
        ui.painter().rect_stroke(r.rect.expand(2.0), CornerRadius::same(6), Stroke::new(2.0, color), egui::StrokeKind::Outside);
    }
}

/// Icon-only buttons: `label` becomes the tooltip and the accessible name (instead of the glyph),
/// and frameless buttons get a visible focus ring.
pub trait Named {
    fn named(self, ui: &egui::Ui, label: &str) -> Self;
}

impl Named for egui::Response {
    fn named(self, ui: &egui::Ui, label: &str) -> Self {
        let enabled = self.enabled();
        let name = label.to_string();
        self.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, &name));
        focus_ring(ui, &self);
        self.on_hover_text(label)
    }
}

/// Shorten to at most `max` characters, ending in "…" when cut.
pub fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

pub fn status_colors(status: &str, p: &theme::Palette) -> (Color32, Color32) {
    match status {
        "completed" | "complete" | "running" | "connected" | "done" => (p.green, p.green.gamma_multiply(0.15)),
        "failed" | "error" | "rejected" | "cancelled" => (p.red, p.red.gamma_multiply(0.15)),
        "in_progress" | "auth_required" | "retrying" => (p.yellow, p.yellow.gamma_multiply(0.15)),
        _ => (p.muted, p.surface2),
    }
}

impl KiroApp {
    pub fn palette(&self) -> &'static theme::Palette {
        theme::palette(self.dark)
    }

    pub fn draw(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        egui::Panel::top("header")
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::symmetric(16, 8)).stroke(Stroke::new(1.0, p.border)))
            .show(ui, |ui| self.header(ui));

        if self.sidebar_open {
            egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(240.0)
                .size_range(180.0..=420.0)
                .frame(egui::Frame::new().fill(p.surface).inner_margin(Margin::same(10)).stroke(Stroke::new(1.0, p.border)))
                .show(ui, |ui| self.sidebar(ui));
        }

        egui::Panel::top("tabbar")
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::symmetric(12, 6)))
            .show(ui, |ui| self.tab_bar(ui));
        egui::Panel::top("tabconfig")
            .frame(egui::Frame::new().fill(p.surface).inner_margin(Margin::symmetric(12, 6)).stroke(Stroke::new(1.0, p.border)))
            .show(ui, |ui| self.tab_config(ui));

        if self.show_settings {
            egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::same(20))).show(ui, |ui| self.settings_page(ui));
        } else {
            // Side panel first so the input bar only spans the chat area, not underneath the panel.
            let tab = self.model.active_tab();
            let show_side = (!self.model.mcp_servers.is_empty() && tab.modes.is_some()) || !tab.active_files.is_empty();
            if show_side {
                egui::Panel::right("side")
                    .resizable(true)
                    .default_size(210.0)
                    .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::same(10)))
                    .show(ui, |ui| self.side_panels(ui));
            }
            egui::Panel::bottom("input")
                .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::symmetric(CHAT_MARGIN, 10)))
                .show(ui, |ui| self.input_area(ui));
            if self.debug && !self.model.logs.is_empty() {
                egui::Panel::bottom("debug")
                    .resizable(true)
                    .default_size(140.0)
                    .frame(egui::Frame::new().fill(p.code_bg).inner_margin(Margin::same(8)).stroke(Stroke::new(1.0, p.border)))
                    .show(ui, |ui| self.debug_panel(ui));
            }
            egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::symmetric(CHAT_MARGIN, 12))).show(ui, |ui| self.chat(ui));
        }

        if self.show_rewind {
            self.rewind_dialog(ui.ctx());
        }
        self.toasts(ui.ctx());
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        ui.horizontal(|ui| {
            if !self.sidebar_open && ui.button("☰").named(ui, "Show sessions (Ctrl+B)").clicked() {
                self.sidebar_open = true;
            }
            ui.label(RichText::new("Kiro").size(17.0).strong().color(p.accent_text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let connected = self.model.active_tab().modes.is_some();
                let (text, fg, bg) = if connected {
                    ("Connected", p.green, p.green.gamma_multiply(0.15))
                } else if self.model.active_tab().connecting {
                    ("Connecting...", p.muted, p.surface2)
                } else {
                    ("Disconnected", p.red, p.red.gamma_multiply(0.15))
                };
                chip(ui, text, fg, bg);
                ui.separator();
                if ui.button("⚙").named(ui, "Settings").clicked() {
                    self.show_settings = !self.show_settings;
                    if self.show_settings {
                        self.open_settings();
                    }
                }
                let icon = if self.dark { "☀" } else { "🌙" };
                if ui.button(icon).named(ui, if self.dark { "Switch to light theme" } else { "Switch to dark theme" }).clicked() {
                    let d = !self.dark;
                    self.set_dark(d);
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        ui.horizontal(|ui| {
            if ui.small_button("×").named(ui, "Hide sessions (Ctrl+B)").clicked() {
                self.sidebar_open = false;
            }
            ui.label(RichText::new("SESSIONS").size(11.0).strong().color(p.muted));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("+ New").clicked() {
                    self.new_tab();
                }
            });
        });
        ui.add_space(6.0);
        let cwd = self.model.active_tab().cwd.clone().unwrap_or_else(|| "~/.kiro-ui/workspace".into());
        let ws = egui::Button::new(RichText::new(format!("📁 {cwd}")).size(12.0).color(p.muted)).fill(p.surface2).truncate();
        if ui.add_enabled_ui(!self.dialog_open(), |ui| ui.add_sized([ui.available_width(), 28.0], ws)).inner.on_hover_text("Change workspace directory for this tab").clicked() {
            self.pick_workspace();
        }
        ui.add_space(6.0);
        ui.add(egui::TextEdit::singleline(&mut self.session_filter).hint_text("Filter sessions...").desired_width(f32::INFINITY));
        ui.add_space(4.0);
        let filter = self.session_filter.to_lowercase();
        let open: Vec<String> = self.model.tabs.iter().filter_map(|t| t.session_id.clone()).collect();
        let mut load = None;
        let titled = self.model.sessions.iter().filter(|s| !s.title.is_empty()).count();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.with_layout(Layout::top_down_justified(Align::Min), |ui| {
                let mut shown = 0;
                for s in self.model.sessions.iter().filter(|s| !s.title.is_empty() && (filter.is_empty() || s.title.to_lowercase().contains(&filter))).take(50) {
                    shown += 1;
                    let is_open = open.contains(&s.id);
                    let label = if is_open { format!("• {}", s.title) } else { s.title.clone() };
                    let color = if is_open { p.text } else { p.muted };
                    let r = ui.add(egui::Button::new(RichText::new(label).color(color)).frame(false).truncate());
                    if r.on_hover_text(&s.title).clicked() {
                        load = Some((s.id.clone(), s.title.clone()));
                    }
                }
                if shown == 0 {
                    let msg = if titled > 0 {
                        format!("No sessions match \"{}\".", self.session_filter.trim())
                    } else if self.model.active_tab().connecting {
                        "Loading sessions…".to_string()
                    } else {
                        "No sessions in this workspace yet. Send a message to start one.".to_string()
                    };
                    ui.add(egui::Label::new(RichText::new(msg).size(12.0).color(p.muted)).wrap());
                }
            });
        });
        if let Some((id, title)) = load {
            self.load_session(&id, &title);
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        let mut activate = None;
        let mut close = None;
        egui::ScrollArea::horizontal().id_salt("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                let many = self.model.tabs.len() > 1;
                for (i, t) in self.model.tabs.iter().enumerate() {
                    let active = t.id == self.model.active;
                    let ghost = p.ghosts[i % 6];
                    let alpha = if active || t.running { 1.0 } else { 0.45 };
                    let frame = egui::Frame::new()
                        .fill(if active { p.surface2 } else { p.bg })
                        .stroke(Stroke::new(1.0, if active { p.border_hover } else { p.border }))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(10, 5));
                    let r = clickable(ui, &format!("Switch to tab {}", t.name), |ui| {
                        frame.show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("👻").color(ghost.gamma_multiply(alpha)));
                                let name = ellipsize(&t.name, 28);
                                ui.label(RichText::new(name).color(if active { p.text } else { p.muted }));
                                if t.running {
                                    ui.label(RichText::new("⟳").color(p.accent_text)).on_hover_text("Running");
                                }
                                if many && ui.add(egui::Button::new(RichText::new("×").size(10.0)).frame(false)).named(ui, &format!("Close tab {}", t.name)).clicked() {
                                    close = Some(t.id.clone());
                                }
                            });
                        })
                    })
                    .response;
                    if r.on_hover_text(&t.name).clicked() {
                        activate = Some(t.id.clone());
                    }
                }
                if ui.button("+").named(ui, "New tab (Ctrl+T)").clicked() {
                    self.new_tab();
                }
            });
        });
        if let Some(id) = close {
            self.close_tab(&id);
        } else if let Some(id) = activate {
            if id != self.model.active {
                self.model.active = id;
                if self.model.active_tab().modes.is_some() {
                    self.list_sessions();
                }
            }
        }
    }

    fn tab_config(&mut self, ui: &mut egui::Ui) {
        let tab_id = self.active_id();
        ui.horizontal(|ui| {
            // ACP "modes" are Kiro agents.
            if let Some(modes) = self.model.active_tab().modes.clone() {
                let current = modes.available.iter().find(|m| m.id == modes.current).map_or(modes.current.clone(), |m| m.name.clone());
                let mut chosen = None;
                egui::ComboBox::from_id_salt("mode").selected_text(current).width(190.0).show_ui(ui, |ui| {
                    for m in &modes.available {
                        if ui.selectable_label(m.id == modes.current, &m.name).on_hover_text(m.description.clone().unwrap_or_default()).clicked() {
                            chosen = Some(m.id.clone());
                        }
                    }
                });
                if let Some(id) = chosen {
                    self.send(json!({"action": "set_mode", "tabId": tab_id, "modeId": id}));
                    if let Some(m) = self.model.active_tab_mut().modes.as_mut() {
                        m.current = id;
                    }
                }
            }
            if let Some(models) = self.model.active_tab().models.clone() {
                let current = models.available.iter().find(|m| m.id == models.current).map_or(models.current.clone(), |m| m.name.clone());
                let mut chosen = None;
                egui::ComboBox::from_id_salt("model").selected_text(current).width(150.0).show_ui(ui, |ui| {
                    for m in &models.available {
                        if ui.selectable_label(m.id == models.current, &m.name).clicked() {
                            chosen = Some(m.id.clone());
                        }
                    }
                });
                if let Some(id) = chosen {
                    self.send(json!({"action": "set_model", "tabId": tab_id, "modelId": id}));
                    let t = self.model.active_tab_mut();
                    if let Some(m) = t.models.as_mut() {
                        m.current = id;
                    }
                    t.effort_supported = false;
                    self.send(json!({"action": "command_options", "tabId": tab_id, "command": "effort", "input": ""}));
                }
            }
            if self.model.active_tab().effort_supported {
                const EFFORTS: [(&str, &str); 6] = [("default", "⚡ Effort"), ("low", "⚡ Low"), ("medium", "⚡⚡ Medium"), ("high", "⚡⚡⚡ High"), ("xhigh", "🔥 X-High"), ("max", "🔥🔥 Max")];
                let cur = self.model.active_tab().effort.clone().unwrap_or_else(|| "default".into());
                let label = EFFORTS.iter().find(|(k, _)| *k == cur).map_or("⚡ Effort", |(_, l)| l);
                let mut chosen = None;
                egui::ComboBox::from_id_salt("effort").selected_text(label).width(110.0).show_ui(ui, |ui| {
                    for (k, l) in EFFORTS {
                        if ui.selectable_label(cur == k, l).clicked() {
                            chosen = Some(k);
                        }
                    }
                });
                if let Some(k) = chosen {
                    self.model.active_tab_mut().effort = (k != "default").then(|| k.to_string());
                    if k != "default" {
                        self.send(json!({"action": "prompt", "tabId": tab_id, "text": format!("/effort {k}")}));
                    }
                }
            }
            const POLICIES: [(&str, &str); 3] = [("ask", "⛨ Ask"), ("approve-reads", "◑ Auto-reads"), ("allow-all", "◎ Allow all")];
            let pol = self.model.active_tab().perm_policy.clone();
            let label = POLICIES.iter().find(|(k, _)| *k == pol).map_or("⛨ Ask", |(_, l)| l);
            let mut chosen = None;
            egui::ComboBox::from_id_salt("policy").selected_text(label).width(110.0).show_ui(ui, |ui| {
                for (k, l) in POLICIES {
                    if ui.selectable_label(pol == k, l).clicked() {
                        chosen = Some(k);
                    }
                }
            });
            if let Some(k) = chosen {
                self.model.active_tab_mut().perm_policy = k.into();
                self.send(json!({"action": "set_permission_policy", "tabId": tab_id, "policy": k}));
                self.save_setting("permPolicy", json!(k));
            }
            self.skills_picker(ui);
            if !self.model.active_tab().messages.is_empty() && ui.add_enabled(!self.dialog_open(), egui::Button::new("⬇")).named(ui, "Export as Markdown").clicked() {
                self.export();
            }
            if let Some(sid) = self.model.active_tab().session_id.clone() {
                let short: String = sid.chars().take(8).collect();
                if ui.button(RichText::new(format!("🗐 {short}")).monospace().size(11.0)).on_hover_text(format!("Session: {sid}\nClick to copy for: kiro-cli chat --resume-id")).clicked() {
                    ui.ctx().copy_text(sid.clone());
                    self.model.toast(format!("Session ID copied — use: kiro-cli chat --resume-id {sid}"), ToastKind::Info);
                }
            }
        });
    }

    /// Per-tab skill selection; chosen skills ride along with every normal prompt.
    fn skills_picker(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        let skills: Vec<model::Prompt> = self.model.prompts.iter().filter(|x| x.is_skill()).cloned().collect();
        if skills.is_empty() {
            return;
        }
        let chosen = self.model.active_tab().skills.clone();
        let label = if chosen.is_empty() { "📚 Skills".to_string() } else { format!("📚 Skills ({})", chosen.len()) };
        let button = ui.button(label).on_hover_text("Choose skills for this tab");
        let mut toggled: Option<String> = None;
        let mut clear = false;
        egui::Popup::from_toggle_button_response(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_width(380.0);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.skill_filter).hint_text("Filter skills...").desired_width(280.0));
                if !chosen.is_empty() && ui.small_button("Clear").clicked() {
                    clear = true;
                }
            });
            ui.add_space(4.0);
            let filter = self.skill_filter.to_lowercase();
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for s in skills.iter().filter(|s| filter.is_empty() || s.name.to_lowercase().contains(&filter) || s.description.to_lowercase().contains(&filter)) {
                    let mut on = chosen.contains(&s.name);
                    let r = ui.checkbox(&mut on, RichText::new(&s.name).monospace().size(12.0));
                    if !s.description.is_empty() {
                        ui.indent(("skill", &s.name), |ui| {
                            let desc: String = s.description.chars().take(110).collect();
                            let more = if s.description.chars().count() > 110 { "…" } else { "" };
                            ui.label(RichText::new(format!("{desc}{more}")).size(11.0).color(p.muted));
                        });
                    }
                    if r.on_hover_text(&s.description).changed() {
                        toggled = Some(s.name.clone());
                    }
                }
            });
        });
        let t = self.model.active_tab_mut();
        if clear {
            t.skills.clear();
        }
        if let Some(name) = toggled {
            match t.skills.iter().position(|x| *x == name) {
                Some(i) => {
                    t.skills.remove(i);
                }
                None => t.skills.push(name),
            }
        }
    }

    fn side_panels(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        egui::ScrollArea::vertical().show(ui, |ui| {
            if !self.model.mcp_servers.is_empty() && self.model.active_tab().modes.is_some() {
                ui.label(RichText::new("MCP SERVERS").size(11.0).strong().color(p.muted));
                let mut open_url = None;
                for s in &self.model.mcp_servers {
                    let (fg, _) = status_colors(&s.status, &p);
                    let tools: Vec<&model::Tool> = self.model.tools.iter().filter(|t| t.source == format!("mcp:{}", s.name)).collect();
                    let title = RichText::new(format!("• {}  {}", s.name, s.tool_count.map(|n| n.to_string()).unwrap_or_default())).color(fg);
                    egui::CollapsingHeader::new(title).id_salt(("mcp", &s.name)).default_open(false).show(ui, |ui| {
                        ui.label(RichText::new(&s.status).size(11.0).color(p.muted));
                        if tools.is_empty() {
                            ui.label(RichText::new(format!("{} tools available", s.tool_count.unwrap_or(0))).size(11.0).color(p.dim));
                        }
                        for t in tools {
                            ui.label(RichText::new(&t.name).monospace().size(11.0)).on_hover_text(&t.description);
                        }
                    });
                    if let Some(url) = self.model.oauth_pending.get(&s.name) {
                        if ui.link(RichText::new("🔑 Authenticate to connect").color(p.yellow)).clicked() {
                            open_url = Some(url.clone());
                        }
                    }
                }
                if let Some(u) = open_url {
                    kiro_ui_core::picker::open_url(&u);
                }
                ui.add_space(10.0);
            }
            let files = self.model.active_tab().active_files.clone();
            if !files.is_empty() {
                ui.label(RichText::new("FILES").size(11.0).strong().color(p.muted));
                let cwd = self.model.active_tab().cwd.clone().unwrap_or_default();
                for f in files.iter().rev().take(8).rev() {
                    let full = if f.path.starts_with('/') { f.path.clone() } else { format!("{cwd}/{}", f.path) };
                    let icon = match f.kind.as_deref() {
                        Some("edit") => "✏",
                        Some("read") => "📖",
                        _ => "📄",
                    };
                    let name = f.path.rsplit('/').next().unwrap_or(&f.path);
                    let label = format!("{icon} {name}{}", f.line.map(|l| format!(":{l}")).unwrap_or_default());
                    let r = ui.add(egui::Button::new(RichText::new(label).size(12.0)).frame(false).truncate()).on_hover_text(&full);
                    if r.clicked() {
                        if let Some(link) = self.editor_link(&full, f.line) {
                            kiro_ui_core::picker::open_url(&link);
                        }
                    }
                }
            }
        });
    }

    fn debug_panel(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Protocol Log").strong().size(12.0));
            if ui.small_button("Clear").clicked() {
                self.model.logs.clear();
            }
        });
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            for l in self.model.logs.iter().rev().take(50).rev() {
                let (arrow, color) = if l.dir == "out" { ("→", p.accent_text) } else { ("←", p.green) };
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(arrow).color(color).monospace().size(11.0));
                    ui.label(RichText::new(&l.msg).monospace().size(11.0).color(p.muted));
                });
            }
        });
    }

    fn toasts(&mut self, ctx: &egui::Context) {
        if self.model.toasts.is_empty() {
            return;
        }
        let p = *self.palette();
        let mut dismiss = None;
        egui::Area::new(egui::Id::unique("toasts"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-14.0, 60.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(360.0);
                for t in &self.model.toasts {
                    let (fg, stroke) = match t.kind {
                        ToastKind::Error => (p.red, p.red.gamma_multiply(0.4)),
                        ToastKind::Warning => (p.yellow, p.yellow.gamma_multiply(0.4)),
                        ToastKind::Info => (p.text, p.border),
                    };
                    egui::Frame::new()
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0, stroke))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.add(egui::Label::new(RichText::new(&t.text).color(fg).size(13.0)).wrap());
                                if ui.add(egui::Button::new("×").frame(false)).named(ui, "Dismiss notification").clicked() {
                                    dismiss = Some(t.id);
                                }
                            });
                        });
                    ui.add_space(6.0);
                }
            });
        if let Some(id) = dismiss {
            self.model.toasts.retain(|t| t.id != id);
        }
    }

    fn rewind_dialog(&mut self, ctx: &egui::Context) {
        let p = *self.palette();
        let msgs = self.model.active_tab().messages.clone();
        let users: Vec<usize> = msgs.iter().enumerate().filter(|(_, m)| m.role.is_user()).map(|(i, _)| i).collect();
        if users.len() < 2 {
            self.show_rewind = false;
            self.model.toast("Nothing to rewind yet — use a new chat to start over.", ToastKind::Info);
            return;
        }
        let mut chosen = None;
        let modal = egui::Modal::new(egui::Id::unique("rewind")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("🕑 Rewind to turn").strong());
            });
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                // Turn 1 is excluded: rewinding to the very start is a new chat.
                for (pos, &idx) in users.iter().enumerate().skip(1).rev() {
                    let next = users.get(pos + 1).copied();
                    let sum = model::turn_summary(&msgs, idx, next);
                    let text: String = msgs[idx].text.chars().take(80).collect();
                    let more = if msgs[idx].text.chars().count() > 80 { "…" } else { "" };
                    let r = clickable(ui, &format!("Rewind to turn {}: {text}", pos + 1), |ui| {
                        egui::Frame::new()
                        .fill(p.surface2)
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(format!("Turn {}", pos + 1)).size(11.0).color(p.accent_text));
                            ui.label(format!("{text}{more}"));
                            if sum.tools > 0 {
                                let mut s = format!("{} {}", sum.tools, if sum.tools == 1 { "tool" } else { "tools" });
                                if !sum.files.is_empty() {
                                    let shown: Vec<&str> = sum.files.iter().take(3).map(String::as_str).collect();
                                    let extra = if sum.files.len() > 3 { format!(", +{}", sum.files.len() - 3) } else { String::new() };
                                    s += &format!(" · {} {} ({}{extra})", sum.files.len(), if sum.files.len() == 1 { "file" } else { "files" }, shown.join(", "));
                                }
                                if sum.commands > 0 {
                                    s += &format!(" · {} {}", sum.commands, if sum.commands == 1 { "command" } else { "commands" });
                                }
                                ui.label(RichText::new(s).size(11.0).color(p.muted));
                            }
                        })
                    })
                    .response;
                    if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        chosen = Some(idx);
                    }
                    ui.add_space(4.0);
                }
            });
            ui.add_space(6.0);
            ui.label(RichText::new("Click a turn to rewind. A new session will branch from that point.").size(11.0).color(p.dim));
            ui.add_space(4.0);
            if ui.button("Close").clicked() {
                ui.close();
            }
        });
        if let Some(idx) = chosen {
            self.rewind(idx);
        } else if modal.should_close() {
            self.show_rewind = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, Pos2, RawInput};

    /// Runs one frame: a clickable area holding a button. Returns (area clicked, button clicked, button rect).
    fn frame(ctx: &egui::Context, events: Vec<Event>) -> (bool, bool, egui::Rect) {
        let mut out = (false, false, egui::Rect::NOTHING);
        let input = RawInput { screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 200.0))), events, ..Default::default() };
        let mut output = ctx.run_ui(input, |ui| {
            let mut button = None;
            let area = clickable(ui, "Switch to tab", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Tab name");
                    button = Some(ui.button("×"));
                });
            });
            let b = button.unwrap();
            out = (area.response.clicked(), b.clicked(), b.rect);
        });
        // No renderer in tests: discard the font texture uploads.
        output.textures_delta.clear();
        out
    }

    fn click_at(pos: Pos2) -> Vec<Vec<Event>> {
        let press = |pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        vec![vec![Event::PointerMoved(pos)], vec![press(true)], vec![press(false)], vec![]]
    }

    #[test]
    fn inner_button_wins_over_clickable_area() {
        let ctx = egui::Context::default();
        let (_, _, rect) = frame(&ctx, vec![]);
        let (mut area, mut button) = (false, false);
        for events in click_at(rect.center()) {
            let (a, b, _) = frame(&ctx, events);
            area |= a;
            button |= b;
        }
        assert!(button, "the close button must receive the click");
        assert!(!area, "the tab area must not also activate");
    }

    #[test]
    fn clicking_the_area_outside_the_button_activates_it() {
        let ctx = egui::Context::default();
        let (_, _, rect) = frame(&ctx, vec![]);
        let (mut area, mut button) = (false, false);
        // The label sits left of the button.
        for events in click_at(Pos2::new(rect.left() - 20.0, rect.center().y)) {
            let (a, b, _) = frame(&ctx, events);
            area |= a;
            button |= b;
        }
        assert!(area && !button);
    }
}

#[cfg(test)]
mod a11y_tests {
    use super::*;

    #[test]
    fn long_tab_names_end_with_an_ellipsis() {
        assert_eq!(ellipsize("Reply with a two-line markdown list", 28), "Reply with a two-line markd…");
        assert_eq!(ellipsize("Short", 28), "Short");
        assert_eq!(ellipsize("exactly twenty-eight chars!!", 28), "exactly twenty-eight chars!!");
        assert_eq!(ellipsize("Use these skills for this re", 10), "Use these…");
    }

    fn accesskit_labels(build: impl FnMut(&mut egui::Ui)) -> Vec<String> {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 200.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, build);
        out.textures_delta.clear();
        let update = out.platform_output.accesskit_update.expect("accesskit update");
        update.nodes.iter().filter_map(|(_, n)| n.label().map(str::to_owned)).collect()
    }

    #[test]
    fn icon_button_is_announced_by_its_name_not_its_glyph() {
        let labels = accesskit_labels(|ui| {
            ui.button("⚙").named(ui, "Settings");
        });
        assert!(labels.iter().any(|l| l == "Settings"), "{labels:?}");
        assert!(!labels.iter().any(|l| l == "⚙"), "{labels:?}");
    }

    #[test]
    fn clickable_area_has_a_name() {
        let labels = accesskit_labels(|ui| {
            clickable(ui, "Switch to tab Fix the bug", |ui| ui.label("Fix the bug"));
        });
        assert!(labels.iter().any(|l| l == "Switch to tab Fix the bug"), "{labels:?}");
    }
}
