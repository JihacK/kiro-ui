//! Reads files picked or dropped in the UI into attachment items:
//! `{"kind":"image","name","mime","data"}` (base64) or `{"kind":"file","name","content"}`.

use base64::Engine;
use serde_json::{json, Value};

const MAX_ATTACHMENT: u64 = 10 * 1024 * 1024;
const IMAGE_EXT: &[(&str, &str)] = &[("png", "image/png"), ("jpg", "image/jpeg"), ("jpeg", "image/jpeg"), ("gif", "image/gif"), ("webp", "image/webp")];

/// Returns `(items, errors)`; a failing file only adds an error.
pub async fn read(paths: Vec<String>) -> (Vec<Value>, Vec<String>) {
    let mut items = Vec::new();
    let mut errors = Vec::new();
    for p in paths {
        let path = std::path::Path::new(&p);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.clone());
        match tokio::fs::metadata(path).await {
            Ok(m) if m.len() > MAX_ATTACHMENT => {
                errors.push(format!("{name}: larger than 10 MB"));
                continue;
            }
            Err(e) => {
                errors.push(format!("{name}: {e}"));
                continue;
            }
            _ => {}
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let Ok(bytes) = tokio::fs::read(path).await else {
            errors.push(format!("{name}: cannot read"));
            continue;
        };
        if let Some((_, mime)) = IMAGE_EXT.iter().find(|(e, _)| *e == ext) {
            let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
            items.push(json!({"kind": "image", "name": name, "mime": mime, "data": data}));
        } else {
            match String::from_utf8(bytes) {
                Ok(content) => items.push(json!({"kind": "file", "name": name, "content": content})),
                Err(_) => errors.push(format!("{name}: not a text file")),
            }
        }
    }
    (items, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("kiro-ui-att-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn s(p: &std::path::Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn image_text_and_errors() {
        let d = temp_dir("read");
        let png = d.join("a.PNG");
        std::fs::write(&png, [0x89, b'P', b'N', b'G', 1, 2, 3]).unwrap();
        let txt = d.join("n.txt");
        std::fs::write(&txt, "héllo").unwrap();
        let bin = d.join("b.bin");
        std::fs::write(&bin, [0xff, 0xfe, 0x00]).unwrap();
        let big = d.join("big.txt");
        std::fs::File::create(&big).unwrap().set_len(MAX_ATTACHMENT + 1).unwrap();
        let missing = d.join("nope.txt");

        let (items, errors) = read(vec![s(&png), s(&txt), s(&bin), s(&big), s(&missing)]).await;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["kind"], "image");
        assert_eq!(items[0]["name"], "a.PNG");
        assert_eq!(items[0]["mime"], "image/png");
        assert_eq!(items[0]["data"], "iVBORwECAw==");
        assert_eq!(items[1], json!({"kind": "file", "name": "n.txt", "content": "héllo"}));
        assert_eq!(errors.len(), 3);
        assert_eq!(errors[0], "b.bin: not a text file");
        assert_eq!(errors[1], "big.txt: larger than 10 MB");
        assert!(errors[2].starts_with("nope.txt: "), "{}", errors[2]);
        let _ = std::fs::remove_dir_all(d);
    }
}
