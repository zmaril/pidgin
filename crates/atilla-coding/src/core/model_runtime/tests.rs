// straitjacket-allow-file:duplication — these cases are translated verbatim from
// pi's `test/model-registry.test.ts`; the repeated `models.json` fixture setup
// and per-provider model assertions are deliberate parallel structure.
//! Tests for [`ModelRuntime`] and [`ModelRegistry`], translated from pi's
//! `test/model-registry.test.ts` (1934 lines),
//! `test/model-runtime-modify-models-compat.test.ts` (201 lines), and the
//! reachable slices of `test/model-runtime-auth-options.test.ts`.
//!
//! Reachable through the ported (credential-blind, synchronous) surface:
//! provider composition (baseUrl/headers/custom-models/modelOverrides/compat),
//! error aggregation, `reload_config`/`refresh`, the registration lifecycle, the
//! registry facade, `get_provider_auth_status`, and the available-snapshot
//! filter. Assertions exercising deferred behavior (credential resolution via
//! `get_api_key_and_headers`/`get_auth`, streaming/`completeSimple`, live
//! `refreshModels`, env-based `checkAuth` availability) are listed in the port
//! report as deferred-to-rich-Provider.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use atilla_ai::get_supported_thinking_levels;
use atilla_ai::providers::registry::{
    create_provider, ApiRouting, CreateProviderOptions, ProviderAuth, RegistryProvider,
};
use atilla_ai::types::{Modality, Model, ModelCost, ModelThinkingLevel};
use serde_json::{json, Value};
use tempfile::{tempdir, TempDir};

use super::*;
use crate::core::auth::auth_storage::AuthStorage;
use crate::core::model_registry::ModelRegistry;
use crate::core::provider_composer::{AuthSource, ExtensionModelConfig};

/// Serializes tests that mutate process-global environment variables and the
/// shared config-value command cache.
static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

/// A runtime plus the temp dir / models.json path backing it, so a test can
/// rewrite the config and `reload_config`.
struct Fixture {
    _dir: TempDir,
    models_path: PathBuf,
    registry: ModelRegistry,
}

impl Fixture {
    fn rewrite(&mut self, providers: Value) {
        fs::write(
            &self.models_path,
            json!({ "providers": providers }).to_string(),
        )
        .unwrap();
    }

    fn runtime(&self) -> &ModelRuntime {
        self.registry.runtime()
    }
}

/// Build a runtime over a fresh temp `models.json` whose `providers` object is
/// `providers`, mirroring the test-utils `createModelRegistry(authStorage, path)`
/// with `allowModelNetwork: false`.
fn fixture(providers: Value) -> Fixture {
    let dir = tempdir().unwrap();
    let models_path = dir.path().join("models.json");
    fs::write(&models_path, json!({ "providers": providers }).to_string()).unwrap();
    let auth_path = dir.path().join("auth.json");
    let credentials = Arc::new(AuthStorage::create(auth_path.to_str()));
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(credentials),
        models_path: ModelsPath::Path(models_path.to_str().unwrap().to_string()),
        allow_model_network: Some(false),
        ..Default::default()
    });
    Fixture {
        _dir: dir,
        models_path,
        registry: ModelRegistry::new(runtime),
    }
}

fn models_for(registry: &ModelRegistry, provider: &str) -> Vec<Model> {
    registry
        .get_all()
        .into_iter()
        .filter(|model| model.provider == provider)
        .collect()
}

fn zero_cost() -> ModelCost {
    ModelCost {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        tiers: None,
    }
}

fn ext_model(id: &str, name: &str) -> ExtensionModelConfig {
    ExtensionModelConfig {
        id: id.to_string(),
        name: name.to_string(),
        api: None,
        base_url: None,
        reasoning: false,
        thinking_level_map: None,
        input: vec![Modality::Text],
        cost: zero_cost(),
        context_window: 128_000,
        max_tokens: 4096,
        headers: None,
        compat: None,
    }
}

/// The registry test's `providerConfig(baseUrl, models, api)` for
/// `register_provider`.
fn provider_config(base_url: &str, model_ids: &[&str], api: &str) -> ProviderConfigInput {
    ProviderConfigInput {
        base_url: Some(base_url.to_string()),
        api_key: Some("test-key".to_string()),
        api: Some(api.to_string()),
        models: Some(
            model_ids
                .iter()
                .map(|id| ExtensionModelConfig {
                    context_window: 100_000,
                    max_tokens: 8000,
                    ..ext_model(id, id)
                })
                .collect(),
        ),
        ..Default::default()
    }
}

fn compat_of<'a>(models: &'a [Model], id: &str) -> Option<&'a Value> {
    models
        .iter()
        .find(|m| m.id == id)
        .and_then(|m| m.compat.as_ref())
}

// ===========================================================================
// baseUrl override (no custom models)
// ===========================================================================

// model-registry.test.ts:100 / :113 — a baseUrl-only override keeps every
// built-in model and rewrites their baseUrl.
#[test]
fn base_url_override_keeps_and_rewrites_builtin_models() {
    let fx = fixture(json!({ "anthropic": { "baseUrl": "https://my-proxy.example.com/v1" } }));
    let models = models_for(&fx.registry, "anthropic");
    assert!(models.len() > 1);
    assert!(models.iter().any(|m| m.id.contains("claude")));
    assert!(models
        .iter()
        .all(|m| m.base_url == "https://my-proxy.example.com/v1"));
}

// model-registry.test.ts:182 — a baseUrl override on one provider leaves others
// untouched.
#[test]
fn base_url_override_does_not_affect_other_providers() {
    let fx = fixture(json!({ "anthropic": { "baseUrl": "https://my-proxy.example.com/v1" } }));
    let google = models_for(&fx.registry, "google");
    assert!(!google.is_empty());
    assert_ne!(google[0].base_url, "https://my-proxy.example.com/v1");
}

// model-registry.test.ts:195 — mix a baseUrl-only override with a models merge.
#[test]
fn mixes_base_url_override_and_models_merge() {
    let fx = fixture(json!({
        "anthropic": { "baseUrl": "https://anthropic-proxy.example.com/v1" },
        "google": {
            "baseUrl": "https://google-proxy.example.com/v1",
            "apiKey": "test-key",
            "api": "google-generative-ai",
            "models": [{ "id": "gemini-custom", "name": "gemini-custom", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 100000, "maxTokens": 8000 }],
        },
    }));
    let anthropic = models_for(&fx.registry, "anthropic");
    assert!(anthropic.len() > 1);
    assert_eq!(
        anthropic[0].base_url,
        "https://anthropic-proxy.example.com/v1"
    );
    let google = models_for(&fx.registry, "google");
    assert!(google.len() > 1);
    assert!(google.iter().any(|m| m.id == "gemini-custom"));
}

// model-registry.test.ts:220 — refresh() picks up a changed baseUrl override.
#[test]
fn refresh_picks_up_base_url_override_changes() {
    let mut fx =
        fixture(json!({ "anthropic": { "baseUrl": "https://first-proxy.example.com/v1" } }));
    assert_eq!(
        models_for(&fx.registry, "anthropic")[0].base_url,
        "https://first-proxy.example.com/v1"
    );
    fx.rewrite(json!({ "anthropic": { "baseUrl": "https://second-proxy.example.com/v1" } }));
    fx.registry.refresh();
    assert_eq!(
        models_for(&fx.registry, "anthropic")[0].base_url,
        "https://second-proxy.example.com/v1"
    );
}

// model-registry.test.ts:146 — a headers-only override composes without error.
#[test]
fn headers_only_override_has_no_error() {
    let fx = fixture(json!({ "anthropic": { "headers": { "X-Custom-Header": "custom-value" } } }));
    assert_eq!(fx.registry.get_error(), None);
    assert!(!models_for(&fx.registry, "anthropic").is_empty());
}

// ===========================================================================
// custom models merge behavior
// ===========================================================================

// model-registry.test.ts:239 — a built-in provider's custom model inherits api
// and baseUrl from the built-ins.
#[test]
fn custom_model_inherits_api_and_base_url() {
    let fx = fixture(json!({
        "openrouter": {
            "models": [{ "id": "fake-provider/fake-model", "name": "Fake model", "reasoning": true, "input": ["text"] }],
        },
    }));
    assert_eq!(fx.registry.get_error(), None);
    let model = fx
        .registry
        .find("openrouter", "fake-provider/fake-model")
        .unwrap();
    assert_eq!(model.api, "openai-completions");
    assert_eq!(model.base_url, "https://openrouter.ai/api/v1");
}

// model-registry.test.ts:264 — a non-built-in provider's custom model still
// requires baseUrl.
#[test]
fn non_builtin_custom_model_requires_base_url() {
    let fx = fixture(json!({
        "my-custom-provider": {
            "apiKey": "test-key",
            "models": [{ "id": "my-model", "api": "openai-completions", "reasoning": false, "input": ["text"] }],
        },
    }));
    assert!(fx.registry.get_error().unwrap().contains("baseUrl"));
}

// model-registry.test.ts:283 — every provider composition error is reported.
#[test]
fn reports_every_provider_composition_error() {
    let fx = fixture(json!({
        "broken-one": { "api": "openai-completions", "models": [{ "id": "one" }] },
        "broken-two": { "api": "openai-completions", "models": [{ "id": "two" }] },
    }));
    let error = fx.registry.get_error().unwrap();
    assert!(error.contains("Provider \"broken-one\""));
    assert!(error.contains("Provider \"broken-two\""));
}

// model-registry.test.ts:296 / :309 — a custom provider with a built-in name
// merges, and a custom model with a built-in id replaces it by id.
#[test]
fn custom_provider_merges_and_replaces_by_id() {
    let fx = fixture(json!({
        "anthropic": {
            "baseUrl": "https://my-proxy.example.com/v1",
            "apiKey": "test-key",
            "api": "anthropic-messages",
            "models": [{ "id": "claude-custom", "name": "claude-custom", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 100000, "maxTokens": 8000 }],
        },
    }));
    let anthropic = models_for(&fx.registry, "anthropic");
    assert!(anthropic.len() > 1);
    assert!(anthropic.iter().any(|m| m.id == "claude-custom"));
    assert!(anthropic.iter().any(|m| m.id.contains("claude")));

    let fx2 = fixture(json!({
        "openrouter": {
            "baseUrl": "https://my-proxy.example.com/v1",
            "apiKey": "test-key",
            "api": "openai-completions",
            "models": [{ "id": "anthropic/claude-sonnet-4", "name": "anthropic/claude-sonnet-4", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 100000, "maxTokens": 8000 }],
        },
    }));
    let sonnets: Vec<Model> = models_for(&fx2.registry, "openrouter")
        .into_iter()
        .filter(|m| m.id == "anthropic/claude-sonnet-4")
        .collect();
    assert_eq!(sonnets.len(), 1);
    assert_eq!(sonnets[0].base_url, "https://my-proxy.example.com/v1");
}

// model-registry.test.ts:337 — provider-level baseUrl applies to both built-in
// and custom models.
#[test]
fn provider_level_base_url_applies_to_all_models() {
    let fx = fixture(json!({
        "anthropic": {
            "baseUrl": "https://merged-proxy.example.com/v1",
            "apiKey": "test-key",
            "api": "anthropic-messages",
            "models": [{ "id": "claude-custom", "name": "claude-custom", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 100000, "maxTokens": 8000 }],
        },
    }));
    assert!(models_for(&fx.registry, "anthropic")
        .iter()
        .all(|m| m.base_url == "https://merged-proxy.example.com/v1"));
}

// model-registry.test.ts:350 / :380 — provider-level compat applies to custom
// models; model-level compat overrides it.
#[test]
fn compat_provider_and_model_precedence() {
    let fx = fixture(json!({
        "demo": {
            "baseUrl": "https://example.com/v1",
            "apiKey": "DEMO_KEY",
            "api": "openai-completions",
            "compat": { "supportsUsageInStreaming": false, "maxTokensField": "max_tokens" },
            "models": [{ "id": "demo-model", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 1000, "maxTokens": 100 }],
        },
    }));
    let compat = fx
        .registry
        .find("demo", "demo-model")
        .unwrap()
        .compat
        .unwrap();
    assert_eq!(compat.get("supportsUsageInStreaming"), Some(&json!(false)));
    assert_eq!(compat.get("maxTokensField"), Some(&json!("max_tokens")));

    let fx2 = fixture(json!({
        "demo": {
            "baseUrl": "https://example.com/v1",
            "apiKey": "DEMO_KEY",
            "api": "openai-completions",
            "compat": { "supportsUsageInStreaming": false, "maxTokensField": "max_tokens" },
            "models": [{ "id": "demo-model", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 1000, "maxTokens": 100, "compat": { "supportsUsageInStreaming": true, "maxTokensField": "max_completion_tokens" } }],
        },
    }));
    let compat2 = fx2
        .registry
        .find("demo", "demo-model")
        .unwrap()
        .compat
        .unwrap();
    assert_eq!(compat2.get("supportsUsageInStreaming"), Some(&json!(true)));
    assert_eq!(
        compat2.get("maxTokensField"),
        Some(&json!("max_completion_tokens"))
    );
}

// model-registry.test.ts:414 — provider-level compat applies to built-in models.
#[test]
fn provider_level_compat_applies_to_builtin_models() {
    let fx = fixture(json!({
        "openrouter": { "compat": { "supportsUsageInStreaming": false, "supportsStrictMode": false } },
    }));
    let models = models_for(&fx.registry, "openrouter");
    assert!(!models.is_empty());
    for model in &models {
        let compat = model.compat.as_ref().unwrap();
        assert_eq!(compat.get("supportsUsageInStreaming"), Some(&json!(false)));
        assert_eq!(compat.get("supportsStrictMode"), Some(&json!(false)));
    }
}

// model-registry.test.ts:435 / :472 — thinkingLevelMap plus assorted compat
// flags round-trip through composition.
#[test]
fn model_schema_thinking_level_map_and_compat_flags() {
    let fx = fixture(json!({
        "demo": {
            "baseUrl": "https://example.com/v1",
            "apiKey": "DEMO_KEY",
            "api": "openai-completions",
            "models": [{ "id": "demo-model", "reasoning": true, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 1000, "maxTokens": 100, "thinkingLevelMap": { "minimal": null, "high": "max" }, "compat": { "supportsStrictMode": false, "cacheControlFormat": "anthropic" } }],
        },
    }));
    assert_eq!(fx.registry.get_error(), None);
    let model = fx.registry.find("demo", "demo-model").unwrap();
    let tlm = model.thinking_level_map.unwrap();
    assert_eq!(tlm.get(&ModelThinkingLevel::Minimal), Some(&None));
    assert_eq!(
        tlm.get(&ModelThinkingLevel::High),
        Some(&Some("max".to_string()))
    );
    let compat = model.compat.unwrap();
    assert_eq!(compat.get("supportsStrictMode"), Some(&json!(false)));
    assert_eq!(compat.get("cacheControlFormat"), Some(&json!("anthropic")));
}

// model-registry.test.ts:567 — model-level baseUrl overrides provider-level
// baseUrl for custom models.
#[test]
fn model_level_base_url_overrides_provider_level() {
    let fx = fixture(json!({
        "opencode-go": {
            "baseUrl": "https://opencode.ai/zen/go/v1",
            "apiKey": "TEST_KEY",
            "models": [
                { "id": "minimax-m2.5", "api": "anthropic-messages", "baseUrl": "https://opencode.ai/zen/go", "reasoning": true, "input": ["text"], "cost": { "input": 0.3, "output": 1.2, "cacheRead": 0.03, "cacheWrite": 0 }, "contextWindow": 204800, "maxTokens": 131072 },
                { "id": "glm-5", "api": "openai-completions", "reasoning": true, "input": ["text"], "cost": { "input": 1, "output": 3.2, "cacheRead": 0.2, "cacheWrite": 0 }, "contextWindow": 204800, "maxTokens": 131072 },
            ],
        },
    }));
    assert_eq!(
        fx.registry
            .find("opencode-go", "minimax-m2.5")
            .unwrap()
            .base_url,
        "https://opencode.ai/zen/go"
    );
    assert_eq!(
        fx.registry.find("opencode-go", "glm-5").unwrap().base_url,
        "https://opencode.ai/zen/go/v1"
    );
}

// model-registry.test.ts:604 — modelOverrides still apply when the provider also
// defines models.
#[test]
fn model_overrides_apply_alongside_defined_models() {
    let fx = fixture(json!({
        "openrouter": {
            "baseUrl": "https://my-proxy.example.com/v1",
            "apiKey": "OPENROUTER_API_KEY",
            "api": "openai-completions",
            "models": [{ "id": "custom/openrouter-model", "name": "Custom OpenRouter Model", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 128000, "maxTokens": 16384 }],
            "modelOverrides": { "anthropic/claude-sonnet-4": { "name": "Overridden Built-in Sonnet" } },
        },
    }));
    let models = models_for(&fx.registry, "openrouter");
    assert!(models.iter().any(|m| m.id == "custom/openrouter-model"));
    assert!(models
        .iter()
        .any(|m| m.id == "anthropic/claude-sonnet-4" && m.name == "Overridden Built-in Sonnet"));
}

// model-registry.test.ts:638 / :657 — refresh() reloads merged custom models and
// removing them restores the built-ins.
#[test]
fn refresh_reloads_and_restores_custom_models() {
    let mut fx = fixture(json!({
        "anthropic": { "baseUrl": "https://first-proxy.example.com/v1", "apiKey": "test-key", "api": "anthropic-messages", "models": [{ "id": "claude-custom", "name": "claude-custom", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 100000, "maxTokens": 8000 }] },
    }));
    assert!(models_for(&fx.registry, "anthropic")
        .iter()
        .any(|m| m.id == "claude-custom"));

    fx.rewrite(json!({
        "anthropic": { "baseUrl": "https://second-proxy.example.com/v1", "apiKey": "test-key", "api": "anthropic-messages", "models": [{ "id": "claude-custom-2", "name": "claude-custom-2", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 100000, "maxTokens": 8000 }] },
    }));
    fx.registry.refresh();
    let anthropic = models_for(&fx.registry, "anthropic");
    assert!(!anthropic.iter().any(|m| m.id == "claude-custom"));
    assert!(anthropic.iter().any(|m| m.id == "claude-custom-2"));
    assert!(anthropic.iter().any(|m| m.id.contains("claude")));

    fx.rewrite(json!({}));
    fx.registry.refresh();
    let anthropic = models_for(&fx.registry, "anthropic");
    assert!(!anthropic.iter().any(|m| m.id == "claude-custom-2"));
    assert!(anthropic.iter().any(|m| m.id.contains("claude")));
}

// ===========================================================================
// modelOverrides (per-model customization)
// ===========================================================================

// model-registry.test.ts:675 — a model override applies to a single built-in
// model and leaves others unchanged.
#[test]
fn model_override_applies_to_single_model() {
    let fx = fixture(json!({
        "openrouter": { "modelOverrides": { "anthropic/claude-sonnet-4": { "name": "Custom Sonnet Name" } } },
    }));
    let models = models_for(&fx.registry, "openrouter");
    assert_eq!(
        models
            .iter()
            .find(|m| m.id == "anthropic/claude-sonnet-4")
            .unwrap()
            .name,
        "Custom Sonnet Name"
    );
    assert_ne!(
        models
            .iter()
            .find(|m| m.id == "anthropic/claude-opus-4")
            .map(|m| m.name.as_str()),
        Some("Custom Sonnet Name")
    );
}

// model-registry.test.ts:718 / :740 — compat overrides deep-merge and are
// independent per model.
#[test]
fn model_override_compat_deep_merge_and_independence() {
    let fx = fixture(json!({
        "openrouter": {
            "modelOverrides": {
                "anthropic/claude-sonnet-4": { "compat": { "openRouterRouting": { "only": ["amazon-bedrock"] } } },
                "anthropic/claude-opus-4": { "compat": { "openRouterRouting": { "only": ["anthropic"] } } },
            },
        },
    }));
    let models = models_for(&fx.registry, "openrouter");
    assert_eq!(
        compat_of(&models, "anthropic/claude-sonnet-4")
            .unwrap()
            .get("openRouterRouting"),
        Some(&json!({ "only": ["amazon-bedrock"] }))
    );
    assert_eq!(
        compat_of(&models, "anthropic/claude-opus-4")
            .unwrap()
            .get("openRouterRouting"),
        Some(&json!({ "only": ["anthropic"] }))
    );
}

// model-registry.test.ts:766 — a model override combines with a baseUrl override.
#[test]
fn model_override_combined_with_base_url_override() {
    let fx = fixture(json!({
        "openrouter": {
            "baseUrl": "https://my-proxy.example.com/v1",
            "modelOverrides": { "anthropic/claude-sonnet-4": { "name": "Proxied Sonnet" } },
        },
    }));
    let models = models_for(&fx.registry, "openrouter");
    let sonnet = models
        .iter()
        .find(|m| m.id == "anthropic/claude-sonnet-4")
        .unwrap();
    assert_eq!(sonnet.base_url, "https://my-proxy.example.com/v1");
    assert_eq!(sonnet.name, "Proxied Sonnet");
    let opus = models
        .iter()
        .find(|m| m.id == "anthropic/claude-opus-4")
        .unwrap();
    assert_eq!(opus.base_url, "https://my-proxy.example.com/v1");
    assert_ne!(opus.name, "Proxied Sonnet");
}

// model-registry.test.ts:792 — an override for an unknown model id is ignored.
#[test]
fn model_override_for_unknown_id_is_ignored() {
    let fx = fixture(json!({
        "openrouter": { "modelOverrides": { "nonexistent/model-id": { "name": "This should not appear" } } },
    }));
    let models = models_for(&fx.registry, "openrouter");
    assert!(!models.iter().any(|m| m.id == "nonexistent/model-id"));
    assert_eq!(fx.registry.get_error(), None);
}

// model-registry.test.ts:812 — a partial cost override merges over the built-in.
#[test]
fn model_override_partial_cost() {
    let fx = fixture(json!({
        "openrouter": { "modelOverrides": { "anthropic/claude-sonnet-4": { "cost": { "input": 99 } } } },
    }));
    let sonnet = fx
        .registry
        .find("openrouter", "anthropic/claude-sonnet-4")
        .unwrap();
    assert_eq!(sonnet.cost.input, 99.0);
    assert!(sonnet.cost.output > 0.0);
}

// model-registry.test.ts:856 / :889 — refresh() picks up and drops model
// override changes.
#[test]
fn refresh_applies_and_restores_model_overrides() {
    let mut fx = fixture(json!({
        "openrouter": { "modelOverrides": { "anthropic/claude-sonnet-4": { "name": "First Name" } } },
    }));
    assert_eq!(
        fx.registry
            .find("openrouter", "anthropic/claude-sonnet-4")
            .unwrap()
            .name,
        "First Name"
    );
    fx.rewrite(json!({
        "openrouter": { "modelOverrides": { "anthropic/claude-sonnet-4": { "name": "Second Name" } } },
    }));
    fx.registry.refresh();
    assert_eq!(
        fx.registry
            .find("openrouter", "anthropic/claude-sonnet-4")
            .unwrap()
            .name,
        "Second Name"
    );
    fx.rewrite(json!({}));
    fx.registry.refresh();
    assert_ne!(
        fx.registry
            .find("openrouter", "anthropic/claude-sonnet-4")
            .unwrap()
            .name,
        "Second Name"
    );
}

// ===========================================================================
// dynamic provider lifecycle
// ===========================================================================

// model-registry.test.ts:918 — getProviderDisplayName resolves registered,
// OAuth, built-in, and fallback names.
#[test]
fn get_provider_display_name_resolution() {
    let mut fx = fixture(json!({}));
    assert_eq!(fx.registry.get_provider_display_name("openai"), "OpenAI");
    assert_eq!(
        fx.registry.get_provider_display_name("github-copilot"),
        "GitHub Copilot"
    );
    assert_eq!(fx.registry.get_provider_display_name("zai"), "Z.AI");
    assert_eq!(
        fx.registry.get_provider_display_name("unknown-provider"),
        "unknown-provider"
    );

    fx.registry
        .register_provider(
            "named-provider",
            ProviderConfigInput {
                name: Some("Named Provider".to_string()),
                base_url: Some("https://provider.test/v1".to_string()),
                api_key: Some("test-key".to_string()),
                api: Some("openai-completions".to_string()),
                models: Some(vec![ext_model("demo-model", "Demo Model")]),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        fx.registry.get_provider_display_name("named-provider"),
        "Named Provider"
    );
}

// model-registry.test.ts:969 — modelOverrides apply to a dynamically registered
// provider's models (name + thinkingLevelMap + supported levels).
#[test]
fn model_overrides_apply_to_registered_provider() {
    let mut fx = fixture(json!({
        "extension-provider": {
            "modelOverrides": { "extension-model": {
                "name": "Overridden Extension Model",
                "thinkingLevelMap": { "off": null, "minimal": null, "low": null, "medium": null, "xhigh": "max" },
                "headers": { "x-model-override": "enabled" },
            } },
        },
    }));
    fx.registry
        .register_provider(
            "extension-provider",
            ProviderConfigInput {
                base_url: Some("https://provider.test/v1".to_string()),
                api_key: Some("test-key".to_string()),
                api: Some("openai-completions".to_string()),
                models: Some(vec![ExtensionModelConfig {
                    reasoning: true,
                    ..ext_model("extension-model", "Extension Model")
                }]),
                ..Default::default()
            },
        )
        .unwrap();
    let model = fx
        .registry
        .find("extension-provider", "extension-model")
        .unwrap();
    assert_eq!(model.name, "Overridden Extension Model");
    assert_eq!(
        get_supported_thinking_levels(&model),
        vec![ModelThinkingLevel::High, ModelThinkingLevel::Xhigh]
    );
}

// model-registry.test.ts:1115 — a failed registerProvider throws and does not
// persist the invalid config; a later refresh still succeeds.
#[test]
fn failed_register_provider_does_not_persist_invalid_stream_simple() {
    let mut fx = fixture(json!({}));
    let err = fx
        .registry
        .register_provider(
            "broken-provider",
            ProviderConfigInput {
                stream_simple: true,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(
        err.0,
        "Provider broken-provider: \"api\" is required when registering streamSimple."
    );
    assert!(fx
        .registry
        .get_registered_provider_config("broken-provider")
        .is_none());
    fx.registry.refresh();
}

// model-registry.test.ts:1129 — a failed re-registration does not remove the
// existing provider's models.
#[test]
fn failed_register_provider_keeps_existing_models() {
    let mut fx = fixture(json!({}));
    fx.registry
        .register_provider(
            "demo-provider",
            ProviderConfigInput {
                base_url: Some("https://provider.test/v1".to_string()),
                api_key: Some("test-key".to_string()),
                api: Some("openai-completions".to_string()),
                models: Some(vec![ext_model("demo-model", "Demo Model")]),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(fx.registry.find("demo-provider", "demo-model").is_some());

    let err = fx
        .registry
        .register_provider(
            "demo-provider",
            ProviderConfigInput {
                base_url: Some("https://provider.test/v2".to_string()),
                api_key: Some("test-key".to_string()),
                models: Some(vec![ext_model("broken-model", "Broken Model")]),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(err
        .0
        .contains("Provider demo-provider, model broken-model: no \"api\" specified."));
    assert!(fx.registry.find("demo-provider", "demo-model").is_some());
    fx.registry.refresh();
    assert!(fx.registry.find("demo-provider", "demo-model").is_some());
}

// model-registry.test.ts:1174 — unregisterProvider removes the runtime overlay
// config.
#[test]
fn unregister_provider_removes_overlay_config() {
    let mut fx = fixture(json!({}));
    fx.registry
        .register_provider(
            "anthropic",
            ProviderConfigInput {
                base_url: Some("https://proxy.test/anthropic".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(fx
        .registry
        .get_registered_provider_config("anthropic")
        .is_some());
    fx.registry.unregister_provider("anthropic");
    assert!(fx
        .registry
        .get_registered_provider_config("anthropic")
        .is_none());
}

// model-registry.test.ts:1228-1319 — dynamic provider override persistence across
// refresh (baseUrl-only keeps builtins; models replace; custom survives).
#[test]
fn dynamic_provider_override_persistence() {
    // baseUrl-only override keeps built-in provider models after refresh.
    let mut fx = fixture(json!({}));
    fx.registry
        .register_provider(
            "anthropic",
            ProviderConfigInput {
                base_url: Some("https://proxy.test/anthropic".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    fx.registry.refresh();
    let anthropic = models_for(&fx.registry, "anthropic");
    assert!(anthropic.len() > 1);
    assert!(anthropic
        .iter()
        .all(|m| m.base_url == "https://proxy.test/anthropic"));

    // models-only + baseUrl override replaces built-in provider models.
    let mut fx2 = fixture(json!({}));
    fx2.registry
        .register_provider(
            "anthropic",
            ProviderConfigInput {
                base_url: Some("https://custom.test/anthropic".to_string()),
                ..provider_config(
                    "https://custom.test/anthropic",
                    &["custom-claude"],
                    "anthropic-messages",
                )
            },
        )
        .unwrap();
    fx2.registry.refresh();
    assert_eq!(
        models_for(&fx2.registry, "anthropic")
            .iter()
            .map(|m| m.id.clone())
            .collect::<Vec<_>>(),
        vec!["custom-claude"]
    );

    // models-only custom provider registration survives refresh.
    let mut fx3 = fixture(json!({}));
    fx3.registry
        .register_provider(
            "custom-provider",
            provider_config(
                "https://custom.test/v1",
                &["custom-a", "custom-b"],
                "openai-completions",
            ),
        )
        .unwrap();
    fx3.registry.refresh();
    assert_eq!(
        models_for(&fx3.registry, "custom-provider")
            .iter()
            .map(|m| m.id.clone())
            .collect::<Vec<_>>(),
        vec!["custom-a", "custom-b"]
    );
}

// ===========================================================================
// provider auth status  (reachable, credential-blind)
// ===========================================================================

fn provider_with_api_key(api_key: &str) -> Value {
    json!({
        "custom-provider": {
            "baseUrl": "https://example.com/v1",
            "apiKey": api_key,
            "api": "anthropic-messages",
            "models": [{ "id": "test-model", "name": "Test Model", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 100000, "maxTokens": 8000 }],
        },
    })
}

// model-registry.test.ts:1643 / :1670 — env-var apiKey (plain and interpolated)
// reports source "environment" with the env-var label.
#[test]
fn auth_status_reports_env_api_key() {
    let _guard = ENV_TEST_LOCK.lock().unwrap();
    std::env::set_var("TEST_API_KEY_STATUS_98765", "status-test-key");
    let fx = fixture(provider_with_api_key("$TEST_API_KEY_STATUS_98765"));
    let status = fx.runtime().get_provider_auth_status("custom-provider");
    assert!(status.configured);
    assert_eq!(status.source, Some(AuthSource::Environment));
    assert_eq!(status.label.as_deref(), Some("TEST_API_KEY_STATUS_98765"));
    std::env::remove_var("TEST_API_KEY_STATUS_98765");

    std::env::set_var("TEST_STATUS_A_98765", "left");
    std::env::set_var("TEST_STATUS_B_98765", "right");
    let fx2 = fixture(provider_with_api_key(
        "${TEST_STATUS_A_98765}_${TEST_STATUS_B_98765}",
    ));
    let status2 = fx2.runtime().get_provider_auth_status("custom-provider");
    assert!(status2.configured);
    assert_eq!(
        status2.label.as_deref(),
        Some("TEST_STATUS_A_98765, TEST_STATUS_B_98765")
    );
    std::env::remove_var("TEST_STATUS_A_98765");
    std::env::remove_var("TEST_STATUS_B_98765");
}

// model-registry.test.ts:1705 / :1741 — a literal apiKey reports source
// "models_json_key"; a command apiKey reports "models_json_command" without
// executing.
#[test]
fn auth_status_literal_and_command_keys() {
    let _guard = ENV_TEST_LOCK.lock().unwrap();
    let fx = fixture(provider_with_api_key("literal_api_key_value"));
    let status = fx.runtime().get_provider_auth_status("custom-provider");
    assert!(status.configured);
    assert_eq!(status.source, Some(AuthSource::ModelsJsonKey));

    let fx2 = fixture(provider_with_api_key("!echo key-value"));
    let status2 = fx2.runtime().get_provider_auth_status("custom-provider");
    assert!(status2.configured);
    assert_eq!(status2.source, Some(AuthSource::ModelsJsonCommand));
}

// model-registry.test.ts:1718 — a missing env apiKey keeps the provider
// unconfigured and out of the available snapshot.
#[test]
fn auth_status_missing_env_key_is_unavailable() {
    let _guard = ENV_TEST_LOCK.lock().unwrap();
    std::env::remove_var("TEST_API_KEY_MISSING_98765");
    let fx = fixture(provider_with_api_key("$TEST_API_KEY_MISSING_98765"));
    let status = fx.runtime().get_provider_auth_status("custom-provider");
    assert!(!status.configured);
    assert_eq!(status.source, None);
    assert!(!fx
        .registry
        .get_available()
        .iter()
        .any(|m| m.provider == "custom-provider"));
}

// model-registry.test.ts:1788 — a configured (command-backed) provider is in the
// available snapshot without the command ever running.
#[test]
fn available_snapshot_includes_command_configured_provider() {
    let _guard = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempdir().unwrap();
    let counter = dir.path().join("counter");
    fs::write(&counter, "0").unwrap();
    let command = format!(
        "!sh -c 'count=$(cat \"{p}\"); echo $((count + 1)) > \"{p}\"; echo key-value'",
        p = counter.to_str().unwrap()
    );
    let fx = fixture(provider_with_api_key(&command));
    assert!(fx
        .registry
        .get_available()
        .iter()
        .any(|m| m.provider == "custom-provider"));
    assert_eq!(fs::read_to_string(&counter).unwrap().trim(), "0");
}

// ===========================================================================
// native provider lifecycle  (modify-models-compat.test.ts, reachable subset)
// ===========================================================================

fn native_provider(id: &str, base_url: &str, model_id: &str) -> RegistryProvider {
    let model = Model {
        id: model_id.to_string(),
        name: model_id.to_string(),
        api: "openai-completions".to_string(),
        provider: id.to_string(),
        base_url: base_url.to_string(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![Modality::Text],
        cost: zero_cost(),
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        compat: None,
    };
    create_provider(CreateProviderOptions {
        id: id.to_string(),
        name: Some("Extension Native".to_string()),
        base_url: Some(base_url.to_string()),
        headers: None,
        auth: ProviderAuth::default(),
        models: vec![model],
        fetch_models: None,
        filter_models: None,
        api: ApiRouting::Unimplemented,
    })
}

// modify-models-compat.test.ts:26 — a native provider registers and its identity
// is visible through the registry, then unregisters cleanly.
#[test]
fn native_provider_registration_lifecycle() {
    let mut fx = fixture(json!({}));
    fx.registry
        .register_native_provider(native_provider(
            "extension-native",
            "https://fallback.test/v1",
            "native",
        ))
        .unwrap();

    assert_eq!(
        fx.registry.get_provider("extension-native").map(|p| p.id()),
        Some("extension-native")
    );
    assert!(fx
        .registry
        .get_registered_native_provider("extension-native")
        .is_some());
    assert!(fx
        .registry
        .get_registered_provider_ids()
        .contains(&"extension-native".to_string()));
    assert!(fx.registry.find("extension-native", "native").is_some());

    // With no overlay the runtime reuses the native provider Arc verbatim.
    let via_get = fx.registry.get_provider("extension-native").unwrap();
    let via_native = fx
        .registry
        .get_registered_native_provider("extension-native")
        .unwrap();
    assert!(Arc::ptr_eq(via_get, via_native));

    fx.registry.unregister_provider("extension-native");
    assert!(fx.registry.get_provider("extension-native").is_none());
}

// modify-models-compat.test.ts:87 — models.json modelOverrides layer above a
// native provider.
#[test]
fn models_json_overrides_apply_above_native_provider() {
    let mut fx = fixture(json!({
        "extension-native": { "modelOverrides": { "native": { "contextWindow": 4242 } } },
    }));
    fx.registry
        .register_native_provider(native_provider(
            "extension-native",
            "https://native.test/v1",
            "native",
        ))
        .unwrap();
    assert_eq!(
        fx.registry
            .find("extension-native", "native")
            .unwrap()
            .context_window,
        4242
    );
}

// ===========================================================================
// runtime seams / defaults
// ===========================================================================

// Runtime overrides mark a provider configured for the available snapshot.
#[test]
fn runtime_api_key_marks_provider_available() {
    let mut fx = fixture(json!({
        "custom-provider": {
            "baseUrl": "https://example.com/v1",
            "api": "openai-completions",
            "models": [{ "id": "m", "name": "m", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 1000, "maxTokens": 100 }],
        },
    }));
    assert!(!fx.runtime().has_configured_auth("custom-provider"));
    fx.registry
        .runtime_mut()
        .set_runtime_api_key("custom-provider", "runtime-key");
    assert!(fx.runtime().has_configured_auth("custom-provider"));
    assert_eq!(
        fx.runtime()
            .get_provider_auth_status("custom-provider")
            .source,
        Some(AuthSource::Runtime)
    );
    fx.registry
        .runtime_mut()
        .remove_runtime_api_key("custom-provider");
    assert!(!fx.runtime().has_configured_auth("custom-provider"));
}

// A disabled models path yields an in-memory store and composes the builtins.
#[test]
fn disabled_models_path_builds_builtin_only_runtime() {
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(Default::default()))),
        models_path: ModelsPath::Disabled,
        allow_model_network: Some(false),
        ..Default::default()
    });
    assert_eq!(runtime.get_error(), None);
    assert!(runtime.get_model("anthropic", "claude-haiku-4-5").is_some());
    assert!(!runtime.allow_model_network());
}
