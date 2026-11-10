//! Translate ACP `session/update` and Kiro `_kiro.dev/*` notifications into UI events.
//!
//! Field semantics mirror the original JS: a missing source field is omitted
//! from the event, and `a || b` falls through on falsy values.

use crate::truthy;
use serde_json::{json, Map, Value};

fn event(kind: &str, tab: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("type".into(), json!(kind));
    m.insert("tabId".into(), json!(tab));
    m
}

/// Copy `keys` from `src` when present (null included, like JSON.stringify).
fn copy(m: &mut Map<String, Value>, src: &Value, keys: &[&str]) {
    for k in keys {
        if let Some(v) = src.get(*k) {
            m.insert((*k).to_string(), v.clone());
        }
    }
}

/// First truthy value among `keys` (`p.a || p.b`), inserted under `name`.
fn first(m: &mut Map<String, Value>, name: &str, src: &Value, keys: &[&str]) {
    if let Some(v) = keys.iter().filter_map(|k| src.get(*k)).find(|v| truthy(v)) {
        m.insert(name.to_string(), v.clone());
    }
}

fn or_empty(v: Option<&Value>) -> Value {
    v.filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!(""))
}

fn or_array(v: Option<&Value>) -> Value {
    v.filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!([]))
}

/// `session/update` → UI event (or `None` for unhandled update kinds).
pub fn session_update(tab: &str, params: &Value) -> Option<Value> {
    let u = params.get("update")?;
    let content = u.get("content");
    let text = content.and_then(|c| c.get("text"));
    let m = match u.get("sessionUpdate")?.as_str()? {
        "agent_message_chunk" => {
            let thinking = content.and_then(|c| c.get("type")).and_then(Value::as_str) == Some("thinking");
            let mut m = event(if thinking { "Thinking" } else { "AgentMessageChunk" }, tab);
            m.insert("text".into(), or_empty(text));
            m
        }
        "agent_thought_chunk" => {
            let mut m = event("Thinking", tab);
            let t = text.filter(|v| truthy(v)).or_else(|| u.get("text"));
            m.insert("text".into(), or_empty(t));
            m
        }
        "tool_call" => {
            let mut m = event("ToolCall", tab);
            copy(&mut m, u, &["toolCallId", "title", "kind", "content", "status", "rawInput", "locations"]);
            m
        }
        "tool_call_update" => {
            let mut m = event("ToolCallUpdate", tab);
            copy(&mut m, u, &["toolCallId", "title", "status", "rawOutput", "locations"]);
            m
        }
        "tool_call_chunk" => {
            let mut m = event("ToolCallChunk", tab);
            copy(&mut m, u, &["toolCallId", "title", "kind", "content"]);
            m
        }
        "plan" => {
            let mut m = event("Plan", tab);
            copy(&mut m, u, &["entries"]);
            m
        }
        "user_message_chunk" => {
            let mut m = event("UserMessageChunk", tab);
            m.insert("text".into(), or_empty(text));
            m
        }
        _ => return None,
    };
    Some(Value::Object(m))
}

/// Kiro extension notification → UI event (or `None` when it should be ignored).
pub fn ext_notification(tab: &str, method: &str, p: &Value) -> Option<Value> {
    let m = match method {
        "_kiro.dev/commands/available" => {
            let mut m = event("CommandsAvailable", tab);
            m.insert("commands".into(), or_array(p.get("commands")));
            m.insert("mcpServers".into(), or_array(p.get("mcpServers")));
            m.insert("tools".into(), or_array(p.get("tools")));
            // MCP prompts and skills (`serverName: "skill:*"`), invoked as `/name`.
            m.insert("prompts".into(), or_array(p.get("prompts")));
            m
        }
        "_kiro.dev/metadata" => spread("Metadata", tab, p),
        "_kiro.dev/mcp/server_initialized" => {
            let mut m = event("McpServerInitialized", tab);
            copy(&mut m, p, &["serverName"]);
            m
        }
        "_kiro.dev/mcp/server_init_failure" => {
            let mut m = event("McpServerInitFailure", tab);
            first(&mut m, "serverName", p, &["serverName", "server_name"]);
            copy(&mut m, p, &["error"]);
            first(&mut m, "oauthUrl", p, &["oauthUrl", "oauth_url"]);
            m
        }
        "_kiro.dev/mcp/oauth_request" => {
            let mut m = event("McpOauthRequest", tab);
            first(&mut m, "serverName", p, &["serverName", "server_name"]);
            first(&mut m, "oauthUrl", p, &["oauthUrl", "oauth_url", "url"]);
            m
        }
        "_kiro.dev/mcp/governance_disabled" => fields("McpGovernanceDisabled", tab, p, &["apiFailure"]),
        "_kiro.dev/compaction/status" => fields("CompactionStatus", tab, p, &["status", "summary"]),
        "_kiro.dev/clear/status" => event("ClearStatus", tab),
        "_kiro.dev/agent/switched" => {
            fields("AgentSwitched", tab, p, &["agentName", "previousAgentName", "welcomeMessage", "model"])
        }
        "_kiro.dev/agent/not_found" => fields("AgentNotFound", tab, p, &["requestedAgent", "fallbackAgent"]),
        "_kiro.dev/agent/config_error" => fields("AgentConfigError", tab, p, &["path", "error"]),
        "_kiro.dev/error/rate_limit" => fields("RateLimitError", tab, p, &["message"]),
        "_kiro.dev/subagent/list_update" => fields("SubagentListUpdate", tab, p, &["subagents", "pendingStages"]),
        "_kiro.dev/session/activity" => fields("SessionActivity", tab, p, &["sessionId", "event"]),
        "_kiro.dev/session/list_update" => fields("SessionListUpdate", tab, p, &["sessions"]),
        "_kiro.dev/session/inbox_notification" => spread("InboxNotification", tab, p),
        "_kiro.dev/session/update" => {
            let u = p.get("update")?;
            match u.get("sessionUpdate").and_then(Value::as_str)? {
                "tool_call_chunk" => fields("ToolCallChunk", tab, u, &["toolCallId", "title", "kind"]),
                "retry_warning" => fields("RetryWarning", tab, u, &["attempt", "maxAttempts", "delaySecs", "message"]),
                _ => return None,
            }
        }
        "_kiro.dev/goal/status" => fields("GoalStatus", tab, p, &["currentIteration", "maxIterations", "status"]),
        _ => {
            let raw: String = p.to_string().chars().take(200).collect();
            let mut m = event("ProtocolLog", tab);
            m.insert("dir".into(), json!("in"));
            m.insert("msg".into(), json!(format!("ext: {method} {raw}")));
            m
        }
    };
    Some(Value::Object(m))
}

fn fields(kind: &str, tab: &str, src: &Value, keys: &[&str]) -> Map<String, Value> {
    let mut m = event(kind, tab);
    copy(&mut m, src, keys);
    m
}

/// `{ type, tabId, ...p }` — payload keys win, as in the JS spread.
fn spread(kind: &str, tab: &str, p: &Value) -> Map<String, Value> {
    let mut m = event(kind, tab);
    if let Some(obj) = p.as_object() {
        for (k, v) in obj {
            m.insert(k.clone(), v.clone());
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(method: &str, p: Value) -> Option<Value> {
        ext_notification("tab-1", method, &p)
    }

    #[test]
    fn commands_available() {
        let e = ext("_kiro.dev/commands/available", json!({"commands": [{"name": "help"}], "mcpServers": [{"name": "mcp1"}], "tools": [{"name": "read"}], "prompts": [{"name": "antislop", "serverName": "skill:config"}]})).unwrap();
        assert_eq!(e["type"], "CommandsAvailable");
        assert_eq!(e["commands"], json!([{"name": "help"}]));
        assert_eq!(e["mcpServers"], json!([{"name": "mcp1"}]));
        assert_eq!(e["prompts"][0]["serverName"], "skill:config");
        assert_eq!(ext("_kiro.dev/commands/available", json!({})).unwrap()["prompts"], json!([]));
    }
    #[test]
    fn metadata_spreads_payload() {
        let e = ext("_kiro.dev/metadata", json!({"contextUsagePercentage": 42})).unwrap();
        assert_eq!(e, json!({"type": "Metadata", "tabId": "tab-1", "contextUsagePercentage": 42}));
    }
    #[test]
    fn mcp_events() {
        assert_eq!(
            ext("_kiro.dev/mcp/server_initialized", json!({"serverName": "browsermcp"})).unwrap(),
            json!({"type": "McpServerInitialized", "tabId": "tab-1", "serverName": "browsermcp"})
        );
        assert_eq!(
            ext("_kiro.dev/mcp/server_init_failure", json!({"serverName": "x", "error": "timeout"})).unwrap(),
            json!({"type": "McpServerInitFailure", "tabId": "tab-1", "serverName": "x", "error": "timeout"})
        );
        assert_eq!(
            ext("_kiro.dev/mcp/oauth_request", json!({"server_name": "mcp1", "url": "https://auth.example.com"})).unwrap(),
            json!({"type": "McpOauthRequest", "tabId": "tab-1", "serverName": "mcp1", "oauthUrl": "https://auth.example.com"})
        );
        assert_eq!(
            ext("_kiro.dev/mcp/governance_disabled", json!({"apiFailure": true})).unwrap(),
            json!({"type": "McpGovernanceDisabled", "tabId": "tab-1", "apiFailure": true})
        );
    }
    #[test]
    fn agent_and_status_events() {
        assert_eq!(ext("_kiro.dev/agent/switched", json!({"agentName": "coder", "model": "claude-4"})).unwrap()["agentName"], "coder");
        assert_eq!(
            ext("_kiro.dev/error/rate_limit", json!({"message": "Too many"})).unwrap(),
            json!({"type": "RateLimitError", "tabId": "tab-1", "message": "Too many"})
        );
        assert_eq!(
            ext("_kiro.dev/compaction/status", json!({"status": {"type": "completed"}, "summary": "done"})).unwrap(),
            json!({"type": "CompactionStatus", "tabId": "tab-1", "status": {"type": "completed"}, "summary": "done"})
        );
        assert_eq!(ext("_kiro.dev/clear/status", json!({})).unwrap(), json!({"type": "ClearStatus", "tabId": "tab-1"}));
        assert_eq!(
            ext("_kiro.dev/agent/not_found", json!({"requestedAgent": "foo", "fallbackAgent": "default"})).unwrap(),
            json!({"type": "AgentNotFound", "tabId": "tab-1", "requestedAgent": "foo", "fallbackAgent": "default"})
        );
        assert_eq!(
            ext("_kiro.dev/goal/status", json!({"currentIteration": 3, "maxIterations": 5, "status": "active"})).unwrap(),
            json!({"type": "GoalStatus", "tabId": "tab-1", "currentIteration": 3, "maxIterations": 5, "status": "active"})
        );
    }
    #[test]
    fn session_events() {
        assert_eq!(
            ext("_kiro.dev/session/activity", json!({"sessionId": "sess-2", "event": {"type": "tool_call"}})).unwrap(),
            json!({"type": "SessionActivity", "tabId": "tab-1", "sessionId": "sess-2", "event": {"type": "tool_call"}})
        );
        assert_eq!(
            ext("_kiro.dev/session/inbox_notification", json!({"message": "done", "from": "sub1"})).unwrap(),
            json!({"type": "InboxNotification", "tabId": "tab-1", "message": "done", "from": "sub1"})
        );
        assert_eq!(
            ext("_kiro.dev/session/update", json!({"update": {"sessionUpdate": "retry_warning", "attempt": 2, "maxAttempts": 5, "delaySecs": 10, "message": "Retry"}})).unwrap(),
            json!({"type": "RetryWarning", "tabId": "tab-1", "attempt": 2, "maxAttempts": 5, "delaySecs": 10, "message": "Retry"})
        );
        assert_eq!(
            ext("_kiro.dev/session/update", json!({"update": {"sessionUpdate": "tool_call_chunk", "toolCallId": "tc-1", "title": "Read", "kind": "read"}})).unwrap(),
            json!({"type": "ToolCallChunk", "tabId": "tab-1", "toolCallId": "tc-1", "title": "Read", "kind": "read"})
        );
        assert_eq!(ext("_kiro.dev/session/update", json!({"update": {"sessionUpdate": "unknown_type"}})), None);
    }
    #[test]
    fn unknown_method_logs() {
        assert_eq!(
            ext("_kiro.dev/unknown", json!({})).unwrap(),
            json!({"type": "ProtocolLog", "tabId": "tab-1", "dir": "in", "msg": "ext: _kiro.dev/unknown {}"})
        );
    }
    #[test]
    fn session_update_kinds() {
        let su = |u: Value| session_update("t", &json!({"sessionId": "s", "update": u}));
        assert_eq!(su(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}})).unwrap(), json!({"type": "AgentMessageChunk", "tabId": "t", "text": "hi"}));
        assert_eq!(su(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "thinking", "text": "hm"}})).unwrap()["type"], "Thinking");
        assert_eq!(su(json!({"sessionUpdate": "agent_thought_chunk", "text": "x"})).unwrap()["text"], "x");
        assert_eq!(
            su(json!({"sessionUpdate": "tool_call_update", "toolCallId": "a", "status": "completed", "content": []})).unwrap(),
            json!({"type": "ToolCallUpdate", "tabId": "t", "toolCallId": "a", "status": "completed"})
        );
        assert_eq!(su(json!({"sessionUpdate": "user_message_chunk"})).unwrap()["text"], "");
        assert_eq!(su(json!({"sessionUpdate": "available_commands_update"})), None);
    }
}
