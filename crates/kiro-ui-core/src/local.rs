//! "Local" UI actions: answered by the core itself, with no agent involved.
//! They bypass the chat rate limit and the first-tab startup wait, and each
//! runs in its own task so a slow dialog or Bedrock call blocks nothing.

use crate::{attachments, picker, suggestions, AppState, JsonMap};
use serde_json::{json, Value};

pub(crate) fn is_local(action: &str) -> bool {
    matches!(
        action,
        "get_settings"
            | "merge_settings"
            | "set_trust_rules"
            | "suggest"
            | "suggestion_models"
            | "suggestion_test"
            | "pick_folder"
            | "pick_files"
            | "attach_paths"
            | "export"
            | "notify"
    )
}

fn object(v: Option<&Value>) -> JsonMap {
    v.and_then(Value::as_object).cloned().unwrap_or_default()
}

fn text<'a>(msg: &'a Value, key: &str) -> &'a str {
    msg.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn tab_id(msg: &Value) -> Value {
    msg.get("tabId").cloned().unwrap_or(Value::Null)
}

fn settings_event(app: &AppState) -> Value {
    json!({"type": "Settings", "settings": app.settings_snapshot(), "trust": app.trust_rules(), "workspace": app.workspace()})
}

/// Base name only, so a UI-supplied name can't point the dialog elsewhere.
fn export_file_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    let base: String = base.chars().filter(|c| !c.is_control()).collect();
    if base.is_empty() || base == "." || base == ".." {
        "export.md".into()
    } else {
        base
    }
}

/// Run a local action and return its reply event, if it has one.
pub(crate) async fn run(app: &AppState, action: &str, msg: &Value) -> Option<Value> {
    let ev = match action {
        "get_settings" => settings_event(app),
        "merge_settings" => {
            app.merge_settings(object(msg.get("patch"))).await;
            settings_event(app)
        }
        "set_trust_rules" => {
            app.set_trust_rules(object(msg.get("rules"))).await;
            settings_event(app)
        }
        "suggest" => {
            let mut ev = suggestions::suggest(app, text(msg, "context")).await;
            ev["type"] = json!("Suggestions");
            ev["tabId"] = tab_id(msg);
            ev
        }
        "suggestion_models" => {
            // Region/profile edits must land before listing.
            if let Some(patch) = msg.get("patch").and_then(Value::as_object) {
                app.merge_settings(patch.clone()).await;
            }
            let mut ev = suggestions::models(app).await;
            ev["type"] = json!("SuggestionModels");
            ev
        }
        "suggestion_test" => {
            let mut ev = suggestions::test(app).await;
            ev["type"] = json!("SuggestionTest");
            ev
        }
        "pick_folder" => {
            let path = picker::pick_folder(text(msg, "start")).await;
            json!({"type": "FolderPicked", "tabId": tab_id(msg), "path": path})
        }
        "pick_files" => {
            let paths = picker::pick_files(text(msg, "start")).await;
            attached(msg, paths).await
        }
        "attach_paths" => {
            let paths = msg.get("paths").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
            attached(msg, paths).await
        }
        "export" => export(app, msg).await,
        "notify" => {
            picker::notify("Kiro", text(msg, "body"));
            return None;
        }
        _ => return None,
    };
    Some(ev)
}

async fn attached(msg: &Value, paths: Vec<String>) -> Value {
    let (items, errors) = attachments::read(paths).await;
    json!({"type": "FilesAttached", "tabId": tab_id(msg), "items": items, "errors": errors})
}

async fn export(app: &AppState, msg: &Value) -> Value {
    let start = match text(msg, "start") {
        "" => app.workspace(),
        s => s.to_string(),
    };
    let name = export_file_name(text(msg, "fileName"));
    let Some(path) = picker::pick_save(&start, &name).await else {
        return json!({"type": "Exported", "cancelled": true});
    };
    match tokio::fs::write(&path, text(msg, "markdown")).await {
        Ok(()) => json!({"type": "Exported", "path": path}),
        Err(e) => json!({"type": "Exported", "error": e.to_string()}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_is_basename() {
        assert_eq!(export_file_name("chat.md"), "chat.md");
        assert_eq!(export_file_name("../../etc/passwd"), "passwd");
        assert_eq!(export_file_name("C:\\x\\y.md"), "y.md");
        assert_eq!(export_file_name("dir/"), "export.md");
        assert_eq!(export_file_name(".."), "export.md");
        assert_eq!(export_file_name(""), "export.md");
        assert_eq!(export_file_name("a\nb.md"), "ab.md");
    }

    #[test]
    fn local_actions_are_recognised() {
        assert!(is_local("get_settings") && is_local("notify"));
        assert!(!is_local("prompt") && !is_local("new_tab"));
    }

    #[tokio::test]
    async fn settings_round_trip_without_agent() {
        let app = crate::tests::test_state("local");
        let ev = run(&app, "merge_settings", &json!({"patch": {"editor": "vscode"}})).await.unwrap();
        assert_eq!(ev["type"], "Settings");
        assert_eq!(ev["settings"]["editor"], "vscode");
        let ev = run(&app, "set_trust_rules", &json!({"rules": {"Read file": "allow_always"}})).await.unwrap();
        assert_eq!(ev["trust"]["Read file"], "allow_always");
        let ev = run(&app, "get_settings", &json!({})).await.unwrap();
        assert_eq!(ev["settings"]["editor"], "vscode");
        assert!(ev["workspace"].is_string());
    }
}
