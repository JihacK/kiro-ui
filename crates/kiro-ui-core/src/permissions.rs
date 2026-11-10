//! Permission auto-resolution: trust rules first, then the tab's policy.

use serde_json::Value;

/// Returns the `optionId` to select automatically, or `None` if the user must be asked.
///
/// `trust` is the stored rule for this tool title (`allow_always` / `reject_always`).
pub fn resolve(trust: Option<&str>, kind: Option<&str>, options: &[Value], policy: &str) -> Option<String> {
    let find = |kinds: [&str; 2]| {
        options
            .iter()
            .find(|o| o.get("kind").and_then(Value::as_str).is_some_and(|k| kinds.contains(&k)))
            .map(|o| match o.get("optionId") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            })
    };
    let allow = find(["allow_once", "allow_always"]);
    let reject = find(["reject_once", "reject_always"]);

    match trust {
        Some("allow_always") if allow.is_some() => return allow,
        Some("reject_always") if reject.is_some() => return reject,
        _ => {}
    }
    match policy {
        "allow-all" => allow,
        "approve-reads" if kind == Some("read") => allow,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Vec<Value> {
        vec![json!({"optionId": "opt-allow", "kind": "allow_once"}), json!({"optionId": "opt-reject", "kind": "reject_once"})]
    }
    fn always() -> Vec<Value> {
        vec![
            json!({"optionId": "opt-allow-always", "kind": "allow_always"}),
            json!({"optionId": "opt-reject-always", "kind": "reject_always"}),
        ]
    }

    #[test]
    fn ask_requires_user() {
        assert_eq!(resolve(None, Some("write"), &base(), "ask"), None);
    }
    #[test]
    fn allow_all_auto_approves() {
        assert_eq!(resolve(None, Some("write"), &base(), "allow-all").as_deref(), Some("opt-allow"));
    }
    #[test]
    fn approve_reads_only_reads() {
        assert_eq!(resolve(None, Some("read"), &base(), "approve-reads").as_deref(), Some("opt-allow"));
        assert_eq!(resolve(None, Some("write"), &base(), "approve-reads"), None);
    }
    #[test]
    fn trust_allow_overrides_ask() {
        assert_eq!(resolve(Some("allow_always"), Some("write"), &always(), "ask").as_deref(), Some("opt-allow-always"));
    }
    #[test]
    fn trust_reject_overrides_allow_all() {
        assert_eq!(resolve(Some("reject_always"), Some("write"), &always(), "allow-all").as_deref(), Some("opt-reject-always"));
    }
}
