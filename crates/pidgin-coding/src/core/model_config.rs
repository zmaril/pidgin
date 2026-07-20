// straitjacket-allow-file:duplication — a faithful transcription of pi's
// `model-config.ts` TypeBox schema: the model/override/provider structs are
// walls of near-identical `Option<T>` fields carrying the same
// skip-serializing-if serde attribute. The clone detector reads these repeated
// optional-field runs as duplicates; they are distinct, load-bearing schema
// declarations kept verbatim to mirror the upstream `models.json` shape, exactly
// as `pidgin-ai`'s `types.rs` does for the same reason.
//! Immutable, credential-blind `models.json` snapshot.
//!
//! Ported from pi's `core/model-config.ts` — the parse/validate/error layer
//! only. It reads the raw `models.json`, strips JSON comments and trailing
//! commas, validates the shape, and exposes an immutable per-provider view.
//! Composition (merging file providers with built-in catalogs, credential
//! resolution) lives in pi's `provider-composer.ts` and is deliberately NOT
//! ported here.
//!
//! Error handling mirrors pi exactly: a missing file is silently an empty config
//! (not an error); read failures, JSON syntax errors, and schema violations are
//! captured as an `error` string rather than thrown.
//!
//! Structs reuse `pidgin_ai::types` for the unambiguous fields (`ModelCost`,
//! `ModelCostTier`, `Modality`, `ThinkingLevelMap`).
//
// NOTE: the per-model/provider `compat` field is a union of the three
// `pidgin_ai::types` `*Compat` structs (`OpenAICompletionsCompat`,
// `OpenAIResponsesCompat`, `AnthropicMessagesCompat`). At this snapshot layer pi
// stores the raw parsed object (it does not resolve the union until composition,
// which is out of scope), so `compat` is kept as `serde_json::Value` to preserve
// it losslessly. It should be typed against those `*Compat` structs when the
// composer lands.

use std::collections::BTreeMap;

use indexmap::IndexMap;
use pidgin_ai::types::{Modality, ModelCost, ModelCostTier, ThinkingLevelMap};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A model definition inside a provider block (`model-config.ts:148`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModel {
    /// The model id (required, non-empty).
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<Modality>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<ModelCost>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// Raw provider `compat` object; see the module note on typing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Value>,
}

/// Cost override shape: like [`ModelCost`] but every base rate is optional
/// (`model-config.ts:168`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonCostOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

/// A per-model override applied to a base model (`model-config.ts:163`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModelOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<Modality>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<ModelsJsonCostOverride>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Value>,
}

/// A provider block (`model-config.ts:183`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonProvider {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    /// The only accepted literal is `"radius"` in pi; kept as a string here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<ModelsJsonModel>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_overrides: Option<BTreeMap<String, ModelsJsonModelOverride>>,
}

/// The top-level `models.json` shape (`model-config.ts:196`).
///
/// `providers` is an [`IndexMap`] so it preserves the file's provider order, as
/// pi does: its `ModelsJson` iterates `Object.entries(config.providers)` into a
/// JS `Map`, both of which keep insertion order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ModelsJson {
    providers: IndexMap<String, ModelsJsonProvider>,
}

/// Strip `//` line comments and trailing commas from JSON, leaving string
/// literals untouched (`utils/json.ts:2`).
fn strip_json_comments(input: &str) -> String {
    let comment_re = Regex::new(r#""(?:\\.|[^"\\])*"|//[^\n]*"#).expect("valid comment regex");
    let step1 = comment_re.replace_all(input, |caps: &regex::Captures| {
        let m = &caps[0];
        if m.starts_with('"') {
            m.to_string()
        } else {
            String::new()
        }
    });

    let comma_re =
        Regex::new(r#""(?:\\.|[^"\\])*"|,(\s*[}\]])"#).expect("valid trailing-comma regex");
    comma_re
        .replace_all(&step1, |caps: &regex::Captures| match caps.get(1) {
            Some(tail) => tail.as_str().to_string(),
            None => caps[0].to_string(),
        })
        .into_owned()
}

/// One immutable load of `models.json` (`model-config.ts:226`).
///
/// Immutability is inherent: the parsed providers are owned and only exposed by
/// shared reference, so there is no Rust analogue to pi's `deepFreeze`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelConfig {
    providers: IndexMap<String, ModelsJsonProvider>,
    error: Option<String>,
}

impl ModelConfig {
    /// An empty config with no error (the ENOENT / no-path outcome).
    fn empty() -> Self {
        ModelConfig {
            providers: IndexMap::new(),
            error: None,
        }
    }

    fn errored(error: String) -> Self {
        ModelConfig {
            providers: IndexMap::new(),
            error: Some(error),
        }
    }

    /// Load from an optional filesystem path (`model-config.ts:235`).
    ///
    /// `None` → empty config. A missing file → empty config (no error). Any other
    /// read error → captured error. Parse/schema errors are captured by
    /// [`ModelConfig::parse`].
    //
    // NOTE: pi runs the path through `normalizePath` (tilde expansion, `file:`
    // URLs). That normalization is a caller concern here; the path is read as
    // given.
    pub fn load(models_json_path: Option<&str>) -> ModelConfig {
        let Some(path) = models_json_path else {
            return ModelConfig::empty();
        };
        match std::fs::read_to_string(path) {
            Ok(content) => ModelConfig::parse(&content, path),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => ModelConfig::empty(),
            Err(err) => {
                ModelConfig::errored(format!("Failed to load models.json: {err}\n\nFile: {path}"))
            }
        }
    }

    /// Parse already-read `models.json` bytes (`model-config.ts:249`).
    ///
    /// JSON comments/trailing commas are stripped first. A syntax error yields a
    /// `Failed to parse` error; a shape violation yields an `Invalid models.json
    /// schema` error. `source_label` is echoed into error messages (the file
    /// path in pi).
    pub fn parse(content: &str, source_label: &str) -> ModelConfig {
        let stripped = strip_json_comments(content);

        // Syntax check first (pi's `JSON.parse`): a malformed document is a
        // "Failed to parse" error, distinct from a schema violation below.
        if let Err(err) = serde_json::from_str::<Value>(&stripped) {
            return ModelConfig::errored(format!(
                "Failed to parse models.json: {err}\n\nFile: {source_label}"
            ));
        }

        // Deserialize straight from the JSON token stream rather than from a
        // `serde_json::Value`: without the `preserve_order` feature a `Value`
        // object is a `BTreeMap` that would alphabetize provider keys before the
        // `IndexMap` ever sees them. `from_str` feeds `IndexMap` keys in document
        // order, preserving pi's insertion-order semantics. The syntax check
        // above guarantees any error here is a shape/schema violation.
        let parsed: ModelsJson = match serde_json::from_str(&stripped) {
            Ok(parsed) => parsed,
            Err(err) => {
                return ModelConfig::errored(format!(
                    "Invalid models.json schema:\n  - {err}\n\nFile: {source_label}"
                ));
            }
        };

        ModelConfig {
            providers: parsed.providers,
            error: None,
        }
    }

    /// Look up a provider by id (`model-config.ts:276`).
    pub fn get_provider(&self, provider_id: &str) -> Option<&ModelsJsonProvider> {
        self.providers.get(provider_id)
    }

    /// All provider ids present in the config, in the file's insertion order
    /// (`model-config.ts:280`).
    //
    // Order is pi-faithful: pi returns `[...this.providers.keys()]` from a JS
    // `Map` built by iterating `Object.entries(config.providers)`, so provider
    // ids come back in the order they appear in `models.json`. The backing
    // `IndexMap` preserves that same insertion order here. Downstream selection
    // ("first available" model, first-provider-by-position) depends on it.
    pub fn get_provider_ids(&self) -> Vec<&str> {
        self.providers.keys().map(String::as_str).collect()
    }

    /// The captured load/parse error, if any (`model-config.ts:284`).
    pub fn get_error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_path_is_empty() {
        let config = ModelConfig::load(None);
        assert!(config.get_error().is_none());
        assert!(config.get_provider_ids().is_empty());
    }

    #[test]
    fn missing_file_is_silent_empty() {
        let config = ModelConfig::load(Some("/no/such/dir/models.json"));
        assert!(config.get_error().is_none(), "ENOENT must not be an error");
        assert!(config.get_provider_ids().is_empty());
    }

    #[test]
    fn read_error_is_captured() {
        // Reading a directory as a file fails with a non-NotFound error.
        let dir = std::env::temp_dir();
        let config = ModelConfig::load(Some(dir.to_str().unwrap()));
        assert!(config
            .get_error()
            .unwrap()
            .contains("Failed to load models.json"));
    }

    #[test]
    fn syntax_error_is_captured() {
        let config = ModelConfig::parse("{ not valid json ", "test");
        assert!(config
            .get_error()
            .unwrap()
            .contains("Failed to parse models.json"));
    }

    #[test]
    fn schema_error_is_captured() {
        // Model without the required `id`.
        let config = ModelConfig::parse(r#"{"providers":{"p":{"models":[{"name":"x"}]}}}"#, "test");
        assert!(config
            .get_error()
            .unwrap()
            .contains("Invalid models.json schema"));
    }

    #[test]
    fn comments_and_trailing_commas_are_stripped() {
        let content = r#"{
            // leading comment
            "providers": {
                "anthropic": { "name": "Anthropic", }, // trailing comma above
            },
        }"#;
        let config = ModelConfig::parse(content, "test");
        assert!(
            config.get_error().is_none(),
            "got: {:?}",
            config.get_error()
        );
        assert_eq!(config.get_provider_ids(), ["anthropic"]);
    }

    #[test]
    fn strip_preserves_comment_like_string_contents() {
        // The `//` and `,` inside string literals must survive.
        let stripped = strip_json_comments(r#"{"url":"https://x.dev","list":[1,]}"#);
        assert!(stripped.contains("https://x.dev"));
        assert_eq!(
            serde_json::from_str::<Value>(&stripped).unwrap()["list"],
            serde_json::json!([1])
        );
    }

    #[test]
    fn provider_ids_preserve_file_insertion_order() {
        // pi keeps providers in the order they appear in models.json (JS `Map`
        // built from `Object.entries`). These ids are deliberately NOT
        // alphabetical: under alphabetical (BTreeMap) ordering the result would
        // be [anthropic, openai, zai], which diverges from pi. IndexMap keeps the
        // file order below.
        let content = r#"{
            "providers": {
                "openai": { "name": "OpenAI" },
                "zai": { "name": "Z.ai" },
                "anthropic": { "name": "Anthropic" }
            }
        }"#;
        let config = ModelConfig::parse(content, "test");
        assert!(
            config.get_error().is_none(),
            "got: {:?}",
            config.get_error()
        );
        assert_eq!(config.get_provider_ids(), ["openai", "zai", "anthropic"]);
    }

    #[test]
    fn provider_and_model_lookup_reuses_ai_types() {
        let content = r#"{
            "providers": {
                "anthropic": {
                    "name": "Anthropic",
                    "models": [
                        {
                            "id": "claude-x",
                            "input": ["text", "image"],
                            "cost": { "input": 3, "output": 15, "cacheRead": 0.3, "cacheWrite": 3.75 },
                            "thinkingLevelMap": { "high": "8000", "off": null }
                        }
                    ]
                }
            }
        }"#;
        let config = ModelConfig::parse(content, "test");
        assert!(
            config.get_error().is_none(),
            "got: {:?}",
            config.get_error()
        );

        let provider = config.get_provider("anthropic").expect("provider present");
        assert_eq!(provider.name.as_deref(), Some("Anthropic"));
        let model = &provider.models.as_ref().unwrap()[0];
        assert_eq!(model.id, "claude-x");
        assert_eq!(
            model.input.as_ref().unwrap(),
            &[Modality::Text, Modality::Image]
        );
        let cost: &ModelCost = model.cost.as_ref().unwrap();
        assert_eq!(cost.input, 3.0);
        assert_eq!(cost.cache_write, 3.75);

        assert!(config.get_provider("missing").is_none());
    }
}
