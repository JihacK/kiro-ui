//! One `Hub` per UI client (= one window). It owns the window's tabs, each
//! mapped to an ACP session, and routes traffic between the UI and the agent.
//!
//! The UI protocol is JSON both ways: the UI sends `{"action": ...}` messages
//! via [`Client::send`] and receives `{"type": ...}` events through the emit
//! callback given to [`Client::start`].
//!
//! Agent modes:
//! - shared (default): all tabs of the window are sessions on ONE agent
//!   process; notifications are routed by `sessionId`.
//! - per-tab: each tab owns its own agent process.

use crate::agent::{self, Agent};
use crate::notifications;
use crate::permissions;
use crate::prompt;
use crate::rpc::{ConnEvent, Incoming, RpcError};
use crate::{truthy, AppState};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

const INIT_TAB: &str = "tab-1";
const SHARED_KEY: &str = "\u{0}shared";
const TERMINATE_GRACE: Duration = Duration::from_millis(200);

struct Tab {
    id: String,
    agent: Arc<Agent>,
    session_id: String,
    modes: Option<Value>,
    models: Option<Value>,
    /// Working directory requested for this tab (None = workspace default).
    cwd: Option<String>,
    perm_policy: String,
    /// Pending permission prompts: toolCallId → JSON-RPC request id.
    perms: HashMap<String, Value>,
    running: u32,
    last_prompt: u64,
    /// Received a non-empty subagent list (so an empty one should clear it).
    has_pipeline: bool,
}

/// A freshly created session, not yet installed as a tab.
struct Seed {
    agent: Arc<Agent>,
    session_id: String,
    modes: Option<Value>,
    models: Option<Value>,
    cwd: Option<String>,
}

#[derive(Default)]
struct Inner {
    /// Insertion-ordered (the "first tab" is used as a fallback for global requests).
    tabs: Vec<Tab>,
    agents: HashMap<u64, Arc<Agent>>,
    /// Shared mode routing: sessionId (incl. subagent sessions) → tabId.
    session_tab: HashMap<String, String>,
    /// Shared mode: in-flight `session/new` per agent, and notifications for
    /// not-yet-known sessions held back until those complete.
    creating: HashMap<u64, usize>,
    buffered: HashMap<u64, Vec<Incoming>>,
    current_shared: Option<u64>,
    prompt_seq: u64,
    restarts: HashMap<String, (u32, Instant)>,
    msg_times: VecDeque<Instant>,
}

impl Inner {
    fn tab(&self, id: &str) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }
    fn tab_mut(&mut self, id: &str) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }
    fn remove_tab(&mut self, id: &str) -> Option<Tab> {
        let i = self.tabs.iter().position(|t| t.id == id)?;
        Some(self.tabs.remove(i))
    }
}

enum Route {
    Tab(String),
    /// Same notification to several tabs (process-wide state).
    Tabs(Vec<String>),
    Buffer,
    Drop,
}

pub type Emit = Box<dyn Fn(Value) + Send + Sync>;

struct Hub {
    app: Arc<AppState>,
    out: Emit,
    events: mpsc::UnboundedSender<ConnEvent>,
    debug: Arc<AtomicBool>,
    closed: AtomicBool,
    /// Becomes true once the first tab's session exists; actions wait for it.
    init_done: watch::Receiver<bool>,
    shared_slot: tokio::sync::Mutex<Option<(String, Arc<Agent>)>>,
    inner: Mutex<Inner>,
}

/// Handle to one UI client's hub. Dropping it (or calling [`Client::close`]) tears down its sessions.
///
/// `send`/`close` may be called from any thread (e.g. a GUI thread): they enter
/// the runtime the client was started on.
pub struct Client {
    hub: Arc<Hub>,
    event_loop: tokio::task::AbortHandle,
    rt: tokio::runtime::Handle,
}

impl Client {
    /// Start a hub on the current tokio runtime and create the first tab (`tab-1`).
    /// `emit` receives every UI event; it must not block.
    pub fn start(app: Arc<AppState>, emit: impl Fn(Value) + Send + Sync + 'static) -> Client {
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<ConnEvent>();
        let (init_tx, init_rx) = watch::channel(false);
        let hub = Arc::new(Hub {
            app,
            out: Box::new(emit),
            events: ev_tx,
            debug: Arc::new(AtomicBool::new(false)),
            closed: AtomicBool::new(false),
            init_done: init_rx,
            shared_slot: tokio::sync::Mutex::new(None),
            inner: Mutex::new(Inner::default()),
        });
        let event_hub = hub.clone();
        let event_loop = tokio::spawn(async move {
            while let Some(ev) = ev_rx.recv().await {
                event_hub.on_event(ev);
            }
        })
        .abort_handle();

        let init_hub = hub.clone();
        tokio::spawn(async move {
            match init_hub.create_session(INIT_TAB, None, None).await {
                Ok(seed) => init_hub.install(INIT_TAB, seed, true),
                Err(e) => init_hub.emit(json!({"type": "error", "tabId": INIT_TAB, "message": format!("Failed to initialize: {e}")})),
            }
            let _ = init_tx.send(true);
        });
        Client { hub, event_loop, rt: tokio::runtime::Handle::current() }
    }

    /// Send a UI action (`{"action": "prompt", "tabId": ..., ...}`). Non-blocking.
    pub fn send(&self, msg: Value) {
        let _rt = self.rt.enter();
        self.hub.on_client_message(msg);
    }

    /// Tear down all tabs and their agent sessions/processes.
    pub fn close(&self) {
        let _rt = self.rt.enter();
        self.hub.close();
        self.event_loop.abort();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.close();
    }
}

fn str_of(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Key used for maps indexed by a JSON id (strings as-is, others serialised).
fn key_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => "undefined".into(),
    }
}

fn put(m: &mut Map<String, Value>, key: &str, v: Option<&Value>) {
    if let Some(v) = v {
        m.insert(key.to_string(), v.clone());
    }
}

fn js_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

impl Hub {
    fn emit(&self, v: Value) {
        (self.out)(v);
    }

    fn emit_error(&self, tab: Option<&str>, message: impl Into<String>) {
        let mut m = Map::new();
        m.insert("type".into(), json!("error"));
        if let Some(t) = tab {
            m.insert("tabId".into(), json!(t));
        }
        m.insert("message".into(), json!(message.into()));
        self.emit(Value::Object(m));
    }

    fn emit_ready(&self, tab: &Tab) {
        let mut m = Map::new();
        m.insert("type".into(), json!("ready"));
        m.insert("tabId".into(), json!(tab.id));
        m.insert("sessionId".into(), json!(tab.session_id));
        put(&mut m, "modes", tab.modes.as_ref());
        put(&mut m, "models", tab.models.as_ref());
        self.emit(Value::Object(m));
    }

    // ---------------------------------------------------------------- agents

    async fn new_agent(self: &Arc<Self>, shared: bool, owner: Option<&str>) -> Result<Arc<Agent>, RpcError> {
        let agent = agent::spawn(&self.app, self.events.clone(), self.debug.clone(), shared, owner.map(str::to_owned)).await?;
        self.inner.lock().unwrap().agents.insert(agent.id, agent.clone());
        if let Err(e) = agent.initialize().await {
            self.inner.lock().unwrap().agents.remove(&agent.id);
            return Err(e);
        }
        Ok(agent)
    }

    /// The window's shared agent, (re)spawned when missing, dead, or its launch settings changed.
    async fn shared_agent(self: &Arc<Self>) -> Result<Arc<Agent>, RpcError> {
        let sig = agent::transport_signature(&self.app);
        let mut slot = self.shared_slot.lock().await;
        if let Some((s, a)) = slot.as_ref() {
            if *s == sig && !a.is_dead() {
                return Ok(a.clone());
            }
        }
        let a = self.new_agent(true, None).await?;
        let previous = slot.replace((sig, a.clone()));
        {
            let mut inner = self.inner.lock().unwrap();
            inner.current_shared = Some(a.id);
            // A replaced (settings changed) agent with no tabs left can go now.
            if let Some((_, old)) = previous {
                if !old.is_dead() && !inner.tabs.iter().any(|t| t.agent.id == old.id) {
                    old.terminate_later(TERMINATE_GRACE);
                }
            }
        }
        Ok(a)
    }

    async fn create_session(self: &Arc<Self>, tab_id: &str, load: Option<String>, cwd: Option<String>) -> Result<Seed, RpcError> {
        let shared = self.app.share_agent_process();
        let agent = if shared { self.shared_agent().await? } else { self.new_agent(false, Some(tab_id)).await? };
        let session_cwd = cwd.as_deref().map(crate::expand_home).unwrap_or_else(|| self.app.workspace());

        if shared {
            let mut inner = self.inner.lock().unwrap();
            if let Some(sid) = &load {
                // One process can't sensibly attach a session to two tabs (per-tab agents refuse it too).
                if inner.tabs.iter().any(|t| t.session_id == *sid && t.id != tab_id && t.agent.id == agent.id && !agent.is_dead()) {
                    return Err(RpcError::local("Session is already open in another tab"));
                }
                // session/load replays history before responding: route it to the tab right away.
                inner.session_tab.insert(sid.clone(), tab_id.to_string());
            }
            *inner.creating.entry(agent.id).or_default() += 1;
        }

        let res = match &load {
            Some(sid) => agent.conn.request("session/load", json!({"sessionId": sid, "cwd": session_cwd, "mcpServers": []})).await,
            None => agent.conn.request("session/new", json!({"cwd": session_cwd, "mcpServers": []})).await,
        };

        let session_id = match (&res, &load) {
            (_, Some(sid)) => sid.clone(),
            (Ok(r), None) => key_of(r.get("sessionId")),
            (Err(_), None) => String::new(),
        };
        if shared {
            let mut inner = self.inner.lock().unwrap();
            if res.is_ok() {
                inner.session_tab.insert(session_id.clone(), tab_id.to_string());
            } else if load.is_some() && inner.session_tab.get(&session_id).is_some_and(|t| t == tab_id) {
                inner.session_tab.remove(&session_id);
            }
            self.finish_creating(&mut inner, &agent);
        }

        match res {
            Ok(r) => Ok(Seed {
                agent,
                session_id,
                modes: r.get("modes").cloned(),
                models: r.get("models").cloned(),
                cwd,
            }),
            Err(e) => {
                if !shared {
                    agent.terminate_later(Duration::ZERO);
                }
                Err(e)
            }
        }
    }

    fn finish_creating(&self, inner: &mut Inner, agent: &Arc<Agent>) {
        let n = inner.creating.entry(agent.id).or_default();
        *n = n.saturating_sub(1);
        if *n == 0 {
            inner.creating.remove(&agent.id);
            for msg in inner.buffered.remove(&agent.id).unwrap_or_default() {
                self.dispatch(inner, agent, msg);
            }
        }
    }

    /// Install `seed` as tab `tab_id` (replacing in place to keep tab order).
    fn install(&self, tab_id: &str, seed: Seed, emit_ready: bool) {
        if self.closed.load(Ordering::SeqCst) {
            self.discard_seed(seed);
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        let tab = Tab {
            id: tab_id.to_string(),
            agent: seed.agent,
            session_id: seed.session_id,
            modes: seed.modes,
            models: seed.models,
            cwd: seed.cwd,
            perm_policy: "ask".into(),
            perms: HashMap::new(),
            running: 0,
            last_prompt: 0,
            has_pipeline: false,
        };
        if emit_ready {
            self.emit_ready(&tab);
        }
        match inner.tabs.iter().position(|t| t.id == tab_id) {
            Some(i) => {
                let old = std::mem::replace(&mut inner.tabs[i], tab);
                inner.tabs[i].perm_policy = old.perm_policy.clone();
                if old.agent.id != inner.tabs[i].agent.id || old.session_id != inner.tabs[i].session_id {
                    self.release_tab(&mut inner, old);
                }
            }
            None => inner.tabs.push(tab),
        }
    }

    fn discard_seed(&self, seed: Seed) {
        let conn = seed.agent.conn.clone();
        let sid = seed.session_id;
        tokio::spawn(async move {
            let _ = conn.request_timeout("_kiro.dev/session/terminate", json!({"sessionId": sid}), Duration::from_secs(5)).await;
        });
        if !seed.agent.shared {
            seed.agent.terminate_later(TERMINATE_GRACE);
        }
    }

    /// Tear down a tab already removed from `inner.tabs`.
    fn release_tab(&self, inner: &mut Inner, tab: Tab) {
        for (_, id) in tab.perms {
            tab.agent.conn.respond(id, json!({"outcome": {"outcome": "cancelled"}}));
        }
        // Drop routes to this tab, unless a replacement tab with the same id is live.
        let replaced = inner.tabs.iter().any(|x| x.id == tab.id);
        if !replaced {
            inner.session_tab.retain(|_, t| *t != tab.id);
        }
        inner.session_tab.remove(&tab.session_id);
        if tab.agent.is_dead() {
            return;
        }
        let conn = tab.agent.conn.clone();
        let sid = tab.session_id.clone();
        tokio::spawn(async move {
            let _ = conn.request_timeout("_kiro.dev/session/terminate", json!({"sessionId": sid}), Duration::from_secs(5)).await;
        });
        let in_use = inner.tabs.iter().any(|t| t.agent.id == tab.agent.id);
        if !tab.agent.shared || (!in_use && inner.current_shared != Some(tab.agent.id)) {
            tab.agent.terminate_later(TERMINATE_GRACE);
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let mut inner = self.inner.lock().unwrap();
        inner.current_shared = None;
        for tab in std::mem::take(&mut inner.tabs) {
            self.release_tab(&mut inner, tab);
        }
        for a in inner.agents.values() {
            a.terminate_later(TERMINATE_GRACE);
        }
    }

    // ---------------------------------------------------------- agent events

    fn on_event(self: &Arc<Self>, ev: ConnEvent) {
        match ev {
            ConnEvent::Raw(aid, dir, line) => {
                if !self.debug.load(Ordering::Relaxed) {
                    return;
                }
                let tab = {
                    let mut inner = self.inner.lock().unwrap();
                    let Some(agent) = inner.agents.get(&aid).cloned() else { return };
                    self.raw_route(&mut inner, &agent, &line)
                };
                if let Some(t) = tab {
                    self.emit(json!({"type": "ProtocolLog", "tabId": t, "dir": dir.as_str(), "msg": line}));
                }
            }
            ConnEvent::Message(aid, msg) => {
                let mut inner = self.inner.lock().unwrap();
                if let Some(agent) = inner.agents.get(&aid).cloned() {
                    self.dispatch(&mut inner, &agent, msg);
                }
            }
            ConnEvent::Closed(aid) => {
                let is_process = self.inner.lock().unwrap().agents.get(&aid).is_some_and(|a| a.is_process);
                // Processes report their exit code separately.
                if !is_process {
                    self.handle_crash(aid, None);
                }
            }
            ConnEvent::Exited(aid, code) => self.handle_crash(aid, code),
            ConnEvent::AuthError(aid) => {
                let inner = self.inner.lock().unwrap();
                let Some(agent) = inner.agents.get(&aid) else { return };
                if !agent.mark_auth_error() {
                    return;
                }
                let mut tabs: Vec<String> = match &agent.owner_tab {
                    Some(t) => vec![t.clone()],
                    None => inner.tabs.iter().filter(|t| t.agent.id == aid).map(|t| t.id.clone()).collect(),
                };
                if tabs.is_empty() {
                    tabs.push(INIT_TAB.into());
                }
                for t in tabs {
                    self.emit(json!({"type": "AuthError", "tabId": t, "message": "Kiro CLI not authenticated. Run `kiro-cli login` in your terminal."}));
                }
            }
        }
    }

    /// Fallback target when a message can't be attributed by sessionId:
    /// the tab most recently prompted (preferring ones with a running turn).
    fn heuristic_tab(&self, inner: &Inner, agent: &Agent) -> Option<String> {
        inner
            .tabs
            .iter()
            .filter(|t| t.agent.id == agent.id)
            .max_by_key(|t| (t.running > 0, t.last_prompt))
            .map(|t| t.id.clone())
    }

    fn route(&self, inner: &mut Inner, agent: &Agent, method: &str, params: &Value) -> Route {
        if !agent.shared {
            return agent.owner_tab.clone().map_or(Route::Drop, Route::Tab);
        }
        if method == "_kiro.dev/subagent/list_update" {
            // No sessionId on this one: attribute it via known subagent sessions, else heuristically.
            let subs: Vec<String> = params
                .get("subagents")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|s| str_of(s.get("sessionId")))
                .collect();
            let empty = [params.get("subagents"), params.get("pendingStages")]
                .iter()
                .all(|v| v.and_then(Value::as_array).is_none_or(|a| a.is_empty()));
            if empty {
                // kiro-cli emits an empty list around every session open; it only matters
                // to tabs currently showing a pipeline.
                if inner.creating.get(&agent.id).copied().unwrap_or(0) > 0 {
                    return Route::Drop;
                }
                let tabs: Vec<String> = inner
                    .tabs
                    .iter_mut()
                    .filter(|t| t.agent.id == agent.id && t.has_pipeline)
                    .map(|t| {
                        t.has_pipeline = false;
                        t.id.clone()
                    })
                    .collect();
                return Route::Tabs(tabs);
            }
            let tab = subs.iter().find_map(|s| inner.session_tab.get(s).cloned()).or_else(|| self.heuristic_tab(inner, agent));
            let Some(tab) = tab else { return Route::Drop };
            for s in subs {
                inner.session_tab.entry(s).or_insert_with(|| tab.clone());
            }
            if let Some(t) = inner.tab_mut(&tab) {
                t.has_pipeline = true;
            }
            return Route::Tab(tab);
        }
        if let Some(sid) = str_of(params.get("sessionId")) {
            if let Some(t) = inner.session_tab.get(&sid) {
                return Route::Tab(t.clone());
            }
            if inner.creating.get(&agent.id).copied().unwrap_or(0) > 0 {
                return Route::Buffer;
            }
        }
        self.heuristic_tab(inner, agent).map_or(Route::Drop, Route::Tab)
    }

    fn raw_route(&self, inner: &mut Inner, agent: &Agent, line: &str) -> Option<String> {
        if !agent.shared {
            return agent.owner_tab.clone();
        }
        let sid = serde_json::from_str::<Value>(line).ok().and_then(|v| str_of(v.get("params").and_then(|p| p.get("sessionId"))));
        sid.and_then(|s| inner.session_tab.get(&s).cloned())
            .or_else(|| self.heuristic_tab(inner, agent))
            .or_else(|| inner.tabs.first().map(|t| t.id.clone()))
    }

    fn dispatch(&self, inner: &mut Inner, agent: &Arc<Agent>, msg: Incoming) {
        match msg {
            Incoming::Notification { method, params } => match self.route(inner, agent, &method, &params) {
                Route::Tab(t) => self.deliver(&t, &method, &params),
                Route::Tabs(ts) => ts.iter().for_each(|t| self.deliver(t, &method, &params)),
                Route::Buffer => inner.buffered.entry(agent.id).or_default().push(Incoming::Notification { method, params }),
                Route::Drop => {}
            },
            Incoming::Request { id, method, params } => match method.as_str() {
                "session/request_permission" => match self.route(inner, agent, &method, &params) {
                    Route::Tab(t) => self.permission_request(inner, agent, &t, id, &params),
                    Route::Buffer => inner.buffered.entry(agent.id).or_default().push(Incoming::Request { id, method, params }),
                    Route::Tabs(_) | Route::Drop => agent.conn.respond(id, json!({"outcome": {"outcome": "cancelled"}})),
                },
                // Advertised capabilities the UI doesn't implement: acknowledge like the original client.
                "fs/write_text_file" | "terminal/release" | "terminal/kill" => agent.conn.respond(id, json!({})),
                _ => agent.conn.respond_error(id, -32601, "Method not found", Some(json!({"method": method}))),
            },
        }
    }

    fn deliver(&self, tab: &str, method: &str, params: &Value) {
        match method {
            "session/update" => {
                if let Some(ev) = notifications::session_update(tab, params) {
                    self.emit(ev);
                }
            }
            "elicitation/complete" => {}
            _ => {
                self.emit(json!({"type": "ProtocolLog", "tabId": tab, "dir": "in", "msg": format!("[ext] {method}")}));
                if let Some(ev) = notifications::ext_notification(tab, method, params) {
                    self.emit(ev);
                }
            }
        }
    }

    fn permission_request(&self, inner: &mut Inner, agent: &Arc<Agent>, tab_id: &str, id: Value, params: &Value) {
        let Some(tab) = inner.tab_mut(tab_id).filter(|t| t.agent.id == agent.id) else {
            agent.conn.respond(id, json!({"outcome": {"outcome": "cancelled"}}));
            return;
        };
        let tool = params.get("toolCall").cloned().unwrap_or(Value::Null);
        let title = tool.get("title").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!(""));
        let title_str = title.as_str().unwrap_or_default();
        let kind = tool.get("kind").and_then(Value::as_str);
        let options = params.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
        let trust = self.app.trust_match(title_str);
        if let Some(option_id) = permissions::resolve(trust, kind, &options, &tab.perm_policy) {
            agent.conn.respond(id, json!({"outcome": {"outcome": "selected", "optionId": option_id}}));
            return;
        }
        let mut m = Map::new();
        m.insert("type".into(), json!("PermissionRequest"));
        m.insert("tabId".into(), json!(tab_id));
        put(&mut m, "requestId", tool.get("toolCallId"));
        m.insert("title".into(), title);
        put(&mut m, "options", params.get("options"));
        tab.perms.insert(key_of(tool.get("toolCallId")), id);
        self.emit(Value::Object(m));
    }

    fn handle_crash(self: &Arc<Self>, aid: u64, code: Option<i32>) {
        let mut inner = self.inner.lock().unwrap();
        let Some(agent) = inner.agents.remove(&aid) else { return };
        inner.creating.remove(&aid);
        inner.buffered.remove(&aid);
        if inner.current_shared == Some(aid) {
            inner.current_shared = None;
        }
        if self.closed.load(Ordering::SeqCst) || self.app.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        let mut affected = Vec::new();
        for t in inner.tabs.iter_mut().filter(|t| t.agent.id == aid) {
            t.perms.clear();
            t.running = 0;
            affected.push(t.id.clone());
        }
        if affected.is_empty() {
            return;
        }
        for t in &affected {
            self.emit(json!({"type": "AgentCrash", "tabId": t, "code": code}));
        }
        if agent.auth_error.load(Ordering::SeqCst) {
            return;
        }
        let key = if agent.shared { SHARED_KEY.to_string() } else { affected[0].clone() };
        let now = Instant::now();
        let entry = inner.restarts.entry(key).or_insert((0, now));
        if now.duration_since(entry.1) > Duration::from_secs(60) {
            *entry = (0, now);
        }
        entry.0 += 1;
        if entry.0 > 5 {
            for t in &affected {
                self.emit_error(Some(t), "Agent crashed too many times. Please restart manually.");
            }
            return;
        }
        drop(inner);

        let hub = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            for tab_id in affected {
                if hub.closed.load(Ordering::SeqCst) {
                    return;
                }
                // Skip tabs closed or already re-created (e.g. by a prompt) meanwhile.
                let cwd = {
                    let inner = hub.inner.lock().unwrap();
                    match inner.tab(&tab_id) {
                        Some(t) if t.agent.id == aid => t.cwd.clone(),
                        _ => continue,
                    }
                };
                match hub.create_session(&tab_id, None, cwd).await {
                    Ok(seed) => hub.install(&tab_id, seed, true),
                    Err(e) => hub.emit_error(Some(&tab_id), format!("Failed to restart agent: {e}")),
                }
            }
        });
    }

    // -------------------------------------------------------- client actions

    fn on_client_message(self: &Arc<Self>, msg: Value) {
        {
            let mut inner = self.inner.lock().unwrap();
            let now = Instant::now();
            inner.msg_times.push_back(now);
            while inner.msg_times.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60)) {
                inner.msg_times.pop_front();
            }
            if inner.msg_times.len() as f64 > self.app.max_msgs_per_min() {
                drop(inner);
                self.emit_error(Some(INIT_TAB), "Rate limit exceeded. Please slow down.");
                return;
            }
        }
        let hub = self.clone();
        // Actions run concurrently (a long `prompt` must not block `cancel`).
        tokio::spawn(async move {
            // Actions sent while the first tab is still starting wait for it.
            let mut init = hub.init_done.clone();
            let _ = init.wait_for(|done| *done).await;
            let action = msg.get("action").and_then(Value::as_str).unwrap_or_default().to_string();
            let tab_id = str_of(msg.get("tabId")).unwrap_or_else(|| INIT_TAB.into());
            if let Err(e) = hub.run_action(&action, &tab_id, &msg).await {
                eprintln!("Error in action={action} tabId={tab_id}: {} {} {}", e.message, e.code.map(|c| c.to_string()).unwrap_or_default(), e.data.as_ref().map(Value::to_string).unwrap_or_default());
                if action == "prompt" {
                    hub.emit(json!({"type": "TurnEnd", "tabId": tab_id, "stopReason": "error"}));
                }
                hub.emit_error(Some(&tab_id), e.message);
            }
        });
    }

    /// (agent, sessionId) of a live tab.
    fn session_of(&self, tab_id: &str) -> Option<(Arc<Agent>, String)> {
        let inner = self.inner.lock().unwrap();
        inner.tab(tab_id).filter(|t| !t.agent.is_dead()).map(|t| (t.agent.clone(), t.session_id.clone()))
    }

    /// Like `session_of`, falling back to the first tab (for window-global requests).
    fn any_session(&self, tab_id: &str) -> Option<(Arc<Agent>, String)> {
        let inner = self.inner.lock().unwrap();
        inner
            .tab(tab_id)
            .or_else(|| inner.tabs.first())
            .filter(|t| !t.agent.is_dead())
            .map(|t| (t.agent.clone(), t.session_id.clone()))
    }

    async fn get_or_create(self: &Arc<Self>, tab_id: &str) -> Result<(Arc<Agent>, String), RpcError> {
        if let Some(s) = self.session_of(tab_id) {
            return Ok(s);
        }
        let cwd = self.inner.lock().unwrap().tab(tab_id).and_then(|t| t.cwd.clone());
        let seed = self.create_session(tab_id, None, cwd).await?;
        let s = (seed.agent.clone(), seed.session_id.clone());
        self.install(tab_id, seed, true);
        Ok(s)
    }

    fn take_tab(&self, tab_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(tab) = inner.remove_tab(tab_id) {
            self.release_tab(&mut inner, tab);
        }
    }

    async fn run_action(self: &Arc<Self>, action: &str, tab_id: &str, msg: &Value) -> Result<(), RpcError> {
        let cwd = str_of(msg.get("cwd"));
        match action {
            "new_tab" => {
                let max = self.app.max_tabs();
                if self.inner.lock().unwrap().tabs.len() as f64 >= max {
                    self.emit_error(str_of(msg.get("tabId")).as_deref(), format!("Max tabs ({}) reached", js_num(max)));
                    return Ok(());
                }
                let seed = self.create_session(tab_id, None, cwd).await?;
                self.install(tab_id, seed, true);
            }
            "close_tab" => self.take_tab(tab_id),
            "prompt" => {
                let (agent, sid) = self.get_or_create(tab_id).await?;
                {
                    let mut inner = self.inner.lock().unwrap();
                    inner.prompt_seq += 1;
                    let seq = inner.prompt_seq;
                    if let Some(t) = inner.tab_mut(tab_id) {
                        t.running += 1;
                        t.last_prompt = seq;
                    }
                }
                let res = agent.conn.request("session/prompt", json!({"sessionId": sid, "prompt": prompt::build(msg)})).await;
                if let Some(t) = self.inner.lock().unwrap().tab_mut(tab_id).filter(|t| t.session_id == sid) {
                    t.running = t.running.saturating_sub(1);
                }
                let result = res?;
                let mut m = Map::new();
                m.insert("type".into(), json!("TurnEnd"));
                m.insert("tabId".into(), json!(tab_id));
                put(&mut m, "stopReason", result.get("stopReason"));
                self.emit(Value::Object(m));
            }
            "cancel" => {
                let target = {
                    let mut inner = self.inner.lock().unwrap();
                    inner.tab_mut(tab_id).map(|t| {
                        for (_, id) in t.perms.drain() {
                            t.agent.conn.respond(id, json!({"outcome": {"outcome": "cancelled"}}));
                        }
                        (t.agent.clone(), t.session_id.clone())
                    })
                };
                if let Some((agent, sid)) = target {
                    agent.conn.notify("session/cancel", json!({"sessionId": sid}));
                }
            }
            "set_mode" | "set_model" => {
                let target = self.inner.lock().unwrap().tab(tab_id).map(|t| (t.agent.clone(), t.session_id.clone()));
                if let Some((agent, sid)) = target {
                    let (method, field) = if action == "set_mode" { ("session/set_mode", "modeId") } else { ("session/set_model", "modelId") };
                    let mut p = json!({"sessionId": sid});
                    put(p.as_object_mut().unwrap(), field, msg.get(field));
                    agent.conn.request(method, p).await?;
                }
            }
            "set_permission_policy" => {
                if let Some(t) = self.inner.lock().unwrap().tab_mut(tab_id) {
                    t.perm_policy = msg.get("policy").and_then(Value::as_str).unwrap_or("ask").to_string();
                }
            }
            "permission_response" => {
                let resolved = {
                    let mut inner = self.inner.lock().unwrap();
                    inner.tab_mut(tab_id).and_then(|t| {
                        let id = t.perms.remove(&key_of(msg.get("requestId")))?;
                        let mut choice = json!({"outcome": "selected"});
                        put(choice.as_object_mut().unwrap(), "optionId", msg.get("optionId"));
                        t.agent.conn.respond(id, json!({"outcome": choice}));
                        Some(())
                    })
                };
                let option = msg.get("optionId").and_then(Value::as_str);
                let title = msg.get("title").filter(|v| truthy(v)).map(|v| key_of(Some(v)));
                if let (Some(()), Some(opt @ ("allow_always" | "reject_always")), Some(title)) = (resolved, option, title) {
                    self.app.set_trust_rule(&title, opt).await;
                }
            }
            "new_chat" => {
                self.take_tab(tab_id);
                let seed = self.create_session(tab_id, None, cwd).await?;
                self.install(tab_id, seed, true);
            }
            "load_session" => {
                self.take_tab(tab_id);
                let sid = str_of(msg.get("sessionId"));
                match self.create_session(tab_id, sid.clone(), None).await {
                    Ok(seed) => self.install(tab_id, seed, false),
                    Err(e) => {
                        eprintln!("[load_session] sessionId={}: {}", sid.as_deref().unwrap_or("undefined"), e.message);
                        match self.create_session(tab_id, None, None).await {
                            Ok(seed) => self.install(tab_id, seed, false),
                            Err(_) => return Ok(()),
                        }
                        let detail = match (&e.code, &e.data) {
                            (Some(code), _) => format!("{} ({code})", e.message),
                            (None, Some(data)) => format!("{}: {data}", e.message),
                            _ => e.message.clone(),
                        };
                        let short: String = sid.as_deref().unwrap_or("undefined").chars().take(8).collect();
                        self.emit_error(Some(tab_id), format!("Failed to load session {short}: {detail}"));
                    }
                }
                let inner = self.inner.lock().unwrap();
                if let Some(t) = inner.tab(tab_id) {
                    self.emit_ready(t);
                }
            }
            "list_sessions" => {
                if let Some((agent, _)) = self.any_session(tab_id) {
                    let list_cwd = cwd.unwrap_or_else(|| self.app.workspace());
                    let sessions = match agent.conn.request("_kiro.dev/session/list", json!({"cwd": list_cwd})).await {
                        Ok(res) => res
                            .get("sessions")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .map(|x| {
                                let mut m = Map::new();
                                put(&mut m, "value", x.get("sessionId"));
                                m.insert("label".into(), x.get("title").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!("")));
                                put(&mut m, "updatedAt", x.get("updatedAt"));
                                put(&mut m, "messageCount", x.get("messageCount"));
                                Value::Object(m)
                            })
                            .collect(),
                        Err(_) => Vec::new(),
                    };
                    self.emit(json!({"type": "SessionList", "sessions": sessions}));
                }
            }
            "command_options" => {
                if let Some((agent, sid)) = self.session_of(tab_id) {
                    let input = msg.get("input").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!(""));
                    let mut p = json!({"sessionId": sid, "input": input});
                    put(p.as_object_mut().unwrap(), "command", msg.get("command"));
                    let res = agent.conn.request("_kiro.dev/commands/options", p).await.unwrap_or(Value::Null);
                    let pick = |k: &str, d: Value| res.get(k).filter(|v| truthy(v)).cloned().unwrap_or(d);
                    let mut m = Map::new();
                    m.insert("type".into(), json!("CommandOptions"));
                    m.insert("tabId".into(), json!(tab_id));
                    put(&mut m, "command", msg.get("command"));
                    m.insert("options".into(), pick("options", json!([])));
                    m.insert("hint".into(), pick("hint", Value::Null));
                    m.insert("panel".into(), pick("panel", Value::Null));
                    self.emit(Value::Object(m));
                }
            }
            "kiro_session_list" => {
                if let Some((agent, _)) = self.any_session(tab_id) {
                    let res = agent.conn.request("_kiro.dev/session/list", json!({"cwd": self.app.workspace()})).await;
                    let sessions = res.ok().and_then(|r| r.get("sessions").filter(|v| truthy(v)).cloned()).unwrap_or_else(|| json!([]));
                    self.emit(json!({"type": "KiroSessionList", "tabId": tab_id, "sessions": sessions}));
                }
            }
            "kiro_settings_list" => {
                if let Some((agent, _)) = self.any_session(tab_id) {
                    let settings = agent.conn.request("_kiro.dev/settings/list", json!({})).await.unwrap_or_else(|_| json!({}));
                    self.emit(json!({"type": "KiroSettingsList", "tabId": tab_id, "settings": settings}));
                }
            }
            "kiro_settings_set" => {
                if let Some((agent, _)) = self.any_session(tab_id) {
                    let mut p = Map::new();
                    put(&mut p, "key", msg.get("key"));
                    put(&mut p, "value", msg.get("value"));
                    match agent.conn.request("_kiro.dev/settings/set", Value::Object(p)).await {
                        Ok(_) => {
                            let mut m = Map::new();
                            m.insert("type".into(), json!("KiroSettingsUpdated"));
                            m.insert("tabId".into(), json!(tab_id));
                            put(&mut m, "key", msg.get("key"));
                            put(&mut m, "value", msg.get("value"));
                            self.emit(Value::Object(m));
                        }
                        Err(e) => self.emit_error(Some(tab_id), e.message),
                    }
                }
            }
            "set_debug" => self.debug.store(msg.get("enabled").is_some_and(truthy), Ordering::Relaxed),
            "set_config_option" => {
                if let Some((agent, sid)) = self.session_of(tab_id) {
                    let mut p = json!({"sessionId": sid});
                    put(p.as_object_mut().unwrap(), "configId", msg.get("configId"));
                    put(p.as_object_mut().unwrap(), "value", msg.get("value"));
                    if let Err(e) = agent.conn.request("session/set_config_option", p).await {
                        self.emit_error(Some(tab_id), e.message);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
