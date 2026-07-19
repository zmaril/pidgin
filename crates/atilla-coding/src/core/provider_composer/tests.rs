// straitjacket-allow-file:duplication — the composer test fixtures build many
// near-identical `models.json` blocks and `Model` literals; these repeated
// setups are distinct behavioral cases translated from pi's
// `model-registry.test.ts`, kept verbatim to pin the composer's semantics.
//! Tests for the provider composer.
//!
//! Two sources feed these cases:
//!
//! 1. Faithful unit tests over the composer's pure functions and the
//!    [`compose_model_provider`] model-layering pipeline, using directly
//!    constructible input/output.
//! 2. Assertions extracted from pi's `test/model-registry.test.ts` (1934 lines)
//!    that pin composer behavior specifically — header resolution, compat
//!    request config, auth-status derivation, and extension-provider validation.
//!    pi has no dedicated `provider-composer.test.ts`; those behaviors are
//!    exercised there through the full `ModelRegistry`/`ModelRuntime`. The line
//!    references below point at the originating registry test.

use std::sync::Mutex;

use atilla_ai::providers::registry::{
    create_provider, ApiRouting, CreateProviderOptions, ProviderAuth, RegistryProvider,
};
use atilla_ai::types::{Modality, Model, ModelCost};
use serde_json::{json, Value};

use super::*;

/// Serializes tests that mutate process-global environment variables and the
/// shared config-value command cache.
static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

fn base_model(id: &str, provider: &str) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: "anthropic-messages".to_string(),
        provider: provider.to_string(),
        base_url: "https://base.example/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![Modality::Text],
        cost: ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.1,
            cache_write: 0.3,
            tiers: None,
        },
        context_window: 200_000,
        max_tokens: 8192,
        headers: None,
        compat: None,
    }
}

fn with_compat(mut model: Model, compat: Value) -> Model {
    model.compat = Some(compat);
    model
}

fn base_provider(id: &str, name: &str, models: Vec<Model>) -> RegistryProvider {
    create_provider(CreateProviderOptions {
        id: id.to_string(),
        name: Some(name.to_string()),
        base_url: Some("https://base.example/v1".to_string()),
        headers: None,
        auth: ProviderAuth::default(),
        models,
        fetch_models: None,
        filter_models: None,
        api: ApiRouting::Unimplemented,
    })
}

/// Parse a single-provider `models.json` and hand back the owned config so the
/// borrowed provider slice can be taken by the caller.
fn config_from(provider_id: &str, block: Value) -> ModelConfig {
    let json = json!({ "providers": { provider_id: block } });
    ModelConfig::parse(&json.to_string(), "test")
}

fn find<'a>(models: &'a [Model], id: &str) -> Option<&'a Model> {
    models.iter().find(|m| m.id == id)
}

// ---------------------------------------------------------------------------
// configured_request_auth_status  (provider-composer.ts:534-548)
// ---------------------------------------------------------------------------

// model-registry.test.ts:1643 — apiKey env var from models.json.
#[test]
fn auth_status_reports_environment_variable() {
    let _guard = ENV_TEST_LOCK.lock().unwrap();
    let var = "TEST_COMPOSER_STATUS_ENV_98765";
    std::env::set_var(var, "status-test-key");

    let config = config_from("p", json!({ "apiKey": format!("${var}") }));
    let status = configured_request_auth_status(config.get_provider("p"), None).unwrap();
    assert_eq!(
        status,
        AuthStatus {
            configured: true,
            source: Some(AuthSource::Environment),
            label: Some(var.to_string()),
        }
    );

    std::env::remove_var(var);
}

// model-registry.test.ts:1670 — interpolated apiKey env vars, comma-joined label.
#[test]
fn auth_status_reports_interpolated_environment_variables() {
    let _guard = ENV_TEST_LOCK.lock().unwrap();
    let var_a = "TEST_COMPOSER_STATUS_A_98765";
    let var_b = "TEST_COMPOSER_STATUS_B_98765";
    std::env::set_var(var_a, "left");
    std::env::set_var(var_b, "right");

    let config = config_from(
        "p",
        json!({ "apiKey": format!("${{{var_a}}}_${{{var_b}}}") }),
    );
    let status = configured_request_auth_status(config.get_provider("p"), None).unwrap();
    assert_eq!(
        status,
        AuthStatus {
            configured: true,
            source: Some(AuthSource::Environment),
            label: Some(format!("{var_a}, {var_b}")),
        }
    );

    std::env::remove_var(var_a);
    std::env::remove_var(var_b);
}

// model-registry.test.ts:1704 — non-env literal apiKey → models_json_key.
#[test]
fn auth_status_reports_literal_key_as_models_json_key() {
    let config = config_from("p", json!({ "apiKey": "literal_api_key_value" }));
    let status = configured_request_auth_status(config.get_provider("p"), None).unwrap();
    assert_eq!(
        status,
        AuthStatus {
            configured: true,
            source: Some(AuthSource::ModelsJsonKey),
            label: None,
        }
    );
}

// model-registry.test.ts:1716 — missing explicit env apiKey stays unconfigured.
#[test]
fn auth_status_missing_env_is_unconfigured() {
    let _guard = ENV_TEST_LOCK.lock().unwrap();
    let var = "TEST_COMPOSER_STATUS_MISSING_98765";
    std::env::remove_var(var);

    let config = config_from("p", json!({ "apiKey": format!("${var}") }));
    let status = configured_request_auth_status(config.get_provider("p"), None).unwrap();
    assert_eq!(
        status,
        AuthStatus {
            configured: false,
            source: None,
            label: None,
        }
    );
}

// model-registry.test.ts:1743 — command apiKey reported without executing it.
#[test]
fn auth_status_reports_command_key_without_executing() {
    let config = config_from("p", json!({ "apiKey": "!echo should-not-run" }));
    let status = configured_request_auth_status(config.get_provider("p"), None).unwrap();
    assert_eq!(
        status,
        AuthStatus {
            configured: true,
            source: Some(AuthSource::ModelsJsonCommand),
            label: None,
        }
    );
}

// provider-composer.ts:547 — an extension-supplied apiKey resolves as fallback.
#[test]
fn auth_status_extension_literal_key_is_fallback() {
    let extension = ProviderConfigInput {
        api_key: Some("extension_literal_key".to_string()),
        ..Default::default()
    };
    let status = configured_request_auth_status(None, Some(&extension)).unwrap();
    assert_eq!(
        status,
        AuthStatus {
            configured: true,
            source: Some(AuthSource::Fallback),
            label: None,
        }
    );
}

// provider-composer.ts:538-539 — no configured key → no status.
#[test]
fn auth_status_absent_when_no_key_configured() {
    assert!(configured_request_auth_status(None, None).is_none());
    let config = config_from("p", json!({ "baseUrl": "https://x.example/v1" }));
    assert!(configured_request_auth_status(config.get_provider("p"), None).is_none());
}

// ---------------------------------------------------------------------------
// resolve_compatibility_request_config  (provider-composer.ts:519-532)
// ---------------------------------------------------------------------------

// model-registry.test.ts:168 — unconfigured compat auth includes static model headers.
#[test]
fn compat_request_includes_static_model_headers() {
    let mut model = base_model("m", "missing-provider");
    model.headers = Some(BTreeMap::from([(
        "X-Static-Model".to_string(),
        "static-value".to_string(),
    )]));

    let result = resolve_compatibility_request_config(&model, None, None).unwrap();
    assert_eq!(
        result,
        CompatibilityRequestConfig {
            headers: Some(BTreeMap::from([(
                "X-Static-Model".to_string(),
                "static-value".to_string()
            )])),
            auth_header: false,
        }
    );
}

// model-registry.test.ts:127 — provider-level headers surface at request time.
#[test]
fn compat_request_includes_configured_provider_headers() {
    let model = base_model("m", "anthropic");
    let config = config_from(
        "anthropic",
        json!({
            "baseUrl": "https://my-proxy.example.com/v1",
            "headers": { "X-Custom-Header": "custom-value" }
        }),
    );
    let result =
        resolve_compatibility_request_config(&model, config.get_provider("anthropic"), None)
            .unwrap();
    assert_eq!(
        result
            .headers
            .unwrap()
            .get("X-Custom-Header")
            .map(String::as_str),
        Some("custom-value")
    );
    assert!(!result.auth_header);
}

// provider-composer.ts:530 — authHeader flows from config; extension overrides.
#[test]
fn compat_request_auth_header_precedence() {
    let model = base_model("m", "p");
    let config = config_from(
        "p",
        json!({ "baseUrl": "https://x.example/v1", "authHeader": true }),
    );
    let result =
        resolve_compatibility_request_config(&model, config.get_provider("p"), None).unwrap();
    assert!(result.auth_header);
    assert!(result.headers.is_none());

    let extension = ProviderConfigInput {
        auth_header: Some(false),
        ..Default::default()
    };
    let result =
        resolve_compatibility_request_config(&model, config.get_provider("p"), Some(&extension))
            .unwrap();
    assert!(!result.auth_header);
}

// ---------------------------------------------------------------------------
// resolve_configured_model_headers  (provider-composer.ts:501-512)
// ---------------------------------------------------------------------------

// model-registry.test.ts:857 — per-model override headers resolve at request time,
// layered with the model definition and extension model headers (extension wins).
#[test]
fn configured_model_headers_layer_override_definition_extension() {
    let model = base_model("shared-id", "p");
    let config = config_from(
        "p",
        json!({
            "baseUrl": "https://x.example/v1",
            "api": "openai-completions",
            "models": [{
                "id": "shared-id",
                "headers": { "X-Def": "def", "X-Shared": "def-shared" }
            }],
            "modelOverrides": {
                "shared-id": { "headers": { "X-Override": "override", "X-Shared": "override-shared" } }
            }
        }),
    );
    let extension = ProviderConfigInput {
        models: Some(vec![ExtensionModelConfig {
            id: "shared-id".to_string(),
            name: "Shared".to_string(),
            api: None,
            base_url: None,
            reasoning: false,
            thinking_level_map: None,
            input: vec![Modality::Text],
            cost: ModelCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                tiers: None,
            },
            context_window: 1000,
            max_tokens: 100,
            headers: Some(BTreeMap::from([(
                "X-Shared".to_string(),
                "ext-shared".to_string(),
            )])),
            compat: None,
        }]),
        ..Default::default()
    };

    let headers =
        resolve_configured_model_headers(&model, config.get_provider("p"), Some(&extension), None)
            .unwrap()
            .unwrap();
    assert_eq!(
        headers.get("X-Override").map(String::as_str),
        Some("override")
    );
    assert_eq!(headers.get("X-Def").map(String::as_str), Some("def"));
    // Extension model headers win the last-write on the shared key.
    assert_eq!(
        headers.get("X-Shared").map(String::as_str),
        Some("ext-shared")
    );
}

// provider-composer.ts:507 — header values are resolved through the config-value
// pipeline (env interpolation).
#[test]
fn configured_model_headers_interpolate_env() {
    let _guard = ENV_TEST_LOCK.lock().unwrap();
    let var = "TEST_COMPOSER_HEADER_ENV_98765";
    std::env::set_var(var, "resolved-secret");

    let model = base_model("m", "p");
    let config = config_from(
        "p",
        json!({
            "baseUrl": "https://x.example/v1",
            "api": "openai-completions",
            "models": [{
                "id": "m",
                "headers": { "Authorization": format!("${var}") }
            }]
        }),
    );
    let headers = resolve_configured_model_headers(&model, config.get_provider("p"), None, None)
        .unwrap()
        .unwrap();
    assert_eq!(
        headers.get("Authorization").map(String::as_str),
        Some("resolved-secret")
    );

    std::env::remove_var(var);
}

// No configured headers → None.
#[test]
fn configured_model_headers_none_when_unset() {
    let model = base_model("m", "p");
    assert!(resolve_configured_model_headers(&model, None, None, None)
        .unwrap()
        .is_none());
}

// ---------------------------------------------------------------------------
// compose_model_provider — baseUrl override layering
// (model-registry.test.ts:98-149)
// ---------------------------------------------------------------------------

// model-registry.test.ts:98/113 — baseUrl override keeps every built-in model and
// rewrites the URL on all of them.
#[test]
fn compose_baseurl_override_keeps_all_models_and_rewrites_url() {
    let base = base_provider(
        "anthropic",
        "Anthropic",
        vec![
            base_model("claude-a", "anthropic"),
            base_model("claude-b", "anthropic"),
        ],
    );
    let config = config_from(
        "anthropic",
        json!({ "baseUrl": "https://my-proxy.example.com/v1" }),
    );
    let provider = compose_model_provider("anthropic", Some(&base), &config, None).unwrap();

    assert_eq!(provider.get_models().len(), 2);
    for model in provider.get_models() {
        assert_eq!(model.base_url, "https://my-proxy.example.com/v1");
    }
    assert_eq!(
        provider.base_url.as_deref(),
        Some("https://my-proxy.example.com/v1")
    );
}

// model-registry.test.ts:146 — a headers-only override composes without error and
// keeps the built-in models.
#[test]
fn compose_headers_only_override_is_valid() {
    let base = base_provider(
        "anthropic",
        "Anthropic",
        vec![base_model("claude-a", "anthropic")],
    );
    let config = config_from(
        "anthropic",
        json!({ "headers": { "X-Custom-Header": "custom-value" } }),
    );
    let provider = compose_model_provider("anthropic", Some(&base), &config, None).unwrap();
    assert_eq!(provider.get_models().len(), 1);
}

// ---------------------------------------------------------------------------
// compose_model_provider — custom model merge behavior
// (model-registry.test.ts:238-346)
// ---------------------------------------------------------------------------

// model-registry.test.ts:238 — built-in custom models inherit api/baseUrl.
#[test]
fn compose_custom_model_inherits_api_and_baseurl() {
    let base = base_provider(
        "openrouter",
        "OpenRouter",
        vec![Model {
            api: "openai-completions".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            ..base_model("existing", "openrouter")
        }],
    );
    let config = config_from(
        "openrouter",
        json!({
            "models": [{
                "id": "fake-provider/fake-model",
                "name": "Fake model",
                "reasoning": true,
                "input": ["text"]
            }]
        }),
    );
    let provider = compose_model_provider("openrouter", Some(&base), &config, None).unwrap();
    let model = find(provider.get_models(), "fake-provider/fake-model").unwrap();
    assert_eq!(model.api, "openai-completions");
    assert_eq!(model.base_url, "https://openrouter.ai/api/v1");
}

// model-registry.test.ts:263 — non-built-in custom models still require baseUrl.
#[test]
fn compose_custom_model_requires_baseurl_without_base() {
    let config = config_from(
        "my-custom-provider",
        json!({
            "apiKey": "test-key",
            "models": [{ "id": "my-model", "api": "openai-completions", "reasoning": false, "input": ["text"] }]
        }),
    );
    let err = compose_model_provider("my-custom-provider", None, &config, None).unwrap_err();
    assert!(err.to_string().contains("baseUrl"), "got: {err}");
}

// model-registry.test.ts:315 — a custom provider sharing a built-in name merges
// custom models with the built-ins.
#[test]
fn compose_custom_models_merge_with_builtins() {
    let base = base_provider(
        "anthropic",
        "Anthropic",
        vec![base_model("claude-builtin", "anthropic")],
    );
    let config = config_from(
        "anthropic",
        json!({
            "baseUrl": "https://my-proxy.example.com/v1",
            "apiKey": "test-key",
            "api": "anthropic-messages",
            "models": [{ "id": "claude-custom", "name": "claude-custom", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 100000, "maxTokens": 8000 }]
        }),
    );
    let provider = compose_model_provider("anthropic", Some(&base), &config, None).unwrap();
    assert_eq!(provider.get_models().len(), 2);
    assert!(find(provider.get_models(), "claude-custom").is_some());
    assert!(find(provider.get_models(), "claude-builtin").is_some());
}

// model-registry.test.ts:325 — a custom model with a built-in id replaces it.
#[test]
fn compose_custom_model_replaces_builtin_by_id() {
    let base = base_provider(
        "openrouter",
        "OpenRouter",
        vec![Model {
            api: "openai-completions".to_string(),
            ..base_model("anthropic/claude-sonnet-4", "openrouter")
        }],
    );
    let config = config_from(
        "openrouter",
        json!({
            "baseUrl": "https://my-proxy.example.com/v1",
            "apiKey": "test-key",
            "api": "openai-completions",
            "models": [{ "id": "anthropic/claude-sonnet-4", "name": "anthropic/claude-sonnet-4", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 100000, "maxTokens": 8000 }]
        }),
    );
    let provider = compose_model_provider("openrouter", Some(&base), &config, None).unwrap();
    let matches: Vec<_> = provider
        .get_models()
        .iter()
        .filter(|m| m.id == "anthropic/claude-sonnet-4")
        .collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].base_url, "https://my-proxy.example.com/v1");
}

// ---------------------------------------------------------------------------
// compose_model_provider — compat merging
// (model-registry.test.ts:350-431)
// ---------------------------------------------------------------------------

// model-registry.test.ts:350 — provider-level compat applies to custom models.
#[test]
fn compose_provider_compat_applies_to_custom_models() {
    let base = base_provider("demo", "demo", vec![]);
    let config = config_from(
        "demo",
        json!({
            "baseUrl": "https://example.com/v1",
            "apiKey": "DEMO_KEY",
            "api": "openai-completions",
            "compat": { "supportsUsageInStreaming": false, "maxTokensField": "max_tokens" },
            "models": [{ "id": "demo-model", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 1000, "maxTokens": 100 }]
        }),
    );
    let provider = compose_model_provider("demo", Some(&base), &config, None).unwrap();
    let compat = find(provider.get_models(), "demo-model")
        .unwrap()
        .compat
        .clone()
        .unwrap();
    assert_eq!(compat["supportsUsageInStreaming"], json!(false));
    assert_eq!(compat["maxTokensField"], json!("max_tokens"));
}

// model-registry.test.ts:380 — model-level compat overrides provider-level.
#[test]
fn compose_model_compat_overrides_provider_compat() {
    let base = base_provider("demo", "demo", vec![]);
    let config = config_from(
        "demo",
        json!({
            "baseUrl": "https://example.com/v1",
            "apiKey": "DEMO_KEY",
            "api": "openai-completions",
            "compat": { "supportsUsageInStreaming": false, "maxTokensField": "max_tokens" },
            "models": [{
                "id": "demo-model", "reasoning": false, "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 1000, "maxTokens": 100,
                "compat": { "supportsUsageInStreaming": true, "maxTokensField": "max_completion_tokens" }
            }]
        }),
    );
    let provider = compose_model_provider("demo", Some(&base), &config, None).unwrap();
    let compat = find(provider.get_models(), "demo-model")
        .unwrap()
        .compat
        .clone()
        .unwrap();
    assert_eq!(compat["supportsUsageInStreaming"], json!(true));
    assert_eq!(compat["maxTokensField"], json!("max_completion_tokens"));
}

// model-registry.test.ts:414 — provider-level compat applies to built-in models.
#[test]
fn compose_provider_compat_applies_to_builtin_models() {
    let base = base_provider(
        "openrouter",
        "OpenRouter",
        vec![
            Model {
                api: "openai-completions".to_string(),
                ..base_model("m1", "openrouter")
            },
            Model {
                api: "openai-completions".to_string(),
                ..base_model("m2", "openrouter")
            },
        ],
    );
    let config = config_from(
        "openrouter",
        json!({ "compat": { "supportsUsageInStreaming": false, "supportsStrictMode": false } }),
    );
    let provider = compose_model_provider("openrouter", Some(&base), &config, None).unwrap();
    assert_eq!(provider.get_models().len(), 2);
    for model in provider.get_models() {
        let compat = model.compat.clone().unwrap();
        assert_eq!(compat["supportsUsageInStreaming"], json!(false));
        assert_eq!(compat["supportsStrictMode"], json!(false));
    }
}

// ---------------------------------------------------------------------------
// compose_model_provider — modelOverrides
// (model-registry.test.ts:604-846)
// ---------------------------------------------------------------------------

// model-registry.test.ts:675 — a single override applies to one model only.
#[test]
fn compose_model_override_applies_to_single_model() {
    let base = base_provider(
        "openrouter",
        "OpenRouter",
        vec![
            base_model("anthropic/claude-sonnet-4", "openrouter"),
            base_model("anthropic/claude-opus-4", "openrouter"),
        ],
    );
    let config = config_from(
        "openrouter",
        json!({ "modelOverrides": { "anthropic/claude-sonnet-4": { "name": "Custom Sonnet Name" } } }),
    );
    let provider = compose_model_provider("openrouter", Some(&base), &config, None).unwrap();
    assert_eq!(
        find(provider.get_models(), "anthropic/claude-sonnet-4")
            .unwrap()
            .name,
        "Custom Sonnet Name"
    );
    assert_ne!(
        find(provider.get_models(), "anthropic/claude-opus-4")
            .unwrap()
            .name,
        "Custom Sonnet Name"
    );
}

// model-registry.test.ts:721 — override compat deep-merges (nested routing keys are
// merged, sibling compat keys preserved).
#[test]
fn compose_model_override_deep_merges_compat() {
    let base = base_provider(
        "openrouter",
        "OpenRouter",
        vec![with_compat(
            base_model("anthropic/claude-sonnet-4", "openrouter"),
            json!({ "supportsStrictMode": false, "openRouterRouting": { "order": ["anthropic"] } }),
        )],
    );
    let config = config_from(
        "openrouter",
        json!({
            "modelOverrides": {
                "anthropic/claude-sonnet-4": { "compat": { "openRouterRouting": { "only": ["amazon-bedrock"] } } }
            }
        }),
    );
    let provider = compose_model_provider("openrouter", Some(&base), &config, None).unwrap();
    let compat = find(provider.get_models(), "anthropic/claude-sonnet-4")
        .unwrap()
        .compat
        .clone()
        .unwrap();
    // Sibling compat key preserved.
    assert_eq!(compat["supportsStrictMode"], json!(false));
    // Nested routing deep-merged: base `order` kept, override `only` added.
    assert_eq!(compat["openRouterRouting"]["order"], json!(["anthropic"]));
    assert_eq!(
        compat["openRouterRouting"]["only"],
        json!(["amazon-bedrock"])
    );
}

// model-registry.test.ts:815 — override changes a single cost field, preserving
// the rest of the built-in cost.
#[test]
fn compose_model_override_partial_cost() {
    let base = base_provider(
        "openrouter",
        "OpenRouter",
        vec![base_model("anthropic/claude-sonnet-4", "openrouter")],
    );
    let config = config_from(
        "openrouter",
        json!({ "modelOverrides": { "anthropic/claude-sonnet-4": { "cost": { "input": 99 } } } }),
    );
    let provider = compose_model_provider("openrouter", Some(&base), &config, None).unwrap();
    let model = find(provider.get_models(), "anthropic/claude-sonnet-4").unwrap();
    assert_eq!(model.cost.input, 99.0);
    // Preserved from the base model.
    assert_eq!(model.cost.output, 2.0);
}

// model-registry.test.ts:795 — an override for a missing id is ignored (no new
// model, no error).
#[test]
fn compose_model_override_for_missing_id_is_ignored() {
    let base = base_provider(
        "openrouter",
        "OpenRouter",
        vec![base_model("anthropic/claude-sonnet-4", "openrouter")],
    );
    let config = config_from(
        "openrouter",
        json!({ "modelOverrides": { "nonexistent/model-id": { "name": "should not appear" } } }),
    );
    let provider = compose_model_provider("openrouter", Some(&base), &config, None).unwrap();
    assert!(find(provider.get_models(), "nonexistent/model-id").is_none());
    assert_eq!(provider.get_models().len(), 1);
}

// model-registry.test.ts:604 — modelOverrides still apply when the provider also
// defines custom models.
#[test]
fn compose_model_override_applies_alongside_custom_models() {
    let base = base_provider(
        "openrouter",
        "OpenRouter",
        vec![Model {
            api: "openai-completions".to_string(),
            ..base_model("anthropic/claude-sonnet-4", "openrouter")
        }],
    );
    let config = config_from(
        "openrouter",
        json!({
            "baseUrl": "https://my-proxy.example.com/v1",
            "apiKey": "OPENROUTER_API_KEY",
            "api": "openai-completions",
            "models": [{ "id": "custom/openrouter-model", "name": "Custom OpenRouter Model", "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 128000, "maxTokens": 16384 }],
            "modelOverrides": { "anthropic/claude-sonnet-4": { "name": "Overridden Built-in Sonnet" } }
        }),
    );
    let provider = compose_model_provider("openrouter", Some(&base), &config, None).unwrap();
    assert!(find(provider.get_models(), "custom/openrouter-model").is_some());
    assert_eq!(
        find(provider.get_models(), "anthropic/claude-sonnet-4")
            .unwrap()
            .name,
        "Overridden Built-in Sonnet"
    );
}

// ---------------------------------------------------------------------------
// apply_models_json structural errors and radius special-case
// (provider-composer.ts:161-199)
// ---------------------------------------------------------------------------

// provider-composer.ts:167-169 — oauth requires baseUrl.
#[test]
fn compose_oauth_requires_baseurl() {
    let config = config_from("p", json!({ "oauth": "some-oauth" }));
    let err = compose_model_provider("p", None, &config, None).unwrap_err();
    assert!(
        err.to_string()
            .contains("\"baseUrl\" is required when \"oauth\" is set"),
        "got: {err}"
    );
}

// provider-composer.ts:171-184 — an empty provider block must specify something.
#[test]
fn compose_empty_provider_block_errors() {
    let config = config_from("p", json!({}));
    let err = compose_model_provider("p", None, &config, None).unwrap_err();
    assert!(err.to_string().contains("must specify"), "got: {err}");
}

// provider-composer.ts:188 — a `radius` oauth provider keeps each model's own
// baseUrl instead of taking the provider baseUrl.
#[test]
fn compose_radius_keeps_model_base_url() {
    let base = base_provider(
        "radius",
        "Radius",
        vec![Model {
            base_url: "https://model.radius/v1".to_string(),
            ..base_model("r1", "radius")
        }],
    );
    let config = config_from(
        "radius",
        json!({ "oauth": "radius", "baseUrl": "https://provider.radius/v1" }),
    );
    let provider = compose_model_provider("radius", Some(&base), &config, None).unwrap();
    assert_eq!(
        find(provider.get_models(), "r1").unwrap().base_url,
        "https://model.radius/v1"
    );
}

// ---------------------------------------------------------------------------
// compose_model_provider — identity precedence (provider-composer.ts:469-471)
// ---------------------------------------------------------------------------

#[test]
fn compose_name_and_base_url_precedence() {
    let base = base_provider("p", "Base Name", vec![base_model("m", "p")]);
    let config = config_from(
        "p",
        json!({ "name": "Config Name", "baseUrl": "https://config.example/v1" }),
    );

    // extension name/baseUrl win over config and base.
    let extension = ProviderConfigInput {
        name: Some("Extension Name".to_string()),
        base_url: Some("https://ext.example/v1".to_string()),
        ..Default::default()
    };
    let provider = compose_model_provider("p", Some(&base), &config, Some(&extension)).unwrap();
    assert_eq!(provider.name, "Extension Name");
    assert_eq!(provider.base_url.as_deref(), Some("https://ext.example/v1"));

    // config wins over base when extension is absent.
    let provider = compose_model_provider("p", Some(&base), &config, None).unwrap();
    assert_eq!(provider.name, "Config Name");
    assert_eq!(
        provider.base_url.as_deref(),
        Some("https://config.example/v1")
    );

    // base name is the fallback when neither config nor extension names it.
    let bare = config_from("p", json!({ "baseUrl": "https://config.example/v1" }));
    let provider = compose_model_provider("p", Some(&base), &bare, None).unwrap();
    assert_eq!(provider.name, "Base Name");

    // provider id is the final fallback.
    let empty = ModelConfig::parse("{\"providers\":{}}", "test");
    let no_name_base = base_provider("p", "", vec![base_model("m", "p")]);
    let provider = compose_model_provider("p", Some(&no_name_base), &empty, None).unwrap();
    assert_eq!(provider.name, "p");
}

// ---------------------------------------------------------------------------
// validate_extension_provider  (provider-composer.ts:399-409)
// ---------------------------------------------------------------------------

// provider-composer.ts:405-406 — streamSimple requires an api.
#[test]
fn validate_stream_simple_requires_api() {
    let extension = ProviderConfigInput {
        stream_simple: true,
        api: None,
        ..Default::default()
    };
    let err = validate_extension_provider("p", &[], None, &extension).unwrap_err();
    assert!(
        err.to_string()
            .contains("\"api\" is required when registering streamSimple"),
        "got: {err}"
    );
}

// A structurally valid extension provider validates without error.
#[test]
fn validate_accepts_valid_extension_provider() {
    let base = [Model {
        api: "openai-completions".to_string(),
        ..base_model("m", "p")
    }];
    let extension = ProviderConfigInput {
        stream_simple: true,
        api: Some("openai-completions".to_string()),
        base_url: Some("https://ext.example/v1".to_string()),
        models: Some(vec![ExtensionModelConfig {
            id: "ext-model".to_string(),
            name: "Ext".to_string(),
            api: None,
            base_url: None,
            reasoning: false,
            thinking_level_map: None,
            input: vec![Modality::Text],
            cost: ModelCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                tiers: None,
            },
            context_window: 1000,
            max_tokens: 100,
            headers: None,
            compat: None,
        }]),
        ..Default::default()
    };
    assert!(validate_extension_provider("p", &base, None, &extension).is_ok());
}

// provider-composer.ts:213-219 — an extension custom model with no api and no base
// to inherit from fails validation.
#[test]
fn validate_extension_custom_model_requires_api() {
    let extension = ProviderConfigInput {
        base_url: Some("https://ext.example/v1".to_string()),
        models: Some(vec![ExtensionModelConfig {
            id: "ext-model".to_string(),
            name: "Ext".to_string(),
            api: None,
            base_url: None,
            reasoning: false,
            thinking_level_map: None,
            input: vec![Modality::Text],
            cost: ModelCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                tiers: None,
            },
            context_window: 1000,
            max_tokens: 100,
            headers: None,
            compat: None,
        }]),
        ..Default::default()
    };
    let err = validate_extension_provider("p", &[], None, &extension).unwrap_err();
    assert!(
        err.to_string().contains("no \"api\" specified"),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------------
// merge_compat unit coverage (provider-composer.ts:78-98)
// ---------------------------------------------------------------------------

#[test]
fn merge_compat_returns_base_when_no_override() {
    let base = json!({ "a": 1 });
    assert_eq!(merge_compat(Some(&base), None), Some(base));
    assert_eq!(merge_compat(None, None), None);
}

#[test]
fn merge_compat_shallow_and_nested() {
    let base = json!({ "keep": true, "openRouterRouting": { "order": ["a"] }, "x": 1 });
    let over = json!({ "x": 2, "openRouterRouting": { "only": ["b"] } });
    let merged = merge_compat(Some(&base), Some(&over)).unwrap();
    assert_eq!(merged["keep"], json!(true));
    assert_eq!(merged["x"], json!(2));
    assert_eq!(merged["openRouterRouting"]["order"], json!(["a"]));
    assert_eq!(merged["openRouterRouting"]["only"], json!(["b"]));
}
