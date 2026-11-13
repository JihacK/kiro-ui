//! The UI's only link to the outside: JSON actions out, JSON events in.
//! Desktop: the in-process core. Browser: a WebSocket to `kiro-ui --server`.

use serde_json::Value;

pub trait Backend {
    /// Queue one `{"action": ...}` message. Must not block.
    fn send(&self, msg: Value);
    /// Events (`{"type": ...}`) received since the last call, oldest first.
    /// Implementations request a repaint when new events arrive.
    fn poll(&self) -> Vec<Value>;
    /// Open a link on the user's machine. Only called with `allowed_url` links.
    fn open_url(&self, url: &str);
    /// Called once when the app exits.
    fn shutdown(&self) {}
}

/// Links the UI may open: web pages and editor deep links.
pub fn allowed_url(url: &str) -> bool {
    ["http://", "https://", "vscode://", "cursor://", "idea://"].iter().any(|s| url.starts_with(s))
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_web_and_editor_links_are_allowed() {
        for ok in ["https://kiro.dev", "http://localhost:3000", "vscode://file//a.rs:3", "cursor://file//a", "idea://open?file=/a"] {
            assert!(super::allowed_url(ok), "{ok}");
        }
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "ssh://x", "", "data:text/html,x"] {
            assert!(!super::allowed_url(bad), "{bad}");
        }
    }
}
