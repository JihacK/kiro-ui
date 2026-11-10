//! Build ACP prompt content blocks from a UI `prompt` message.

use serde_json::{json, Map, Value};

fn copy(dst: &mut Map<String, Value>, src: &Value, from: &str, to: &str) {
    if let Some(v) = src.get(from) {
        dst.insert(to.to_string(), v.clone());
    }
}

/// Images first, then attached files as embedded resources, then the text.
pub fn build(msg: &Value) -> Vec<Value> {
    let mut prompt = Vec::new();
    for img in msg.get("images").and_then(Value::as_array).into_iter().flatten() {
        let mut b = Map::new();
        b.insert("type".into(), json!("image"));
        copy(&mut b, img, "data", "data");
        copy(&mut b, img, "mimeType", "mimeType");
        prompt.push(Value::Object(b));
    }
    for f in msg.get("files").and_then(Value::as_array).into_iter().flatten() {
        let name = match f.get("name") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
            None => "undefined".into(),
        };
        let mut r = Map::new();
        r.insert("uri".into(), json!(format!("file:///{name}")));
        copy(&mut r, f, "content", "text");
        r.insert("mimeType".into(), json!("text/plain"));
        prompt.push(json!({"type": "resource", "resource": r}));
    }
    let mut t = Map::new();
    t.insert("type".into(), json!("text"));
    copy(&mut t, msg, "text", "text");
    prompt.push(Value::Object(t));
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_only() {
        assert_eq!(build(&json!({"text": "Hello"})), vec![json!({"type": "text", "text": "Hello"})]);
    }
    #[test]
    fn images_before_text() {
        let r = build(&json!({"text": "Describe", "images": [{"data": "b64", "mimeType": "image/png"}]}));
        assert_eq!(r, vec![json!({"type": "image", "data": "b64", "mimeType": "image/png"}), json!({"type": "text", "text": "Describe"})]);
    }
    #[test]
    fn files_as_resources() {
        let r = build(&json!({"text": "Review", "files": [{"name": "app.ts", "content": "const x = 1;"}]}));
        assert_eq!(
            r,
            vec![
                json!({"type": "resource", "resource": {"uri": "file:///app.ts", "text": "const x = 1;", "mimeType": "text/plain"}}),
                json!({"type": "text", "text": "Review"})
            ]
        );
    }
    #[test]
    fn images_and_files_before_text() {
        let r = build(&json!({"text": "Go", "images": [{"data": "img", "mimeType": "image/png"}], "files": [{"name": "f.md", "content": "# Hi"}]}));
        assert_eq!(r.len(), 3);
        assert_eq!(r[0]["type"], "image");
        assert_eq!(r[1]["type"], "resource");
        assert_eq!(r[2], json!({"type": "text", "text": "Go"}));
    }
    #[test]
    fn empty_arrays_text_only() {
        assert_eq!(build(&json!({"text": "Just text", "images": [], "files": []})), vec![json!({"type": "text", "text": "Just text"})]);
    }
}
