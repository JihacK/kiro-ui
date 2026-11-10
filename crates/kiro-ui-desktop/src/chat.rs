//! Conversation view: messages, tool calls (with diffs), thinking, plan,
//! local command panels, waiting indicator and permission prompts.

use crate::app::KiroApp;
use crate::model::{Msg, Role, ToolEntry};
use crate::theme::Palette;
use crate::view::{chip, clickable, status_colors};
use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke};
use egui_commonmark::CommonMarkViewer;
use serde_json::Value;
use similar::{ChangeTag, TextDiff};

/// Diffs above this many lines are summarised instead of rendered.
const MAX_DIFF_LINES: usize = 1500;

enum Click {
    Copy(u64, String),
    Retry(usize),
    Rewind,
    ToggleTool(u64),
    ToggleGroup(u64),
    ToggleThinking,
    OpenFile(String),
    Permission(Value, String, String),
}

impl KiroApp {
    pub fn chat(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        let mut clicks: Vec<Click> = Vec::new();
        // Borrow the tab immutably (no per-frame clone); the markdown cache is moved out meanwhile.
        let mut md = std::mem::take(&mut self.md);
        self.chat_inner(ui, &p, &mut md, &mut clicks);
        self.md = md;
        self.apply_chat_clicks(ui, clicks);
    }

    fn chat_inner(&self, ui: &mut egui::Ui, p: &Palette, md: &mut egui_commonmark::CommonMarkCache, clicks: &mut Vec<Click>) {
        let p = *p;
        let tab = self.model.active_tab();
        let loading = tab.connecting;
        let has_messages = !tab.messages.is_empty() || tab.thinking.is_some();

        egui::ScrollArea::vertical().id_salt(("chat", &tab.id)).stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
          crate::view::centered_column(ui, crate::view::CHAT_MAX_WIDTH, |ui| {
            if loading {
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Starting agent session...").color(p.muted));
                });
            }
            if !has_messages && !loading {
                ui.add_space((ui.available_height() * 0.25).max(40.0));
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("👻").size(56.0).color(p.accent));
                    ui.add_space(8.0);
                    match tab.current_mode() {
                        Some(m) => {
                            ui.label(RichText::new(&m.name).size(16.0).color(p.text));
                            ui.label(RichText::new(m.description.clone().unwrap_or_else(|| "What can I help you with?".into())).size(15.0).color(p.muted));
                        }
                        None => {
                            ui.label(RichText::new("What can I help you with?").size(16.0).color(p.muted));
                        }
                    }
                });
            }

            let last_user = tab.messages.iter().rposition(|m| m.role == Role::User);
            let last_chat = tab.messages.iter().rposition(|m| m.role.is_user() || matches!(m.role, Role::Assistant | Role::AssistantStream));
            let can_retry = tab.last_stop.as_deref().is_some_and(|s| s != "end_turn");
            let mut i = 0;
            while i < tab.messages.len() {
                let m = &tab.messages[i];
                match m.role {
                    Role::Tool => {
                        let start = i;
                        while i < tab.messages.len() && tab.messages[i].role == Role::Tool {
                            i += 1;
                        }
                        let group = &tab.messages[start..i];
                        if group.len() > 1 {
                            self.tool_group(ui, &p, group, clicks);
                        } else if let Some(t) = &group[0].tool {
                            self.tool_block(ui, &p, group[0].uid, t, clicks);
                        }
                        ui.add_space(4.0);
                        continue;
                    }
                    Role::Panel => {
                        self.panel_card(ui, &p, m.panel.as_deref().unwrap_or_default());
                    }
                    Role::System => {
                        egui::Frame::new().fill(p.red.gamma_multiply(0.1)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(&m.text).color(p.red));
                        });
                    }
                    Role::User | Role::UserStream => {
                        let retry = Some(i) == last_user && can_retry;
                        let (hover_id, show) = actions_visibility(ui, m.uid, tab.running, Some(i) == last_chat || retry);
                        let block = ui.scope(|ui| {
                            egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(10)).inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.add(egui::Label::new(RichText::new(&m.text).color(p.text)).selectable(true).wrap());
                            });
                            if !tab.running {
                                ui.scope(|ui| {
                                    if !show {
                                        ui.set_invisible();
                                    }
                                    self.message_actions(ui, &p, m, i, retry, clicks);
                                });
                            }
                        });
                        remember_hover(ui, hover_id, &block.response);
                    }
                    Role::Assistant | Role::AssistantStream => {
                        let (hover_id, show) = actions_visibility(ui, m.uid, tab.running, Some(i) == last_chat);
                        let block = ui.scope(|ui| {
                            ui.push_id(m.uid, |ui| {
                                CommonMarkViewer::new().show(ui, md, &m.text);
                            });
                            if !tab.running {
                                ui.scope(|ui| {
                                    if !show {
                                        ui.set_invisible();
                                    }
                                    self.message_actions(ui, &p, m, i, false, clicks);
                                });
                            }
                        });
                        remember_hover(ui, hover_id, &block.response);
                    }
                }
                ui.add_space(8.0);
                i += 1;
            }

            if let Some(th) = &tab.thinking {
                let secs = th.start.elapsed().as_secs();
                egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let label = if th.collapsed { "Thinking, show reasoning" } else { "Thinking, hide reasoning" };
                    let r = clickable(ui, label, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(if th.collapsed { "⏵ 💭 Thinking" } else { "⏷ 💭 Thinking" }).color(p.accent_text));
                            ui.label(RichText::new(format!("{secs}s")).color(p.dim).size(11.0));
                        })
                    });
                    if r.response.clicked() {
                        clicks.push(Click::ToggleThinking);
                    }
                    if !th.collapsed {
                        ui.add(egui::Label::new(RichText::new(&th.text).color(p.muted).size(13.0)).wrap());
                    }
                });
                ui.add_space(8.0);
            }

            if !tab.plan.is_empty() {
                egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("Plan").strong());
                    for e in &tab.plan {
                        let (icon, color) = match e.status.as_str() {
                            "completed" => ("✔", p.green),
                            "in_progress" => ("⏵", p.accent_text),
                            _ => ("○", p.muted),
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(icon).color(color));
                            let mut t = RichText::new(&e.content);
                            if e.status == "completed" {
                                t = t.strikethrough().color(p.muted);
                            }
                            ui.label(t);
                            if e.priority == "high" {
                                ui.label(RichText::new("!").color(p.red).strong());
                            }
                        });
                    }
                });
                ui.add_space(8.0);
            }

            if tab.running && tab.thinking.is_none() {
                let t = ui.input(|i| i.time);
                let ghost = p.ghosts[self.model.tabs.iter().position(|x| x.id == tab.id).unwrap_or(0) % 6];
                ui.horizontal(|ui| {
                    ui.label(RichText::new("👻").color(ghost));
                    if self.reduce_motion {
                        ui.label(RichText::new("Working…").color(p.muted));
                    } else {
                        let dots = 1 + ((t * 3.0) as usize % 3);
                        ui.label(RichText::new(".".repeat(dots)).color(p.muted).size(18.0));
                    }
                });
            }

            for perm in &tab.permissions {
                egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.yellow.gamma_multiply(0.5))).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(format!("🛡 {}", if perm.title.is_empty() { "Permission required" } else { &perm.title })).color(p.text));
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        for o in &perm.options {
                            // Only the one-time allow is the filled primary; "always" writes a lasting trust rule.
                            let btn = match o.kind.as_str() {
                                "allow_once" => egui::Button::new(RichText::new(&o.name).color(Color32::WHITE)).fill(p.accent),
                                "allow_always" => egui::Button::new(RichText::new(&o.name).color(p.accent_text)).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0, p.accent)),
                                k if k.contains("reject") => egui::Button::new(RichText::new(&o.name).color(p.red)).fill(p.red.gamma_multiply(0.12)),
                                _ => egui::Button::new(RichText::new(&o.name).color(p.text)).fill(p.surface2),
                            };
                            if ui.add(btn).clicked() {
                                clicks.push(Click::Permission(perm.request_id.clone(), o.id.clone(), perm.title.clone()));
                            }
                        }
                    });
                });
                ui.add_space(6.0);
            }
            ui.add_space(12.0);
          });
        });
    }

    fn apply_chat_clicks(&mut self, ui: &mut egui::Ui, clicks: Vec<Click>) {
        for c in clicks {
            match c {
                Click::Copy(uid, text) => {
                    ui.ctx().copy_text(text);
                    self.copied = Some((uid, std::time::Instant::now()));
                }
                Click::Retry(i) => self.retry(i),
                Click::Rewind => self.show_rewind = true,
                Click::ToggleTool(uid) => {
                    if let Some(t) = self.model.active_tab_mut().messages.iter_mut().find(|m| m.uid == uid).and_then(|m| m.tool.as_mut()) {
                        t.expanded = !t.expanded;
                    }
                }
                Click::ToggleGroup(uid) => {
                    let e = self.collapsed_groups.entry(uid).or_insert(false);
                    *e = !*e;
                }
                Click::ToggleThinking => {
                    if let Some(th) = self.model.active_tab_mut().thinking.as_mut() {
                        th.collapsed = !th.collapsed;
                    }
                }
                Click::OpenFile(path) => {
                    if let Some(link) = self.editor_link(&path, None) {
                        kiro_ui_core::picker::open_url(&link);
                    }
                }
                Click::Permission(id, opt, title) => self.respond_permission(id, &opt, &title),
            }
        }
    }

    fn message_actions(&self, ui: &mut egui::Ui, p: &Palette, m: &Msg, idx: usize, retry: bool, clicks: &mut Vec<Click>) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let copied = self.copied.is_some_and(|(uid, at)| uid == m.uid && at.elapsed().as_secs_f32() < 1.5);
            let small = |s: &str, c: Color32| egui::Button::new(RichText::new(s).size(11.0).color(c)).frame(false);
            if ui.add(small(if copied { "✔ Copied" } else { "🗐 Copy" }, if copied { p.green } else { p.dim })).clicked() {
                clicks.push(Click::Copy(m.uid, m.text.clone()));
            }
            if m.role == Role::User {
                if retry && ui.add(small("⟲ Retry", p.dim)).clicked() {
                    clicks.push(Click::Retry(idx));
                }
                if ui.add(small("🕑 Rewind", p.dim)).on_hover_text("Rewind to a previous turn").clicked() {
                    clicks.push(Click::Rewind);
                }
            }
        });
    }

    fn tool_group(&self, ui: &mut egui::Ui, p: &Palette, group: &[Msg], clicks: &mut Vec<Click>) {
        let key = group[0].uid;
        let collapsed = self.collapsed_groups.get(&key).copied().unwrap_or(false);
        let done = group.iter().filter(|m| m.tool.as_ref().is_some_and(|t| t.status == "completed")).count();
        egui::Frame::new().stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(8, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let label = format!("{} tool calls, {done} completed, {}", group.len(), if collapsed { "expand" } else { "collapse" });
            let r = clickable(ui, &label, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(if collapsed { "⏵" } else { "⏷" }).color(p.dim).size(11.0));
                    ui.label(RichText::new(format!("{} tool calls", group.len())).color(p.muted));
                    let (fg, bg) = status_colors(if done == group.len() { "completed" } else { "pending" }, p);
                    chip(ui, &format!("{done}/{}", group.len()), fg, bg);
                })
            });
            if r.response.clicked() {
                clicks.push(Click::ToggleGroup(key));
            }
            if !collapsed {
                for m in group {
                    if let Some(t) = &m.tool {
                        self.tool_block(ui, p, m.uid, t, clicks);
                    }
                }
            }
        });
    }

    fn tool_block(&self, ui: &mut egui::Ui, p: &Palette, uid: u64, t: &ToolEntry, clicks: &mut Vec<Click>) {
        let expandable = !t.content.is_empty() || t.raw_input.is_some() || t.raw_output.is_some() || !t.stream_output.is_empty();
        egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let header = clickable(ui, &format!("Tool call {}: {}, show details", t.title, t.status), |ui| ui.horizontal(|ui| {
                if expandable {
                    ui.label(RichText::new(if t.expanded { "⏷" } else { "⏵" }).color(p.dim).size(11.0));
                }
                ui.label(RichText::new("🔧").color(p.accent_text));
                let title = if t.title.is_empty() { t.kind.clone().unwrap_or_else(|| "tool".into()) } else { t.title.clone() };
                // "Read /path/to/file" → clickable path.
                match split_path(&title) {
                    Some((head, path)) => {
                        ui.label(head);
                        if self.editor != "none" {
                            if ui.link(RichText::new(path).monospace().size(12.0)).on_hover_text(format!("Open in {}", self.editor)).clicked() {
                                clicks.push(Click::OpenFile(path.to_string()));
                            }
                        } else {
                            ui.label(RichText::new(path).monospace().size(12.0));
                        }
                    }
                    None => {
                        ui.add(egui::Label::new(title).truncate());
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (fg, bg) = status_colors(&t.status, p);
                    chip(ui, &t.status, fg, bg);
                });
            }));
            if expandable && header.response.clicked() {
                clicks.push(Click::ToggleTool(uid));
            }
            if expandable && t.expanded {
                ui.add_space(4.0);
                if !t.stream_output.is_empty() {
                    code_box(ui, p, &t.stream_output);
                }
                for (ci, c) in t.content.iter().enumerate() {
                    if c.get("type").and_then(Value::as_str) == Some("diff") {
                        let path = c.get("path").and_then(Value::as_str).unwrap_or_default();
                        if !path.is_empty() && ui.link(RichText::new(path).monospace().size(12.0)).clicked() {
                            clicks.push(Click::OpenFile(path.to_string()));
                        }
                        let old = c.get("oldText").and_then(Value::as_str).unwrap_or_default();
                        let new = c.get("newText").and_then(Value::as_str).unwrap_or_default();
                        ui.push_id((uid, ci), |ui| diff_view(ui, p, old, new));
                    } else {
                        code_box(ui, p, &serde_json::to_string_pretty(c).unwrap_or_default());
                    }
                }
                for (label, v) in [("Input", &t.raw_input), ("Output", &t.raw_output)] {
                    if let Some(v) = v {
                        let text = match v {
                            Value::String(s) => s.clone(),
                            other => serde_json::to_string_pretty(other).unwrap_or_default(),
                        };
                        egui::CollapsingHeader::new(RichText::new(label).size(12.0).color(p.muted)).id_salt((uid, label)).show(ui, |ui| code_box(ui, p, &text));
                    }
                }
            }
        });
    }

    fn panel_card(&self, ui: &mut egui::Ui, p: &Palette, kind: &str) {
        let tab = self.model.active_tab();
        let md = &tab.metadata;
        let pct = md.context_pct.round();
        let agent = tab.current_mode().map(|m| m.name.clone()).or_else(|| tab.modes.as_ref().map(|m| m.current.clone())).unwrap_or_else(|| "—".into());
        egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(14)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let row = |ui: &mut egui::Ui, k: &str, v: String| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(k).color(p.muted));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(v));
                });
            };
            match kind {
                "context" => {
                    ui.label(RichText::new("Context Window").strong());
                    ui.add(egui::ProgressBar::new((md.context_pct as f32 / 100.0).clamp(0.01, 1.0)).text(format!("{pct}% used")).fill(p.accent));
                    row(ui, "Active agent", agent);
                    if let Some(ms) = md.turn_ms {
                        row(ui, "Last turn", format!("{:.1}s", ms / 1000.0));
                    }
                    if md.cumulative.input > 0.0 {
                        row(ui, "Tokens (in / out)", format!("{} / {}", md.cumulative.input as u64, md.cumulative.output as u64));
                    }
                    if md.cumulative.cost > 0.0 {
                        row(ui, "Session cost", format!("${:.4}", md.cumulative.cost));
                    }
                    ui.label(RichText::new("/compact Summarize history · /clear Erase history").size(11.0).color(p.dim));
                }
                "mcp" => {
                    ui.label(RichText::new(format!("MCP Servers ({})", self.model.mcp_servers.len())).strong());
                    if self.model.mcp_servers.is_empty() {
                        ui.label(RichText::new("No servers configured").color(p.dim));
                    }
                    for s in &self.model.mcp_servers {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&s.name).strong());
                            let (fg, bg) = status_colors(&s.status, p);
                            chip(ui, &s.status, fg, bg);
                            ui.label(RichText::new(format!("{} tools", s.tool_count.unwrap_or(0))).color(p.dim).size(11.0));
                        });
                        let names: Vec<&str> = self.model.tools.iter().filter(|t| t.source == format!("mcp:{}", s.name)).map(|t| t.name.as_str()).collect();
                        if !names.is_empty() {
                            ui.label(RichText::new(names.join("  ")).monospace().size(11.0).color(p.muted));
                        }
                    }
                }
                "tools" => {
                    ui.label(RichText::new(format!("Available Tools ({})", self.model.tools.len())).strong());
                    let builtin: Vec<&str> = self.model.tools.iter().filter(|t| t.source == "built-in").map(|t| t.name.as_str()).collect();
                    let mcp: Vec<&str> = self.model.tools.iter().filter(|t| t.source.starts_with("mcp:")).map(|t| t.name.as_str()).collect();
                    for (label, list) in [("Built-in", builtin), ("MCP", mcp)] {
                        if !list.is_empty() {
                            ui.label(RichText::new(format!("{label} ({})", list.len())).color(p.muted));
                            ui.label(RichText::new(list.join("  ")).monospace().size(11.0));
                        }
                    }
                }
                "help" => {
                    ui.label(RichText::new("Available Commands").strong());
                    egui::Grid::new("help-grid").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                        for c in &self.model.commands {
                            ui.label(RichText::new(&c.name).monospace().color(p.accent_text));
                            ui.label(RichText::new(&c.description).color(p.muted));
                            ui.end_row();
                        }
                    });
                }
                other => {
                    ui.label(RichText::new(other).strong());
                    ui.label(RichText::new("No data available").color(p.dim));
                }
            }
        });
    }
}

/// Message actions show on the hovered message and always on the last one (keyboard reachable).
/// Hover is read from the previous frame; the row keeps its space so nothing shifts.
fn actions_visibility(ui: &egui::Ui, uid: u64, running: bool, is_last: bool) -> (egui::Id, bool) {
    let id = ui.scope_id().with(("msg-hover", uid));
    let hovered = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    (id, !running && (is_last || hovered))
}

fn remember_hover(ui: &egui::Ui, id: egui::Id, block: &egui::Response) {
    // `rect_contains_pointer` keeps the last position after the pointer leaves the window.
    let hovered = ui.ctx().input(|i| i.pointer.hover_pos().is_some()) && ui.rect_contains_pointer(block.rect);
    if ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false) != hovered {
        ui.ctx().data_mut(|d| d.insert_temp(id, hovered));
        ui.ctx().request_repaint();
    }
}

/// Split "Verb /some/path" (or "Verb dir/file") into the head and the path.
fn split_path(title: &str) -> Option<(&str, &str)> {
    let (head, last) = title.rsplit_once(' ')?;
    let looks_like_path = last.starts_with('/') || (last.contains('/') && !last.starts_with("http"));
    (looks_like_path && !head.is_empty()).then_some((head, last))
}

fn code_box(ui: &mut egui::Ui, p: &Palette, text: &str) {
    egui::Frame::new().fill(p.code_bg).corner_radius(CornerRadius::same(6)).inner_margin(Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(text).monospace().size(12.0).color(p.muted)).selectable(true).wrap());
        });
    });
}

/// Unified line diff with line numbers and +/- tinting.
fn diff_view(ui: &mut egui::Ui, p: &Palette, old: &str, new: &str) {
    let total = old.lines().count() + new.lines().count();
    if total > MAX_DIFF_LINES * 2 {
        ui.label(RichText::new(format!("Diff too large to display ({total} lines)")).color(p.dim));
        return;
    }
    let diff = TextDiff::from_lines(old, new);
    egui::Frame::new().fill(p.code_bg).corner_radius(CornerRadius::same(6)).inner_margin(Margin::same(6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::both().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut shown = 0;
            for group in diff.grouped_ops(3) {
                if shown > 0 {
                    ui.label(RichText::new("⋯").color(p.dim).monospace());
                }
                for op in group {
                    for ch in diff.iter_changes(&op) {
                        shown += 1;
                        if shown > MAX_DIFF_LINES {
                            ui.label(RichText::new("… (truncated)").color(p.dim));
                            return;
                        }
                        let (sign, fg, bg) = match ch.tag() {
                            ChangeTag::Delete => ("-", p.red, p.red.gamma_multiply(0.12)),
                            ChangeTag::Insert => ("+", p.green, p.green.gamma_multiply(0.12)),
                            ChangeTag::Equal => (" ", p.muted, Color32::TRANSPARENT),
                        };
                        let num = ch.new_index().or(ch.old_index()).map(|n| format!("{:>4}", n + 1)).unwrap_or_default();
                        let line = ch.value().trim_end_matches('\n');
                        egui::Frame::new().fill(bg).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(num).monospace().size(11.0).color(p.dim));
                                ui.label(RichText::new(format!("{sign} {line}")).monospace().size(12.0).color(fg));
                            });
                        });
                    }
                }
            }
            if shown == 0 {
                ui.label(RichText::new("No changes").color(p.dim));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Pos2, RawInput};

    /// Two messages (the second is last). Returns each one's "actions visible" flag and the first block's rect.
    fn frame(ctx: &egui::Context, events: Vec<Event>) -> (Vec<bool>, egui::Rect) {
        let mut flags = Vec::new();
        let mut first = egui::Rect::NOTHING;
        let input = RawInput { screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 300.0))), events, ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            for (uid, is_last) in [(1u64, false), (2u64, true)] {
                let (id, show) = actions_visibility(ui, uid, false, is_last);
                let block = ui.scope(|ui| {
                    ui.label("message");
                    ui.add_space(30.0);
                });
                remember_hover(ui, id, &block.response);
                if uid == 1 {
                    first = block.response.rect;
                }
                flags.push(show);
            }
        });
        out.textures_delta.clear();
        (flags, first)
    }

    #[test]
    fn actions_show_on_last_message_and_on_hover_only() {
        let ctx = egui::Context::default();
        let (flags, first) = frame(&ctx, vec![]);
        assert_eq!(flags, vec![false, true], "no pointer: only the last message shows actions");

        frame(&ctx, vec![Event::PointerMoved(first.center())]);
        let (flags, _) = frame(&ctx, vec![]);
        assert_eq!(flags, vec![true, true], "hovered message shows actions");

        frame(&ctx, vec![Event::PointerGone]);
        let (flags, _) = frame(&ctx, vec![]);
        assert_eq!(flags, vec![false, true], "actions hide again when the pointer leaves the window");

        frame(&ctx, vec![Event::PointerMoved(first.center())]);
        frame(&ctx, vec![Event::PointerMoved(Pos2::new(390.0, 290.0))]);
        let (flags, _) = frame(&ctx, vec![]);
        assert_eq!(flags, vec![false, true], "actions hide when the pointer moves elsewhere");

        let ctx = egui::Context::default();
        let input = RawInput::default();
        let mut out = ctx.run_ui(input, |ui| assert!(!actions_visibility(ui, 9, true, true).1, "hidden while a turn runs"));
        out.textures_delta.clear();
    }
}
