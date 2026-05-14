//! Small helpers used during runtime construction.
//!
//! Most of the resolution logic lives inline in [`super::builder::RuntimeBuilder::build`];
//! anything that is sufficiently independent to test on its own lives here.

use std::collections::{HashMap, HashSet};

use crate::types::{EmbeddingProfileRow, LlmProfileRow};

/// Collect the unique set of `api_key_secret` names referenced by a slice of
/// LLM and embedding profile rows. The order of the returned vector is not
/// guaranteed; callers should treat it as a set.
pub(crate) fn collect_secret_names(
    llm_profiles: &[LlmProfileRow],
    embedding_profiles: &[EmbeddingProfileRow],
) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for p in llm_profiles {
        if let Some(name) = p.api_key_secret.as_deref() {
            if seen.insert(name.to_owned()) {
                out.push(name.to_owned());
            }
        }
    }
    for p in embedding_profiles {
        if let Some(name) = p.api_key_secret.as_deref() {
            if seen.insert(name.to_owned()) {
                out.push(name.to_owned());
            }
        }
    }
    out
}

/// Inject `_resolved_api_key` into a JSON `params` blob, ensuring the result
/// is always an object. Returns the original blob unchanged when there is no
/// secret to inject.
pub(crate) fn inject_resolved_key(
    params: serde_json::Value,
    secret_name: Option<&str>,
    secrets: &HashMap<String, String>,
) -> serde_json::Value {
    let Some(name) = secret_name else {
        return params;
    };
    let Some(val) = secrets.get(name) else {
        return params;
    };
    let mut obj = match params {
        serde_json::Value::Object(m) => m,
        // Promote scalar / null params to an empty object so we always have
        // a place to attach the resolved key.
        _ => serde_json::Map::new(),
    };
    obj.insert(
        "_resolved_api_key".to_owned(),
        serde_json::Value::String(val.clone()),
    );
    serde_json::Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn llm(name: &str, secret: Option<&str>) -> LlmProfileRow {
        LlmProfileRow {
            name: name.into(),
            provider: "p".into(),
            model: "m".into(),
            api_key_secret: secret.map(|s| s.to_owned()),
            base_url: None,
            params: serde_json::Value::Null,
        }
    }

    fn embed(name: &str, secret: Option<&str>) -> EmbeddingProfileRow {
        EmbeddingProfileRow {
            name: name.into(),
            provider: "p".into(),
            model: "m".into(),
            dimension: 4,
            api_key_secret: secret.map(|s| s.to_owned()),
            base_url: None,
            params: serde_json::Value::Null,
        }
    }

    #[test]
    fn collect_secret_names_dedupes_across_profile_kinds() {
        let llms = vec![
            llm("a", Some("OPENAI_KEY")),
            llm("b", Some("ANTHROPIC_KEY")),
            llm("c", None),
        ];
        let embeds = vec![
            embed("a", Some("OPENAI_KEY")), // duplicate
            embed("b", Some("VOYAGE_KEY")),
            embed("c", None),
        ];
        let mut names = collect_secret_names(&llms, &embeds);
        names.sort();
        assert_eq!(
            names,
            vec![
                "ANTHROPIC_KEY".to_string(),
                "OPENAI_KEY".to_string(),
                "VOYAGE_KEY".to_string(),
            ]
        );
    }

    #[test]
    fn inject_resolved_key_on_null_creates_object() {
        let mut secrets = HashMap::new();
        secrets.insert("K".to_string(), "abc".to_string());
        let out = inject_resolved_key(serde_json::Value::Null, Some("K"), &secrets);
        assert_eq!(out, serde_json::json!({"_resolved_api_key": "abc"}));
    }

    #[test]
    fn inject_resolved_key_merges_with_existing_object() {
        let mut secrets = HashMap::new();
        secrets.insert("K".to_string(), "abc".to_string());
        let params = serde_json::json!({"temperature": 0.2});
        let out = inject_resolved_key(params, Some("K"), &secrets);
        assert_eq!(
            out,
            serde_json::json!({"temperature": 0.2, "_resolved_api_key": "abc"})
        );
    }

    #[test]
    fn inject_resolved_key_no_secret_name_is_passthrough() {
        let params = serde_json::json!({"x": 1});
        let secrets: HashMap<String, String> = HashMap::new();
        let out = inject_resolved_key(params.clone(), None, &secrets);
        assert_eq!(out, params);
    }

    #[test]
    fn inject_resolved_key_missing_secret_is_passthrough() {
        let params = serde_json::json!({"x": 1});
        let secrets: HashMap<String, String> = HashMap::new();
        let out = inject_resolved_key(params.clone(), Some("MISSING"), &secrets);
        assert_eq!(out, params);
    }
}
