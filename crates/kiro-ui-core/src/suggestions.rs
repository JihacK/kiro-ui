//! Follow-up suggestions via Amazon Bedrock.
//!
//! AWS clients are created lazily on first use and cached per (region, profile),
//! so users who never enable suggestions pay nothing at runtime.

use crate::AppState;
use aws_sdk_bedrock::types::{InferenceProfileStatus, InferenceType, ModelModality};
use aws_sdk_bedrockruntime::primitives::Blob;
use serde_json::{json, Value};
use tokio::sync::Mutex;

#[derive(Default)]
pub struct ClientCache {
    inner: Mutex<Option<(String, aws_config::SdkConfig)>>,
}

struct Config {
    region: String,
    profile: Option<String>,
    model_id: String,
    count: usize,
    enabled: bool,
}

fn resolve_model_id(raw: &str) -> String {
    if raw.contains('.') && !raw.contains('/') && !raw.starts_with("us.") && !raw.starts_with("eu.") {
        format!("us.{raw}")
    } else {
        raw.to_string()
    }
}

fn config(app: &AppState) -> Config {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let count = app
        .setting("suggestionsCount")
        .and_then(|c| c.trim().parse::<f64>().ok())
        .filter(|n| *n > 0.0)
        .map_or(3, |n| n as usize);
    Config {
        region: app.setting("suggestionsRegion").or_else(|| env("AWS_REGION")).unwrap_or_else(|| "us-east-1".into()),
        profile: app.setting("suggestionsProfile").or_else(|| env("AWS_PROFILE")),
        model_id: resolve_model_id(&app.setting("suggestionsModel").unwrap_or_else(|| "amazon.nova-lite-v1:0".into())),
        count,
        enabled: app.setting("suggestionsEnabled").as_deref() != Some("false"),
    }
}

async fn sdk_config(app: &AppState, cfg: &Config) -> aws_config::SdkConfig {
    let key = format!("{}|{}", cfg.region, cfg.profile.as_deref().unwrap_or(""));
    let mut cache = app.suggestions.inner.lock().await;
    if let Some((k, c)) = cache.as_ref() {
        if *k == key {
            return c.clone();
        }
    }
    let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest()).region(aws_config::Region::new(cfg.region.clone()));
    if let Some(p) = &cfg.profile {
        loader = loader.profile_name(p);
    }
    let conf = loader.load().await;
    *cache = Some((key, conf.clone()));
    conf
}

/// `Name: message` for SDK errors, like the JS SDK's `${e.name}: ${e.message}`.
fn describe<E, R>(e: &aws_sdk_bedrockruntime::error::SdkError<E, R>) -> (String, String)
where
    E: aws_sdk_bedrockruntime::error::ProvideErrorMetadata + std::error::Error + 'static,
    R: std::fmt::Debug,
{
    use aws_sdk_bedrockruntime::error::ProvideErrorMetadata;
    match (e.code(), e.message()) {
        (Some(code), Some(msg)) => (code.to_string(), msg.to_string()),
        (Some(code), None) => (code.to_string(), code.to_string()),
        _ => {
            let full = aws_sdk_bedrockruntime::error::DisplayErrorContext(e).to_string();
            if full.contains("no credentials found") || full.contains("CredentialsNotLoaded") {
                // Same wording as the JS SDK, instead of the full provider-chain dump.
                return ("CredentialsProviderError".into(), "Could not load credentials from any providers".into());
            }
            let first: String = full.lines().next().unwrap_or("Unknown error").chars().take(300).collect();
            ("Error".into(), first)
        }
    }
}

async fn invoke(app: &AppState, cfg: &Config, body: Value) -> Result<Value, (String, String)> {
    let client = aws_sdk_bedrockruntime::Client::new(&sdk_config(app, cfg).await);
    let resp = client
        .invoke_model()
        .model_id(&cfg.model_id)
        .content_type("application/json")
        .accept("application/json")
        .body(Blob::new(body.to_string()))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    serde_json::from_slice(resp.body.as_ref()).map_err(|e| ("SyntaxError".into(), e.to_string()))
}

/// Pull the first `[...]` JSON array of strings out of the model's text.
fn parse_suggestions(text: &str, count: usize) -> Vec<String> {
    let (Some(start), Some(end)) = (text.find('['), text.rfind(']')) else { return vec![] };
    if end < start {
        return vec![];
    }
    match serde_json::from_str::<Vec<Value>>(&text[start..=end]) {
        Ok(items) if items.iter().all(Value::is_string) => {
            items.into_iter().filter_map(|v| v.as_str().map(str::to_owned)).take(count).collect()
        }
        _ => vec![],
    }
}

/// Follow-up suggestions for the last assistant text: `{"suggestions": [...], "error"?}`.
pub async fn suggest(app: &AppState, last: &str) -> Value {
    let cfg = config(app);
    // JS `.length` counts UTF-16 units.
    if last.encode_utf16().count() < 80 || !cfg.enabled {
        return json!({"suggestions": []});
    }
    let convo: String = String::from_utf16_lossy(&last.encode_utf16().take(2000).collect::<Vec<_>>());
    let n = cfg.count;
    let prompt = format!(
        "You are a helpful assistant. Based on the following conversation, generate exactly {n} follow-up questions or actions the user would likely want to do next. Each must be directly related to the specific content discussed, under 80 characters, and phrased as a request to an AI assistant.\n\nCONVERSATION:\n{convo}\n\nRespond with ONLY a JSON array of {n} strings. No explanation, no markdown, just the array."
    );
    let body = json!({"messages": [{"role": "user", "content": [{"text": prompt}]}], "inferenceConfig": {"maxTokens": 200}});
    match invoke(app, &cfg, body).await {
        Ok(parsed) => {
            let text = parsed
                .pointer("/output/message/content/0/text")
                .or_else(|| parsed.pointer("/content/0/text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            json!({"suggestions": parse_suggestions(text, n)})
        }
        Err((name, message)) => {
            eprintln!("[suggestions] {name} {message}");
            json!({"suggestions": [], "error": format!("{name}: {message}")})
        }
    }
}

/// Probe the configured model: `{"ok": true, "model"}` or `{"ok": false, "error"}`.
pub async fn test(app: &AppState) -> Value {
    let cfg = config(app);
    let body = json!({"messages": [{"role": "user", "content": [{"text": "Say \"ok\""}]}], "inferenceConfig": {"maxTokens": 10}});
    match invoke(app, &cfg, body).await {
        Ok(_) => json!({"ok": true, "model": cfg.model_id}),
        Err((_, message)) => json!({"ok": false, "error": message}),
    }
}

/// Text models usable for suggestions: `{"models": [{id, name, group}], "error"?}`.
pub async fn models(app: &AppState) -> Value {
    let cfg = config(app);
    let client = aws_sdk_bedrock::Client::new(&sdk_config(app, &cfg).await);
    let fm = client.list_foundation_models().by_output_modality(ModelModality::Text).by_inference_type(InferenceType::OnDemand).send();
    let ip = client.list_inference_profiles().send();
    let (fm, ip) = tokio::join!(fm, ip);
    let err = |m: String| json!({"models": [], "error": m});
    let fm = match fm {
        Ok(r) => r,
        Err(e) => return err(describe(&e).1),
    };
    let ip = match ip {
        Ok(r) => r,
        Err(e) => return err(describe(&e).1),
    };
    let mut models: Vec<(String, String, &str)> = Vec::new();
    for p in ip.inference_profile_summaries() {
        if *p.status() == InferenceProfileStatus::Active && !p.inference_profile_id().is_empty() {
            let name = Some(p.inference_profile_name()).filter(|n| !n.is_empty()).unwrap_or(p.inference_profile_id());
            models.push((p.inference_profile_id().to_string(), name.to_string(), "Inference Profiles"));
        }
    }
    for m in fm.model_summaries() {
        if !m.model_id().is_empty() {
            models.push((m.model_id().to_string(), m.model_name().unwrap_or(m.model_id()).to_string(), "Foundation Models"));
        }
    }
    models.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()).then_with(|| a.1.cmp(&b.1)));
    let list: Vec<Value> = models.into_iter().map(|(id, name, group)| json!({"id": id, "name": name, "group": group})).collect();
    json!({"models": list})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_id_prefix() {
        assert_eq!(resolve_model_id("amazon.nova-lite-v1:0"), "us.amazon.nova-lite-v1:0");
        assert_eq!(resolve_model_id("eu.amazon.nova-lite-v1:0"), "eu.amazon.nova-lite-v1:0");
        assert_eq!(resolve_model_id("arn:aws:bedrock:us-east-1:1:inference-profile/x.y"), "arn:aws:bedrock:us-east-1:1:inference-profile/x.y");
    }

    #[test]
    fn suggestions_parsing() {
        assert_eq!(parse_suggestions("Sure: [\"a\", \"b\", \"c\"] done", 2), vec!["a", "b"]);
        assert!(parse_suggestions("[1, 2]", 3).is_empty());
        assert!(parse_suggestions("no array", 3).is_empty());
    }
}
