// straitjacket-allow-file:duplication — a faithful transcription of pi's
// `provider-composer.ts`. The model-layering helpers (`apply_models_json`,
// `apply_extension`, `model_from_json`, `apply_model_override`) are walls of
// near-identical `a ?? b ?? default` field-precedence expressions, and the
// header/auth-status helpers repeat the same `config`/`extension` merge shape.
// The clone detector reads these repeated precedence runs as duplicates; they
// are distinct, load-bearing composition rules kept verbatim to mirror pi's
// provider-composition semantics, exactly as `model_config.rs` does for the
// same `models.json` shape.
//! Credential-blind provider composition.
//!
//! Ported from pi's `core/provider-composer.ts` at pinned commit `3da591ab`.
//! The composer layers three model sources — the built-in catalog (`base`),
//! the user's `models.json` (`config`), and an extension's `registerProvider`
//! input (`extension`) — into one provider view, resolving request headers and
//! deriving auth status **without reading any credential**.
//!
//! # Scope of this slice
//!
//! pi's `composeModelProvider` returns a pi-ai `Provider` object: identity and
//! model metadata, *plus* composed auth handlers (`auth.apiKey`/`auth.oauth`),
//! a `refreshModels` orchestrator, a `filterModels` delegate, and
//! `stream`/`streamSimple` closures wired through `lazyStream` +
//! `getApiProvider`. This port covers the credential-blind, synchronously
//! computable surface:
//!
//! - the model-layering pipeline (`apply_models_json` -> `apply_extension` ->
//!   `modelOverrides`), including compat merging and per-model overrides;
//! - the pure request-config helpers `resolve_configured_model_headers`,
//!   `resolve_compatibility_request_config`, and `configured_request_auth_status`;
//! - `validate_extension_provider`;
//! - `compose_model_provider`, which resolves provider identity/`baseUrl`/
//!   `headers` precedence and the layered model list into a [`ComposedProvider`].
//!
//! The following composer behaviors are **deferred to the model-runtime slice**
//! because atilla's landed types cannot represent them without fabrication (see
//! the port report):
//!
//! - **auth-handler composition** (`composeApiKeyAuth`/`composeOAuthAuth`,
//!   `adaptOAuth`, `withConfiguredAuth`, `configContextEnv`, and the
//!   `"no authentication method configured"` throw). pi reads the base
//!   provider's rich `auth.apiKey`/`auth.oauth` handlers; atilla's
//!   `RegistryProvider` exposes only the pared-down `ProviderAuth { name,
//!   apiKeyEnvVars }`, and pi-ai's OAuth handler shape (closure `login(callbacks)`
//!   / `refresh` / `toAuth` with `OAuthLoginCallbacks`) differs fundamentally
//!   from atilla's flow-machine `OAuthAuth`.
//! - **streaming** (`stream`/`streamSimple` via `lazyStream`, `getApiProvider`,
//!   and `supportsBaseApi`). atilla's streaming seam is the synchronous
//!   `StreamResult`/`ApiRouting` dispatch; `lazyStream` does not exist.
//! - **`refreshModels`** orchestration and the stateful
//!   `extensionOAuthCredential` / `refreshedExtensionModels` capture, plus the
//!   OAuth `modifyModels` projection that only fires after a live refresh.
//! - **`filterModels`** delegation to the base provider.
//!
//! The `ExtensionOAuthConfig` login/refresh/getApiKey/modifyModels members and
//! `ProviderConfigInput`'s `streamSimple`/`refreshModels` closures are likewise
//! effectful runtime concerns; the types here carry the data fields the pure
//! surface reads and represent the effectful members as presence markers.

use std::collections::{BTreeMap, HashMap};

use atilla_ai::providers::registry::{ProviderHeaders, RegistryProvider};
use atilla_ai::types::{Modality, Model, ModelCost, ThinkingLevelMap};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model_config::{
    ModelConfig, ModelsJsonModel, ModelsJsonModelOverride, ModelsJsonProvider,
};
use super::resolve_config_value::{
    clear_config_value_cache, get_config_value_env_var_names, is_command_config_value,
    is_config_value_configured, resolve_headers_or_throw, ConfigValueError,
};

/// A model definition inside an extension's `registerProvider` input
/// (`provider-composer.ts:53-66`).
///
/// Mirrors the inline `models[]` entry type of pi's [`ProviderConfigInput`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionModelConfig {
    /// The model id (required).
    pub id: String,
    /// The display name (required, unlike the `models.json` model shape).
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Whether the model supports reasoning/thinking.
    pub reasoning: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    /// Accepted input modalities.
    pub input: Vec<Modality>,
    /// Token cost rates.
    pub cost: ModelCost,
    /// The context-window size.
    pub context_window: u64,
    /// The per-request max output tokens.
    pub max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// Raw compat blob; see [`super::model_config`] on typing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Value>,
}

/// OAuth config an extension may attach to a provider (`provider-composer.ts:33-41`).
///
/// pi's `ExtensionOAuthConfig` additionally carries the effectful
/// `login`/`refreshToken`/`getApiKey`/`modifyModels` members driving the
/// extension OAuth flow; those are runtime concerns (deferred to the runtime
/// slice) and are not represented here. The composer's pure surface reads only
/// [`name`](Self::name) (a provider-name fallback in `compose_model_provider`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionOAuthConfig {
    /// The OAuth handler name (also a provider-name fallback).
    pub name: String,
    /// Retained for extension source compatibility; ignored by canonical auth
    /// flows (pi's deprecated `usesCallbackServer`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uses_callback_server: Option<bool>,
}

/// Input type for the extension `registerProvider` API (`provider-composer.ts:44-68`).
///
/// The effectful members of pi's `ProviderConfigInput` — the `streamSimple`
/// stream function and the async `refreshModels` fetch — are runtime concerns
/// (deferred). They are represented here as presence markers
/// ([`stream_simple`](Self::stream_simple), [`refresh_models`](Self::refresh_models))
/// because their *presence* still drives the pure surface: `stream_simple`
/// requires `api` in [`validate_extension_provider`], and both influence whether
/// the runtime treats the provider as refreshable.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProviderConfigInput {
    /// The provider display name.
    pub name: Option<String>,
    /// The provider base URL.
    pub base_url: Option<String>,
    /// A configured API key (literal, `$ENV`, or `!command`).
    pub api_key: Option<String>,
    /// The provider's default api id.
    pub api: Option<String>,
    /// Whether the extension registered a `streamSimple` function (presence
    /// marker; the function itself is a runtime concern).
    pub stream_simple: bool,
    /// Extra provider-level request headers.
    pub headers: Option<BTreeMap<String, String>>,
    /// Whether to inject `Authorization: Bearer <key>`.
    pub auth_header: Option<bool>,
    /// An attached OAuth config.
    pub oauth: Option<ExtensionOAuthConfig>,
    /// Custom / replacement model definitions.
    pub models: Option<Vec<ExtensionModelConfig>>,
    /// Whether the extension registered a `refreshModels` fetch (presence
    /// marker; the fetch itself is a runtime concern).
    pub refresh_models: bool,
}

/// The origin of a provider's configured auth (`provider-composer.ts:72`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthSource {
    /// A stored credential.
    Stored,
    /// A runtime override.
    Runtime,
    /// An environment variable.
    Environment,
    /// An extension-provided fallback api key.
    Fallback,
    /// A literal `models.json` api key.
    ModelsJsonKey,
    /// A `models.json` `!command` api key.
    ModelsJsonCommand,
}

/// A provider's configured auth status (`provider-composer.ts:70-74`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    /// Whether auth is configured and resolvable.
    pub configured: bool,
    /// Where the auth comes from, when configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<AuthSource>,
    /// A human-readable label (e.g. the env-var name(s)).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Compatibility request config for a single model (`provider-composer.ts:514-517`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompatibilityRequestConfig {
    /// The merged model + configured request headers, if any.
    pub headers: Option<BTreeMap<String, String>>,
    /// Whether to inject `Authorization: Bearer <key>`.
    pub auth_header: bool,
}

/// The composed provider view produced by [`compose_model_provider`].
///
/// Carries the credential-blind fields pi's composed `Provider` exposes that are
/// synchronously computable here: identity, `baseUrl`/`headers` precedence, and
/// the layered model list. Auth handlers, streaming, `refreshModels`, and
/// `filterModels` are deferred to the runtime slice (see the module docs).
#[derive(Debug, Clone, PartialEq)]
pub struct ComposedProvider {
    /// The provider id.
    pub id: String,
    /// The resolved provider display name.
    pub name: String,
    /// The resolved provider base URL, if any.
    pub base_url: Option<String>,
    /// The base provider's headers (inherited verbatim, pi's `headers:
    /// base?.headers`).
    pub headers: Option<ProviderHeaders>,
    models: Vec<Model>,
}

impl ComposedProvider {
    /// The layered model list (pi's `getModels()` initial snapshot).
    pub fn get_models(&self) -> &[Model] {
        &self.models
    }

    /// Consume the provider, yielding its layered model list.
    pub fn into_models(self) -> Vec<Model> {
        self.models
    }
}

/// A provider-composition error, mirroring the messages pi throws from the
/// composer's model-layering pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeError(pub String);

impl std::fmt::Display for ComposeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ComposeError {}

/// Clear the API-key (config-value command) cache (`provider-composer.ts:76`).
pub fn clear_api_key_cache() {
    clear_config_value_cache();
}

/// Deep-merge two `compat` blobs (`provider-composer.ts:78-98`).
///
/// A shallow spread of `override` over `base`, then a nested spread of the three
/// object-valued keys `openRouterRouting` / `vercelGatewayRouting` /
/// `chatTemplateKwargs` so that per-key routing settings merge instead of
/// replace. Returns `base` unchanged when `override` is absent.
fn merge_compat(base: Option<&Value>, over: Option<&Value>) -> Option<Value> {
    let Some(over) = over else {
        return base.cloned();
    };
    let base_obj = base.and_then(Value::as_object);
    let over_obj = over.as_object();
    let mut merged = base_obj.cloned().unwrap_or_default();
    if let Some(over_obj) = over_obj {
        for (key, value) in over_obj {
            merged.insert(key.clone(), value.clone());
        }
    }
    for key in [
        "openRouterRouting",
        "vercelGatewayRouting",
        "chatTemplateKwargs",
    ] {
        let base_value = base_obj.and_then(|m| m.get(key));
        let over_value = over_obj.and_then(|m| m.get(key));
        let base_is_object = matches!(base_value, Some(Value::Object(_)));
        let over_is_object = matches!(over_value, Some(Value::Object(_)));
        if base_is_object || over_is_object {
            let mut nested = base_value
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if let Some(over_nested) = over_value.and_then(Value::as_object) {
                for (nested_key, nested_value) in over_nested {
                    nested.insert(nested_key.clone(), nested_value.clone());
                }
            }
            merged.insert(key.to_string(), Value::Object(nested));
        }
    }
    Some(Value::Object(merged))
}

/// Apply a `models.json` per-model override to a base model
/// (`provider-composer.ts:100-122`).
fn apply_model_override(model: &Model, override_config: &ModelsJsonModelOverride) -> Model {
    let thinking_level_map = match &override_config.thinking_level_map {
        Some(over) => {
            let mut merged = model.thinking_level_map.clone().unwrap_or_default();
            for (level, value) in over {
                merged.insert(*level, value.clone());
            }
            Some(merged)
        }
        None => model.thinking_level_map.clone(),
    };
    let cost = match &override_config.cost {
        Some(over) => ModelCost {
            input: over.input.unwrap_or(model.cost.input),
            output: over.output.unwrap_or(model.cost.output),
            cache_read: over.cache_read.unwrap_or(model.cost.cache_read),
            cache_write: over.cache_write.unwrap_or(model.cost.cache_write),
            tiers: over.tiers.clone().or_else(|| model.cost.tiers.clone()),
        },
        None => model.cost.clone(),
    };
    Model {
        name: override_config
            .name
            .clone()
            .unwrap_or_else(|| model.name.clone()),
        reasoning: override_config.reasoning.unwrap_or(model.reasoning),
        thinking_level_map,
        input: override_config
            .input
            .clone()
            .unwrap_or_else(|| model.input.clone()),
        cost,
        context_window: override_config
            .context_window
            .unwrap_or(model.context_window),
        max_tokens: override_config.max_tokens.unwrap_or(model.max_tokens),
        compat: merge_compat(model.compat.as_ref(), override_config.compat.as_ref()),
        ..model.clone()
    }
}

/// Build a model from a `models.json` custom-model definition
/// (`provider-composer.ts:124-159`).
fn model_from_json(
    provider_id: &str,
    definition: &ModelsJsonModel,
    provider_config: &ModelsJsonProvider,
    defaults: Option<&Model>,
) -> Result<Model, ComposeError> {
    let api = definition
        .api
        .clone()
        .or_else(|| provider_config.api.clone())
        .or_else(|| defaults.map(|m| m.api.clone()));
    let Some(api) = api else {
        return Err(ComposeError(format!(
            "Provider {provider_id}, model {}: no \"api\" specified. Set at provider or model level.",
            definition.id
        )));
    };
    let base_url = definition
        .base_url
        .clone()
        .or_else(|| provider_config.base_url.clone())
        .or_else(|| defaults.map(|m| m.base_url.clone()));
    let Some(base_url) = base_url else {
        return Err(ComposeError(format!(
            "Provider {provider_id}: \"baseUrl\" is required when defining custom models."
        )));
    };
    if definition.context_window == Some(0) {
        return Err(ComposeError(format!(
            "Provider {provider_id}, model {}: invalid contextWindow",
            definition.id
        )));
    }
    if definition.max_tokens == Some(0) {
        return Err(ComposeError(format!(
            "Provider {provider_id}, model {}: invalid maxTokens",
            definition.id
        )));
    }
    Ok(Model {
        id: definition.id.clone(),
        name: definition
            .name
            .clone()
            .unwrap_or_else(|| definition.id.clone()),
        api,
        provider: provider_id.to_string(),
        base_url,
        reasoning: definition.reasoning.unwrap_or(false),
        thinking_level_map: definition.thinking_level_map.clone(),
        input: definition
            .input
            .clone()
            .unwrap_or_else(|| vec![Modality::Text]),
        cost: definition.cost.clone().unwrap_or(ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        }),
        context_window: definition.context_window.unwrap_or(128_000),
        max_tokens: definition.max_tokens.unwrap_or(16_384),
        headers: None,
        compat: merge_compat(provider_config.compat.as_ref(), definition.compat.as_ref()),
    })
}

/// Layer a `models.json` provider block onto the base catalog
/// (`provider-composer.ts:161-199`).
fn apply_models_json(
    provider_id: &str,
    base_models: &[Model],
    config: Option<&ModelsJsonProvider>,
) -> Result<Vec<Model>, ComposeError> {
    let Some(config) = config else {
        return Ok(base_models.to_vec());
    };
    if config.oauth.is_some() && config.base_url.is_none() {
        return Err(ComposeError(format!(
            "Provider {provider_id}: \"baseUrl\" is required when \"oauth\" is set."
        )));
    }
    let has_overrides = config
        .model_overrides
        .as_ref()
        .is_some_and(|overrides| !overrides.is_empty());
    let has_models = config
        .models
        .as_ref()
        .is_some_and(|models| !models.is_empty());
    if !has_models
        && config.base_url.is_none()
        && config.headers.is_none()
        && config.compat.is_none()
        && !has_overrides
        && config.api_key.is_none()
        && config.oauth.is_none()
        && config.auth_header.is_none()
    {
        return Err(ComposeError(format!(
            "Provider {provider_id}: must specify \"baseUrl\", \"headers\", \"compat\", \"modelOverrides\", or \"models\"."
        )));
    }

    let is_radius = config.oauth.as_deref() == Some("radius");
    let mut models: Vec<Model> = base_models
        .iter()
        .map(|model| Model {
            base_url: if is_radius {
                model.base_url.clone()
            } else {
                config
                    .base_url
                    .clone()
                    .unwrap_or_else(|| model.base_url.clone())
            },
            compat: merge_compat(model.compat.as_ref(), config.compat.as_ref()),
            ..model.clone()
        })
        .collect();
    for definition in config.models.iter().flatten() {
        let existing_index = models.iter().position(|model| model.id == definition.id);
        let defaults = match existing_index {
            Some(index) => models.get(index),
            None => models.first(),
        };
        let model = model_from_json(provider_id, definition, config, defaults)?;
        match existing_index {
            Some(index) => models[index] = model,
            None => models.push(model),
        }
    }
    Ok(models)
}

/// Layer an extension's model definitions onto the already-layered models
/// (`provider-composer.ts:201-228`).
fn apply_extension(
    provider_id: &str,
    models: &[Model],
    config: Option<&ProviderConfigInput>,
) -> Result<Vec<Model>, ComposeError> {
    let Some(config) = config else {
        return Ok(models.to_vec());
    };
    let Some(definitions) = &config.models else {
        return Ok(match &config.base_url {
            Some(base_url) => models
                .iter()
                .map(|model| Model {
                    base_url: base_url.clone(),
                    ..model.clone()
                })
                .collect(),
            None => models.to_vec(),
        });
    };
    definitions
        .iter()
        .map(|definition| {
            let defaults = models
                .iter()
                .find(|model| model.id == definition.id)
                .or_else(|| models.first());
            let api = definition
                .api
                .clone()
                .or_else(|| config.api.clone())
                .or_else(|| defaults.map(|m| m.api.clone()));
            let Some(api) = api else {
                return Err(ComposeError(format!(
                    "Provider {provider_id}, model {}: no \"api\" specified. Set at provider or model level.",
                    definition.id
                )));
            };
            let base_url = definition
                .base_url
                .clone()
                .or_else(|| config.base_url.clone())
                .or_else(|| defaults.map(|m| m.base_url.clone()));
            let Some(base_url) = base_url else {
                return Err(ComposeError(format!(
                    "Provider {provider_id}: \"baseUrl\" is required when defining custom models."
                )));
            };
            Ok(Model {
                id: definition.id.clone(),
                name: definition.name.clone(),
                api,
                provider: provider_id.to_string(),
                base_url,
                reasoning: definition.reasoning,
                thinking_level_map: definition.thinking_level_map.clone(),
                input: definition.input.clone(),
                cost: definition.cost.clone(),
                context_window: definition.context_window,
                max_tokens: definition.max_tokens,
                headers: None,
                compat: definition.compat.clone(),
            })
        })
        .collect()
}

/// Collect the per-model request headers from every config layer
/// (`provider-composer.ts:384-397`).
fn raw_model_headers(
    model: &Model,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<HashMap<String, String>> {
    let mut headers: HashMap<String, String> = HashMap::new();
    if let Some(override_headers) = config
        .and_then(|c| c.model_overrides.as_ref())
        .and_then(|overrides| overrides.get(&model.id))
        .and_then(|entry| entry.headers.as_ref())
    {
        for (key, value) in override_headers {
            headers.insert(key.clone(), value.clone());
        }
    }
    if let Some(definition_headers) = config
        .and_then(|c| c.models.as_ref())
        .and_then(|models| models.iter().find(|entry| entry.id == model.id))
        .and_then(|entry| entry.headers.as_ref())
    {
        for (key, value) in definition_headers {
            headers.insert(key.clone(), value.clone());
        }
    }
    if let Some(extension_headers) = extension
        .and_then(|e| e.models.as_ref())
        .and_then(|models| models.iter().find(|entry| entry.id == model.id))
        .and_then(|entry| entry.headers.as_ref())
    {
        for (key, value) in extension_headers {
            headers.insert(key.clone(), value.clone());
        }
    }
    if headers.is_empty() {
        None
    } else {
        Some(headers)
    }
}

/// The configured api key across the config layers (`provider-composer.ts:264-269`).
fn configured_api_key(
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<String> {
    extension
        .and_then(|e| e.api_key.clone())
        .or_else(|| config.and_then(|c| c.api_key.clone()))
}

/// The merged provider-level request headers (`provider-composer.ts:271-277`).
fn configured_headers(
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<HashMap<String, String>> {
    let config_headers = config.and_then(|c| c.headers.as_ref());
    let extension_headers = extension.and_then(|e| e.headers.as_ref());
    if config_headers.is_none() && extension_headers.is_none() {
        return None;
    }
    let mut headers: HashMap<String, String> = HashMap::new();
    for (key, value) in config_headers.into_iter().flatten() {
        headers.insert(key.clone(), value.clone());
    }
    for (key, value) in extension_headers.into_iter().flatten() {
        headers.insert(key.clone(), value.clone());
    }
    Some(headers)
}

/// The layered model list for a provider, pi's `getModels` initial snapshot
/// (`provider-composer.ts:425-438`).
///
/// The stateful extension-OAuth `modifyModels` projection (which only fires
/// after a live refresh delivers an OAuth credential) is deferred to the runtime
/// slice; at compose time no such credential exists, matching pi's initial
/// state.
fn compose_models(
    provider_id: &str,
    base_models: &[Model],
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Result<Vec<Model>, ComposeError> {
    let layered = apply_extension(
        provider_id,
        &apply_models_json(provider_id, base_models, config)?,
        extension,
    )?;
    Ok(layered
        .into_iter()
        .map(|model| {
            match config
                .and_then(|c| c.model_overrides.as_ref())
                .and_then(|overrides| overrides.get(&model.id))
            {
                Some(override_config) => apply_model_override(&model, override_config),
                None => model,
            }
        })
        .collect())
}

/// Validate an extension provider's structural config (`provider-composer.ts:399-409`).
///
/// `base_models` is the base provider's model list (pi's `base?.getModels() ??
/// []`). Errors mirror the composer's thrown messages.
pub fn validate_extension_provider(
    provider_id: &str,
    base_models: &[Model],
    models_config: Option<&ModelsJsonProvider>,
    extension: &ProviderConfigInput,
) -> Result<(), ComposeError> {
    if extension.stream_simple && extension.api.is_none() {
        return Err(ComposeError(format!(
            "Provider {provider_id}: \"api\" is required when registering streamSimple."
        )));
    }
    let after_json = apply_models_json(provider_id, base_models, models_config)?;
    apply_extension(provider_id, &after_json, Some(extension))?;
    Ok(())
}

/// Compose built-in, `models.json`, and extension layers without reading
/// credentials (`provider-composer.ts:411-499`).
///
/// Resolves provider identity/`baseUrl`/`headers` precedence and the layered
/// model list. Composition errors are surfaced eagerly (pi calls `getModels()`
/// once to report structural errors immediately). Auth handlers, streaming,
/// `refreshModels`, and `filterModels` are deferred to the runtime slice.
pub fn compose_model_provider(
    provider_id: &str,
    base: Option<&RegistryProvider>,
    model_config: &ModelConfig,
    extension: Option<&ProviderConfigInput>,
) -> Result<ComposedProvider, ComposeError> {
    let config = model_config.get_provider(provider_id);
    let base_models = base.map(RegistryProvider::get_models).unwrap_or_default();
    let models = compose_models(provider_id, &base_models, config, extension)?;

    let name = extension
        .and_then(|e| e.name.clone())
        .or_else(|| config.and_then(|c| c.name.clone()))
        .or_else(|| base.map(|b| b.name().to_string()))
        .or_else(|| extension.and_then(|e| e.oauth.as_ref().map(|o| o.name.clone())))
        .unwrap_or_else(|| provider_id.to_string());
    let base_url = extension
        .and_then(|e| e.base_url.clone())
        .or_else(|| config.and_then(|c| c.base_url.clone()))
        .or_else(|| base.and_then(|b| b.base_url().map(str::to_string)));

    Ok(ComposedProvider {
        id: provider_id.to_string(),
        name,
        base_url,
        headers: base.and_then(|b| b.headers().cloned()),
        models,
    })
}

/// Resolve a model's configured request headers (`provider-composer.ts:501-512`).
pub fn resolve_configured_model_headers(
    model: &Model,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
    env: Option<&HashMap<String, String>>,
) -> Result<Option<BTreeMap<String, String>>, ConfigValueError> {
    let resolved = resolve_headers_or_throw(
        raw_model_headers(model, config, extension).as_ref(),
        &format!("model \"{}/{}\"", model.provider, model.id),
        env,
    )?;
    Ok(resolved.map(into_btree))
}

/// Resolve a model's compatibility request config (`provider-composer.ts:519-532`).
pub fn resolve_compatibility_request_config(
    model: &Model,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Result<CompatibilityRequestConfig, ConfigValueError> {
    let mut combined: HashMap<String, String> = HashMap::new();
    for (key, value) in configured_headers(config, extension).into_iter().flatten() {
        combined.insert(key, value);
    }
    for (key, value) in raw_model_headers(model, config, extension)
        .into_iter()
        .flatten()
    {
        combined.insert(key, value);
    }
    let combined = if combined.is_empty() {
        None
    } else {
        Some(combined)
    };
    let configured = resolve_headers_or_throw(
        combined.as_ref(),
        &format!("model \"{}/{}\"", model.provider, model.id),
        None,
    )?;

    let headers = if model.headers.is_some() || configured.is_some() {
        let mut merged: BTreeMap<String, String> = model.headers.clone().unwrap_or_default();
        for (key, value) in configured.into_iter().flatten() {
            merged.insert(key, value);
        }
        Some(merged)
    } else {
        None
    };
    Ok(CompatibilityRequestConfig {
        headers,
        auth_header: extension
            .and_then(|e| e.auth_header)
            .or_else(|| config.and_then(|c| c.auth_header))
            .unwrap_or(false),
    })
}

/// Derive a provider's configured auth status (`provider-composer.ts:534-548`).
pub fn configured_request_auth_status(
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<AuthStatus> {
    let value = configured_api_key(config, extension)?;
    if is_command_config_value(&value) {
        return Some(AuthStatus {
            configured: true,
            source: Some(AuthSource::ModelsJsonCommand),
            label: None,
        });
    }
    let names = get_config_value_env_var_names(&value);
    if !names.is_empty() {
        return Some(if is_config_value_configured(&value, None) {
            AuthStatus {
                configured: true,
                source: Some(AuthSource::Environment),
                label: Some(names.join(", ")),
            }
        } else {
            AuthStatus {
                configured: false,
                source: None,
                label: None,
            }
        });
    }
    let source = if extension.and_then(|e| e.api_key.as_ref()).is_some() {
        AuthSource::Fallback
    } else {
        AuthSource::ModelsJsonKey
    };
    Some(AuthStatus {
        configured: true,
        source: Some(source),
        label: None,
    })
}

/// Sort a resolved header map into a deterministic [`BTreeMap`] for the typed
/// public surface (`resolve_headers_or_throw` yields an unordered [`HashMap`]).
fn into_btree(headers: HashMap<String, String>) -> BTreeMap<String, String> {
    headers.into_iter().collect()
}

#[cfg(test)]
mod tests;
