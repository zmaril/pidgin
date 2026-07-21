// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `images-models.ts`: the provider CRUD, the auth-merge in `generateImages`, and
// the `AssistantImages` error shells mirror the chat `providers/registry` port
// and the sibling image runtime files by design; the clone detector reads the
// shared boundary-type construction as duplicative.

//! The image-generation model collection — the Rust port of pi-ai's
//! `images-models.ts` (`packages/ai/src/images-models.ts`).
//!
//! This is the image-side counterpart of the chat `providers/registry`
//! ([`crate::providers::Models`]): [`ImagesProvider`] is an image-generation
//! provider (id/name metadata, auth, model listing, generation), and
//! [`ImagesModelsImpl`] is the runtime collection that applies auth and dispatches
//! generation. It reuses the already-ported auth subsystem
//! ([`crate::auth`]) — credential store, auth context, and
//! [`resolve_provider_auth`](crate::auth::resolve_provider_auth).
//!
//! # Sync-port notes
//!
//! - pi's async methods (`Promise<...>`) are synchronous here; network for the
//!   OAuth resolve path would go through the [`crate::seams`] transport, but the
//!   image providers ported here are api-key only, so that path is never taken.
//! - `resolve_provider_auth` takes an [`OAuthFlow`](crate::auth::OAuthFlow) that
//!   bundles the transport/clock/timers for the OAuth branch. The image
//!   providers never reach it, so this collection builds a flow backed by a
//!   no-network transport and the system clock.
//! - pi's `createImagesProvider` single-flights concurrent `refreshModels()`
//!   calls. That is a JS-concurrency concern with no synchronous-Rust analog
//!   (sequential calls cannot overlap), so the dedupe collapses; each
//!   `refresh_models()` performs one fetch.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::auth::{
    resolve_provider_auth, AuthContext, AuthProvider, AuthResolutionOverrides, AuthResult,
    CredentialStore, DefaultAuthContext, InMemoryCredentialStore, ModelsError, OAuthFlow,
    ProviderAuth, ProviderEnv, ProviderHeaders,
};
use crate::seams::clock::SystemClock;
use crate::seams::http::{HttpRequest, HttpResponse, HttpTransport};
use crate::seams::provider::AbortSignal;
use crate::seams::storage::SystemEnv;
use crate::types::{
    AssistantImages, ImagesContext, ImagesModel, ImagesOptions, ImagesStopReason, ProviderImages,
};

/// The plain-error a dynamic provider's fetch hook raises, the analog of the
/// bare `Error` pi's `refreshModels()` rejects with (re-wrapped into a
/// [`ModelsError`] `model_source` by [`ImagesModelsImpl::refresh`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRefreshError {
    /// The failure message.
    pub message: String,
}

impl ProviderRefreshError {
    /// Build a refresh error from a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// The fetch hook a dynamic [`ImagesProvider`] runs to refresh its model list.
pub type RefreshModelsFn =
    Box<dyn Fn() -> Result<Vec<ImagesModel>, ProviderRefreshError> + Send + Sync>;

/// An image-generation provider: the image-side counterpart of the chat
/// provider (pi's `ImagesProvider`, `images-models.ts:13-45`).
///
/// Owns id/name metadata, auth, model listing, and generation behavior.
pub trait ImagesProvider: Send + Sync {
    /// The provider id, the credential-store key (pi's `readonly id`).
    fn id(&self) -> &str;

    /// The display name (pi's `readonly name`).
    fn name(&self) -> &str;

    /// The `{ id, auth }` slice [`resolve_provider_auth`] consumes.
    ///
    /// pi exposes `readonly auth: ProviderAuth`; the Rust resolver reads the id
    /// alongside the auth, so this surfaces the [`AuthProvider`] slice that
    /// bundles both (`self.auth().auth` is pi's `ImagesProvider.auth`).
    fn auth(&self) -> &AuthProvider;

    /// The current known models, synchronously (pi's `getModels()`). Static
    /// providers return their catalog; dynamic providers return the list as of
    /// the last [`refresh_models`](Self::refresh_models).
    fn get_models(&self) -> Vec<ImagesModel>;

    /// Dynamic providers only: fetch and update the model list. `None` for a
    /// static provider (pi's absent `refreshModels`). On error the list stays at
    /// its last-known state and a later call retries.
    fn refresh_models(&self) -> Option<Result<(), ProviderRefreshError>>;

    /// Generate images through this provider (pi's `generateImages`), honoring
    /// `signal` for cooperative abort (threaded as a separate parameter — see
    /// [`ProviderImages`](crate::types::ProviderImages)).
    fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
        signal: Option<&AbortSignal>,
    ) -> AssistantImages;
}

/// The mutating half of pi's `MutableImagesModels` (`images-models.ts:95-100`):
/// upsert/replace, remove, and clear providers. Kept as a trait so the read-only
/// surface of [`ImagesModelsImpl`] and the mutators stay distinct, mirroring the
/// chat [`MutableModels`](crate::providers::MutableModels) split.
pub trait MutableImagesModels {
    /// Upsert a provider by id (pi's `setProvider`); replaces any existing
    /// provider with the same id, preserving its position.
    fn set_provider(&mut self, provider: Arc<dyn ImagesProvider>);
    /// Remove a provider by id (pi's `deleteProvider`).
    fn delete_provider(&mut self, id: &str);
    /// Remove every provider (pi's `clearProviders`).
    fn clear_providers(&mut self);
}

/// Options for [`create_images_models`] / [`builtin_images_models`], the image
/// analog of pi's `CreateModelsOptions` (`models.ts`).
///
/// The chat port folds this into [`crate::providers::Models`] constructors
/// rather than a dedicated struct; the image port keeps pi's options shape so
/// `createImagesModels({ authContext })` transcribes directly.
#[derive(Default)]
pub struct CreateImagesModelsOptions {
    /// The credential store (pi's `credentials`). Defaults to an in-memory store.
    pub credentials: Option<Arc<dyn CredentialStore>>,
    /// The auth context (pi's `authContext`). Defaults to the process env.
    pub auth_context: Option<Arc<dyn AuthContext + Send + Sync>>,
}

/// A [`HttpTransport`] that performs no network I/O.
///
/// The image providers are api-key only, so `resolve_provider_auth`'s OAuth
/// branch — the only consumer of the flow transport — is never reached. This
/// stands in for it without pulling a real HTTP stack into the default build.
struct NoNetworkTransport;

impl HttpTransport for NoNetworkTransport {
    fn send(&self, _request: &HttpRequest) -> std::io::Result<HttpResponse> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "image model collection has no network transport",
        ))
    }
}

/// The runtime collection of image-generation providers, pi's `ImagesModelsImpl`
/// (`images-models.ts:102-262`). Applies auth and dispatches generation.
///
/// Carries pi's `ImagesModels` read surface as inherent methods (mirroring how
/// the chat [`Models`](crate::providers::Models) collection is a concrete type),
/// and implements [`MutableImagesModels`] for the mutators.
pub struct ImagesModelsImpl {
    providers: Vec<Arc<dyn ImagesProvider>>,
    credentials: Arc<dyn CredentialStore>,
    auth_context: Arc<dyn AuthContext + Send + Sync>,
}

impl ImagesModelsImpl {
    fn new(options: CreateImagesModelsOptions) -> Self {
        Self {
            providers: Vec::new(),
            credentials: options
                .credentials
                .unwrap_or_else(|| Arc::new(InMemoryCredentialStore::new())),
            auth_context: options
                .auth_context
                .unwrap_or_else(|| Arc::new(DefaultAuthContext::new(SystemEnv::new()))),
        }
    }

    /// All providers, in registration order (pi's `getProviders`).
    pub fn get_providers(&self) -> &[Arc<dyn ImagesProvider>] {
        &self.providers
    }

    /// The provider with `id`, if any (pi's `getProvider`).
    pub fn get_provider(&self, id: &str) -> Option<&Arc<dyn ImagesProvider>> {
        self.providers.iter().find(|p| p.id() == id)
    }

    /// Last-known models from one provider or all providers (pi's `getModels`).
    /// Best-effort: an unknown provider yields no models.
    pub fn get_models(&self, provider: Option<&str>) -> Vec<ImagesModel> {
        match provider {
            Some(id) => self
                .get_provider(id)
                .map(|p| p.get_models())
                .unwrap_or_default(),
            None => self.providers.iter().flat_map(|p| p.get_models()).collect(),
        }
    }

    /// A single model by provider + id (pi's `getModel`).
    pub fn get_model(&self, provider: &str, id: &str) -> Option<ImagesModel> {
        self.get_models(Some(provider))
            .into_iter()
            .find(|m| m.id == id)
    }

    /// Ask a dynamic provider (or all of them) to re-fetch, pi's `refresh`
    /// (`images-models.ts:170-186`).
    ///
    /// With a provider id: a single-provider fetch failure rejects with a
    /// [`ModelsError`] `model_source`; an unknown or static provider is a no-op.
    /// Without one: every provider is refreshed best-effort (errors swallowed),
    /// matching pi's `Promise.allSettled`.
    pub fn refresh(&self, provider: Option<&str>) -> Result<(), ModelsError> {
        match provider {
            Some(id) => {
                let Some(entry) = self.get_provider(id) else {
                    return Ok(());
                };
                match entry.refresh_models() {
                    None | Some(Ok(())) => Ok(()),
                    Some(Err(error)) => Err(ModelsError::model_source(format!(
                        "Model refresh failed for {id}"
                    ))
                    .with_cause(error.message)),
                }
            }
            None => {
                for entry in &self.providers {
                    let _ = entry.refresh_models();
                }
                Ok(())
            }
        }
    }

    /// Resolve provider-scoped auth by provider id, pi's `getAuth(providerId, ...)`
    /// overload. `Ok(None)` for an unknown or unconfigured provider.
    pub fn get_auth_for_provider(
        &self,
        provider_id: &str,
        overrides: Option<&AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        let Some(provider) = self.get_provider(provider_id) else {
            return Ok(None);
        };
        self.resolve_auth(provider.as_ref(), overrides)
    }

    /// Resolve auth for an image model, pi's `getAuth(model, ...)` overload —
    /// which, unlike the chat `Models`, resolves purely by the model's provider
    /// id (pi's images-models `getAuth` does not merge model headers).
    pub fn get_auth_for_model(
        &self,
        model: &ImagesModel,
        overrides: Option<&AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        self.get_auth_for_provider(&model.provider, overrides)
    }

    /// Resolve a provider's auth through the shared resolver, building the
    /// no-network OAuth flow the api-key path never consults.
    fn resolve_auth(
        &self,
        provider: &dyn ImagesProvider,
        overrides: Option<&AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        let transport = NoNetworkTransport;
        let clock = SystemClock::new();
        let flow = OAuthFlow {
            http: &transport,
            clock: &clock,
            timers: &clock,
            signal: None,
        };
        resolve_provider_auth(
            provider.auth(),
            self.credentials.as_ref(),
            self.auth_context.as_ref(),
            &flow,
            overrides,
        )
    }

    /// Generate images through the owning provider with auth resolved and merged,
    /// pi's `generateImages` (`images-models.ts:213-262`).
    ///
    /// Explicit request options win per field; headers/env merge per key. Never
    /// returns an `Err`: failures become an [`AssistantImages`] with a `stopReason`
    /// of `error`.
    pub fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
        signal: Option<&AbortSignal>,
    ) -> AssistantImages {
        let Some(provider) = self.get_provider(&model.provider) else {
            return self.error_images(model, format!("Unknown provider: {}", model.provider));
        };

        let overrides = AuthResolutionOverrides {
            api_key: options.and_then(|o| o.api_key.clone()),
            env: options.and_then(|o| o.env.clone()),
        };
        let resolution = match self.get_auth_for_model(model, Some(&overrides)) {
            Ok(resolution) => resolution,
            Err(error) => return self.error_images(model, error.message),
        };

        // pi: `if (!auth) return provider.generateImages(model, context, options)`.
        // `auth` is `resolution?.auth`, falsy only when nothing resolved.
        let Some(resolution) = resolution else {
            return provider.generate_images(model, context, options, signal);
        };
        let auth = resolution.auth;

        // requestModel = auth.baseUrl ? { ...model, baseUrl } : model.
        let request_model = match &auth.base_url {
            Some(base_url) => {
                let mut request_model = model.clone();
                request_model.base_url = base_url.clone();
                request_model
            }
            None => model.clone(),
        };

        // Explicit request options win per-field (pi: `options?.apiKey ?? auth.apiKey`).
        let api_key = options
            .and_then(|o| o.api_key.clone())
            .or_else(|| auth.api_key.clone());
        let headers = merge_headers(
            auth.headers.as_ref(),
            options.and_then(|o| o.headers.as_ref()),
        );
        let env = merge_env(
            resolution.env.as_ref(),
            options.and_then(|o| o.env.as_ref()),
        );

        let mut request_options = options.cloned().unwrap_or_default();
        request_options.api_key = api_key;
        request_options.headers = headers;
        request_options.env = env;

        provider.generate_images(&request_model, context, Some(&request_options), signal)
    }

    /// Build pi's `stopReason: "error"` [`AssistantImages`] shell
    /// (`images-models.ts:253-261`).
    ///
    /// pi stamps `Date.now()` here; the collection carries no injected clock, so
    /// — like the chat registry's pre-dispatch error shell
    /// ([`Models::stream`](crate::providers::Models::stream)'s `error_result`) —
    /// the timestamp is a deterministic `0` rather than a fresh wall-clock read.
    fn error_images(&self, model: &ImagesModel, message: String) -> AssistantImages {
        AssistantImages {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            output: Vec::new(),
            response_id: None,
            usage: None,
            stop_reason: ImagesStopReason::Error,
            error_message: Some(message),
            timestamp: 0,
        }
    }
}

impl MutableImagesModels for ImagesModelsImpl {
    fn set_provider(&mut self, provider: Arc<dyn ImagesProvider>) {
        if let Some(slot) = self.providers.iter_mut().find(|p| p.id() == provider.id()) {
            *slot = provider;
        } else {
            self.providers.push(provider);
        }
    }

    fn delete_provider(&mut self, id: &str) {
        self.providers.retain(|p| p.id() != id);
    }

    fn clear_providers(&mut self) {
        self.providers.clear();
    }
}

/// Build an empty image-model collection, pi's `createImagesModels`
/// (`images-models.ts:264-266`).
pub fn create_images_models(options: CreateImagesModelsOptions) -> ImagesModelsImpl {
    ImagesModelsImpl::new(options)
}

/// Options for [`create_images_provider`], pi's `CreateImagesProviderOptions`
/// (`images-models.ts:268-289`).
pub struct CreateImagesProviderOptions {
    /// The provider id.
    pub id: String,
    /// Display name; defaults to `id`.
    pub name: Option<String>,
    /// Provider auth (every provider has auth semantics).
    pub auth: ProviderAuth,
    /// The initial model list (empty for purely dynamic providers).
    pub models: Vec<ImagesModel>,
    /// A dynamic provider's fetch hook; `None` for a static provider.
    pub refresh_models: Option<RefreshModelsFn>,
    /// The image-generation api implementation.
    pub api: Arc<dyn ProviderImages>,
}

/// The concrete provider built by [`create_images_provider`].
struct CreatedImagesProvider {
    auth: AuthProvider,
    name: String,
    models: Mutex<Vec<ImagesModel>>,
    refresh_models: Option<RefreshModelsFn>,
    api: Arc<dyn ProviderImages>,
}

impl ImagesProvider for CreatedImagesProvider {
    fn id(&self) -> &str {
        &self.auth.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn auth(&self) -> &AuthProvider {
        &self.auth
    }

    fn get_models(&self) -> Vec<ImagesModel> {
        self.models.lock().unwrap().clone()
    }

    fn refresh_models(&self) -> Option<Result<(), ProviderRefreshError>> {
        let refresh = self.refresh_models.as_ref()?;
        Some(match refresh() {
            Ok(models) => {
                *self.models.lock().unwrap() = models;
                Ok(())
            }
            Err(error) => Err(error),
        })
    }

    fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
        signal: Option<&AbortSignal>,
    ) -> AssistantImages {
        self.api.generate_images(model, context, options, signal)
    }
}

/// Build an image-generation provider from parts, pi's `createImagesProvider`
/// (`images-models.ts:291-322`). The single-flight refresh dedupe collapses in
/// the synchronous port (see the module note).
pub fn create_images_provider(input: CreateImagesProviderOptions) -> Arc<dyn ImagesProvider> {
    let name = input.name.unwrap_or_else(|| input.id.clone());
    Arc::new(CreatedImagesProvider {
        auth: AuthProvider {
            id: input.id,
            auth: input.auth,
        },
        name,
        models: Mutex::new(input.models),
        refresh_models: input.refresh_models,
        api: input.api,
    })
}

/// Merge `{ ...authHeaders, ...optionsHeaders }` into a plain header record, pi's
/// `auth.headers || options?.headers ? { ... } : undefined`. `authHeaders`
/// null-valued entries (provider-strip markers) are dropped.
fn merge_headers(
    auth_headers: Option<&ProviderHeaders>,
    options_headers: Option<&BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    if auth_headers.is_none() && options_headers.is_none() {
        return None;
    }
    let mut merged = BTreeMap::new();
    if let Some(headers) = auth_headers {
        for (name, value) in headers {
            if let Some(value) = value {
                merged.insert(name.clone(), value.clone());
            }
        }
    }
    if let Some(headers) = options_headers {
        merged.extend(headers.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    Some(merged)
}

/// Merge `{ ...resolutionEnv, ...optionsEnv }`, pi's `resolution.env ||
/// options?.env ? { ... } : undefined`.
fn merge_env(
    resolution_env: Option<&ProviderEnv>,
    options_env: Option<&BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    if resolution_env.is_none() && options_env.is_none() {
        return None;
    }
    let mut merged = BTreeMap::new();
    if let Some(env) = resolution_env {
        merged.extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    if let Some(env) = options_env {
        merged.extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    Some(merged)
}

#[cfg(test)]
mod tests;
