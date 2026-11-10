//! Kiro UI core: bridges UI clients to `kiro-cli acp` (Agent Client Protocol
//! over newline-delimited JSON-RPC), plus settings, trust rules and suggestions.
//!
//! Transport-agnostic: the native egui app drives a [`Client`] directly; the
//! optional `kiro-ui-server` binary exposes the same client over a WebSocket.

mod agent;
mod hub;
mod notifications;
mod permissions;
pub mod picker;
mod prompt;
mod rpc;
pub mod suggestions;

pub use hub::Client;

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, Weak};

pub type JsonMap = Map<String, Value>;

/// Process-wide state shared by all clients.
pub struct AppState {
    /// Auth token for the optional HTTP/WebSocket server.
    pub token: String,
    trust_file: PathBuf,
    settings_file: PathBuf,
    settings: RwLock<JsonMap>,
    trust: RwLock<JsonMap>,
    /// Serialises writes to the settings/trust files.
    save_lock: tokio::sync::Mutex<()>,
    /// Every spawned agent, so shutdown can terminate them all.
    agents: Mutex<Vec<Weak<agent::Agent>>>,
    pub(crate) shutting_down: std::sync::atomic::AtomicBool,
    suggestions: suggestions::ClientCache,
}

/// JS-style truthiness for JSON values (`""`, `0`, `false`, `null` are falsy).
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

impl AppState {
    /// Settings value as a string (numbers/bools are stringified), `None` if unset or empty.
    pub fn setting(&self, key: &str) -> Option<String> {
        let s = self.settings.read().unwrap();
        match s.get(key) {
            Some(Value::String(v)) if !v.is_empty() => Some(v.clone()),
            Some(v @ (Value::Number(_) | Value::Bool(_))) => Some(v.to_string()),
            _ => None,
        }
    }

    /// `Number(setting) || default` semantics from the original server.
    fn setting_num(&self, key: &str, default: f64) -> f64 {
        match self.setting(key).and_then(|v| v.trim().parse::<f64>().ok()) {
            Some(n) if n != 0.0 && !n.is_nan() => n,
            _ => default,
        }
    }

    pub fn settings_snapshot(&self) -> JsonMap {
        self.settings.read().unwrap().clone()
    }

    pub fn workspace(&self) -> String {
        match self.setting("workspace") {
            Some(w) => expand_home(&w),
            None => home_dir().join(".kiro-ui").join("workspace").to_string_lossy().into_owned(),
        }
    }
    pub fn max_tabs(&self) -> f64 {
        self.setting_num("maxTabs", 10.0)
    }
    pub fn max_msgs_per_min(&self) -> f64 {
        self.setting_num("maxMsgsPerMin", 30.0)
    }
    pub fn max_child_mem_mb(&self) -> f64 {
        self.setting_num("maxChildMemMb", 512.0)
    }
    /// All tabs of a window share one agent process unless explicitly disabled.
    pub fn share_agent_process(&self) -> bool {
        self.setting("shareAgentProcess").as_deref() != Some("false")
    }

    pub async fn merge_settings(&self, patch: JsonMap) -> JsonMap {
        let snapshot = {
            let mut s = self.settings.write().unwrap();
            for (k, v) in patch {
                s.insert(k, v);
            }
            s.clone()
        };
        self.save_json(&self.settings_file, &snapshot).await;
        snapshot
    }

    pub fn trust_rules(&self) -> JsonMap {
        self.trust.read().unwrap().clone()
    }

    pub fn trust_match(&self, title: &str) -> Option<&'static str> {
        match self.trust.read().unwrap().get(title).and_then(Value::as_str) {
            Some("allow_always") => Some("allow_always"),
            Some("reject_always") => Some("reject_always"),
            _ => None,
        }
    }

    pub async fn set_trust_rules(&self, rules: JsonMap) -> JsonMap {
        *self.trust.write().unwrap() = rules.clone();
        self.save_json(&self.trust_file, &rules).await;
        rules
    }

    pub async fn set_trust_rule(&self, title: &str, policy: &str) {
        let snapshot = {
            let mut t = self.trust.write().unwrap();
            t.insert(title.to_string(), Value::String(policy.to_string()));
            t.clone()
        };
        self.save_json(&self.trust_file, &snapshot).await;
    }

    async fn save_json(&self, path: &Path, value: &JsonMap) {
        let _guard = self.save_lock.lock().await;
        if let Some(dir) = path.parent() {
            let _ = tokio::fs::create_dir_all(dir).await;
        }
        let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into());
        if let Err(e) = tokio::fs::write(path, text).await {
            eprintln!("[kiro-ui] failed to write {}: {e}", path.display());
        }
    }

    pub(crate) fn register_agent(&self, a: &Arc<agent::Agent>) {
        let mut agents = self.agents.lock().unwrap();
        agents.retain(|w| w.strong_count() > 0);
        agents.push(Arc::downgrade(a));
    }

    /// Terminate every agent process (SIGTERM, then SIGKILL after 3s).
    pub async fn shutdown(&self) {
        self.shutting_down.store(true, std::sync::atomic::Ordering::SeqCst);
        let agents: Vec<_> = self.agents.lock().unwrap().iter().filter_map(Weak::upgrade).collect();
        let tasks: Vec<_> = agents.into_iter().map(|a| tokio::spawn(async move { a.terminate().await })).collect();
        for t in tasks {
            let _ = t.await;
        }
    }
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Expand a leading `~` / `~/` to the home directory.
pub fn expand_home(path: &str) -> String {
    match path.strip_prefix('~') {
        Some("") => home_dir().to_string_lossy().into_owned(),
        Some(rest) if rest.starts_with('/') || rest.starts_with('\\') => format!("{}{rest}", home_dir().to_string_lossy()),
        _ => path.to_string(),
    }
}

fn read_json_map(path: &Path) -> JsonMap {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default()
}

fn random_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("OS random source unavailable");
    hex::encode(buf)
}

/// Load settings and trust rules from `~/.kiro-ui` and build the shared state.
pub fn init_state() -> Arc<AppState> {
    let dir = home_dir().join(".kiro-ui");
    let trust_file = dir.join("trust.json");
    let settings_file = dir.join("settings.json");
    let token = std::env::var("KIRO_UI_TOKEN").ok().filter(|t| !t.is_empty()).unwrap_or_else(random_token);
    let state = Arc::new(AppState {
        token,
        settings: RwLock::new(read_json_map(&settings_file)),
        trust: RwLock::new(read_json_map(&trust_file)),
        trust_file,
        settings_file,
        save_lock: tokio::sync::Mutex::new(()),
        agents: Mutex::new(Vec::new()),
        shutting_down: std::sync::atomic::AtomicBool::new(false),
        suggestions: suggestions::ClientCache::default(),
    });
    let _ = std::fs::create_dir_all(state.workspace());
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state_in(dir: &Path) -> AppState {
        let trust_file = dir.join("nested").join("trust.json");
        let settings_file = dir.join("settings.json");
        AppState {
            token: "t".into(),
            settings: RwLock::new(read_json_map(&settings_file)),
            trust: RwLock::new(read_json_map(&trust_file)),
            trust_file,
            settings_file,
            save_lock: tokio::sync::Mutex::new(()),
            agents: Mutex::new(Vec::new()),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            suggestions: suggestions::ClientCache::default(),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kiro-ui-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn read_json_map_handles_missing_invalid_and_valid() {
        let d = temp_dir("read");
        assert!(read_json_map(&d.join("missing.json")).is_empty());
        std::fs::write(d.join("bad.json"), "not json").unwrap();
        assert!(read_json_map(&d.join("bad.json")).is_empty());
        std::fs::write(d.join("ok.json"), r#"{"Read file":"allow_always"}"#).unwrap();
        assert_eq!(read_json_map(&d.join("ok.json")).get("Read file"), Some(&json!("allow_always")));
        let _ = std::fs::remove_dir_all(d);
    }

    #[tokio::test]
    async fn trust_rules_match_and_persist() {
        let d = temp_dir("trust");
        let s = state_in(&d);
        assert_eq!(s.trust_match("Write file"), None);
        s.set_trust_rule("Write file", "reject_always").await;
        s.set_trust_rule("Read src/app.ts", "allow_always").await;
        assert_eq!(s.trust_match("Write file"), Some("reject_always"));
        assert_eq!(s.trust_match("Read src/app.ts"), Some("allow_always"));
        // Directory is created and JSON is pretty-printed with 2 spaces.
        let saved = std::fs::read_to_string(d.join("nested").join("trust.json")).unwrap();
        assert_eq!(saved, "{\n  \"Write file\": \"reject_always\",\n  \"Read src/app.ts\": \"allow_always\"\n}");
        s.set_trust_rules(JsonMap::new()).await;
        assert_eq!(s.trust_match("Write file"), None);
        let _ = std::fs::remove_dir_all(d);
    }

    #[tokio::test]
    async fn settings_defaults_and_merge() {
        let d = temp_dir("settings");
        let s = state_in(&d);
        assert_eq!(s.max_tabs(), 10.0);
        assert!(s.share_agent_process());
        let merged = s.merge_settings(json!({"maxTabs": "4", "shareAgentProcess": "false", "maxMsgsPerMin": "0"}).as_object().unwrap().clone()).await;
        assert_eq!(merged.get("maxTabs"), Some(&json!("4")));
        assert_eq!(s.max_tabs(), 4.0);
        assert_eq!(s.max_msgs_per_min(), 30.0, "0 falls back to the default like `Number(x) || 30`");
        assert!(!s.share_agent_process());
        assert!(d.join("settings.json").exists());
        let _ = std::fs::remove_dir_all(d);
    }
}
