// straitjacket-allow-file:duplication — a faithful transcription of the AUTH
// layer of pi's `provider-composer.ts`. The composed `ApiKeyAuth`/`OAuthAuth`
// handlers mirror pi's parallel `resolve`/`check`/`toAuth` closures member for
// member, and the `config`/`extension` field-precedence helpers repeat the same
// `extension ?? config` / merge shape the credential-blind half (ported in
// pidgin-coding's `provider_composer.rs`) also uses. The clone detector reads
// these repeated precedence runs and parallel handler bodies as duplicates; they
// are distinct, load-bearing auth-composition rules kept verbatim to mirror pi's
// per-request auth resolution.
//! Provider-composer AUTH layer.
//!
//! Ported from the credential-*aware* half of pi's
//! `packages/coding-agent/src/core/provider-composer.ts` at pinned commit
//! `3da591ab`. In pi this lives in the coding-agent package alongside the
//! credential-blind half, and so it does here: this module lives in pidgin-coding
//! next to [`provider_composer`](super::provider_composer). Its primitives operate
//! on pidgin-ai's own rich auth traits ([`ApiKeyAuth`], [`OAuthAuth`],
//! [`ProviderAuth`], [`ModelAuth`], [`AuthResult`]) and on
//! [`RegistryProvider`](pidgin_ai::providers::registry::RegistryProvider), which
//! pidgin-coding reaches cross-crate. The runtime (model-runtime, same crate)
//! calls [`compose_model_provider`] to assemble the rich composed provider.
//!
//! # Split from the credential-blind half
//!
//! pidgin-coding's `core/provider_composer.rs` (PR #119) already ported the
//! pure, synchronously-computable half — the model-layering pipeline
//! (`apply_models_json`/`apply_extension`/`modelOverrides`), the request-header
//! helpers, `configured_request_auth_status`, `validate_extension_provider`, and
//! the credential-blind identity/model-list assembly of `composeModelProvider`.
//! This module adds only the credential-aware AUTH surface pi keeps in the same
//! file: [`with_configured_auth`] (`:250`), [`config_context_env`] (`:279`),
//! [`compose_api_key_auth`] (`:293`), [`compose_oauth_auth`] (`:359`), and the
//! auth/streaming assembly of [`compose_model_provider`] (`:412`). It does not
//! re-port the pure half; [`compose_model_provider`] takes the already-layered
//! model list and resolved identity from that half and contributes the rich
//! [`ProviderAuth`], the `"no authentication method configured"` throw, the
//! `supportsBaseApi` streaming dispatch, and the eager stream wiring.
//!
//! # Config-value resolution
//!
//! pi's composers call `resolve-config-value.ts` (`resolveConfigValueOrThrow` /
//! `resolveHeadersOrThrow` / `getConfigValueEnvVarNames` / `isCommandConfigValue`)
//! to turn a configured `$ENV` / `!command` / literal string into a value. That
//! resolver is ported in this crate as
//! [`resolve_config_value`](super::resolve_config_value); now that the composers
//! and the resolver co-locate in `pidgin-coding`, the composers call it directly
//! (the earlier `ConfigValueResolver` seam existed only to cross the former
//! pidgin-ai <-> pidgin-coding boundary and has collapsed). The composed
//! api-key/OAuth handlers speak [`BTreeMap`] for their configured headers/env, so
//! small `btree_to_hash`/`into_btree` bridges adapt those to the resolver's
//! [`HashMap`](std::collections::HashMap) surface at the call boundary.
//!
//! # `adaptOAuth` bridge
//!
//! pi's `adaptOAuth` (`:230`) bridges an extension's callback-style OAuth flow
//! (`login(callbacks)` / `refreshToken` / `getApiKey`) to a canonical
//! [`OAuthAuth`]. pidgin-ai's [`OAuthAuth`] is flow-machine-shaped, so the
//! re-inversion (presenting the push login as a suspend/resume flow machine)
//! lives in [`super::extension_oauth_adapt`] behind a thread + channel bridge;
//! [`adapt_oauth`] is the thin call into it. [`compose_oauth_auth`] references it
//! exactly where pi references `adaptOAuth`, so composing an *extension* OAuth
//! provider drives the bridge while composing a base OAuth provider (the tested
//! path) does not.
//!
//! # Streaming
//!
//! pidgin-ai's stream seam ([`RegistryProvider::stream`] / the api-registry
//! [`get_api_provider`]) is eager and synchronous — auth resolves synchronously —
//! so `streamWith` (`:446`) is wired directly through it with no `lazyStream`.
//! The `extension.streamSimple` branch (an effectful runtime concern, deferred
//! with the rest of the runtime slice) is not wired here.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use pidgin_ai::auth::error::AuthFlowError;
use pidgin_ai::auth::oauth::extension::ExtensionOAuthLogin;
use pidgin_ai::auth::oauth::flow::OAuthFlowMachine;
use pidgin_ai::auth::types::{
    ApiKeyAuth, ApiKeyCredential, AuthCheck, AuthContext, AuthInteraction, AuthPrompt,
    AuthPromptKind, AuthResult, AuthType, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuth,
    ProviderHeaders,
};
use pidgin_ai::compat::get_api_provider;
use pidgin_ai::providers::registry::RegistryProvider;
use pidgin_ai::seams::provider::{AbortSignal, StreamResult};
use pidgin_ai::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Model, StopReason,
    StreamOptions, Usage, UsageCost,
};

use super::extension_oauth_adapt::adapt_extension_oauth;
use super::resolve_config_value::{
    get_config_value_env_var_names, is_command_config_value, resolve_config_value_or_throw,
    resolve_headers_or_throw,
};

/// Bridge the composers' [`BTreeMap`] header/env surface onto the resolver's
/// [`HashMap`] one (pi keeps a single map type; the Rust ports differ, so the
/// boundary converts).
fn btree_to_hash(map: &BTreeMap<String, String>) -> HashMap<String, String> {
    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// The inverse of [`btree_to_hash`], reordering the resolver's unordered
/// [`HashMap`] result back into the composers' deterministic [`BTreeMap`].
fn into_btree(map: HashMap<String, String>) -> BTreeMap<String, String> {
    map.into_iter().collect()
}

/// Minimal ai-side mirror of the `models.json` provider block's auth-relevant
/// fields, the `config` layer of pi's composers (`provider-composer.ts`).
///
/// `pidgin-coding` adapts its richer `ModelsJsonProvider` into this at the call
/// site; only the fields the AUTH layer reads (`apiKey` / `headers` /
/// `authHeader`) are carried.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderAuthConfig {
    /// A configured API key (literal, `$ENV`, or `!command`).
    pub api_key: Option<String>,
    /// Provider-level request headers (values may be `$ENV` refs).
    pub headers: Option<BTreeMap<String, String>>,
    /// Whether to inject `Authorization: Bearer <key>`.
    pub auth_header: Option<bool>,
}

/// OAuth config an extension may attach to a provider (`provider-composer.ts:33-41`).
///
/// pi's `ExtensionOAuthConfig` carries the effectful
/// `login`/`refreshToken`/`getApiKey` members that drive the extension OAuth
/// flow; [`login`](Self::login) carries those (as an [`ExtensionOAuthLogin`]
/// callable) so [`adapt_oauth`] can bridge them onto the flow-machine
/// [`OAuthAuth`]. The concrete login is JS in the extension plane
/// (pidgin-extensions); it wires its closures onto the trait, so `login` is
/// `None` until that wiring runs. pi's `modifyModels` is a model-layering concern
/// of the credential-blind half and is not modeled on this auth surface.
#[derive(Clone)]
pub struct ExtensionOAuthConfig {
    /// The OAuth handler name (also a provider-name fallback).
    pub name: String,
    /// Retained for extension source compatibility; ignored by canonical auth
    /// flows (pi's deprecated `usesCallbackServer`).
    pub uses_callback_server: Option<bool>,
    /// The extension's callback-driven login callable (pi's
    /// `login`/`refreshToken`/`getApiKey`), wired by the extension plane. `None`
    /// until wired; [`adapt_oauth`]'s flow machines then report a wiring error.
    pub login: Option<Arc<dyn ExtensionOAuthLogin>>,
}

// `login` is a trait object, so `Debug`/`PartialEq`/`Eq` (which slice C's config
// surface exposed) are hand-written to skip it: two configs are equal on their
// declarative fields, and the login callable is opaque.
impl std::fmt::Debug for ExtensionOAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionOAuthConfig")
            .field("name", &self.name)
            .field("uses_callback_server", &self.uses_callback_server)
            .field("login", &self.login.as_ref().map(|_| "<login>"))
            .finish()
    }
}

impl PartialEq for ExtensionOAuthConfig {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.uses_callback_server == other.uses_callback_server
    }
}

impl Eq for ExtensionOAuthConfig {}

/// Minimal ai-side mirror of the extension `registerProvider` input's
/// auth-relevant fields, the `extension` layer of pi's composers
/// (`provider-composer.ts:44-68`).
#[derive(Default)]
pub struct ExtensionAuthConfig {
    /// A configured API key (literal, `$ENV`, or `!command`).
    pub api_key: Option<String>,
    /// Provider-level request headers (values may be `$ENV` refs).
    pub headers: Option<BTreeMap<String, String>>,
    /// Whether to inject `Authorization: Bearer <key>`.
    pub auth_header: Option<bool>,
    /// An attached OAuth config (bridged through [`adapt_oauth`]).
    pub oauth: Option<ExtensionOAuthConfig>,
}

/// The configured api key across the config layers (`provider-composer.ts:264-269`).
fn configured_api_key(
    config: Option<&ProviderAuthConfig>,
    extension: Option<&ExtensionAuthConfig>,
) -> Option<String> {
    extension
        .and_then(|e| e.api_key.clone())
        .or_else(|| config.and_then(|c| c.api_key.clone()))
}

/// The merged provider-level request headers (`provider-composer.ts:271-277`).
fn configured_headers(
    config: Option<&ProviderAuthConfig>,
    extension: Option<&ExtensionAuthConfig>,
) -> Option<BTreeMap<String, String>> {
    let config_headers = config.and_then(|c| c.headers.as_ref());
    let extension_headers = extension.and_then(|e| e.headers.as_ref());
    if config_headers.is_none() && extension_headers.is_none() {
        return None;
    }
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in config_headers.into_iter().flatten() {
        headers.insert(key.clone(), value.clone());
    }
    for (key, value) in extension_headers.into_iter().flatten() {
        headers.insert(key.clone(), value.clone());
    }
    Some(headers)
}

/// The configured `authHeader` flag, extension winning over config
/// (`extension?.authHeader ?? config?.authHeader ?? false`).
fn configured_auth_header(
    config: Option<&ProviderAuthConfig>,
    extension: Option<&ExtensionAuthConfig>,
) -> bool {
    extension
        .and_then(|e| e.auth_header)
        .or_else(|| config.and_then(|c| c.auth_header))
        .unwrap_or(false)
}

/// Merge configured headers onto a resolved [`ModelAuth`] and, when `auth_header`
/// is set, inject `Authorization: Bearer <apiKey>` (`provider-composer.ts:250-262`).
///
/// Throws the verbatim `"authHeader requires a resolved API key"` when
/// `auth_header` is set but no api key resolved.
pub fn with_configured_auth(
    auth: ModelAuth,
    headers: Option<&BTreeMap<String, String>>,
    auth_header: bool,
) -> Result<ModelAuth, AuthFlowError> {
    // `auth.headers || headers ? { ...auth.headers, ...headers } : undefined`.
    let mut merged: Option<ProviderHeaders> = if auth.headers.is_some() || headers.is_some() {
        let mut m: ProviderHeaders = auth.headers.clone().unwrap_or_default();
        for (key, value) in headers.into_iter().flatten() {
            m.insert(key.clone(), Some(value.clone()));
        }
        Some(m)
    } else {
        None
    };
    if auth_header {
        let Some(api_key) = auth.api_key.as_ref() else {
            return Err(AuthFlowError::new("authHeader requires a resolved API key"));
        };
        let mut m = merged.unwrap_or_default();
        m.insert(
            "Authorization".to_string(),
            Some(format!("Bearer {api_key}")),
        );
        merged = Some(m);
    }
    Ok(ModelAuth {
        headers: merged,
        ..auth
    })
}

/// Assemble the env context for resolving config values: seed with `explicit`,
/// then fill in any referenced env-var names from `ctx` (`provider-composer.ts:279-291`).
pub fn config_context_env(
    values: &[&str],
    ctx: &dyn AuthContext,
    explicit: Option<&BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    let mut env: BTreeMap<String, String> = explicit.cloned().unwrap_or_default();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for value in values {
        for name in get_config_value_env_var_names(value) {
            if !seen.insert(name.clone()) {
                continue;
            }
            if env.contains_key(&name) {
                continue;
            }
            if let Some(resolved) = ctx.env(&name) {
                env.insert(name, resolved);
            }
        }
    }
    if env.is_empty() {
        None
    } else {
        Some(env)
    }
}

/// The composed api-key auth returned by [`compose_api_key_auth`], layering a
/// configured key + headers + `authHeader` over an inherited base handler
/// (`provider-composer.ts:293-357`).
struct ComposedApiKeyAuth {
    provider_id: String,
    name: String,
    inherited: Option<Box<dyn ApiKeyAuth>>,
    raw_key: Option<String>,
    raw_headers: Option<BTreeMap<String, String>>,
    auth_header: bool,
}

impl ComposedApiKeyAuth {
    /// Resolve + merge the configured headers over a base [`AuthResult`], then
    /// apply the `authHeader` Bearer injection (`provider-composer.ts:350-354`).
    fn finish(
        &self,
        credential: Option<&ApiKeyCredential>,
        ctx: &dyn AuthContext,
        base: AuthResult,
    ) -> Result<AuthResult, AuthFlowError> {
        let AuthResult {
            auth: base_auth,
            env: result_env,
            source,
        } = base;
        // explicitEnv = { ...(credential?.env ?? {}), ...(result.env ?? {}) }.
        let mut explicit_env: BTreeMap<String, String> = BTreeMap::new();
        for (key, value) in credential
            .and_then(|c| c.env.as_ref())
            .into_iter()
            .flatten()
        {
            explicit_env.insert(key.clone(), value.clone());
        }
        for (key, value) in result_env.as_ref().into_iter().flatten() {
            explicit_env.insert(key.clone(), value.clone());
        }
        let explicit_ref = if explicit_env.is_empty() {
            None
        } else {
            Some(&explicit_env)
        };
        let header_values: Vec<&str> = self
            .raw_headers
            .as_ref()
            .map(|h| h.values().map(String::as_str).collect())
            .unwrap_or_default();
        let header_env = config_context_env(&header_values, ctx, explicit_ref);
        let raw_headers = self.raw_headers.as_ref().map(btree_to_hash);
        let header_env_hash = header_env.as_ref().map(btree_to_hash);
        let headers = resolve_headers_or_throw(
            raw_headers.as_ref(),
            &format!("provider \"{}\"", self.provider_id),
            header_env_hash.as_ref(),
        )
        .map_err(|e| AuthFlowError::new(e.to_string()))?
        .map(into_btree);
        let auth = with_configured_auth(base_auth, headers.as_ref(), self.auth_header)?;
        Ok(AuthResult {
            auth,
            env: result_env,
            source,
        })
    }
}

impl ApiKeyAuth for ComposedApiKeyAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Option<Result<ApiKeyCredential, AuthFlowError>> {
        // `inherited?.login ?? (default "Enter API key" secret prompt)`.
        if let Some(inherited) = &self.inherited {
            if let Some(result) = inherited.login(interaction) {
                return Some(result);
            }
        }
        let result = interaction
            .prompt(AuthPrompt {
                signal: None,
                kind: AuthPromptKind::Secret {
                    message: "Enter API key".to_string(),
                    placeholder: None,
                },
            })
            .map(|key| ApiKeyCredential {
                key: Some(key),
                env: None,
            });
        Some(result)
    }

    fn check(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Option<AuthCheck> {
        // # Sync-port deviation
        //
        // pi branches on whether `inherited` *has* a `check` method
        // (`if (inherited?.check) return inherited.check(input)`). pidgin-ai's
        // [`ApiKeyAuth::check`] has a default `None` impl, so "has a check" is not
        // observable; this port calls `inherited.check` and, when it yields
        // `None`, falls through to the resolve-based fallback, which is the same
        // observable result pi produces for an inherited handler without a check.
        if let Some(cred) = credential {
            if let Some(inherited) = &self.inherited {
                if let Some(check) = inherited.check(ctx, Some(cred)) {
                    return Some(check);
                }
            }
            if cred.key.is_some() {
                return Some(AuthCheck {
                    source: Some("stored credential".to_string()),
                    check_type: AuthType::ApiKey,
                });
            }
            return self
                .inherited
                .as_ref()
                .and_then(|inherited| inherited.resolve(ctx, Some(cred)).ok().flatten())
                .map(|resolved| AuthCheck {
                    source: resolved.source,
                    check_type: AuthType::ApiKey,
                });
        }
        if let Some(raw_key) = &self.raw_key {
            if is_command_config_value(raw_key) {
                return Some(AuthCheck {
                    source: Some("configured API key".to_string()),
                    check_type: AuthType::ApiKey,
                });
            }
            for name in get_config_value_env_var_names(raw_key) {
                ctx.env(&name)?;
            }
            return Some(AuthCheck {
                source: Some("configured API key".to_string()),
                check_type: AuthType::ApiKey,
            });
        }
        if let Some(inherited) = &self.inherited {
            if let Some(check) = inherited.check(ctx, None) {
                return Some(check);
            }
            return inherited
                .resolve(ctx, None)
                .ok()
                .flatten()
                .map(|resolved| AuthCheck {
                    source: resolved.source,
                    check_type: AuthType::ApiKey,
                });
        }
        None
    }

    fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, AuthFlowError> {
        let base: Option<AuthResult> = if let Some(cred) = credential {
            if let Some(inherited) = &self.inherited {
                inherited.resolve(ctx, Some(cred))?
            } else {
                cred.key.as_ref().map(|key| AuthResult {
                    auth: ModelAuth {
                        api_key: Some(key.clone()),
                        ..ModelAuth::default()
                    },
                    env: cred.env.clone(),
                    source: Some("stored credential".to_string()),
                })
            }
        } else if let Some(raw_key) = &self.raw_key {
            let env = config_context_env(&[raw_key.as_str()], ctx, None);
            let env_hash = env.as_ref().map(btree_to_hash);
            let key = resolve_config_value_or_throw(
                raw_key,
                &format!("API key for provider \"{}\"", self.provider_id),
                env_hash.as_ref(),
            )
            .map_err(|e| AuthFlowError::new(e.to_string()))?;
            if let Some(inherited) = &self.inherited {
                let synthetic = ApiKeyCredential {
                    key: Some(key),
                    env: None,
                };
                inherited.resolve(ctx, Some(&synthetic))?
            } else {
                Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(key),
                        ..ModelAuth::default()
                    },
                    env: None,
                    source: Some("configured API key".to_string()),
                })
            }
        } else if let Some(inherited) = &self.inherited {
            inherited.resolve(ctx, None)?
        } else {
            None
        };
        match base {
            Some(base) => Ok(Some(self.finish(credential, ctx, base)?)),
            None => Ok(None),
        }
    }
}

/// Compose a provider's api-key auth handler from an inherited base handler and
/// the configured `config`/`extension` layers (`provider-composer.ts:293-357`).
///
/// Returns `None` for an OAuth-only provider (`!inherited && rawKey === undefined
/// && oauth`): no fabricated api-key login method. `base_has_oauth` reports
/// whether the base provider carries an OAuth handler (pi's `base?.auth.oauth`).
pub fn compose_api_key_auth(
    provider_id: &str,
    inherited: Option<Box<dyn ApiKeyAuth>>,
    base_has_oauth: bool,
    config: Option<&ProviderAuthConfig>,
    extension: Option<&ExtensionAuthConfig>,
) -> Option<Box<dyn ApiKeyAuth>> {
    let raw_key = configured_api_key(config, extension);
    let has_oauth = extension.is_some_and(|e| e.oauth.is_some()) || base_has_oauth;
    // OAuth-only providers get no fabricated API-key login method.
    if inherited.is_none() && raw_key.is_none() && has_oauth {
        return None;
    }
    let raw_headers = configured_headers(config, extension);
    let auth_header = configured_auth_header(config, extension);
    let name = inherited
        .as_ref()
        .map(|i| i.name().to_string())
        .unwrap_or_else(|| "API key".to_string());
    Some(Box::new(ComposedApiKeyAuth {
        provider_id: provider_id.to_string(),
        name,
        inherited,
        raw_key,
        raw_headers,
        auth_header,
    }))
}

/// The composed OAuth auth returned by [`compose_oauth_auth`], wrapping a source
/// handler with a `to_auth` that layers configured headers + `authHeader`
/// (`provider-composer.ts:359-382`).
struct ComposedOAuthAuth {
    provider_id: String,
    inner: Box<dyn OAuthAuth>,
    raw_headers: Option<BTreeMap<String, String>>,
    auth_header: bool,
}

impl OAuthAuth for ComposedOAuthAuth {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn login_label(&self) -> Option<&str> {
        self.inner.login_label()
    }

    fn login_machine(&self) -> Box<dyn OAuthFlowMachine> {
        self.inner.login_machine()
    }

    fn refresh_machine(&self, credential: &OAuthCredential) -> Box<dyn OAuthFlowMachine> {
        self.inner.refresh_machine(credential)
    }

    fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError> {
        let auth = self.inner.to_auth(credential)?;
        // pi reads `credential.env` off the open OAuthCredentials index
        // signature; here it lives under the flattened `extra` map.
        let env: Option<BTreeMap<String, String>> = credential.extra.get("env").and_then(|value| {
            value.as_object().map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
        });
        let raw_headers = self.raw_headers.as_ref().map(btree_to_hash);
        let env_hash = env.as_ref().map(btree_to_hash);
        let headers = resolve_headers_or_throw(
            raw_headers.as_ref(),
            &format!("provider \"{}\"", self.provider_id),
            env_hash.as_ref(),
        )
        .map_err(|e| AuthFlowError::new(e.to_string()))?
        .map(into_btree);
        with_configured_auth(auth, headers.as_ref(), self.auth_header)
    }
}

/// Compose a provider's OAuth auth handler (`provider-composer.ts:359-382`).
///
/// The source handler is the extension's OAuth (via [`adapt_oauth`])
/// when present, else the base provider's OAuth (`base_oauth`). Returns `None`
/// when neither is present. The returned handler layers the configured headers
/// and `authHeader` onto the source's `to_auth`.
pub fn compose_oauth_auth(
    provider_id: &str,
    base_oauth: Option<Box<dyn OAuthAuth>>,
    config: Option<&ProviderAuthConfig>,
    extension: Option<&ExtensionAuthConfig>,
) -> Option<Box<dyn OAuthAuth>> {
    let inner = match extension.and_then(|e| e.oauth.clone()) {
        // `extension?.oauth ? adaptOAuth(extension.oauth) : base?.auth.oauth`.
        Some(oauth) => adapt_oauth(oauth),
        None => base_oauth?,
    };
    let raw_headers = configured_headers(config, extension);
    let auth_header = configured_auth_header(config, extension);
    Some(Box::new(ComposedOAuthAuth {
        provider_id: provider_id.to_string(),
        inner,
        raw_headers,
        auth_header,
    }))
}

/// Bridge an extension's callback-style OAuth config to a canonical
/// [`OAuthAuth`] (`provider-composer.ts:230-248` `adaptOAuth`).
///
/// pi's `adaptOAuth` wraps the extension's push-based `login(callbacks)` /
/// `refreshToken` / `getApiKey` closures, mapping the callback surface onto
/// `interaction.notify` / `interaction.prompt`. pidgin-ai's [`OAuthAuth`] is the
/// pull flow machine, so the re-inversion — presenting the push login as a
/// suspend/resume machine — lives in [`super::extension_oauth_adapt`] behind a
/// thread + channel bridge. This function just hands that adapter the config's
/// [`ExtensionOAuthConfig::login`] callable; it is only reached when composing an
/// *extension* OAuth provider.
pub fn adapt_oauth(config: ExtensionOAuthConfig) -> Box<dyn OAuthAuth> {
    adapt_extension_oauth(config.name, config.login)
}

/// The composed provider produced by [`compose_model_provider`].
///
/// Carries the rich composed [`ProviderAuth`] (api-key and/or OAuth handlers),
/// the already-layered model list, resolved identity, and the eager stream
/// dispatch. This is pidgin-ai's counterpart to pi's composed `Provider`; the
/// credential-blind identity/model assembly is done by
/// `pidgin-coding::core::provider_composer` (PR #119) and passed in.
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
    /// The composed rich auth handlers.
    pub auth: ProviderAuth,
    models: Vec<Model>,
    base: Option<Arc<RegistryProvider>>,
}

impl ComposedProvider {
    /// The layered model list (pi's `getModels()` snapshot).
    pub fn get_models(&self) -> &[Model] {
        &self.models
    }

    /// Whether the base provider serves `model.api` (pi's `supportsBaseApi`,
    /// `provider-composer.ts:445`).
    pub fn supports_base_api(&self, model: &Model) -> bool {
        self.base
            .as_ref()
            .map(|base| base.get_models().iter().any(|entry| entry.api == model.api))
            .unwrap_or(false)
    }

    /// Stream a response for `model`, pi's `streamWith` (`provider-composer.ts:446-466`)
    /// wired to the eager seam: delegate to the base provider when it serves the
    /// model's api, else dispatch through the api registry
    /// ([`get_api_provider`]). The `extension.streamSimple` branch is a runtime
    /// concern and is not wired here.
    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> StreamResult {
        if let Some(base) = &self.base {
            if self.supports_base_api(model) {
                return base.stream(model, context, options, signal);
            }
        }
        match get_api_provider(&model.api) {
            Some(api) => match api.stream(model, context, options, signal) {
                Ok(result) => result,
                Err(error) => error_stream(model, error.to_string()),
            },
            None => error_stream(
                model,
                format!("No API provider registered for api: {}", model.api),
            ),
        }
    }
}

/// An error thrown while composing a provider's auth, mirroring pi's
/// `composeModelProvider` throw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeAuthError(pub String);

impl std::fmt::Display for ComposeAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ComposeAuthError {}

/// Inputs to [`compose_model_provider`].
///
/// The already-layered `models` and resolved `name`/`base_url`/`headers` come
/// from the credential-blind half (`pidgin-coding::core::provider_composer`, PR
/// #119); the `base_api_key`/`base_oauth` are the base provider's rich handlers
/// (pi's `base?.auth.apiKey`/`base?.auth.oauth`); `base` is retained for stream
/// dispatch and `supportsBaseApi`.
pub struct ComposeModelProviderInput {
    /// The provider id.
    pub provider_id: String,
    /// The base provider, for stream dispatch and `supportsBaseApi`.
    pub base: Option<Arc<RegistryProvider>>,
    /// The base provider's inherited api-key handler (pi's `base?.auth.apiKey`).
    pub base_api_key: Option<Box<dyn ApiKeyAuth>>,
    /// The base provider's inherited OAuth handler (pi's `base?.auth.oauth`).
    pub base_oauth: Option<Box<dyn OAuthAuth>>,
    /// The `models.json` provider auth config.
    pub config: Option<ProviderAuthConfig>,
    /// The extension provider auth config.
    pub extension: Option<ExtensionAuthConfig>,
    /// The already-layered model list.
    pub models: Vec<Model>,
    /// The resolved provider display name.
    pub name: String,
    /// The resolved provider base URL.
    pub base_url: Option<String>,
    /// The base provider's headers.
    pub headers: Option<ProviderHeaders>,
}

/// Guard that at least one auth method composed, else throw pi's verbatim
/// `"Provider {id}: no authentication method configured."` (`provider-composer.ts:443`).
fn require_auth_method(
    provider_id: &str,
    api_key: Option<Box<dyn ApiKeyAuth>>,
    oauth: Option<Box<dyn OAuthAuth>>,
) -> Result<ProviderAuth, ComposeAuthError> {
    if api_key.is_none() && oauth.is_none() {
        return Err(ComposeAuthError(format!(
            "Provider {provider_id}: no authentication method configured."
        )));
    }
    Ok(ProviderAuth { api_key, oauth })
}

/// Assemble the composed provider's rich auth over an already-layered model list
/// and resolved identity (the AUTH half of pi's `composeModelProvider`,
/// `provider-composer.ts:412-499`).
///
/// Composes [`compose_api_key_auth`] + [`compose_oauth_auth`], throws
/// `"Provider {id}: no authentication method configured."` when neither
/// composes, and wires the eager `streamWith` dispatch.
pub fn compose_model_provider(
    input: ComposeModelProviderInput,
) -> Result<ComposedProvider, ComposeAuthError> {
    let ComposeModelProviderInput {
        provider_id,
        base,
        base_api_key,
        base_oauth,
        config,
        extension,
        models,
        name,
        base_url,
        headers,
    } = input;

    let base_has_oauth = base_oauth.is_some();
    let api_key = compose_api_key_auth(
        &provider_id,
        base_api_key,
        base_has_oauth,
        config.as_ref(),
        extension.as_ref(),
    );
    let oauth = compose_oauth_auth(
        &provider_id,
        base_oauth,
        config.as_ref(),
        extension.as_ref(),
    );
    let auth = require_auth_method(&provider_id, api_key, oauth)?;

    Ok(ComposedProvider {
        id: provider_id,
        name,
        base_url,
        headers,
        auth,
        models,
        base,
    })
}

/// A single-`error` [`StreamResult`], the eager-seam analog of pi's `lazyStream`
/// catch path (mirrors the private `error_result` in [`crate::providers::registry`]).
fn error_stream(model: &Model, message: String) -> StreamResult {
    let error = AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: zero_usage(),
        stop_reason: StopReason::Error,
        error_message: Some(message),
        timestamp: 0,
    };
    StreamResult {
        events: vec![AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: error.clone(),
        }],
        message: error,
    }
}

fn zero_usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
    }
}

#[cfg(test)]
mod tests;
