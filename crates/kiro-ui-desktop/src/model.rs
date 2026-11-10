//! UI state and event handling (port of the web client's `useMessageHandler`).
//!
//! `Model::handle` applies one core event to the state and returns the side
//! effects (follow-up actions, suggestion requests, notifications) for the app
//! to execute, which keeps this module pure and testable.

use kiro_ui_core::truthy;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

pub const INIT_TAB: &str = "tab-1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    UserStream,
    Assistant,
    AssistantStream,
    System,
    Tool,
    Panel,
}

impl Role {
    pub fn is_user(self) -> bool {
        matches!(self, Role::User | Role::UserStream)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ToolEntry {
    pub id: String,
    pub title: String,
    pub kind: Option<String>,
    pub content: Vec<Value>,
    pub status: String,
    pub expanded: bool,
    pub raw_input: Option<Value>,
    pub raw_output: Option<Value>,
    pub stream_output: String,
}

#[derive(Debug, Clone)]
pub struct Msg {
    /// Stable per-message id (egui widget ids, layout cache).
    pub uid: u64,
    pub role: Role,
    pub text: String,
    pub panel: Option<String>,
    pub tool: Option<ToolEntry>,
}

#[derive(Debug, Clone)]
pub struct Thinking {
    pub text: String,
    pub start: Instant,
    pub collapsed: bool,
}

#[derive(Debug, Clone)]
pub struct PermOption {
    pub id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct Permission {
    pub request_id: Value,
    pub title: String,
    pub options: Vec<PermOption>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalStatus {
    Active,
    Complete,
    Incomplete,
}

#[derive(Debug, Clone)]
pub struct Goal {
    pub text: String,
    pub max: u64,
    pub current: u64,
    pub status: GoalStatus,
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input: f64,
    pub output: f64,
    pub cost: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub context_pct: f64,
    pub turn_ms: Option<f64>,
    pub cumulative: Usage,
}

#[derive(Debug, Clone)]
pub struct PlanEntry {
    pub content: String,
    pub priority: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct ActiveFile {
    pub path: String,
    pub line: Option<u64>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Subagent {
    pub session_id: String,
    pub name: String,
    pub role: Option<String>,
    pub status: String,
    pub loop_iteration: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Mode {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Modes {
    pub current: String,
    pub available: Vec<Mode>,
}

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Models {
    pub current: String,
    pub available: Vec<ModelInfo>,
}

#[derive(Debug, Clone)]
pub struct Tab {
    pub id: String,
    pub name: String,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub messages: Vec<Msg>,
    pub thinking: Option<Thinking>,
    pub permissions: Vec<Permission>,
    pub running: bool,
    pub connecting: bool,
    pub metadata: Metadata,
    pub queue: Vec<String>,
    pub stream: String,
    pub last_stop: Option<String>,
    pub modes: Option<Modes>,
    pub models: Option<Models>,
    pub perm_policy: String,
    pub effort: Option<String>,
    pub effort_supported: bool,
    pub plan: Vec<PlanEntry>,
    pub active_files: Vec<ActiveFile>,
    pub suggestions: Vec<String>,
    pub goal: Option<Goal>,
    pub subagents: Vec<Subagent>,
    pub subagent_activity: HashMap<String, String>,
    /// Streamed assistant text since the last turn, used as suggestion context.
    pub accumulator: String,
    /// Skills the user picked for this tab; applied to every normal prompt.
    pub skills: Vec<String>,
}

impl Tab {
    pub fn new(id: &str, name: &str) -> Tab {
        Tab {
            id: id.into(),
            name: name.into(),
            session_id: None,
            cwd: None,
            messages: Vec::new(),
            thinking: None,
            permissions: Vec::new(),
            running: false,
            connecting: false,
            metadata: Metadata::default(),
            queue: Vec::new(),
            stream: String::new(),
            last_stop: None,
            modes: None,
            models: None,
            perm_policy: "ask".into(),
            effort: None,
            effort_supported: false,
            plan: Vec::new(),
            active_files: Vec::new(),
            suggestions: Vec::new(),
            goal: None,
            subagents: Vec::new(),
            subagent_activity: HashMap::new(),
            accumulator: String::new(),
            skills: Vec::new(),
        }
    }

    /// Finalise streaming messages (`*-stream` → final role).
    fn settle_streams(&mut self) {
        for m in &mut self.messages {
            m.role = match m.role {
                Role::AssistantStream => Role::Assistant,
                Role::UserStream => Role::User,
                r => r,
            };
        }
    }

    pub fn current_mode(&self) -> Option<&Mode> {
        let m = self.modes.as_ref()?;
        m.available.iter().find(|x| x.id == m.current)
    }
}

#[derive(Debug, Clone)]
pub struct McpServer {
    pub name: String,
    pub status: String,
    pub tool_count: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct Command {
    pub name: String,
    pub description: String,
}

/// An MCP prompt or a Kiro skill (`source` starts with `skill:`), invoked as `/name`.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub name: String,
    pub description: String,
    pub source: String,
}

impl Prompt {
    pub fn is_skill(&self) -> bool {
        self.source.starts_with("skill:")
    }
}

/// Add the tab's chosen skills to a normal prompt so the agent activates them.
/// The note goes last so Kiro still titles the session from the user's own words.
/// Slash commands are passed through untouched.
pub fn with_skills(skills: &[String], text: &str) -> String {
    if skills.is_empty() || text.starts_with('/') {
        return text.to_string();
    }
    format!("{text}\n\n(Use these skills for this request: {}.)", skills.join(", "))
}

#[derive(Debug, Clone)]
pub struct CmdOption {
    pub name: String,
    pub description: String,
    /// Empty for top-level command names; the argument value otherwise.
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub text: String,
    pub kind: ToastKind,
    pub created: Instant,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub dir: String,
    pub msg: String,
}

/// Side effects requested by event handling.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Send an action to the core, optionally after a delay (ms).
    Send(Value, u64),
    /// Ask Bedrock for follow-up suggestions for this tab.
    Suggest { tab: String, context: String },
    /// Desktop notification (only shown when the window is unfocused).
    Notify(String),
}

pub struct Model {
    pub tabs: Vec<Tab>,
    pub active: String,
    pub tab_counter: u64,
    pub commands: Vec<Command>,
    pub prompts: Vec<Prompt>,
    pub mcp_servers: Vec<McpServer>,
    pub tools: Vec<Tool>,
    pub oauth_pending: HashMap<String, String>,
    pub sessions: Vec<Session>,
    pub kiro_settings: Option<serde_json::Map<String, Value>>,
    pub logs: VecDeque<LogLine>,
    pub toasts: Vec<Toast>,
    /// Options pushed by the agent for the slash-command popup.
    pub cmd_options: Option<Vec<CmdOption>>,
    pub cmd_hint: Option<String>,
    /// Bumped whenever `cmd_options` changes so the input resets its selection.
    pub cmd_options_rev: u64,
    /// (server, error) pairs already toasted, so a repeated failure isn't shown twice.
    toasted_mcp_failures: HashSet<(String, String)>,
    next_uid: u64,
    next_toast: u64,
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

fn s(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(x) => Some(x.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn s_or(v: Option<&Value>, d: &str) -> String {
    s(v).filter(|x| !x.is_empty()).unwrap_or_else(|| d.to_string())
}

fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
}

impl Model {
    pub fn new() -> Model {
        let mut first = Tab::new(INIT_TAB, "New Chat");
        first.connecting = true;
        Model {
            tabs: vec![first],
            active: INIT_TAB.into(),
            tab_counter: 1,
            commands: Vec::new(),
            prompts: Vec::new(),
            mcp_servers: Vec::new(),
            tools: Vec::new(),
            oauth_pending: HashMap::new(),
            sessions: Vec::new(),
            kiro_settings: None,
            logs: VecDeque::new(),
            toasts: Vec::new(),
            cmd_options: None,
            cmd_hint: None,
            cmd_options_rev: 0,
            toasted_mcp_failures: HashSet::new(),
            next_uid: 1,
            next_toast: 1,
        }
    }

    pub fn tab(&self, id: &str) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }
    pub fn tab_mut(&mut self, id: &str) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }
    pub fn active_tab(&self) -> &Tab {
        self.tab(&self.active).unwrap_or(&self.tabs[0])
    }
    pub fn active_tab_mut(&mut self) -> &mut Tab {
        let id = self.active.clone();
        let i = self.tabs.iter().position(|t| t.id == id).unwrap_or(0);
        &mut self.tabs[i]
    }

    pub fn msg(&mut self, role: Role, text: impl Into<String>) -> Msg {
        let uid = self.next_uid;
        self.next_uid += 1;
        Msg { uid, role, text: text.into(), panel: None, tool: None }
    }

    pub fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        let id = self.next_toast;
        self.next_toast += 1;
        self.toasts.push(Toast { id, text: text.into(), kind, created: Instant::now() });
        let n = self.toasts.len();
        if n > 5 {
            self.toasts.drain(..n - 5);
        }
    }

    /// Allocate a new tab id (`tab-N`) and add the tab.
    pub fn add_tab(&mut self, name: &str, cwd: Option<String>) -> String {
        self.tab_counter += 1;
        let id = format!("tab-{}", self.tab_counter);
        let mut t = Tab::new(&id, name);
        t.connecting = true;
        t.cwd = cwd;
        self.tabs.push(t);
        self.active = id.clone();
        id
    }

    pub fn close_tab(&mut self, id: &str) {
        if self.tabs.len() <= 1 {
            return;
        }
        if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
            self.tabs.remove(i);
            if self.active == id {
                self.active = self.tabs[i.min(self.tabs.len() - 1)].id.clone();
            }
        }
    }

    fn with_tab(&mut self, id: &str, f: impl FnOnce(&mut Tab, &mut u64)) {
        let mut uid = self.next_uid;
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
            f(t, &mut uid);
        }
        self.next_uid = uid;
    }

    fn apply_session_titles(&mut self) {
        for t in &mut self.tabs {
            let Some(sid) = &t.session_id else { continue };
            if let Some(m) = self.sessions.iter().find(|s| &s.id == sid) {
                if !m.title.is_empty() && m.title != t.name && !m.title.contains("title not available") {
                    t.name = m.title.clone();
                }
            }
        }
    }

    /// Apply one core event. Returns effects for the app to run.
    pub fn handle(&mut self, data: &Value) -> Vec<Effect> {
        let mut fx = Vec::new();
        let tid = s(data.get("tabId")).filter(|x| !x.is_empty()).unwrap_or_else(|| INIT_TAB.into());
        let ty = data.get("type").and_then(Value::as_str).unwrap_or_default();
        let text = || s(data.get("text")).unwrap_or_default();
        match ty {
            "ready" => {
                let modes = parse_modes(data.get("modes"));
                let models = parse_models(data.get("models"));
                let sid = s(data.get("sessionId"));
                let mut ctx = String::new();
                self.with_tab(&tid, |t, _| {
                    // Re-apply the tab's choices to a fresh session (e.g. after a crash restart).
                    if let (Some(old), Some(new)) = (&t.modes, &modes) {
                        if !old.current.is_empty() && old.current != new.current {
                            fx.push(Effect::Send(json!({"action": "set_mode", "tabId": tid, "modeId": old.current}), 100));
                        }
                    }
                    if let (Some(old), Some(new)) = (&t.models, &models) {
                        if !old.current.is_empty() && old.current != new.current {
                            fx.push(Effect::Send(json!({"action": "set_model", "tabId": tid, "modelId": old.current}), 100));
                        }
                    }
                    if t.perm_policy != "ask" {
                        fx.push(Effect::Send(json!({"action": "set_permission_policy", "tabId": tid, "policy": t.perm_policy}), 100));
                    }
                    let keep_mode = t.modes.as_ref().map(|m| m.current.clone()).filter(|c| !c.is_empty());
                    let keep_model = t.models.as_ref().map(|m| m.current.clone()).filter(|c| !c.is_empty());
                    t.modes = modes.map(|mut m| {
                        if let Some(c) = keep_mode {
                            m.current = c;
                        }
                        m
                    });
                    t.models = models.map(|mut m| {
                        if let Some(c) = keep_model {
                            m.current = c;
                        }
                        m
                    });
                    t.running = false;
                    t.connecting = false;
                    t.stream.clear();
                    t.session_id = sid;
                    t.settle_streams();
                    ctx = std::mem::take(&mut t.accumulator);
                });
                fx.push(Effect::Send(json!({"action": "list_sessions", "tabId": self.active, "cwd": self.active_tab().cwd}), 500));
                fx.push(Effect::Send(json!({"action": "command_options", "tabId": tid, "command": "effort", "input": ""}), 500));
                if ctx.encode_utf16().count() >= 40 {
                    fx.push(Effect::Suggest { tab: tid.clone(), context: ctx });
                }
            }
            "UserMessageChunk" => {
                let chunk = text();
                self.with_tab(&tid, |t, uid| match t.messages.last_mut() {
                    Some(last) if last.role == Role::UserStream => last.text.push_str(&chunk),
                    _ => {
                        t.messages.push(Msg { uid: *uid, role: Role::UserStream, text: chunk, panel: None, tool: None });
                        *uid += 1;
                    }
                });
            }
            "AgentMessageChunk" => {
                let chunk = text();
                self.with_tab(&tid, |t, uid| {
                    t.accumulator.push_str(&chunk);
                    t.stream.push_str(&chunk);
                    for m in &mut t.messages {
                        if m.role == Role::UserStream {
                            m.role = Role::User;
                        }
                    }
                    match t.messages.last_mut() {
                        Some(last) if last.role == Role::AssistantStream => last.text = t.stream.clone(),
                        _ => {
                            t.messages.push(Msg { uid: *uid, role: Role::AssistantStream, text: t.stream.clone(), panel: None, tool: None });
                            *uid += 1;
                        }
                    }
                    if let Some(th) = &mut t.thinking {
                        th.collapsed = true;
                    }
                });
            }
            "Thinking" => {
                let chunk = text();
                self.with_tab(&tid, |t, _| match &mut t.thinking {
                    Some(th) => th.text.push_str(&chunk),
                    None => t.thinking = Some(Thinking { text: chunk, start: Instant::now(), collapsed: false }),
                });
            }
            "ToolCall" => {
                let id = s(data.get("toolCallId")).unwrap_or_default();
                let entry = ToolEntry {
                    id: id.clone(),
                    title: s(data.get("title")).unwrap_or_default(),
                    kind: s(data.get("kind")),
                    content: data.get("content").and_then(Value::as_array).cloned().unwrap_or_default(),
                    status: s_or(data.get("status"), "pending"),
                    expanded: false,
                    raw_input: data.get("rawInput").filter(|v| !v.is_null()).cloned(),
                    raw_output: None,
                    stream_output: String::new(),
                };
                let locs: Vec<ActiveFile> = data
                    .get("locations")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|l| {
                        Some(ActiveFile { path: s(l.get("path"))?, line: l.get("line").and_then(Value::as_u64), kind: s(data.get("kind")) })
                    })
                    .collect();
                self.with_tab(&tid, |t, uid| {
                    for m in &mut t.messages {
                        if m.role == Role::AssistantStream {
                            m.role = Role::Assistant;
                        }
                    }
                    t.stream.clear();
                    if !locs.is_empty() {
                        t.active_files.retain(|f| !locs.iter().any(|l| l.path == f.path));
                        t.active_files.extend(locs);
                    }
                    match t.messages.iter_mut().find(|m| m.tool.as_ref().is_some_and(|x| x.id == id)) {
                        Some(m) => m.tool = Some(entry),
                        None => {
                            t.messages.push(Msg { uid: *uid, role: Role::Tool, text: String::new(), panel: None, tool: Some(entry) });
                            *uid += 1;
                        }
                    }
                });
            }
            "ToolCallUpdate" => {
                let id = s(data.get("toolCallId")).unwrap_or_default();
                let status = s(data.get("status"));
                let raw = data.get("rawOutput").filter(|v| !v.is_null()).cloned();
                self.with_tab(&tid, |t, _| {
                    for m in &mut t.messages {
                        if let Some(tool) = m.tool.as_mut().filter(|x| x.id == id) {
                            if let Some(st) = &status {
                                tool.status = st.clone();
                            }
                            if raw.is_some() {
                                tool.raw_output = raw.clone();
                            }
                        }
                    }
                });
            }
            "ToolCallChunk" => {
                let id = s(data.get("toolCallId")).unwrap_or_default();
                let chunk = data.get("content").and_then(|c| c.get("text")).and_then(Value::as_str).unwrap_or_default().to_string();
                let title = s(data.get("title")).unwrap_or_default();
                let kind = s(data.get("kind"));
                self.with_tab(&tid, |t, uid| {
                    if let Some(tool) = t.messages.iter_mut().filter_map(|m| m.tool.as_mut()).find(|x| x.id == id) {
                        tool.stream_output.push_str(&chunk);
                        return;
                    }
                    for m in &mut t.messages {
                        if m.role == Role::AssistantStream {
                            m.role = Role::Assistant;
                        }
                    }
                    t.stream.clear();
                    let tool = ToolEntry { id, title, kind, status: "pending".into(), expanded: true, ..Default::default() };
                    t.messages.push(Msg { uid: *uid, role: Role::Tool, text: String::new(), panel: None, tool: Some(tool) });
                    *uid += 1;
                });
            }
            "PermissionRequest" => {
                let p = Permission {
                    request_id: data.get("requestId").cloned().unwrap_or(Value::Null),
                    title: s(data.get("title")).unwrap_or_default(),
                    options: data
                        .get("options")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .map(|o| PermOption {
                            id: s(o.get("optionId")).unwrap_or_default(),
                            name: s(o.get("name")).unwrap_or_else(|| s(o.get("optionId")).unwrap_or_default()),
                            kind: s(o.get("kind")).unwrap_or_default(),
                        })
                        .collect(),
                };
                self.with_tab(&tid, |t, _| t.permissions.push(p));
            }
            "TurnEnd" => {
                let stop = s(data.get("stopReason"));
                let mut suggest_ctx = String::new();
                let mut next_prompt = None;
                self.with_tab(&tid, |t, uid| {
                    t.settle_streams();
                    if let Some(g) = &mut t.goal {
                        if g.status == GoalStatus::Active {
                            g.current += 1;
                            if g.current > g.max {
                                g.current = g.max;
                                g.status = GoalStatus::Incomplete;
                            }
                        }
                    }
                    t.thinking = None;
                    t.permissions.clear();
                    t.plan.clear();
                    t.stream.clear();
                    t.last_stop = stop;
                    t.suggestions.clear();
                    if !t.queue.is_empty() {
                        let next = t.queue.remove(0);
                        t.messages.push(Msg { uid: *uid, role: Role::User, text: next.clone(), panel: None, tool: None });
                        *uid += 1;
                        next_prompt = Some(next);
                        return;
                    }
                    if t.accumulator.is_empty() {
                        let recent: Vec<String> = t
                            .messages
                            .iter()
                            .filter(|m| m.role == Role::User || m.role == Role::Assistant)
                            .rev()
                            .take(6)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .map(|m| format!("{}: {}", if m.role == Role::User { "USER" } else { "ASSISTANT" }, m.text.chars().take(500).collect::<String>()))
                            .collect();
                        t.accumulator = recent.join("\n");
                    }
                    if let Some(g) = &mut t.goal {
                        if g.status == GoalStatus::Active {
                            g.status = GoalStatus::Complete;
                        }
                    }
                    t.running = false;
                    suggest_ctx = std::mem::take(&mut t.accumulator);
                });
                if let Some(next) = next_prompt {
                    let skills = self.tab(&tid).map(|t| t.skills.clone()).unwrap_or_default();
                    fx.push(Effect::Send(json!({"action": "prompt", "tabId": tid, "text": with_skills(&skills, &next)}), 0));
                } else {
                    // Suggest only after the last queued prompt; queued turns keep accumulating context.
                    if suggest_ctx.encode_utf16().count() >= 40 {
                        fx.push(Effect::Suggest { tab: tid.clone(), context: suggest_ctx });
                    }
                    fx.push(Effect::Notify("Task completed".into()));
                }
                fx.push(Effect::Send(json!({"action": "list_sessions", "tabId": self.active, "cwd": self.active_tab().cwd}), 0));
            }
            "CommandsAvailable" => {
                self.commands = arr(data.get("commands"))
                    .map(|c| Command { name: s(c.get("name")).unwrap_or_default(), description: s(c.get("description")).unwrap_or_default() })
                    .collect();
                self.mcp_servers = arr(data.get("mcpServers"))
                    .map(|m| McpServer {
                        name: s(m.get("name")).unwrap_or_default(),
                        status: s(m.get("status")).unwrap_or_default(),
                        tool_count: m.get("toolCount").and_then(Value::as_u64),
                    })
                    .collect();
                self.tools = arr(data.get("tools"))
                    .map(|t| Tool {
                        name: s(t.get("name")).unwrap_or_default(),
                        description: s(t.get("description")).unwrap_or_default(),
                        source: s(t.get("source")).unwrap_or_default(),
                    })
                    .collect();
                self.prompts = arr(data.get("prompts"))
                    .map(|p| Prompt {
                        name: s(p.get("name")).unwrap_or_default(),
                        // Skill descriptions arrive with their YAML quotes.
                        description: s(p.get("description")).unwrap_or_default().trim().trim_matches('"').to_string(),
                        source: s(p.get("serverName")).unwrap_or_default(),
                    })
                    .filter(|p| !p.name.is_empty())
                    .collect();
                self.prompts.sort_by(|a, b| a.name.cmp(&b.name));
            }
            "Metadata" => {
                let m = data.get("meteringUsage").filter(|v| v.is_object());
                self.with_tab(&tid, |t, _| {
                    t.metadata.context_pct = num(data.get("contextUsagePercentage")).unwrap_or(0.0);
                    t.metadata.turn_ms = num(data.get("turnDurationMs"));
                    if let Some(m) = m {
                        t.metadata.cumulative.input += num(m.get("inputTokens")).unwrap_or(0.0);
                        t.metadata.cumulative.output += num(m.get("outputTokens")).unwrap_or(0.0);
                        t.metadata.cumulative.cost += num(m.get("cost")).unwrap_or(0.0);
                    }
                });
            }
            "Plan" => {
                let entries = arr(data.get("entries"))
                    .map(|e| PlanEntry {
                        content: s(e.get("content")).unwrap_or_default(),
                        priority: s(e.get("priority")).unwrap_or_default(),
                        status: s(e.get("status")).unwrap_or_default(),
                    })
                    .collect();
                self.with_tab(&tid, |t, _| t.plan = entries);
            }
            "GoalStatus" => {
                let cur = data.get("currentIteration").and_then(Value::as_u64).filter(|n| *n > 0);
                let max = data.get("maxIterations").and_then(Value::as_u64).filter(|n| *n > 0);
                let st = s(data.get("status"));
                self.with_tab(&tid, |t, _| {
                    if let Some(g) = &mut t.goal {
                        g.current = cur.unwrap_or(g.current);
                        g.max = max.unwrap_or(g.max);
                        g.status = match st.as_deref() {
                            Some("active") => GoalStatus::Active,
                            Some("complete") => GoalStatus::Complete,
                            Some("incomplete") => GoalStatus::Incomplete,
                            _ => g.status,
                        };
                    }
                });
            }
            "SessionList" => {
                self.sessions = arr(data.get("sessions"))
                    .map(|x| Session { id: s(x.get("value")).unwrap_or_default(), title: s(x.get("label")).unwrap_or_default() })
                    .collect();
                self.apply_session_titles();
            }
            "SessionListUpdate" => {
                if data.get("sessions").is_some_and(|v| v.is_array()) {
                    self.sessions = arr(data.get("sessions"))
                        .map(|x| Session {
                            id: s(x.get("sessionId")).unwrap_or_default(),
                            title: s(x.get("title")).filter(|t| !t.is_empty()).or_else(|| s(x.get("name"))).unwrap_or_default(),
                        })
                        .collect();
                    self.apply_session_titles();
                }
            }
            "CommandOptions" => {
                let opts: Vec<Value> = arr(data.get("options")).cloned().collect();
                if data.get("command").and_then(Value::as_str) == Some("effort") {
                    self.with_tab(&tid, |t, _| t.effort_supported = !opts.is_empty());
                } else if let Some(panel) = data.get("panel").filter(|p| truthy(p)) {
                    let body = s(panel.get("content")).filter(|x| !x.is_empty()).or_else(|| s(panel.get("text"))).unwrap_or_else(|| panel.to_string());
                    let m = self.msg(Role::Assistant, body);
                    self.with_tab(&tid, |t, _| t.messages.push(m));
                    self.cmd_options = None;
                    self.cmd_hint = None;
                } else {
                    self.cmd_options = (!opts.is_empty()).then(|| {
                        opts.iter()
                            .map(|o| {
                                let value = s(o.get("value")).unwrap_or_default();
                                CmdOption {
                                    name: s(o.get("label")).filter(|x| !x.is_empty()).unwrap_or_else(|| value.clone()),
                                    description: s(o.get("description")).unwrap_or_default(),
                                    value,
                                }
                            })
                            .collect()
                    });
                    self.cmd_options_rev += 1;
                    self.cmd_hint = s(data.get("hint")).filter(|x| !x.is_empty());
                }
            }
            "error" => {
                let message = s(data.get("message")).unwrap_or_default();
                let m = self.msg(Role::System, message.clone());
                self.with_tab(&tid, |t, _| {
                    t.running = false;
                    t.messages.push(m);
                });
                self.toast(message, ToastKind::Error);
            }
            "McpServerInitialized" => {
                // Connections show in the MCP panel; only failures and sign-in prompts get toasts.
                let name = s(data.get("serverName")).unwrap_or_default();
                self.toasted_mcp_failures.retain(|(n, _)| *n != name);
                for srv in self.mcp_servers.iter_mut().filter(|x| x.name == name) {
                    srv.status = "running".into();
                }
                self.oauth_pending.remove(&name);
            }
            "McpServerInitFailure" => {
                let name = s(data.get("serverName")).unwrap_or_default();
                let error = s(data.get("error")).unwrap_or_default();
                let oauth = s(data.get("oauthUrl")).unwrap_or_default();
                let url = if !oauth.is_empty() { oauth } else { find_https(&error).unwrap_or_default() };
                let auth_re = |x: &str| ["oauth", "auth", "login", "authorize"].iter().any(|k| x.to_lowercase().contains(k));
                if !url.is_empty() && auth_re(&format!("{url}{error}")) {
                    self.oauth_pending.insert(name.clone(), url);
                    self.set_mcp_status(&name, "auth_required", true);
                } else if self.oauth_pending.contains_key(&name) {
                    self.toast(format!("MCP \"{name}\" auth complete — retrying connection..."), ToastKind::Info);
                    self.oauth_pending.remove(&name);
                    self.set_mcp_status(&name, "retrying", false);
                    fx.push(Effect::Send(json!({"action": "prompt", "tabId": self.active, "text": "/mcp reconnect"}), 1000));
                } else {
                    if self.toasted_mcp_failures.insert((name.clone(), error.clone())) {
                        self.toast(format!("MCP server \"{name}\" failed: {error}"), ToastKind::Error);
                    }
                    self.set_mcp_status(&name, "failed", false);
                }
            }
            "McpOauthRequest" => {
                if let Some(url) = s(data.get("oauthUrl")).filter(|u| u.starts_with("https://")) {
                    let name = s(data.get("serverName")).unwrap_or_default();
                    self.oauth_pending.insert(name.clone(), url);
                    self.set_mcp_status(&name, "auth_required", true);
                }
            }
            "McpGovernanceDisabled" => {
                let msg = if data.get("apiFailure").is_some_and(truthy) {
                    "MCP disabled: failed to retrieve settings"
                } else {
                    "MCP has been disabled by your administrator"
                };
                self.toast(msg, ToastKind::Warning);
            }
            "CompactionStatus" => match data.pointer("/status/type").and_then(Value::as_str) {
                Some("started") => self.toast("Context compaction started...", ToastKind::Info),
                Some("completed") => self.toast("Context compaction completed", ToastKind::Info),
                Some("error") => {
                    let e = s(data.pointer("/status/error")).unwrap_or_default();
                    self.toast(format!("Compaction error: {e}"), ToastKind::Error)
                }
                _ => {}
            },
            "ClearStatus" => self.with_tab(&tid, |t, _| {
                t.messages.clear();
                t.thinking = None;
            }),
            "AgentSwitched" => {
                let agent = s(data.get("agentName"));
                let welcome = s(data.get("welcomeMessage")).filter(|w| !w.is_empty());
                let wm = welcome.map(|w| self.msg(Role::Assistant, w));
                self.with_tab(&tid, |t, _| {
                    if let (Some(a), Some(m)) = (&agent, &mut t.modes) {
                        m.current = a.clone();
                    }
                    if let Some(w) = wm {
                        t.messages.push(w);
                    }
                });
                self.toast(format!("Switched to {}", agent.unwrap_or_default()), ToastKind::Info);
            }
            "AgentNotFound" => {
                let r = s(data.get("requestedAgent")).unwrap_or_default();
                let f = s(data.get("fallbackAgent")).unwrap_or_default();
                self.toast(format!("Agent \"{r}\" not found, using \"{f}\""), ToastKind::Warning);
            }
            "AgentConfigError" => {
                let e = s(data.get("error")).unwrap_or_default();
                self.toast(format!("Agent config error: {e}"), ToastKind::Error);
            }
            "RateLimitError" => {
                let m = s(data.get("message")).filter(|x| !x.is_empty()).unwrap_or_else(|| "Rate limit exceeded. Please wait.".into());
                self.toast(m, ToastKind::Error);
            }
            "InboxNotification" => {
                let m = s(data.get("message")).filter(|x| !x.is_empty()).unwrap_or_else(|| "New notification".into());
                self.toast(format!("Message from subagent: {m}"), ToastKind::Info);
            }
            "SubagentListUpdate" => {
                let mut list: Vec<Subagent> = arr(data.get("subagents"))
                    .map(|x| Subagent {
                        session_id: s(x.get("sessionId")).unwrap_or_default(),
                        name: s(x.get("name")).unwrap_or_default(),
                        role: s(x.get("role")),
                        status: s_or(x.get("status"), "pending"),
                        loop_iteration: x.get("loopIteration").and_then(Value::as_u64),
                    })
                    .collect();
                list.extend(arr(data.get("pendingStages")).map(|x| Subagent {
                    session_id: s(x.get("name")).filter(|n| !n.is_empty()).or_else(|| s(x.get("sessionId"))).unwrap_or_default(),
                    name: s(x.get("name")).unwrap_or_default(),
                    role: s(x.get("role")),
                    status: "pending".into(),
                    loop_iteration: None,
                }));
                self.with_tab(&tid, |t, _| t.subagents = list);
            }
            "SessionActivity" => {
                let sid = s(data.get("sessionId")).unwrap_or_default();
                let ev = data.get("event");
                let label = ev.and_then(|e| s(e.get("title"))).or_else(|| s(ev)).unwrap_or_default();
                self.with_tab(&tid, |t, _| {
                    t.subagent_activity.insert(sid, label);
                });
            }
            "RetryWarning" => {
                let g = |k: &str| data.get(k).map(|v| v.to_string()).unwrap_or_default();
                self.toast(format!("Retrying ({}/{}) in {}s...", g("attempt"), g("maxAttempts"), g("delaySecs")), ToastKind::Warning);
            }
            "KiroSettingsList" => {
                self.kiro_settings = Some(data.get("settings").and_then(Value::as_object).cloned().unwrap_or_default());
            }
            "KiroSettingsUpdated" => {
                if let (Some(map), Some(k)) = (&mut self.kiro_settings, s(data.get("key"))) {
                    map.insert(k, data.get("value").cloned().unwrap_or(Value::Null));
                }
            }
            "AgentCrash" => {
                self.with_tab(&tid, |t, _| t.running = false);
                self.toast("Agent process crashed — restarting...", ToastKind::Warning);
            }
            "AuthError" => {
                self.with_tab(&tid, |t, _| t.running = false);
                let m = s(data.get("message")).filter(|x| !x.is_empty()).unwrap_or_else(|| "Kiro CLI not authenticated. Run `kiro-cli login` in your terminal.".into());
                self.toast(m, ToastKind::Error);
            }
            "ProtocolLog" => {
                self.logs.push_back(LogLine { dir: s(data.get("dir")).unwrap_or_default(), msg: s(data.get("msg")).unwrap_or_default() });
                while self.logs.len() > 200 {
                    self.logs.pop_front();
                }
            }
            _ => {}
        }
        fx
    }

    fn set_mcp_status(&mut self, name: &str, status: &str, add_if_missing: bool) {
        match self.mcp_servers.iter_mut().find(|x| x.name == name) {
            Some(srv) => srv.status = status.into(),
            None if add_if_missing => self.mcp_servers.push(McpServer { name: name.into(), status: status.into(), tool_count: None }),
            None => {}
        }
    }
}

fn arr(v: Option<&Value>) -> impl Iterator<Item = &Value> {
    v.and_then(Value::as_array).into_iter().flatten()
}

fn find_https(s: &str) -> Option<String> {
    let i = s.find("https://")?;
    Some(s[i..].split_whitespace().next().unwrap_or_default().to_string())
}

fn parse_modes(v: Option<&Value>) -> Option<Modes> {
    let v = v.filter(|v| v.is_object())?;
    Some(Modes {
        current: s(v.get("currentModeId")).unwrap_or_default(),
        available: arr(v.get("availableModes"))
            .map(|m| Mode {
                id: s(m.get("id")).unwrap_or_default(),
                name: s(m.get("name")).unwrap_or_default(),
                description: s(m.get("description")).filter(|d| !d.is_empty()),
            })
            .collect(),
    })
}

fn parse_models(v: Option<&Value>) -> Option<Models> {
    let v = v.filter(|v| v.is_object())?;
    Some(Models {
        current: s(v.get("currentModelId")).unwrap_or_default(),
        available: arr(v.get("availableModels"))
            .map(|m| ModelInfo { id: s(m.get("modelId")).unwrap_or_default(), name: s(m.get("name")).unwrap_or_default() })
            .collect(),
    })
}

/// `/goal [--max N] text` → (text, max iterations).
pub fn parse_goal(text: &str) -> Option<(String, u64)> {
    let rest = text.strip_prefix("/goal")?;
    if !rest.starts_with(char::is_whitespace) || text.trim() == "/goal clear" {
        return None;
    }
    let rest = rest.trim_start();
    if let Some(after) = rest.strip_prefix("--max") {
        let after = after.trim_start();
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        let tail = after[digits.len()..].trim();
        if !digits.is_empty() && after[digits.len()..].starts_with(char::is_whitespace) && !tail.is_empty() {
            return Some((tail.to_string(), digits.parse().unwrap_or(5)));
        }
    }
    let t = rest.trim();
    (!t.is_empty()).then(|| (t.to_string(), 5))
}

/// Files touched by tool calls between two user turns (for the rewind timeline).
pub struct TurnSummary {
    pub tools: usize,
    pub files: Vec<String>,
    pub commands: usize,
}

pub fn turn_summary(messages: &[Msg], from: usize, to: Option<usize>) -> TurnSummary {
    let end = to.unwrap_or(messages.len()).min(messages.len());
    let mut files: Vec<String> = Vec::new();
    let mut add = |p: &str| {
        let name = p.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(p).to_string();
        if !files.contains(&name) {
            files.push(name);
        }
    };
    let mut tools = 0;
    let mut commands = 0;
    for m in messages.get(from + 1..end).unwrap_or_default() {
        let Some(t) = m.tool.as_ref().filter(|_| m.role == Role::Tool) else { continue };
        tools += 1;
        for c in &t.content {
            if let Some(p) = c.get("path").and_then(Value::as_str) {
                add(p);
            }
        }
        if let Some(p) = t.title.split_whitespace().find(|w| w.starts_with('/') || w.starts_with("./") || w.starts_with("~/")) {
            add(p);
        }
        let lower = t.title.to_lowercase();
        if t.kind.as_deref() == Some("shell") || ["bash", "shell", "execute", "run", "npm", "npx", "git", "cd "].iter().any(|p| lower.starts_with(p)) {
            commands += 1;
        }
    }
    TurnSummary { tools, files, commands }
}

pub fn export_markdown(tab: &Tab) -> String {
    let mut lines = vec![format!("# {}\n", tab.name)];
    for m in &tab.messages {
        match m.role {
            Role::User => lines.push(format!("## User\n\n{}\n", m.text)),
            Role::Assistant => lines.push(format!("## Assistant\n\n{}\n", m.text)),
            Role::Tool => {
                if let Some(t) = &m.tool {
                    lines.push(format!("> **{}** — {}\n", t.title, t.status));
                }
            }
            _ => {}
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(v: Value) -> Value {
        v
    }

    #[test]
    fn streaming_turn_flow() {
        let mut m = Model::new();
        m.handle(&ev(json!({"type": "ready", "tabId": "tab-1", "sessionId": "s1", "modes": {"currentModeId": "a", "availableModes": [{"id": "a", "name": "A"}]}})));
        assert!(!m.active_tab().connecting);
        assert_eq!(m.active_tab().session_id.as_deref(), Some("s1"));
        m.active_tab_mut().running = true;
        m.handle(&ev(json!({"type": "Thinking", "tabId": "tab-1", "text": "hm"})));
        m.handle(&ev(json!({"type": "AgentMessageChunk", "tabId": "tab-1", "text": "Hel"})));
        m.handle(&ev(json!({"type": "AgentMessageChunk", "tabId": "tab-1", "text": "lo"})));
        let t = m.active_tab();
        assert_eq!(t.messages.len(), 1);
        assert_eq!(t.messages[0].text, "Hello");
        assert_eq!(t.messages[0].role, Role::AssistantStream);
        assert!(t.thinking.as_ref().unwrap().collapsed);
        m.handle(&ev(json!({"type": "ToolCall", "tabId": "tab-1", "toolCallId": "t1", "title": "Read /a/b.rs", "kind": "read", "locations": [{"path": "/a/b.rs"}]})));
        m.handle(&ev(json!({"type": "ToolCallUpdate", "tabId": "tab-1", "toolCallId": "t1", "status": "completed"})));
        let t = m.active_tab();
        assert_eq!(t.messages[0].role, Role::Assistant, "tool call settles the stream");
        assert_eq!(t.messages[1].tool.as_ref().unwrap().status, "completed");
        assert_eq!(t.active_files.len(), 1);
        let fx = m.handle(&ev(json!({"type": "TurnEnd", "tabId": "tab-1", "stopReason": "end_turn"})));
        let t = m.active_tab();
        assert!(!t.running && t.thinking.is_none());
        assert!(fx.iter().any(|e| matches!(e, Effect::Notify(_))));
        assert!(!fx.iter().any(|e| matches!(e, Effect::Suggest { .. })), "under 40 chars of context: no suggestions");
    }

    #[test]
    fn queue_sends_next_prompt_on_turn_end() {
        let mut m = Model::new();
        m.active_tab_mut().running = true;
        m.active_tab_mut().queue = vec!["next one".into(), "later".into()];
        let fx = m.handle(&json!({"type": "TurnEnd", "tabId": "tab-1", "stopReason": "end_turn"}));
        assert!(fx.contains(&Effect::Send(json!({"action": "prompt", "tabId": "tab-1", "text": "next one"}), 0)));
        let t = m.active_tab();
        assert!(t.running, "still running the queued prompt");
        assert_eq!(t.queue, vec!["later".to_string()]);
        assert_eq!(t.messages.last().unwrap().text, "next one");
    }

    #[test]
    fn ready_reapplies_previous_choices() {
        let mut m = Model::new();
        m.handle(&json!({"type": "ready", "tabId": "tab-1", "sessionId": "s1", "modes": {"currentModeId": "a", "availableModes": []}, "models": {"currentModelId": "m1", "availableModels": []}}));
        m.active_tab_mut().modes.as_mut().unwrap().current = "b".into();
        m.active_tab_mut().perm_policy = "allow-all".into();
        let fx = m.handle(&json!({"type": "ready", "tabId": "tab-1", "sessionId": "s2", "modes": {"currentModeId": "a", "availableModes": []}, "models": {"currentModelId": "m1", "availableModels": []}}));
        assert!(fx.contains(&Effect::Send(json!({"action": "set_mode", "tabId": "tab-1", "modeId": "b"}), 100)));
        assert!(fx.contains(&Effect::Send(json!({"action": "set_permission_policy", "tabId": "tab-1", "policy": "allow-all"}), 100)));
        assert_eq!(m.active_tab().modes.as_ref().unwrap().current, "b");
    }

    #[test]
    fn goal_iterations() {
        let mut m = Model::new();
        m.active_tab_mut().goal = Some(Goal { text: "x".into(), max: 2, current: 1, status: GoalStatus::Active });
        m.active_tab_mut().queue = vec!["again".into()];
        m.handle(&json!({"type": "TurnEnd", "tabId": "tab-1"}));
        assert_eq!(m.active_tab().goal.as_ref().unwrap().current, 2);
        m.handle(&json!({"type": "TurnEnd", "tabId": "tab-1"}));
        let g = m.active_tab().goal.as_ref().unwrap();
        assert_eq!((g.current, g.status), (2, GoalStatus::Incomplete));
    }

    #[test]
    fn skills_parsed_and_prefixed() {
        let mut m = Model::new();
        m.handle(&json!({"type": "CommandsAvailable", "commands": [], "prompts": [
            {"name": "trace_crash", "description": "Trace", "serverName": "rea"},
            {"name": "antislop-code", "description": "\"Comment hygiene\"", "serverName": "skill:config"}
        ]}));
        assert_eq!(m.prompts.len(), 2);
        let skill = m.prompts.iter().find(|p| p.name == "antislop-code").unwrap();
        assert!(skill.is_skill() && skill.description == "Comment hygiene");
        assert!(!m.prompts.iter().find(|p| p.name == "trace_crash").unwrap().is_skill());

        let skills = vec!["antislop-code".to_string(), "antislop-ui".to_string()];
        assert_eq!(with_skills(&skills, "fix it"), "fix it\n\n(Use these skills for this request: antislop-code, antislop-ui.)");
        assert_eq!(with_skills(&skills, "/compact"), "/compact", "slash commands pass through");
        assert_eq!(with_skills(&[], "fix it"), "fix it");

        m.active_tab_mut().skills = skills;
        m.active_tab_mut().queue = vec!["queued".into()];
        let fx = m.handle(&json!({"type": "TurnEnd", "tabId": "tab-1"}));
        assert!(fx.iter().any(|e| matches!(e, Effect::Send(v, 0) if v["text"].as_str().unwrap().starts_with("queued\n\n") && v["text"].as_str().unwrap().contains("antislop-ui"))));
    }

    #[test]
    fn parse_goal_variants() {
        assert_eq!(parse_goal("/goal ship it"), Some(("ship it".into(), 5)));
        assert_eq!(parse_goal("/goal --max 3 ship it"), Some(("ship it".into(), 3)));
        assert_eq!(parse_goal("/goal clear"), None);
        assert_eq!(parse_goal("/goals"), None);
    }

    #[test]
    fn mcp_oauth_and_failure() {
        let mut m = Model::new();
        m.handle(&json!({"type": "McpServerInitFailure", "serverName": "gh", "error": "needs auth at https://auth.example.com/login now"}));
        assert_eq!(m.oauth_pending.get("gh").map(String::as_str), Some("https://auth.example.com/login"));
        assert_eq!(m.mcp_servers[0].status, "auth_required");
        let fx = m.handle(&json!({"type": "McpServerInitFailure", "serverName": "gh", "error": "boom"}));
        assert!(fx.iter().any(|e| matches!(e, Effect::Send(v, 1000) if v["text"] == "/mcp reconnect")));
        m.handle(&json!({"type": "McpServerInitFailure", "serverName": "x", "error": "No such file"}));
        m.handle(&json!({"type": "McpServerInitFailure", "serverName": "x", "error": "No such file"}));
        assert_eq!(m.toasts.iter().filter(|t| t.kind == ToastKind::Error && t.text.contains("\"x\" failed")).count(), 1, "repeated failure is toasted once");
        let before = m.toasts.len();
        m.handle(&json!({"type": "McpServerInitialized", "serverName": "y"}));
        assert_eq!(m.toasts.len(), before, "connections don't toast");
    }

    #[test]
    fn session_titles_rename_tabs() {
        let mut m = Model::new();
        m.handle(&json!({"type": "ready", "tabId": "tab-1", "sessionId": "s1"}));
        m.handle(&json!({"type": "SessionList", "sessions": [{"value": "s1", "label": "Fix the bug"}, {"value": "s2", "label": "Other"}]}));
        assert_eq!(m.active_tab().name, "Fix the bug");
        assert_eq!(m.sessions.len(), 2);
    }

    #[test]
    fn rewind_summary_and_export() {
        let mut m = Model::new();
        let u = m.msg(Role::User, "edit");
        let mut t1 = m.msg(Role::Tool, "");
        t1.tool = Some(ToolEntry { title: "Write".into(), content: vec![json!({"type": "diff", "path": "/src/App.tsx"})], ..Default::default() });
        let mut t2 = m.msg(Role::Tool, "");
        t2.tool = Some(ToolEntry { title: "git status".into(), kind: Some("shell".into()), ..Default::default() });
        let a = m.msg(Role::Assistant, "done");
        let msgs = vec![u, t1, t2, a];
        let s = turn_summary(&msgs, 0, None);
        assert_eq!((s.tools, s.commands), (2, 1));
        assert_eq!(s.files, vec!["App.tsx".to_string()]);
        let mut tab = Tab::new("t", "Chat");
        tab.messages = msgs;
        let md = export_markdown(&tab);
        assert!(md.starts_with("# Chat\n") && md.contains("## User\n\nedit") && md.contains("> **Write**"));
    }
}
