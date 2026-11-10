//! Settings page (same sections as the web UI), reading and writing the core's
//! settings/trust store directly.

use crate::app::{Bg, KiroApp};
use crate::view::Named;
use eframe::egui::{self, CornerRadius, Margin, RichText, Stroke};
use kiro_ui_core::{suggestions, JsonMap};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const KIRO_SETTING_DEBOUNCE: Duration = Duration::from_millis(500);
const SAVED_MARK: Duration = Duration::from_millis(1500);

fn saved_mark(ui: &mut egui::Ui, saved: &[String], key: &str, color: egui::Color32) {
    if saved.iter().any(|k| k == key) {
        ui.label(RichText::new("✔ Saved").size(12.0).color(color));
    }
}

impl KiroApp {
    pub fn open_settings(&mut self) {
        let s = &self.state;
        let get = |k: &str, d: &str| s.setting(k).unwrap_or_else(|| d.to_string());
        self.settings.workspace = s.setting("workspace").unwrap_or_default();
        self.settings.transport = get("transport", "process");
        self.settings.acp_command = get("acpCommand", "kiro-cli acp");
        self.settings.tcp_host = get("tcpHost", "localhost");
        self.settings.tcp_port = get("tcpPort", "9000");
        self.settings.share = s.share_agent_process();
        self.settings.max_tabs = get("maxTabs", "10");
        self.settings.max_mem = get("maxChildMemMb", "512");
        self.settings.max_msgs = get("maxMsgsPerMin", "30");
        self.settings.sugg_enabled = s.setting("suggestionsEnabled").as_deref() != Some("false");
        self.settings.sugg_region = s.setting("suggestionsRegion").unwrap_or_default();
        self.settings.sugg_profile = s.setting("suggestionsProfile").unwrap_or_default();
        self.settings.sugg_model = s.setting("suggestionsModel").unwrap_or_default();
        self.settings.sugg_count = get("suggestionsCount", "3");
        self.settings.trust = s.trust_rules();
        self.settings.test_result = None;
        self.settings.kiro_edits.clear();
        self.settings.loaded = true;
        self.send(json!({"action": "kiro_settings_list"}));
        self.fetch_models();
    }

    fn fetch_models(&mut self) {
        self.settings.models_loading = true;
        self.settings.models_error = None;
        let state = self.state.clone();
        self.spawn_bg(async move {
            let r = suggestions::models(&state).await;
            let list = r
                .get("models")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|m| {
                    let g = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                    (g("id"), g("name"), g("group"))
                })
                .collect();
            Bg::Models(list, r.get("error").and_then(Value::as_str).map(str::to_owned))
        });
    }

    fn flash(&mut self, key: &str) {
        self.settings.saved_flash.insert(key.to_string(), Instant::now());
    }

    fn flashed(&self, key: &str) -> bool {
        self.settings.saved_flash.get(key).is_some_and(|at| at.elapsed() < SAVED_MARK)
    }

    /// Save one setting and show its "✔ Saved" mark.
    fn save_flashed(&mut self, key: &str, value: Value) {
        self.save_setting(key, value);
        self.flash(key);
    }

    fn recently_saved(&mut self) -> Vec<String> {
        self.settings.saved_flash.retain(|_, at| at.elapsed() < SAVED_MARK);
        self.settings.saved_flash.keys().cloned().collect()
    }

    pub fn settings_page(&mut self, ui: &mut egui::Ui) {
        let p = *self.palette();
        if !self.settings.loaded {
            self.open_settings();
        }
        self.flush_kiro_settings();
        ui.horizontal(|ui| {
            ui.heading("Settings");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("×").named(ui, "Close settings").clicked() {
                    self.show_settings = false;
                }
            });
        });
        ui.add_space(8.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(760.0);
            let section = |ui: &mut egui::Ui, title: &str, desc: &str, body: &mut dyn FnMut(&mut egui::Ui)| {
                egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(14)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(title).strong().size(15.0));
                    ui.label(RichText::new(desc).size(12.0).color(p.muted));
                    ui.add_space(6.0);
                    body(ui);
                });
                ui.add_space(12.0);
            };

            let saved = self.recently_saved();
            let mut save_ws = false;
            let mut editor_changed = None;
            let editor = self.editor.clone();
            let ws_flash = self.flashed("workspace");
            section(ui, "General", "Workspace and editor preferences.", &mut |ui| {
                egui::Grid::new("general").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                    ui.label("Workspace directory");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.settings.workspace).hint_text("~/.kiro-ui/workspace").desired_width(280.0));
                        save_ws = ui.button(if ws_flash { "✔ Saved" } else { "Save" }).clicked();
                    });
                    ui.end_row();
                    ui.label("Editor for file links");
                    let names = [("vscode", "VS Code"), ("cursor", "Cursor"), ("idea", "IntelliJ IDEA"), ("none", "None (plain text)")];
                    let cur = names.iter().find(|(k, _)| *k == editor).map_or("VS Code", |(_, l)| l);
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("editor").selected_text(cur).show_ui(ui, |ui| {
                            for (k, l) in names {
                                if ui.selectable_label(editor == k, l).clicked() {
                                    editor_changed = Some(k.to_string());
                                }
                            }
                        });
                        saved_mark(ui, &saved, "editor", p.green);
                    });
                    ui.end_row();
                });
            });
            if save_ws {
                self.save_setting("workspace", json!(self.settings.workspace));
                self.flash("workspace");
            }
            if let Some(e) = editor_changed {
                self.save_flashed("editor", json!(e));
                self.editor = e;
            }

            let mut save_transport = false;
            let mut share_changed = None;
            let tr_flash = self.flashed("transport");
            section(ui, "ACP Transport", "How to connect to the Kiro agent. Changes apply to new sessions only.", &mut |ui| {
                egui::Grid::new("transport").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                    ui.label("Transport mode");
                    let label = if self.settings.transport == "tcp" { "TCP (connect to host:port)" } else { "Process (spawn command)" };
                    egui::ComboBox::from_id_salt("transport").selected_text(label).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.settings.transport, "process".to_string(), "Process (spawn command)");
                        ui.selectable_value(&mut self.settings.transport, "tcp".to_string(), "TCP (connect to host:port)");
                    });
                    ui.end_row();
                    if self.settings.transport == "tcp" {
                        ui.label("Host");
                        ui.add(egui::TextEdit::singleline(&mut self.settings.tcp_host).hint_text("localhost").desired_width(200.0));
                        ui.end_row();
                        ui.label("Port");
                        ui.add(egui::TextEdit::singleline(&mut self.settings.tcp_port).hint_text("9000").desired_width(80.0));
                        ui.end_row();
                    } else {
                        ui.label("Command");
                        ui.add(egui::TextEdit::singleline(&mut self.settings.acp_command).hint_text("kiro-cli acp").desired_width(280.0));
                        ui.end_row();
                    }
                    ui.label("Share agent process");
                    let label = if self.settings.share { "One process for all tabs (less memory)" } else { "One process per tab" };
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut self.settings.share, label).changed() {
                            share_changed = Some(self.settings.share);
                        }
                        saved_mark(ui, &saved, "shareAgentProcess", p.green);
                    });
                    ui.end_row();
                    ui.label("");
                    save_transport = ui.button(if tr_flash { "✔ Saved" } else { "Save transport" }).clicked();
                    ui.end_row();
                });
            });
            if save_transport {
                let st = &self.settings;
                let mut patch = JsonMap::new();
                for (k, v) in [("transport", &st.transport), ("acpCommand", &st.acp_command), ("tcpHost", &st.tcp_host), ("tcpPort", &st.tcp_port)] {
                    patch.insert(k.into(), json!(v));
                }
                let state = self.state.clone();
                self.rt.spawn(async move {
                    state.merge_settings(patch).await;
                });
                self.flash("transport");
            }
            if let Some(v) = share_changed {
                self.save_flashed("shareAgentProcess", json!(v.to_string()));
            }

            let mut saves: Vec<(&str, Value)> = Vec::new();
            let mut refresh = false;
            let mut test = false;
            let models = self.settings.models.clone();
            let loading = self.settings.models_loading;
            let models_error = self.settings.models_error.clone();
            let testing = self.settings.testing;
            let test_result = self.settings.test_result.clone();
            section(ui, "Suggestions", "Generate follow-up suggestions via Amazon Bedrock after each turn. Requires AWS credentials.", &mut |ui| {
                egui::Grid::new("sugg").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                    ui.label("Enabled");
                    let label = if self.settings.sugg_enabled { "On" } else { "Off" };
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut self.settings.sugg_enabled, label).changed() {
                            saves.push(("suggestionsEnabled", json!(self.settings.sugg_enabled.to_string())));
                        }
                        saved_mark(ui, &saved, "suggestionsEnabled", p.green);
                    });
                    ui.end_row();
                    ui.label("AWS Region");
                    ui.horizontal(|ui| {
                        if ui.add(egui::TextEdit::singleline(&mut self.settings.sugg_region).hint_text("us-east-1").desired_width(160.0)).lost_focus() {
                            saves.push(("suggestionsRegion", json!(self.settings.sugg_region)));
                        }
                        saved_mark(ui, &saved, "suggestionsRegion", p.green);
                    });
                    ui.end_row();
                    ui.label("AWS Profile");
                    ui.horizontal(|ui| {
                        if ui.add(egui::TextEdit::singleline(&mut self.settings.sugg_profile).hint_text("default").desired_width(160.0)).lost_focus() {
                            saves.push(("suggestionsProfile", json!(self.settings.sugg_profile)));
                        }
                        saved_mark(ui, &saved, "suggestionsProfile", p.green);
                    });
                    ui.end_row();
                    ui.label("Model");
                    ui.horizontal(|ui| {
                        let cur = if self.settings.sugg_model.is_empty() {
                            "amazon.nova-lite-v1:0 (default)".to_string()
                        } else {
                            models.iter().find(|m| m.0 == self.settings.sugg_model).map_or(self.settings.sugg_model.clone(), |m| m.1.clone())
                        };
                        egui::ComboBox::from_id_salt("sugg-model").selected_text(cur).width(280.0).show_ui(ui, |ui| {
                            if ui.selectable_label(self.settings.sugg_model.is_empty(), "amazon.nova-lite-v1:0 (default)").clicked() {
                                self.settings.sugg_model.clear();
                                saves.push(("suggestionsModel", json!("")));
                            }
                            for group in ["Inference Profiles", "Foundation Models"] {
                                let items: Vec<_> = models.iter().filter(|m| m.2 == group).collect();
                                if items.is_empty() {
                                    continue;
                                }
                                ui.label(RichText::new(group).size(11.0).color(p.dim));
                                for (id, name, _) in items {
                                    if ui.selectable_label(self.settings.sugg_model == *id, name).clicked() {
                                        self.settings.sugg_model = id.clone();
                                        saves.push(("suggestionsModel", json!(id)));
                                    }
                                }
                            }
                        });
                        if loading {
                            ui.spinner();
                        }
                        refresh = ui.button("↻").named(ui, "Reload models").clicked();
                        saved_mark(ui, &saved, "suggestionsModel", p.green);
                    });
                    ui.end_row();
                    if let Some(e) = &models_error {
                        ui.label("");
                        ui.add(egui::Label::new(RichText::new(format!("Could not load models: {e}")).size(12.0).color(p.red)).wrap());
                        ui.end_row();
                    }
                    ui.label("Suggestions count");
                    ui.horizontal(|ui| {
                        if ui.add(egui::TextEdit::singleline(&mut self.settings.sugg_count).desired_width(60.0)).lost_focus() {
                            saves.push(("suggestionsCount", json!(self.settings.sugg_count)));
                        }
                        saved_mark(ui, &saved, "suggestionsCount", p.green);
                    });
                    ui.end_row();
                    ui.label("Test connection");
                    ui.horizontal(|ui| {
                        test = ui.add_enabled(!testing, egui::Button::new("Test model")).clicked();
                        if testing {
                            ui.label(RichText::new("Testing...").color(p.dim));
                        }
                        match &test_result {
                            Some((true, m)) => {
                                ui.label(RichText::new(format!("✔ {m}")).color(p.green));
                            }
                            Some((false, m)) => {
                                ui.label(RichText::new(m).color(p.red));
                            }
                            None => {}
                        }
                    });
                    ui.end_row();
                });
            });
            for (k, v) in saves {
                self.save_flashed(k, v);
            }
            if refresh {
                // Region/profile edits must land before listing models.
                let st = &self.settings;
                let mut patch = JsonMap::new();
                patch.insert("suggestionsRegion".into(), json!(st.sugg_region));
                patch.insert("suggestionsProfile".into(), json!(st.sugg_profile));
                let state = self.state.clone();
                self.settings.models_loading = true;
                self.settings.models_error = None;
                self.spawn_bg(async move {
                    state.merge_settings(patch).await;
                    let r = suggestions::models(&state).await;
                    let list = r
                        .get("models")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .map(|m| {
                            let g = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                            (g("id"), g("name"), g("group"))
                        })
                        .collect();
                    Bg::Models(list, r.get("error").and_then(Value::as_str).map(str::to_owned))
                });
            }
            if test {
                self.settings.testing = true;
                self.settings.test_result = None;
                let state = self.state.clone();
                self.spawn_bg(async move {
                    let r = suggestions::test(&state).await;
                    let ok = r.get("ok").and_then(Value::as_bool).unwrap_or(false);
                    let msg = if ok { r.get("model") } else { r.get("error") }.and_then(Value::as_str).unwrap_or_default().to_string();
                    Bg::TestResult(ok, msg)
                });
            }

            let mut remove = None;
            let trust = self.settings.trust.clone();
            section(ui, "Trust Rules", "Persistent trust rules for tool calls. Tools marked \"Always allow\" or \"Always reject\" auto-apply without prompting.", &mut |ui| {
                if trust.is_empty() {
                    ui.label(RichText::new("No trust rules configured yet.").italics().color(p.dim));
                }
                egui::Grid::new("trust").num_columns(3).spacing([16.0, 6.0]).show(ui, |ui| {
                    for (tool, policy) in &trust {
                        let policy = policy.as_str().unwrap_or_default();
                        ui.label(tool);
                        let color = if policy.starts_with("allow") { p.green } else { p.red };
                        ui.label(RichText::new(policy.replace('_', " ")).color(color).size(12.0));
                        if ui.small_button("×").named(ui, &format!("Remove rule {tool}")).clicked() {
                            remove = Some(tool.clone());
                        }
                        ui.end_row();
                    }
                });
            });
            if let Some(k) = remove {
                self.settings.trust.remove(&k);
                let rules = self.settings.trust.clone();
                let state = self.state.clone();
                self.rt.spawn(async move {
                    state.set_trust_rules(rules).await;
                });
            }

            if let Some(ks) = self.model.kiro_settings.clone().filter(|m| !m.is_empty()) {
                let mut changed: Vec<(String, String)> = Vec::new();
                section(ui, "Kiro Agent Settings", "Settings from the Kiro CLI agent. Changes are applied immediately.", &mut |ui| {
                    egui::Grid::new("kiro").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                        for (k, v) in &ks {
                            ui.label(RichText::new(k).monospace().size(12.0));
                            match v {
                                Value::Bool(b) => {
                                    let mut val = *b;
                                    let label = val.to_string();
                                    if ui.checkbox(&mut val, label).changed() {
                                        changed.push((k.clone(), val.to_string()));
                                    }
                                }
                                other => {
                                    let current = match other {
                                        Value::String(s) => s.clone(),
                                        Value::Null => String::new(),
                                        x => x.to_string(),
                                    };
                                    let edit = self.settings.kiro_edits.entry(k.clone()).or_insert(current);
                                    if ui.add(egui::TextEdit::singleline(edit).desired_width(260.0)).changed() {
                                        changed.push((k.clone(), edit.clone()));
                                    }
                                }
                            }
                            ui.end_row();
                        }
                    });
                });
                for (k, v) in changed {
                    self.settings.kiro_edits.insert(k.clone(), v);
                    self.settings.kiro_pending.insert(k, Instant::now() + KIRO_SETTING_DEBOUNCE);
                }
            }

            let mut limit_saves: Vec<(&str, String)> = Vec::new();
            section(ui, "Limits", "Max sessions and prompts per minute apply right away; the memory limit applies to newly started agents.", &mut |ui| {
                egui::Grid::new("limits").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                    for (label, key, field, note) in [
                        ("Max concurrent sessions", "maxTabs", &mut self.settings.max_tabs, ""),
                        ("Agent memory limit (MB)", "maxChildMemMb", &mut self.settings.max_mem, "Node-based ACP agents only; kiro-cli ignores it"),
                        ("Max prompts per minute", "maxMsgsPerMin", &mut self.settings.max_msgs, ""),
                    ] {
                        ui.label(label);
                        ui.horizontal(|ui| {
                            if ui.add(egui::TextEdit::singleline(field).desired_width(80.0)).lost_focus() {
                                limit_saves.push((key, field.clone()));
                            }
                            if !note.is_empty() {
                                ui.label(RichText::new(note).size(12.0).color(p.muted));
                            }
                            saved_mark(ui, &saved, key, p.green);
                        });
                        ui.end_row();
                    }
                });
            });
            for (k, v) in limit_saves {
                self.save_flashed(k, json!(v));
            }

            let mut debug = self.debug;
            let mut reduce = self.reduce_motion;
            section(ui, "Accessibility & Advanced", "Motion preference and developer diagnostics.", &mut |ui| {
                ui.checkbox(&mut reduce, "Reduce motion (no animated indicators or transitions)");
                ui.checkbox(&mut debug, "Show protocol log (raw ACP JSON-RPC traffic)");
            });
            if reduce != self.reduce_motion {
                self.set_reduce_motion(reduce);
                self.save_setting("reduceMotion", json!(reduce.to_string()));
            }
            if debug != self.debug {
                self.debug = debug;
                self.send(json!({"action": "set_debug", "enabled": debug}));
                self.save_setting("debugEnabled", json!(debug.to_string()));
            }
        });
    }

    /// Debounced `kiro_settings_set` for agent settings edited in text fields.
    fn flush_kiro_settings(&mut self) {
        let now = Instant::now();
        let due: Vec<String> = self.settings.kiro_pending.iter().filter(|(_, at)| now >= **at).map(|(k, _)| k.clone()).collect();
        for k in due {
            self.settings.kiro_pending.remove(&k);
            let v = self.settings.kiro_edits.get(&k).cloned().unwrap_or_default();
            self.send(json!({"action": "kiro_settings_set", "key": k, "value": v}));
        }
    }
}
