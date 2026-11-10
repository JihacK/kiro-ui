//! Input area: subagent pipeline, suggestions, goal banner, message queue,
//! context meter, slash-command popup, attachments and the prompt box.

use crate::app::{Attachment, KiroApp};
use crate::model::{CmdOption, GoalStatus};
use crate::view::chip;
use eframe::egui::{self, Color32, CornerRadius, Key, Margin, Modifiers, RichText, Stroke};
use serde_json::json;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(150);

enum QueueOp {
    Up(usize),
    Down(usize),
    Merge(usize),
    Remove(usize),
    Clear,
}

impl KiroApp {
    pub fn input_area(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        let tab_id = self.active_id();
        self.flush_command_options(&tab_id);
        self.sync_external_options(&tab_id);

        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width().min(940.0));
            self.subagent_panel(ui);
            self.suggestions_row(ui);
            self.goal_banner(ui);
            self.queue_list(ui);
            self.context_meter(ui);
            self.command_popup(ui);

            let running = self.model.active_tab().running;
            let connected = self.model.active_tab().modes.is_some();
            let tab_index = self.model.tabs.iter().position(|t| t.id == tab_id).unwrap_or(0);
            let accent = p.ghosts[tab_index % 6];

            // Active skills for this tab
            let skills = self.model.active_tab().skills.clone();
            if !skills.is_empty() {
                let mut remove = None;
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("📚").color(p.muted)).on_hover_text("Skills applied to every prompt in this tab");
                    for (i, name) in skills.iter().enumerate() {
                        egui::Frame::new().fill(p.accent_soft).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(8, 3)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(name).size(12.0).color(p.accent));
                                if ui.add(egui::Button::new(RichText::new("×").size(10.0)).frame(false)).on_hover_text("Remove skill").clicked() {
                                    remove = Some(i);
                                }
                            });
                        });
                    }
                });
                if let Some(i) = remove {
                    self.model.active_tab_mut().skills.remove(i);
                }
            }

            // Attachments
            let attachments: Vec<String> = self.ui_for(&tab_id).attachments.iter().map(|a| a.name().to_string()).collect();
            if !attachments.is_empty() {
                let mut remove = None;
                ui.horizontal_wrapped(|ui| {
                    for (i, name) in attachments.iter().enumerate() {
                        egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(8, 3)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let icon = if self.tab_ui[&tab_id].attachments.get(i).is_some_and(|a| matches!(a, Attachment::Image { .. })) { "🖼" } else { "📄" };
                                ui.label(RichText::new(format!("{icon} {name}")).size(12.0));
                                if ui.add(egui::Button::new(RichText::new("×").size(10.0)).frame(false)).clicked() {
                                    remove = Some(i);
                                }
                            });
                        });
                    }
                });
                if let Some(i) = remove {
                    self.ui_for(&tab_id).attachments.remove(i);
                }
            }

            let input_id = egui::Id::unique(("prompt", &tab_id));
            let focused = ui.memory(|m| m.has_focus(input_id));
            let mut do_send = false;
            if focused {
                if self.tab_ui.get(&tab_id).is_some_and(|u| u.popup.is_some()) {
                    self.popup_keys(ui, &tab_id);
                } else {
                    let (enter, esc) = ui.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape)));
                    do_send = enter;
                    if esc && running {
                        self.cancel();
                    }
                }
            }

            egui::Frame::new()
                .fill(p.surface)
                .stroke(Stroke::new(1.0, if focused { accent } else { p.border }))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let width = ui.available_width() - 78.0;
                        let tu = self.tab_ui.entry(tab_id.clone()).or_default();
                        let r = ui.add(
                            egui::TextEdit::multiline(&mut tu.input)
                                .id(input_id)
                                .hint_text("Message Kiro... (/ for commands, Ctrl+T new tab, Ctrl+B sidebar)")
                                .desired_rows(1)
                                .desired_width(width)
                                .frame(egui::Frame::NONE)
                                .lock_focus(true),
                        );
                        if r.changed() {
                            self.on_input_changed(&tab_id);
                        }
                        if !running && !focused && !self.show_rewind && ui.memory(|m| m.focused().is_none()) {
                            r.request_focus();
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
                            if running {
                                if ui.add(egui::Button::new(RichText::new("■").color(p.text)).fill(p.surface2).min_size(egui::vec2(32.0, 30.0))).on_hover_text("Cancel (Esc)").clicked() {
                                    self.cancel();
                                }
                            } else {
                                let tu = &self.tab_ui[&tab_id];
                                let can = connected && (!tu.input.trim().is_empty() || !tu.attachments.is_empty());
                                let btn = egui::Button::new(RichText::new("⬆").strong().color(Color32::WHITE)).fill(if can { p.accent } else { p.accent.gamma_multiply(0.35) }).min_size(egui::vec2(32.0, 30.0));
                                if ui.add_enabled(can, btn).on_hover_text("Send (Enter)").clicked() {
                                    do_send = true;
                                }
                            }
                            if ui.add_enabled(!self.dialog_open(), egui::Button::new("📎").frame(false)).on_hover_text("Attach image or file (or drop files on the window)").clicked() {
                                self.attach_files();
                            }
                        });
                    });
                });

            if do_send && connected {
                let tu = self.ui_for(&tab_id);
                let text = tu.input.trim().to_string();
                if !text.is_empty() || !tu.attachments.is_empty() {
                    let attachments = std::mem::take(&mut tu.attachments);
                    tu.input.clear();
                    tu.popup = None;
                    self.model.cmd_hint = None;
                    self.handle_send(text, attachments);
                }
            }
        });
    }

    /// Typing `/x` filters known commands, prompts and skills locally; `/cmd partial` asks the agent (debounced).
    fn on_input_changed(&mut self, tab_id: &str) {
        let mut commands: Vec<CmdOption> = self
            .model
            .commands
            .iter()
            .map(|c| CmdOption { name: c.name.clone(), description: c.description.clone(), value: String::new() })
            .collect();
        commands.extend(self.model.prompts.iter().map(|p| CmdOption {
            name: format!("/{}", p.name),
            description: if p.is_skill() { format!("skill · {}", p.description) } else { p.description.clone() },
            value: String::new(),
        }));
        let tu = self.ui_for(tab_id);
        let val = tu.input.clone();
        if let Some(rest) = val.strip_prefix('/') {
            match rest.split_once(' ') {
                None => {
                    let f = rest.to_lowercase();
                    let matches: Vec<CmdOption> = commands.into_iter().filter(|c| c.name.trim_start_matches('/').to_lowercase().starts_with(&f)).collect();
                    tu.popup = (!matches.is_empty()).then_some(matches);
                    tu.popup_idx = 0;
                    tu.pending_options = None;
                    self.model.cmd_hint = None;
                }
                Some((cmd, partial)) => {
                    tu.pending_options = Some((Instant::now() + DEBOUNCE, cmd.to_string(), partial.to_string()));
                }
            }
        } else {
            tu.popup = None;
            tu.pending_options = None;
            self.model.cmd_hint = None;
        }
    }

    fn flush_command_options(&mut self, tab_id: &str) {
        let due = self.tab_ui.get(tab_id).and_then(|u| u.pending_options.as_ref()).is_some_and(|(at, _, _)| Instant::now() >= *at);
        if due {
            if let Some((_, command, input)) = self.ui_for(tab_id).pending_options.take() {
                self.send(json!({"action": "command_options", "tabId": tab_id, "command": command, "input": input}));
            }
        }
    }

    /// Options pushed by the agent (`CommandOptions`) replace the popup content.
    fn sync_external_options(&mut self, tab_id: &str) {
        let rev = self.model.cmd_options_rev;
        let opts = self.model.cmd_options.clone();
        let tu = self.ui_for(tab_id);
        if tu.cmd_rev_seen != rev {
            tu.cmd_rev_seen = rev;
            // Only while the user is still typing a command.
            if tu.input.starts_with('/') {
                tu.popup = opts;
                tu.popup_idx = 0;
            }
        }
    }

    fn popup_keys(&mut self, ui: &mut egui::Ui, tab_id: &str) {
        let (down, up, pick, esc) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter) | i.consume_key(Modifiers::NONE, Key::Tab),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let tu = self.ui_for(tab_id);
        let n = tu.popup.as_ref().map_or(0, Vec::len);
        if down {
            tu.popup_idx = (tu.popup_idx + 1).min(n.saturating_sub(1));
        }
        if up {
            tu.popup_idx = tu.popup_idx.saturating_sub(1);
        }
        if esc {
            tu.popup = None;
            self.model.cmd_hint = None;
        } else if pick {
            let idx = tu.popup_idx;
            self.pick_option(tab_id, idx);
        }
    }

    fn pick_option(&mut self, tab_id: &str, idx: usize) {
        let tu = self.ui_for(tab_id);
        let Some(item) = tu.popup.as_ref().and_then(|p| p.get(idx)).cloned() else { return };
        if item.value.is_empty() {
            tu.input = format!("{} ", item.name);
        } else {
            let head = tu.input.split(' ').next().unwrap_or_default().to_string();
            tu.input = format!("{head} {} ", item.value);
        }
        tu.popup = None;
        tu.pending_options = None;
        self.model.cmd_hint = None;
        // Put the caret at the end of the completed text.
        let id = egui::Id::unique(("prompt", tab_id));
        let len = self.tab_ui[tab_id].input.chars().count();
        if let Some(mut state) = egui::TextEdit::load_state(&self.ctx, id) {
            state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(len))));
            state.store(&self.ctx, id);
        }
    }

    fn command_popup(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        let tab_id = self.active_id();
        let popup = self.tab_ui.get(&tab_id).and_then(|u| u.popup.clone());
        let Some(items) = popup else {
            if let Some(h) = &self.model.cmd_hint {
                ui.label(RichText::new(h).size(12.0).color(p.muted));
            }
            return;
        };
        let sel = self.tab_ui[&tab_id].popup_idx;
        let mut clicked = None;
        egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(4)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                for (i, c) in items.iter().enumerate() {
                    let fill = if i == sel { p.accent_soft } else { Color32::TRANSPARENT };
                    let r = egui::Frame::new().fill(fill).corner_radius(CornerRadius::same(5)).inner_margin(Margin::symmetric(8, 4)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&c.name).monospace().color(p.accent));
                            ui.label(RichText::new(&c.description).size(12.0).color(p.muted));
                        });
                    });
                    let r = r.response.interact(egui::Sense::click());
                    if i == sel {
                        r.scroll_to_me(None);
                    }
                    if r.clicked() {
                        clicked = Some(i);
                    }
                }
            });
        });
        if let Some(i) = clicked {
            self.pick_option(&tab_id, i);
        }
    }

    fn subagent_panel(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        let tab = self.model.active_tab();
        if tab.subagents.is_empty() {
            return;
        }
        let done = tab.subagents.iter().filter(|s| s.status == "completed").count();
        egui::CollapsingHeader::new(RichText::new(format!("Pipeline ({done}/{})", tab.subagents.len())).color(p.muted)).id_salt("pipeline").default_open(true).show(ui, |ui| {
            for s in &tab.subagents {
                ui.horizontal(|ui| {
                    let icon = match s.status.as_str() {
                        "pending" => "⏳",
                        "running" => "⟳",
                        "completed" => "✔",
                        "failed" => "✖",
                        _ => "?",
                    };
                    ui.label(icon);
                    ui.label(&s.name);
                    if let Some(r) = &s.role {
                        ui.label(RichText::new(r).size(11.0).color(p.dim));
                    }
                    if let Some(n) = s.loop_iteration.filter(|n| *n > 1) {
                        ui.label(RichText::new(format!("×{n}")).size(11.0).color(p.yellow));
                    }
                    if s.status == "running" {
                        if let Some(a) = tab.subagent_activity.get(&s.session_id) {
                            ui.add(egui::Label::new(RichText::new(a).size(11.0).color(p.muted)).truncate());
                        }
                    }
                });
            }
        });
    }

    fn suggestions_row(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        let tab = self.model.active_tab();
        if tab.suggestions.is_empty() || tab.running {
            return;
        }
        let mut chosen = None;
        ui.horizontal_wrapped(|ui| {
            for s in &tab.suggestions {
                if ui.add(egui::Button::new(RichText::new(s).size(12.0).color(p.text)).fill(p.surface2).corner_radius(CornerRadius::same(14))).clicked() {
                    chosen = Some(s.clone());
                }
            }
        });
        if let Some(s) = chosen {
            self.send_text(&s);
        }
    }

    fn goal_banner(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        let tab_id = self.active_id();
        let Some(g) = self.model.active_tab().goal.clone() else { return };
        let (icon, label, color) = match g.status {
            GoalStatus::Complete => ("✔", "Goal complete".to_string(), p.green),
            GoalStatus::Incomplete => ("⚠", format!("Goal incomplete ({}/{})", g.current, g.max), p.yellow),
            GoalStatus::Active => ("⟳", format!("Iteration {}/{}", g.current, g.max), p.accent),
        };
        let mut cancel = false;
        let mut dismiss = false;
        egui::Frame::new().fill(color.gamma_multiply(0.1)).stroke(Stroke::new(1.0, color.gamma_multiply(0.4))).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(10, 5)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{icon} {label}")).color(color));
                let text: String = g.text.chars().take(50).collect();
                let more = if g.text.chars().count() > 50 { "…" } else { "" };
                ui.add(egui::Label::new(RichText::new(format!("{text}{more}")).color(p.muted)).truncate());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if g.status == GoalStatus::Active {
                        cancel = ui.small_button("Cancel").clicked();
                    } else {
                        dismiss = ui.small_button("Dismiss").clicked();
                    }
                });
            });
        });
        if cancel {
            self.model.active_tab_mut().goal = None;
            self.send(json!({"action": "prompt", "tabId": tab_id, "text": "/goal clear"}));
        }
        if dismiss {
            self.model.active_tab_mut().goal = None;
        }
    }

    fn queue_list(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        if self.model.active_tab().queue.is_empty() {
            return;
        }
        let mut op = None;
        let mut send_now = false;
        egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let queue = &mut self.model.active_tab_mut().queue;
            let n = queue.len();
            for (i, item) in queue.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}", i + 1)).color(p.dim));
                    if ui.add_enabled(i > 0, egui::Button::new("⏶").small()).on_hover_text("Move up").clicked() {
                        op = Some(QueueOp::Up(i));
                    }
                    if ui.add_enabled(i + 1 < n, egui::Button::new("⏷").small()).on_hover_text("Move down").clicked() {
                        op = Some(QueueOp::Down(i));
                    }
                    ui.add(egui::TextEdit::singleline(item).desired_width(ui.available_width() - 60.0));
                    if i + 1 < n && ui.small_button("➕").on_hover_text("Merge with next").clicked() {
                        op = Some(QueueOp::Merge(i));
                    }
                    if ui.small_button("×").on_hover_text("Remove").clicked() {
                        op = Some(QueueOp::Remove(i));
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{n} queued")).size(12.0).color(p.muted));
                if ui.small_button("Clear all").clicked() {
                    op = Some(QueueOp::Clear);
                }
                // Cancelling the running turn makes TurnEnd send the next queued message.
                if ui.small_button("Send Now ⚡").clicked() {
                    send_now = true;
                }
            });
        });
        let q = &mut self.model.active_tab_mut().queue;
        match op {
            Some(QueueOp::Up(i)) => q.swap(i - 1, i),
            Some(QueueOp::Down(i)) => q.swap(i, i + 1),
            Some(QueueOp::Merge(i)) => {
                let next = q.remove(i + 1);
                q[i] = format!("{}\n\n{next}", q[i]);
            }
            Some(QueueOp::Remove(i)) => {
                q.remove(i);
            }
            Some(QueueOp::Clear) => q.clear(),
            None => {}
        }
        if send_now {
            self.cancel();
        }
    }

    fn context_meter(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        let md = self.model.active_tab().metadata.clone();
        if md.context_pct <= 0.0 {
            return;
        }
        let mut compact = false;
        ui.horizontal(|ui| {
            let color = if md.context_pct >= 90.0 {
                p.red
            } else if md.context_pct >= 70.0 {
                p.yellow
            } else {
                p.accent
            };
            ui.add(egui::ProgressBar::new((md.context_pct as f32 / 100.0).clamp(0.0, 1.0)).desired_width(90.0).desired_height(6.0).fill(color));
            ui.label(RichText::new(format!("{}%", md.context_pct.round())).size(11.0).color(p.muted));
            if md.context_pct >= 50.0 && ui.add(egui::Button::new(RichText::new("Compact").size(11.0)).small()).on_hover_text("Compact context").clicked() {
                compact = true;
            }
            let u = &md.cumulative;
            if u.input > 0.0 || u.output > 0.0 {
                let cost = if u.cost > 0.0 { format!(" · ${:.3}", u.cost) } else { String::new() };
                let tip = format!("In: {} · Out: {}{}", u.input as u64, u.output as u64, if u.cost > 0.0 { format!(" · ${:.4}", u.cost) } else { String::new() });
                chip(ui, &format!("{:.1}k{cost}", (u.input + u.output) / 1000.0), p.muted, p.surface2);
                ui.label("").on_hover_text(tip);
            }
        });
        if compact {
            self.send_text("/compact");
        }
    }
}
