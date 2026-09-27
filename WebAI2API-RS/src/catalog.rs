//! Static adapter and model metadata for the Rust runtime.
//!
//! The checked-in manifest is extracted from the original adapter registry. It
//! is embedded in the binary so catalog queries do not load or execute JS.

use std::collections::HashSet;
use std::sync::OnceLock;

use serde_json::{json, Map, Value};

const CATALOG_JSON: &str = include_str!("../catalog/adapters.json");

static ADAPTERS: OnceLock<Vec<Value>> = OnceLock::new();

fn catalog() -> &'static [Value] {
    ADAPTERS
        .get_or_init(|| {
            serde_json::from_str(CATALOG_JSON).expect("embedded adapter catalog must be valid JSON")
        })
        .as_slice()
}

/// All adapter manifests, retaining unknown serializable metadata.
pub fn adapters() -> &'static [Value] {
    catalog()
}

/// Return the manifest for an adapter ID.
pub fn adapter(id: &str) -> Option<&'static Value> {
    catalog()
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
}

/// Return an adapter's raw model manifest, including adapter-specific fields.
pub fn lookup_model(adapter_id: &str, model_id: &str) -> Option<&'static Value> {
    adapter(adapter_id)?
        .get("models")?
        .as_array()?
        .iter()
        .find(|model| model.get("id").and_then(Value::as_str) == Some(model_id))
}

/// Resolve model type using the registry's unknown-model default (`image`).
pub fn model_type(adapter_id: &str, model_id: &str) -> &'static str {
    lookup_model(adapter_id, model_id)
        .and_then(|model| model.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("image")
}

/// Resolve image policy using the registry's unknown-model default (`optional`).
pub fn image_policy(adapter_id: &str, model_id: &str) -> &'static str {
    lookup_model(adapter_id, model_id)
        .and_then(|model| model.get("imagePolicy"))
        .and_then(Value::as_str)
        .unwrap_or("optional")
}

/// Check whether a model passes the original registry's adapter model filter.
/// Missing or malformed filters allow every model. Unknown modes behave as the
/// registry's default blacklist mode.
pub fn model_enabled(config: &Value, adapter_id: &str, model_id: &str) -> bool {
    let Some(filter) = config
        .get("backend")
        .and_then(|backend| backend.get("adapter"))
        .and_then(|adapters| adapters.get(adapter_id))
        .and_then(|adapter| adapter.get("modelFilter"))
    else {
        return true;
    };

    let Some(list) = filter.get("list").and_then(Value::as_array) else {
        return true;
    };
    let contained = list.iter().any(|value| value.as_str() == Some(model_id));
    if filter.get("mode").and_then(Value::as_str) == Some("whitelist") {
        contained
    } else {
        !contained
    }
}

/// OpenAI-compatible model list for one adapter, applying its configured filter.
pub fn models_for_adapter(adapter_id: &str, config: &Value) -> Value {
    let data = adapter(adapter_id)
        .and_then(|entry| entry.get("models"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let object = model.as_object()?;
            let id = object.get("id")?.as_str()?;
            Some((object, id))
        })
        .filter(|(_, id)| model_enabled(config, adapter_id, id))
        .map(|(model, id)| model_record(model, adapter_id, id))
        .collect::<Vec<_>>();
    json!({ "object": "list", "data": data })
}

/// Aggregate models in adapter order, retaining the first occurrence of each ID.
/// `Worker.getModels()` emits all unqualified IDs first, then all qualified IDs.
/// The pool de-duplicates IDs across workers.
pub fn aggregate_models(adapter_ids: &[String], config: &Value) -> Value {
    let mut seen = HashSet::new();
    let mut data = Vec::new();

    for adapter_id in adapter_ids {
        let Some(models) = adapter(adapter_id)
            .and_then(|entry| entry.get("models"))
            .and_then(Value::as_array)
        else {
            continue;
        };

        let enabled = models
            .iter()
            .filter_map(|model| {
                let id = model.get("id")?.as_str()?;
                model_enabled(config, adapter_id, id)
                    .then(|| model.as_object().map(|object| (object, id)))?
            })
            .collect::<Vec<_>>();

        // Worker.getModels() first includes every unqualified ID.
        for (model, model_id) in &enabled {
            if !seen.insert((*model_id).to_owned()) {
                continue;
            }
            let mut record = model_record(model, adapter_id, model_id);
            record["owned_by"] = Value::String("internal_server".to_owned());
            data.push(record);
        }
    }

    for adapter_id in adapter_ids {
        let Some(models) = adapter(adapter_id)
            .and_then(|entry| entry.get("models"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for model in models {
            let Some(model_id) = model.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !model_enabled(config, adapter_id, model_id) {
                continue;
            }
            let Some(model) = model.as_object() else {
                continue;
            };
            let qualified_id = format!("{adapter_id}/{model_id}");
            if !seen.insert(qualified_id.clone()) {
                continue;
            }
            let mut record = model_record(model, adapter_id, model_id);
            record["id"] = Value::String(qualified_id);
            record["owned_by"] = Value::String(adapter_id.clone());
            data.push(record);
        }
    }

    json!({ "object": "list", "data": data })
}

/// Clone manifests for admin/UI callers and attach each adapter's configured
/// filter, defaulting to the WebUI's empty blacklist.
pub fn list_adapters(config: &Value) -> Vec<Value> {
    catalog()
        .iter()
        .map(|entry| {
            let mut manifest = entry.clone();
            if let Some(object) = manifest.as_object_mut() {
                object.insert(
                    "modelFilter".to_owned(),
                    configured_model_filter(config, entry),
                );
            }
            manifest
        })
        .collect()
}

fn configured_model_filter(config: &Value, manifest: &Value) -> Value {
    let adapter_id = manifest
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    config
        .get("backend")
        .and_then(|backend| backend.get("adapter"))
        .and_then(|adapters| adapters.get(adapter_id))
        .and_then(|adapter| adapter.get("modelFilter"))
        .cloned()
        .unwrap_or_else(|| json!({ "mode": "blacklist", "list": [] }))
}

fn model_record(model: &Map<String, Value>, adapter_id: &str, model_id: &str) -> Value {
    let image_policy = model
        .get("imagePolicy")
        .and_then(Value::as_str)
        .unwrap_or("optional");
    let model_type = model.get("type").and_then(Value::as_str).unwrap_or("image");
    json!({
        "id": model_id,
        "object": "model",
        "created": chrono::Utc::now().timestamp(),
        "owned_by": adapter_id,
        "image_policy": image_policy,
        "type": model_type
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_has_expected_adapter_and_model_counts() {
        assert_eq!(adapters().len(), 19);
        assert_eq!(
            adapters()
                .iter()
                .map(|entry| entry["models"].as_array().unwrap().len())
                .sum::<usize>(),
            318
        );
        let ids = adapters()
            .iter()
            .map(|entry| entry["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            [
                "chatgpt",
                "chatgpt_text",
                "claude_text",
                "deepseek_text",
                "doubao",
                "doubao_text",
                "gemini",
                "gemini_biz",
                "gemini_biz_text",
                "gemini_text",
                "google_flow",
                "lmarena",
                "lmarena_text",
                "nanobananafree_ai",
                "sora",
                "test",
                "zai_is",
                "zai_is_text",
                "zenmux_ai_text"
            ]
        );
    }

    #[test]
    fn whitelist_blacklist_and_malformed_filter_match_registry_semantics() {
        let whitelist = json!({"backend":{"adapter":{"lmarena":{"modelFilter":{"mode":"whitelist","list":["a"]}}}}});
        assert!(model_enabled(&whitelist, "lmarena", "a"));
        assert!(!model_enabled(&whitelist, "lmarena", "b"));

        let blacklist = json!({"backend":{"adapter":{"lmarena":{"modelFilter":{"mode":"blacklist","list":["a"]}}}}});
        assert!(!model_enabled(&blacklist, "lmarena", "a"));
        assert!(model_enabled(&blacklist, "lmarena", "b"));
        assert!(model_enabled(&json!({}), "lmarena", "a"));
        let malformed = json!({"backend":{"adapter":{"lmarena":{"modelFilter":{"mode":"whitelist","list":"a"}}}}});
        assert!(model_enabled(&malformed, "lmarena", "a"));
    }

    #[test]
    fn aggregate_includes_qualified_ids_and_deduplicates_duplicate_workers() {
        let models = aggregate_models(&["lmarena".to_owned(), "lmarena".to_owned()], &Value::Null);
        let data = models["data"].as_array().unwrap();
        let ids = data
            .iter()
            .map(|model| model["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        let unique = ids.iter().copied().collect::<HashSet<_>>();
        assert_eq!(ids.len(), unique.len());
        assert_eq!(data.len(), 112);
        assert!(data[..56]
            .iter()
            .all(|model| model["owned_by"] == "internal_server"));
        assert!(data[56..].iter().all(|model| {
            model["owned_by"] == "lmarena" && model["id"].as_str().unwrap().starts_with("lmarena/")
        }));
    }

    #[test]
    fn model_metadata_defaults_and_response_timestamp_are_applied() {
        assert_eq!(model_type("chatgpt", "gpt-image-1.5"), "image");
        assert_eq!(image_policy("chatgpt", "gpt-image-1.5"), "optional");
        assert_eq!(model_type("missing", "missing"), "image");
        assert_eq!(image_policy("missing", "missing"), "optional");

        let models = models_for_adapter("chatgpt_text", &Value::Null);
        let model = &models["data"][0];
        assert_eq!(model["type"], "text");
        assert_eq!(model["image_policy"], "optional");
        assert!(model["created"].as_i64().unwrap() > 0);
        assert_eq!(
            lookup_model("zenmux_ai_text", "gemini-3-flash-preview").unwrap()["providers"],
            json!(["google-vertex"])
        );
    }

    #[test]
    fn adapter_listing_preserves_unknown_metadata_and_adds_configured_filter() {
        let config = json!({"backend":{"adapter":{"chatgpt_text":{"modelFilter":{"mode":"whitelist","list":["gpt-instant"]}}}}});
        let listed = list_adapters(&config);
        let chatgpt = listed
            .iter()
            .find(|entry| entry["id"] == "chatgpt_text")
            .unwrap();
        assert_eq!(chatgpt["configSchema"][0]["key"], "temporaryChat");
        assert!(chatgpt["navigationHandlers"].is_array());
        assert_eq!(chatgpt["modelFilter"]["mode"], "whitelist");
        assert_eq!(chatgpt["models"][0]["codeName"], "Instant");
    }
}
