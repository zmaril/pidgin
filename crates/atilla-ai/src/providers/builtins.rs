//! The built-in provider set, ported from pi's `packages/ai/src/providers/all.ts`
//! (pinned commit `3da591ab`).
//!
//! pi builds 35 static provider factories plus the purely-dynamic `radius`
//! provider, each wrapping `createProvider`. Here the model catalog
//! ([`atilla_model_catalog::catalog`]) is the model source: every catalog
//! provider becomes a [`RegistryProvider`] whose baseline models are the mapped
//! catalog entries, and [`radius_provider`] is appended as the one dynamic
//! provider with no catalog entry.
//!
//! # API backends
//!
//! pi's builtins wire concrete HTTP stream implementations (e.g.
//! `anthropicMessagesApi()`). Those clients are not yet ported, so builtins use
//! [`ApiRouting::Unimplemented`]: model listing, pricing, and metadata are fully
//! available, and a stream attempt yields the "no API implementation" error.
//! The catalog preserves each model's `api` discriminant, so routing is ready to
//! be wired backend-by-backend without changing this construction.

use std::collections::BTreeMap;
use std::sync::Arc;

use atilla_model_catalog::{catalog, Modality as CatModality, Model as CatModel};

use crate::providers::anthropic_backend::AnthropicMessagesBackend;
use crate::providers::registry::{
    create_provider, ApiRouting, CreateProviderOptions, Models, MutableModels, ProviderAuth,
    RefreshContext, RegistryProvider, StreamBackendRef,
};
use crate::seams::clock::Clock;
use crate::seams::http::HttpTransport;
use crate::types::{Modality, Model, ModelCost, ModelCostTier, ModelThinkingLevel};

/// Map a catalog [`CatModel`] onto the canonical [`Model<serde_json::Value>`]
/// the registry exposes.
///
/// The two `Model` types share pi's wire shape but are distinct Rust structs:
/// the catalog's is forward-compatible (`extra` catch-all, flattened cost rates,
/// untyped `compat`) while the registry's mirrors `types.ts` exactly. This
/// bridges them field-for-field, converting the flattened cost rates, the
/// string-keyed thinking-level map, and the modality list (dropping any
/// modality the registry does not model, e.g. a future `audio`).
pub fn catalog_model_to_ai(model: &CatModel) -> Model {
    Model {
        id: model.id.clone(),
        name: model.name.clone(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        base_url: model.base_url.clone(),
        reasoning: model.reasoning,
        thinking_level_map: model.thinking_level_map.as_ref().map(map_thinking_levels),
        input: model.input.iter().filter_map(map_modality).collect(),
        cost: ModelCost {
            input: model.cost.rates.input,
            output: model.cost.rates.output,
            cache_read: model.cost.rates.cache_read,
            cache_write: model.cost.rates.cache_write,
            tiers: model.cost.tiers.as_ref().map(|tiers| {
                tiers
                    .iter()
                    .map(|tier| ModelCostTier {
                        input_tokens_above: tier.input_tokens_above,
                        input: tier.rates.input,
                        output: tier.rates.output,
                        cache_read: tier.rates.cache_read,
                        cache_write: tier.rates.cache_write,
                    })
                    .collect()
            }),
        },
        context_window: model.context_window,
        max_tokens: model.max_tokens,
        headers: model.headers.clone(),
        compat: model.compat.clone(),
    }
}

fn map_modality(modality: &CatModality) -> Option<Modality> {
    match modality {
        CatModality::Text => Some(Modality::Text),
        CatModality::Image => Some(Modality::Image),
        CatModality::Other => None,
    }
}

fn map_thinking_levels(
    map: &BTreeMap<String, Option<String>>,
) -> BTreeMap<ModelThinkingLevel, Option<String>> {
    map.iter()
        .filter_map(|(key, value)| parse_thinking_level(key).map(|level| (level, value.clone())))
        .collect()
}

fn parse_thinking_level(key: &str) -> Option<ModelThinkingLevel> {
    match key {
        "off" => Some(ModelThinkingLevel::Off),
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::Xhigh),
        "max" => Some(ModelThinkingLevel::Max),
        _ => None,
    }
}

/// The display name and base URL for a built-in catalog provider, mirroring the
/// per-provider `createProvider({ name, baseUrl })` values in pi's provider
/// files. Downstream runtime tests assert these exact names.
fn provider_config(id: &str) -> (&'static str, Option<&'static str>) {
    match id {
        "amazon-bedrock" => ("Amazon Bedrock", None),
        "ant-ling" => ("Ant Ling", Some("https://api.ant-ling.com/v1")),
        "anthropic" => ("Anthropic", Some("https://api.anthropic.com")),
        "azure-openai-responses" => ("Azure OpenAI", None),
        "cerebras" => ("Cerebras", Some("https://api.cerebras.ai/v1")),
        "cloudflare-ai-gateway" => ("Cloudflare AI Gateway", None),
        "cloudflare-workers-ai" => ("Cloudflare Workers AI", None),
        "deepseek" => ("DeepSeek", Some("https://api.deepseek.com")),
        "fireworks" => ("Fireworks", Some("https://api.fireworks.ai/inference")),
        "github-copilot" => (
            "GitHub Copilot",
            Some("https://api.individual.githubcopilot.com"),
        ),
        "google" => (
            "Google",
            Some("https://generativelanguage.googleapis.com/v1beta"),
        ),
        "google-vertex" => ("Google Vertex AI", None),
        "groq" => ("Groq", Some("https://api.groq.com/openai/v1")),
        "huggingface" => ("Hugging Face", Some("https://router.huggingface.co/v1")),
        "kimi-coding" => ("Kimi For Coding", Some("https://api.kimi.com/coding")),
        "minimax" => ("MiniMax", Some("https://api.minimax.io/anthropic")),
        "minimax-cn" => ("MiniMax CN", Some("https://api.minimaxi.com/anthropic")),
        "mistral" => ("Mistral", Some("https://api.mistral.ai")),
        "moonshotai" => ("Moonshot AI", Some("https://api.moonshot.ai/v1")),
        "moonshotai-cn" => ("Moonshot AI CN", Some("https://api.moonshot.cn/v1")),
        "nvidia" => ("NVIDIA", Some("https://integrate.api.nvidia.com/v1")),
        "openai" => ("OpenAI", Some("https://api.openai.com/v1")),
        "openai-codex" => ("OpenAI Codex", Some("https://chatgpt.com/backend-api")),
        "opencode" => ("OpenCode Zen", None),
        "opencode-go" => ("OpenCode Zen Go", None),
        "openrouter" => ("OpenRouter", Some("https://openrouter.ai/api/v1")),
        "together" => ("Together", Some("https://api.together.ai/v1")),
        "vercel-ai-gateway" => ("Vercel AI Gateway", Some("https://ai-gateway.vercel.sh")),
        "xai" => ("xAI", Some("https://api.x.ai/v1")),
        "xiaomi" => ("Xiaomi", Some("https://api.xiaomimimo.com/v1")),
        "xiaomi-token-plan-ams" => (
            "Xiaomi Token Plan AMS",
            Some("https://token-plan-ams.xiaomimimo.com/v1"),
        ),
        "xiaomi-token-plan-cn" => (
            "Xiaomi Token Plan CN",
            Some("https://token-plan-cn.xiaomimimo.com/v1"),
        ),
        "xiaomi-token-plan-sgp" => (
            "Xiaomi Token Plan SGP",
            Some("https://token-plan-sgp.xiaomimimo.com/v1"),
        ),
        "zai" => ("Z.AI", Some("https://api.z.ai/api/coding/paas/v4")),
        "zai-coding-cn" => (
            "Z.AI Coding CN",
            Some("https://open.bigmodel.cn/api/coding/paas/v4"),
        ),
        // Fall back to the id as the name for any provider added upstream that
        // this table has not caught up with.
        other => (leak_id(other), None),
    }
}

/// Leak the id as a `'static` display-name fallback. Only reached for a provider
/// the catalog gained but [`provider_config`] has not yet named — a bounded,
/// one-time set.
fn leak_id(id: &str) -> &'static str {
    Box::leak(id.to_string().into_boxed_str())
}

/// Build a [`RegistryProvider`] for one catalog provider with the given stream
/// routing, using its catalog models as the baseline and its known display name /
/// base URL / env auth. Shared by [`provider_from_catalog`] (which passes
/// [`ApiRouting::Unimplemented`]) and
/// [`provider_from_catalog_with_transport`] (which binds a live backend).
fn catalog_provider(id: &str, api: ApiRouting) -> RegistryProvider {
    let (name, base_url) = provider_config(id);
    let models: Vec<Model> = catalog()
        .provider(id)
        .map(|entries| entries.values().map(catalog_model_to_ai).collect())
        .unwrap_or_default();
    create_provider(CreateProviderOptions {
        id: id.to_string(),
        name: Some(name.to_string()),
        base_url: base_url.map(str::to_string),
        headers: None,
        auth: env_auth(id, name),
        models,
        fetch_models: None,
        api,
    })
}

/// Build a [`RegistryProvider`] for one catalog provider, using its catalog
/// models as the baseline and its known display name / base URL / env auth. The
/// stream backend is [`ApiRouting::Unimplemented`] (pi's builtins wire a concrete
/// HTTP client; see [`provider_from_catalog_with_transport`] for the bound form).
pub fn provider_from_catalog(id: &str) -> RegistryProvider {
    catalog_provider(id, ApiRouting::Unimplemented)
}

/// Build a [`RegistryProvider`] for one catalog provider with a live stream
/// backend wired for the api dialects that have a transport-aware adapter,
/// mirroring pi's builtins wiring a concrete stream implementation per provider
/// (`anthropicMessagesApi()` and friends).
///
/// Today only `anthropic` binds a real backend
/// ([`AnthropicMessagesBackend`](crate::providers::AnthropicMessagesBackend),
/// [`ApiRouting::Single`]); every other builtin falls back to
/// [`provider_from_catalog`]'s [`ApiRouting::Unimplemented`]. The already-ported
/// sibling dialects (openai_completions/responses, google, bedrock, mistral,
/// azure) are a documented follow-up: each needs its own `Provider` adapter
/// bridging the generic seam onto its typed driver, deferred here to keep this a
/// small, focused, green change.
pub fn provider_from_catalog_with_transport(
    id: &str,
    transport: &Arc<dyn HttpTransport>,
    clock: &Arc<dyn Clock>,
) -> RegistryProvider {
    if id == "anthropic" {
        let backend: StreamBackendRef = Arc::new(AnthropicMessagesBackend::new(
            transport.clone(),
            clock.clone(),
        ));
        return catalog_provider(id, ApiRouting::Single(backend));
    }
    // Follow-up (port): bind the remaining ported dialects (openai_completions,
    // openai_responses, google, bedrock, mistral, azure) with their own
    // transport-aware `Provider` adapters; until then they stay Unimplemented.
    provider_from_catalog(id)
}

/// The env-API-key auth descriptor for a provider, derived from the same env-var
/// table as [`crate::env_api_keys::get_api_key_env_vars`].
fn env_auth(id: &str, name: &str) -> ProviderAuth {
    match crate::env_api_keys::get_api_key_env_vars(id) {
        Some(vars) => ProviderAuth::env_api_key(format!("{name} API key"), &vars),
        None => ProviderAuth {
            name: format!("{name} credentials"),
            api_key_env_vars: Vec::new(),
        },
    }
}

/// The purely-dynamic Radius gateway provider, pi's `radiusProvider`
/// (`providers/radius.ts`). It has no catalog entry: its models come only from a
/// dynamic refresh, so [`RegistryProvider::get_models`] is empty until refreshed.
///
/// The gateway fetch (`loadRadiusGatewayConfig`) requires network access and is
/// not ported; the fetch hook is present (so the provider is refreshable) but
/// yields no models offline.
pub fn radius_provider() -> RegistryProvider {
    create_provider(CreateProviderOptions {
        id: "radius".to_string(),
        name: Some("Radius".to_string()),
        base_url: None,
        headers: None,
        auth: ProviderAuth::env_api_key("Radius API key", &["RADIUS_API_KEY"]),
        models: Vec::new(),
        // The real gateway fetch is deferred; refreshing offline yields nothing.
        fetch_models: Some(std::sync::Arc::new(|_ctx: &RefreshContext| Vec::new())),
        api: ApiRouting::Unimplemented,
    })
}

/// All built-in providers, freshly constructed: every catalog provider plus the
/// dynamic [`radius_provider`]. pi's `builtinProviders` (`all.ts:78`).
pub fn builtin_providers() -> Vec<RegistryProvider> {
    let mut providers: Vec<RegistryProvider> =
        catalog().providers().map(provider_from_catalog).collect();
    providers.push(radius_provider());
    providers
}

/// All built-in providers with live stream backends bound where a
/// transport-aware adapter exists, pi's `builtinProviders` wired for real HTTP.
///
/// Identical to [`builtin_providers`] except each catalog provider is built via
/// [`provider_from_catalog_with_transport`], so `anthropic` routes through
/// [`ApiRouting::Single`] over the injected `transport`/`clock` instead of
/// [`ApiRouting::Unimplemented`]. Providers whose dialect has no adapter yet stay
/// Unimplemented (see [`provider_from_catalog_with_transport`]).
pub fn builtin_providers_with_transport(
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
) -> Vec<RegistryProvider> {
    let mut providers: Vec<RegistryProvider> = catalog()
        .providers()
        .map(|id| provider_from_catalog_with_transport(id, &transport, &clock))
        .collect();
    providers.push(radius_provider());
    providers
}

/// A [`Models`] collection with every built-in provider registered, pi's
/// `builtinModels` (`all.ts:120`).
pub fn builtin_models() -> Models {
    let mut models = Models::new();
    for provider in builtin_providers() {
        models.set_provider(provider);
    }
    models
}

#[cfg(test)]
mod tests {
    use super::*;

    // providers.test.ts:26 — builtinModels registers every builtin provider with
    // models; anthropic is present; getModel resolves and carries its api.
    #[test]
    fn builtin_models_registers_every_provider() {
        let models = builtin_models();
        let providers = models.get_providers();
        assert_eq!(providers.len(), builtin_providers().len());
        assert!(providers.iter().any(|p| p.id() == "anthropic"));

        let anthropic = models.get_model("anthropic", "claude-haiku-4-5");
        assert_eq!(
            anthropic.as_ref().map(|m| m.api.as_str()),
            Some("anthropic-messages")
        );

        let all = models.get_models(None);
        assert!(all.len() > 500, "expected >500 models, got {}", all.len());

        // Static providers list models immediately; Radius is purely dynamic.
        for provider in providers {
            let list = models.get_models(Some(provider.id()));
            if provider.id() == "radius" {
                assert!(list.is_empty(), "radius should list no models");
            } else {
                assert!(!list.is_empty(), "{} should list models", provider.id());
            }
            assert!(list.iter().all(|m| m.provider == provider.id()));
        }
    }

    // providers.test.ts:47 — official Kimi K3 pricing for the Moonshot providers.
    #[test]
    fn moonshot_kimi_k3_pricing() {
        let models = builtin_models();
        for provider in ["moonshotai", "moonshotai-cn"] {
            let cost = models.get_model(provider, "kimi-k3").expect("kimi-k3").cost;
            assert_eq!(cost.input, 3.0);
            assert_eq!(cost.output, 15.0);
            assert_eq!(cost.cache_read, 0.3);
            assert_eq!(cost.cache_write, 0.0);
            assert!(cost.tiers.is_none());
        }
    }

    // providers.test.ts:59 — API-equivalent implied pricing for Kimi Coding.
    #[test]
    fn kimi_coding_pricing() {
        let models = builtin_models();
        let expected = [
            ("k2p7", 0.95, 4.0, 0.19, 0.0),
            ("k3", 3.0, 15.0, 0.3, 0.0),
            ("kimi-for-coding-highspeed", 1.9, 8.0, 0.38, 0.0),
        ];
        for (id, input, output, cache_read, cache_write) in expected {
            let cost = models.get_model("kimi-coding", id).expect(id).cost;
            assert_eq!(cost.input, input, "{id} input");
            assert_eq!(cost.output, output, "{id} output");
            assert_eq!(cost.cache_read, cache_read, "{id} cacheRead");
            assert_eq!(cost.cache_write, cache_write, "{id} cacheWrite");
        }
    }

    // The catalog has 35 providers; builtins add radius for 36.
    #[test]
    fn builtin_provider_count() {
        assert_eq!(catalog().provider_count(), 35);
        assert_eq!(builtin_providers().len(), 36);
    }

    /// Assert that `models.get_model(provider, id)` carries a `compat` blob whose
    /// `key` equals `expected` verbatim — the invariant pi's conformance fixtures
    /// depend on for per-model compat flags.
    fn assert_compat_flag(
        models: &Models,
        provider: &str,
        id: &str,
        key: &str,
        expected: serde_json::Value,
    ) {
        let model = models
            .get_model(provider, id)
            .unwrap_or_else(|| panic!("{provider}/{id} should resolve"));
        let compat = model
            .compat
            .as_ref()
            .unwrap_or_else(|| panic!("{provider}/{id} should carry a compat blob"));
        assert_eq!(
            compat.get(key),
            Some(&expected),
            "{provider}/{id} compat.{key} must survive the catalog->Model mapping verbatim"
        );
    }

    // The catalog->Model mapping must not drop per-model compat flags: pi's
    // downstream conformance fixtures read forceAdaptiveThinking /
    // supportsTemperature / supportsLongCacheRetention straight out of `compat`.
    // The mapping carries `compat: model.compat.clone()` verbatim, so each flag
    // must equal its catalog JSON value exactly.
    #[test]
    fn catalog_compat_flags_survive_mapping() {
        use serde_json::json;
        let models = builtin_models();

        // forceAdaptiveThinking (reasoning-related) — anthropic + kimi-coding.
        assert_compat_flag(
            &models,
            "anthropic",
            "claude-opus-4-8",
            "forceAdaptiveThinking",
            json!(true),
        );
        assert_compat_flag(
            &models,
            "anthropic",
            "claude-fable-5",
            "forceAdaptiveThinking",
            json!(true),
        );
        assert_compat_flag(
            &models,
            "kimi-coding",
            "k3",
            "forceAdaptiveThinking",
            json!(true),
        );
        assert_compat_flag(
            &models,
            "kimi-coding",
            "k2p7",
            "forceAdaptiveThinking",
            json!(true),
        );
        assert_compat_flag(
            &models,
            "kimi-coding",
            "kimi-for-coding-highspeed",
            "forceAdaptiveThinking",
            json!(true),
        );
        // kimi k3 also carries allowEmptySignature alongside it.
        assert_compat_flag(
            &models,
            "kimi-coding",
            "k3",
            "allowEmptySignature",
            json!(true),
        );

        // supportsTemperature — anthropic opus-4-8 pins it to false.
        assert_compat_flag(
            &models,
            "anthropic",
            "claude-opus-4-8",
            "supportsTemperature",
            json!(false),
        );

        // supportsLongCacheRetention — opencode deepseek-v4-flash pins it to false.
        assert_compat_flag(
            &models,
            "opencode",
            "deepseek-v4-flash",
            "supportsLongCacheRetention",
            json!(false),
        );
    }

    // Enumerating the whole builtin catalog, the set of (provider, id) whose
    // compat carries `forceAdaptiveThinking == true` must be non-empty and must
    // include the known adaptive-thinking models. Guards against the mapping
    // silently dropping the flag for the enumerated (non-lookup) path.
    #[test]
    fn adaptive_thinking_set_from_enumeration() {
        let models = builtin_models();
        let adaptive: std::collections::BTreeSet<(String, String)> = models
            .get_models(None)
            .into_iter()
            .filter(|m| {
                m.compat
                    .as_ref()
                    .and_then(|c| c.get("forceAdaptiveThinking"))
                    == Some(&serde_json::Value::Bool(true))
            })
            .map(|m| (m.provider.clone(), m.id.clone()))
            .collect();

        assert!(
            !adaptive.is_empty(),
            "expected some forceAdaptiveThinking models"
        );
        for expected in [
            ("anthropic", "claude-opus-4-8"),
            ("anthropic", "claude-fable-5"),
            ("kimi-coding", "k3"),
            ("kimi-coding", "k2p7"),
            ("kimi-coding", "kimi-for-coding-highspeed"),
        ] {
            let key = (expected.0.to_string(), expected.1.to_string());
            assert!(
                adaptive.contains(&key),
                "{}/{} should be in the forceAdaptiveThinking set",
                expected.0,
                expected.1
            );
        }
    }
}
