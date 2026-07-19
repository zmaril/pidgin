//! The `Models` runtime layer, ported from pi's `packages/ai/src/models.ts`
//! (`ModelsImpl`) at pinned commit `3da591ab`.
//!
//! This is the availability / auth-status / live-refresh half of pi's
//! `ModelsImpl`, split out of [`super`] so each file stays under the size
//! ceiling. It adds, on top of the [`Models`] provider registry and streaming
//! surface, four net-new capabilities:
//!
//! - [`Models::get_available`] (pi `getAvailable`, `models.ts:394`) — like
//!   [`Models::get_models`] but restricted to providers whose auth is configured,
//!   then narrowed by each provider's optional
//!   [`filter_models`](super::RegistryProvider::filter_models).
//! - [`Models::check_auth`] (pi `checkAuth`, `models.ts:388`) — the resolved auth
//!   status for a provider, without refreshing OAuth.
//! - [`Models::get_auth_for_provider`] / [`Models::get_auth_for_model`] (pi's two
//!   `getAuth` overloads, `models.ts:411-412`) — provider-scoped auth resolution,
//!   with model-header merge for the model overload.
//! - [`Models::refresh`] (pi `refresh`, `models.ts:276`) + `resolve_refresh_credential`
//!   (pi `resolveRefreshCredential`, `models.ts:330`) — live-refresh every
//!   configured dynamic provider, threading the resolved credential into the
//!   [`RefreshContext`](super::RefreshContext) seam.
//!
//! # Port deviations
//!
//! The existing [`Models`] registry carries the simplified
//! [`ProviderAuth`](super::ProviderAuth) (an env-API-key descriptor: name plus
//! env-var precedence list) rather than pi's full `ProviderAuth { apiKey, oauth }`
//! with a credential store. So this layer resolves auth through the same
//! [`env_api_key_auth`] handler the streaming `apply_auth` already uses, reached
//! through the injected [`AuthContext`](crate::auth::AuthContext). The OAuth read
//! path (stored-credential ownership, expired-token refresh under the store lock)
//! stays deferred with the rest of the OAuth surface — the same deferral the
//! streaming half documents. Auth values still reuse pi's real result types
//! ([`AuthResult`], [`AuthCheck`], [`ModelAuth`], [`Credential`],
//! [`AuthResolutionOverrides`]); no parallel auth model is introduced. Because a
//! provider's dynamic fetch cannot fail in this synchronous seam,
//! [`RefreshResult::errors`] is always empty (pi collects provider fetch
//! rejections there).

// straitjacket-allow-file:duplication

use std::collections::BTreeMap;

use crate::auth::{
    env_api_key_auth, ApiKeyAuth, ApiKeyCredential, AuthCheck, AuthResolutionOverrides, AuthResult,
    AuthType, Credential, ModelAuth, ModelsError,
};
use crate::types::Model;

use super::{Models, ProviderHeaders, RefreshContext, RegistryProvider};

/// Options for [`Models::refresh`], pi's `ModelsRefreshOptions` (`models.ts:46`).
#[derive(Debug, Clone)]
pub struct RefreshOptions {
    /// False during offline/cache-only initialization (pi's `allowNetwork`,
    /// defaulting to `true`).
    pub allow_network: bool,
    /// Bypass provider freshness checks and fetch immediately when the network is
    /// allowed (pi's `force`).
    pub force: bool,
}

impl Default for RefreshOptions {
    /// pi's `allowNetwork ?? true`: network refresh on, force off.
    fn default() -> Self {
        Self {
            allow_network: true,
            force: false,
        }
    }
}

/// The outcome of [`Models::refresh`], pi's `ModelsRefreshResult` (`models.ts:53`).
#[derive(Debug, Clone, Default)]
pub struct RefreshResult {
    /// Whether the refresh was cancelled. Always `false` in this synchronous port
    /// (pi threads an `AbortSignal`; cancellation is deferred with the async seam).
    pub aborted: bool,
    /// Per-provider fetch errors, keyed by provider id. Always empty here: the
    /// synchronous fetch hook returns a model list and cannot fail (pi collects
    /// provider `refreshModels` rejections in this map).
    pub errors: BTreeMap<String, ModelsError>,
}

/// Merge header maps, pi's `mergeHeaders` (`models.ts:202`): `override` wins,
/// replacing any base entry whose name matches case-insensitively while keeping
/// the override's own casing. `None`/`None` yields `None`.
fn merge_headers(
    base: Option<&ProviderHeaders>,
    override_headers: Option<&ProviderHeaders>,
) -> Option<ProviderHeaders> {
    if base.is_none() && override_headers.is_none() {
        return None;
    }
    let mut merged = base.cloned().unwrap_or_default();
    if let Some(override_headers) = override_headers {
        for (name, value) in override_headers {
            let lower = name.to_lowercase();
            let clashes: Vec<String> = merged
                .keys()
                .filter(|existing| existing.to_lowercase() == lower)
                .cloned()
                .collect();
            for existing in clashes {
                merged.remove(&existing);
            }
            merged.insert(name.clone(), value.clone());
        }
    }
    Some(merged)
}

/// A model's static headers as a [`ProviderHeaders`] overlay. pi's `Model.headers`
/// is already `ProviderHeaders`; the Rust [`Model`] carries `BTreeMap<String,
/// String>`, so each value lifts to `Some(value)`.
fn model_header_overlay(model: &Model) -> Option<ProviderHeaders> {
    model.headers.as_ref().map(|headers| {
        headers
            .iter()
            .map(|(k, v)| (k.clone(), Some(v.clone())))
            .collect()
    })
}

impl Models {
    /// Resolve provider-scoped auth through the simplified env-API-key handler,
    /// applying `overrides` (pi's `resolveProviderAuth` reduced to the api-key /
    /// ambient case; `resolve.ts:37`).
    ///
    /// An explicit `overrides.api_key` wins (resolved as a stored credential); an
    /// ambient/keyless provider (no env vars) resolves to an empty [`ModelAuth`],
    /// pi's `ambientAuth` "configured, no auth values"; otherwise the first set
    /// env var among the provider's descriptor resolves. `Ok(None)` means the
    /// provider is not configured.
    fn resolve_auth(
        &self,
        provider: &RegistryProvider,
        overrides: Option<&AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        let auth = provider.auth();
        let vars: Vec<&str> = auth.api_key_env_vars.iter().map(String::as_str).collect();

        // Explicit api-key override wins (pi resolve.ts:49): resolve it as a
        // stored credential so the handler reports the "stored credential" source.
        if let Some(overrides) = overrides {
            if let Some(api_key) = &overrides.api_key {
                let credential = ApiKeyCredential {
                    key: Some(api_key.clone()),
                    env: overrides.env.clone(),
                };
                let handler = env_api_key_auth(auth.name.clone(), &vars);
                return handler
                    .resolve(self.auth_context.as_ref(), Some(&credential))
                    .map_err(|error| self.wrap_api_key_error(provider, error));
            }
        }

        // Ambient/keyless provider: configured with an empty ModelAuth
        // (pi ambientAuth, always configured).
        if auth.api_key_env_vars.is_empty() {
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: overrides.and_then(|o| o.env.clone()),
                source: None,
            }));
        }

        // Ambient env resolution: the first set env var resolves.
        let handler = env_api_key_auth(auth.name.clone(), &vars);
        handler
            .resolve(self.auth_context.as_ref(), None)
            .map_err(|error| self.wrap_api_key_error(provider, error))
    }

    fn wrap_api_key_error(
        &self,
        provider: &RegistryProvider,
        error: crate::auth::AuthFlowError,
    ) -> ModelsError {
        ModelsError::auth(format!(
            "API key auth failed for provider {}",
            provider.id()
        ))
        .with_cause(error.to_string())
    }

    /// The resolved auth status for `provider`, pi's `checkProviderAuth`
    /// (`models.ts:364`) reduced to the api-key / ambient case: a side-effect-free
    /// check that never refreshes OAuth. `Ok(None)` means unconfigured.
    fn check_provider_auth(
        &self,
        provider: &RegistryProvider,
    ) -> Result<Option<AuthCheck>, ModelsError> {
        let auth = provider.auth();
        // Ambient/keyless provider is always configured; pi's `resolveProviderAuth`
        // yields no source label for it.
        if auth.api_key_env_vars.is_empty() {
            return Ok(Some(AuthCheck {
                source: None,
                check_type: AuthType::ApiKey,
            }));
        }
        let vars: Vec<&str> = auth.api_key_env_vars.iter().map(String::as_str).collect();
        let handler = env_api_key_auth(auth.name.clone(), &vars);
        match handler.resolve(self.auth_context.as_ref(), None) {
            Ok(Some(result)) => Ok(Some(AuthCheck {
                source: result.source,
                check_type: AuthType::ApiKey,
            })),
            Ok(None) => Ok(None),
            Err(error) => Err(ModelsError::auth(format!(
                "API key auth check failed for provider {}",
                provider.id()
            ))
            .with_cause(error.to_string())),
        }
    }

    /// Check whether a provider has complete auth configuration without refreshing
    /// OAuth, pi's `checkAuth` (`models.ts:388`). `Ok(None)` for an unknown or
    /// unconfigured provider.
    pub fn check_auth(&self, provider_id: &str) -> Result<Option<AuthCheck>, ModelsError> {
        match self.get_provider(provider_id) {
            Some(provider) => self.check_provider_auth(provider),
            None => Ok(None),
        }
    }

    /// Return the models whose providers have complete auth configuration, pi's
    /// `getAvailable` (`models.ts:394`). Like [`Models::get_models`] but each
    /// provider is first gated by [`Models::check_auth`], then its catalog is
    /// narrowed by [`filter_models`](super::RegistryProvider::filter_models).
    ///
    /// pi passes the provider's *stored* credential to `filterModels`; this port
    /// has no credential store yet, so the filter receives `None` (the stored
    /// credential is always absent here).
    pub fn get_available(&self, provider_id: Option<&str>) -> Result<Vec<Model>, ModelsError> {
        let providers: Vec<&RegistryProvider> = match provider_id {
            Some(id) => self
                .get_provider(id)
                .map(|p| p.as_ref())
                .into_iter()
                .collect(),
            None => self.get_providers().iter().map(|p| p.as_ref()).collect(),
        };
        let mut available = Vec::new();
        for provider in providers {
            if self.check_provider_auth(provider)?.is_none() {
                continue;
            }
            available.extend(provider.filter_models(provider.get_models(), None));
        }
        Ok(available)
    }

    /// Resolve provider-scoped auth by provider id, pi's `getAuth(providerId, ...)`
    /// overload (`models.ts:411`). `Ok(None)` for an unknown or unconfigured
    /// provider.
    ///
    /// # `getAuth` overload mapping
    ///
    /// pi's single `getAuth` is overloaded on `string | Model`. Rust has no
    /// overloading, so the two call shapes become two methods: this one for the
    /// provider-id shape, and [`Models::get_auth_for_model`] for the model shape.
    /// The provider-id shape returns provider-scoped auth verbatim; the model
    /// shape additionally merges the model's static headers (pi's
    /// `providerOrModel.headers` branch, `models.ts:421`).
    pub fn get_auth_for_provider(
        &self,
        provider_id: &str,
        overrides: Option<&AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        match self.get_provider(provider_id) {
            Some(provider) => self.resolve_auth(provider, overrides),
            None => Ok(None),
        }
    }

    /// Resolve provider auth plus static model headers, pi's `getAuth(model, ...)`
    /// overload (`models.ts:412`). Resolves the owning provider's auth, then — only
    /// for the model shape and only when the model declares headers — merges those
    /// headers into the resolved request auth (pi `models.ts:421-428`). `Ok(None)`
    /// for an unknown or unconfigured provider.
    ///
    /// See [`Models::get_auth_for_provider`] for the overload mapping rationale.
    pub fn get_auth_for_model(
        &self,
        model: &Model,
        overrides: Option<&AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        let Some(provider) = self.get_provider(&model.provider) else {
            return Ok(None);
        };
        let Some(mut result) = self.resolve_auth(provider, overrides)? else {
            return Ok(None);
        };
        if let Some(overlay) = model_header_overlay(model) {
            result.auth.headers = merge_headers(result.auth.headers.as_ref(), Some(&overlay));
        }
        Ok(Some(result))
    }

    /// Resolve the effective credential a dynamic provider's fetch should thread,
    /// pi's `resolveRefreshCredential` (`models.ts:330`) reduced to the api-key /
    /// ambient case.
    ///
    /// An ambient/keyless provider yields a keyless api-key credential (pi's
    /// api-key branch with `result.auth.apiKey` undefined). A provider whose env
    /// var resolves yields that key. An unconfigured provider yields `None`, so
    /// [`Models::refresh`] skips it. The OAuth branch (refresh-before-fetch) is
    /// deferred with the rest of the OAuth read path.
    fn resolve_refresh_credential(&self, provider: &RegistryProvider) -> Option<Credential> {
        let auth = provider.auth();
        if auth.api_key_env_vars.is_empty() {
            return Some(Credential::ApiKey(ApiKeyCredential::default()));
        }
        let vars: Vec<&str> = auth.api_key_env_vars.iter().map(String::as_str).collect();
        let handler = env_api_key_auth(auth.name.clone(), &vars);
        match handler.resolve(self.auth_context.as_ref(), None) {
            Ok(Some(result)) => Some(Credential::ApiKey(ApiKeyCredential {
                key: result.auth.api_key,
                env: result.env,
            })),
            _ => None,
        }
    }

    /// Live-refresh every configured dynamic provider, pi's `refresh`
    /// (`models.ts:276`). For each refreshable provider it resolves the effective
    /// credential (skipping unconfigured providers) and drives the provider's
    /// [`refresh_models`](super::RegistryProvider::refresh_models) with a
    /// [`RefreshContext`] carrying that credential plus the `allow_network`/`force`
    /// options.
    ///
    /// Static and unconfigured providers are skipped, mirroring pi. The returned
    /// [`RefreshResult::errors`] is always empty (the synchronous fetch hook
    /// cannot fail; see the module deviation note).
    pub fn refresh(&self, options: &RefreshOptions) -> RefreshResult {
        let errors = BTreeMap::new();
        for provider in self.get_providers() {
            if !provider.is_refreshable() {
                continue;
            }
            let Some(credential) = self.resolve_refresh_credential(provider) else {
                continue;
            };
            let context = RefreshContext {
                allow_network: options.allow_network,
                force: options.force,
                credential: Some(credential),
            };
            provider.refresh_models(&context);
        }
        RefreshResult {
            aborted: false,
            errors,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::auth::{AuthType, Credential, DefaultAuthContext};
    use crate::providers::{create_provider, ApiRouting, CreateProviderOptions, MutableModels};
    use crate::seams::provider::{AbortSignal, Provider as StreamBackend, StreamResult};
    use crate::seams::storage::MemoryEnv;
    use crate::types::{
        AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Modality, Model,
        ModelCost, StopReason, StreamOptions, Usage, UsageCost,
    };

    use super::super::ProviderAuth;
    use super::*;

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

    /// A backend that returns a deterministic `stop` message so streaming paths
    /// reach a provider (the runtime tests here exercise auth/refresh, not events).
    struct OkBackend;

    impl StreamBackend for OkBackend {
        fn api(&self) -> &str {
            "test-api"
        }

        fn stream(
            &self,
            model: &Model,
            _context: &Context,
            _options: Option<&StreamOptions>,
            _signal: Option<&AbortSignal>,
        ) -> StreamResult {
            let message = AssistantMessage {
                role: AssistantRole::Assistant,
                content: Vec::new(),
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                response_model: None,
                response_id: None,
                diagnostics: None,
                usage: zero_usage(),
                stop_reason: StopReason::Stop,
                error_message: None,
                timestamp: 0,
            };
            StreamResult {
                events: vec![AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: message.clone(),
                }],
                message,
            }
        }
    }

    fn model_for(provider: &str, id: &str) -> Model {
        Model {
            id: id.to_string(),
            name: id.to_string(),
            api: "test-api".to_string(),
            provider: provider.to_string(),
            base_url: "https://example.test/v1".to_string(),
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
            context_window: 10000,
            max_tokens: 1000,
            headers: None,
            compat: None,
        }
    }

    /// A [`Models`] whose auth context is backed by `env` (an in-memory env map),
    /// mirroring pi's `createModels` with an injected context.
    fn models_with_env(env: MemoryEnv) -> Models {
        Models::with_auth_context(Arc::new(DefaultAuthContext::new(env)))
    }

    fn ok_backend() -> ApiRouting {
        ApiRouting::Single(Arc::new(OkBackend))
    }

    fn opts(id: &str) -> CreateProviderOptions {
        CreateProviderOptions {
            id: id.to_string(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth::default(),
            models: vec![model_for(id, "model-a")],
            fetch_models: None,
            api: ok_backend(),
        }
    }

    // models-runtime.test.ts:369 — "resolves auth: stored credential owns the
    // provider, ambient only when nothing stored" (the api-key / override half
    // portable onto the simplified env-key auth; the OAuth/stored-credential
    // branches are deferred with the OAuth read path). Both getAuth overloads
    // resolve the same provider-scoped auth; an explicit api-key override wins.
    #[test]
    fn get_auth_resolves_env_and_override() {
        let env = MemoryEnv::new().with_env("P1_KEY", "env-key");
        let mut models = models_with_env(env);
        models.set_provider(create_provider(CreateProviderOptions {
            auth: ProviderAuth::env_api_key("Test API key", &["P1_KEY"]),
            ..opts("p1")
        }));
        let model = model_for("p1", "model-a");

        // model and provider-id overloads resolve the same provider-scoped auth.
        assert_eq!(
            models
                .get_auth_for_model(&model, None)
                .unwrap()
                .unwrap()
                .auth
                .api_key
                .as_deref(),
            Some("env-key")
        );
        assert_eq!(
            models
                .get_auth_for_provider("p1", None)
                .unwrap()
                .unwrap()
                .auth
                .api_key
                .as_deref(),
            Some("env-key")
        );

        // An explicit api-key override wins.
        let overrides = AuthResolutionOverrides {
            api_key: Some("explicit-key".into()),
            env: None,
        };
        assert_eq!(
            models
                .get_auth_for_model(&model, Some(&overrides))
                .unwrap()
                .unwrap()
                .auth
                .api_key
                .as_deref(),
            Some("explicit-key")
        );
    }

    // models-runtime.test.ts:638 — "adds model headers only for model auth": the
    // provider-id overload leaves auth.headers unset; the model overload merges the
    // model's static headers into the resolved auth. (The transformHeaders /
    // stream-option merge half runs on the deferred applyAuth request path.)
    #[test]
    fn get_auth_adds_model_headers_only_for_model() {
        let env = MemoryEnv::new().with_env("P1_KEY", "key");
        let mut models = models_with_env(env);
        models.set_provider(create_provider(CreateProviderOptions {
            auth: ProviderAuth::env_api_key("Test API key", &["P1_KEY"]),
            ..opts("p1")
        }));
        let mut model = model_for("p1", "model-a");
        model.headers = Some(BTreeMap::from([
            ("x-model".to_string(), "model".to_string()),
            ("x-shared".to_string(), "model".to_string()),
        ]));

        // Provider-id overload: no model headers.
        assert!(models
            .get_auth_for_provider("p1", None)
            .unwrap()
            .unwrap()
            .auth
            .headers
            .is_none());

        // Model overload: the model's static headers appear on the resolved auth.
        let headers = models
            .get_auth_for_model(&model, None)
            .unwrap()
            .unwrap()
            .auth
            .headers
            .unwrap();
        assert_eq!(headers.get("x-model"), Some(&Some("model".to_string())));
        assert_eq!(headers.get("x-shared"), Some(&Some("model".to_string())));
    }

    // models-runtime.test.ts:398 — "checks provider auth without refreshing OAuth
    // and filters available models" (api-key / ambient half). A configured env-key
    // provider reports its env-var source; an unset one is undefined; getAvailable
    // lists only configured providers.
    #[test]
    fn check_auth_and_get_available() {
        let env = MemoryEnv::new().with_env("AMB_KEY", "env-key");
        let mut models = models_with_env(env);
        // Configured: env var set.
        models.set_provider(create_provider(CreateProviderOptions {
            auth: ProviderAuth::env_api_key("Ambient key", &["AMB_KEY"]),
            ..opts("ambient")
        }));
        // Unconfigured: env var unset.
        models.set_provider(create_provider(CreateProviderOptions {
            auth: ProviderAuth::env_api_key("Missing key", &["MISS_KEY"]),
            ..opts("missing")
        }));
        // Ambient/keyless: always configured.
        models.set_provider(create_provider(CreateProviderOptions {
            auth: ProviderAuth::default(),
            ..opts("keyless")
        }));

        assert_eq!(
            models.check_auth("ambient").unwrap(),
            Some(AuthCheck {
                source: Some("AMB_KEY".to_string()),
                check_type: AuthType::ApiKey,
            })
        );
        assert_eq!(models.check_auth("missing").unwrap(), None);
        assert_eq!(
            models.check_auth("keyless").unwrap(),
            Some(AuthCheck {
                source: None,
                check_type: AuthType::ApiKey,
            })
        );
        // Unknown provider: undefined.
        assert_eq!(models.check_auth("ghost").unwrap(), None);

        // getAvailable lists only configured providers, in registration order.
        let available: Vec<String> = models
            .get_available(None)
            .unwrap()
            .into_iter()
            .map(|m| m.provider)
            .collect();
        assert_eq!(
            available,
            vec!["ambient".to_string(), "keyless".to_string()]
        );
        // Scoped to one provider.
        let scoped: Vec<String> = models
            .get_available(Some("ambient"))
            .unwrap()
            .into_iter()
            .map(|m| m.provider)
            .collect();
        assert_eq!(scoped, vec!["ambient".to_string()]);
        // Scoped to an unconfigured provider: nothing.
        assert!(models.get_available(Some("missing")).unwrap().is_empty());
    }

    // getAvailable applies a provider's filterModels to its catalog
    // (models.ts:407 `provider.filterModels?.(models, credential) ?? models`).
    #[test]
    fn get_available_applies_filter_models() {
        let mut models = models_with_env(MemoryEnv::new());
        // Keep only the model whose id is "keep"; attached via the builder so the
        // public CreateProviderOptions stays unchanged for downstream callers.
        let provider = create_provider(CreateProviderOptions {
            models: vec![model_for("p1", "keep"), model_for("p1", "drop")],
            ..opts("p1")
        })
        .with_filter_models(Arc::new(|catalog: &[Model], _cred| {
            catalog.iter().filter(|m| m.id == "keep").cloned().collect()
        }));
        models.set_provider(provider);

        let ids: Vec<String> = models
            .get_available(None)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, vec!["keep".to_string()]);
    }

    // models-runtime.test.ts:286 — "passes effective API-key credentials and
    // refresh options while skipping unconfigured providers". refresh() threads the
    // resolved credential and the force flag into each configured dynamic
    // provider's fetch, and never calls an unconfigured provider's fetch.
    #[test]
    fn refresh_threads_credential_and_skips_unconfigured() {
        let env = MemoryEnv::new().with_env("CFG_KEY", "cfg-value");
        let mut models = models_with_env(env);

        let seen_credential: Arc<Mutex<Option<Credential>>> = Arc::new(Mutex::new(None));
        let seen_force = Arc::new(Mutex::new(None));
        let cred_sink = seen_credential.clone();
        let force_sink = seen_force.clone();
        models.set_provider(create_provider(CreateProviderOptions {
            auth: ProviderAuth::env_api_key("Configured key", &["CFG_KEY"]),
            fetch_models: Some(Arc::new(move |ctx: &RefreshContext| {
                *cred_sink.lock().unwrap() = ctx.credential.clone();
                *force_sink.lock().unwrap() = Some(ctx.force);
                vec![model_for("configured", "fetched")]
            })),
            ..opts("configured")
        }));

        let unconfigured_fetches = Arc::new(Mutex::new(0u32));
        let unconfigured_sink = unconfigured_fetches.clone();
        models.set_provider(create_provider(CreateProviderOptions {
            auth: ProviderAuth::env_api_key("Missing key", &["UNSET_KEY"]),
            fetch_models: Some(Arc::new(move |_ctx: &RefreshContext| {
                *unconfigured_sink.lock().unwrap() += 1;
                Vec::new()
            })),
            ..opts("unconfigured")
        }));

        // Static provider: no fetch, never refreshed.
        models.set_provider(create_provider(opts("static")));

        let result = models.refresh(&RefreshOptions {
            allow_network: true,
            force: true,
        });
        assert!(!result.aborted);
        assert!(result.errors.is_empty());

        // The configured provider fetched with the resolved api-key credential.
        assert_eq!(
            *seen_credential.lock().unwrap(),
            Some(Credential::ApiKey(ApiKeyCredential {
                key: Some("cfg-value".to_string()),
                env: None,
            }))
        );
        assert_eq!(*seen_force.lock().unwrap(), Some(true));
        // Its dynamic model is now listed.
        assert!(models.get_model("configured", "fetched").is_some());

        // The unconfigured provider was skipped: its fetch never ran.
        assert_eq!(*unconfigured_fetches.lock().unwrap(), 0);
    }

    // An ambient/keyless dynamic provider is refreshed with a keyless credential
    // (pi resolveRefreshCredential api-key branch, apiKey undefined).
    #[test]
    fn refresh_uses_keyless_credential_for_ambient_provider() {
        let mut models = models_with_env(MemoryEnv::new());
        let seen: Arc<Mutex<Option<Credential>>> = Arc::new(Mutex::new(None));
        let sink = seen.clone();
        models.set_provider(create_provider(CreateProviderOptions {
            auth: ProviderAuth::default(),
            fetch_models: Some(Arc::new(move |ctx: &RefreshContext| {
                *sink.lock().unwrap() = ctx.credential.clone();
                vec![model_for("ambient", "fetched")]
            })),
            ..opts("ambient")
        }));

        models.refresh(&RefreshOptions::default());
        assert_eq!(
            *seen.lock().unwrap(),
            Some(Credential::ApiKey(ApiKeyCredential::default()))
        );
        assert!(models.get_model("ambient", "fetched").is_some());
    }
}
