//! The concrete [`ExtensionOAuthLogin`] seam implementation —
//! [`DenoExtensionOAuthLogin`].
//!
//! atilla-ai defines the `ExtensionOAuthLogin` trait (the effectful members of
//! pi's `ExtensionOAuthConfig` — `login` / `refreshToken` / `getApiKey`) and the
//! [`adapt_extension_oauth`] adapter that drives it; the trait object is injected
//! through `ExtensionOAuthConfig.login: Option<Arc<dyn ExtensionOAuthLogin>>`
//! (composer.rs), the inversion point. This module supplies the *real*
//! deno-backed impl: it invokes a captured provider's live JS `oauth` closures
//! (kept in `globalThis.__atilla.registry.providers`, keyed by provider name)
//! over the shared one-shot invoke-stored primitive
//! ([`JsPlaneHandle::invoke_stored`]).
//!
//! # Dependency inversion (why this lives in atilla-extensions)
//!
//! `atilla-extensions` may depend on `atilla-ai` (added, optional, under the
//! `deno` feature); `atilla-ai` depends only downward (atilla-model-catalog),
//! never on atilla-coding or atilla-extensions, so there is NO cycle. The real
//! `impl ExtensionOAuthLogin` therefore lives here and is wrapped as an
//! `Arc<dyn ExtensionOAuthLogin>` for the composer's `Option` field — the same
//! inversion as the `ExtensionLoader` / `ExtensionRunner` seams.
//!
//! # Sync-over-async bridge (off any ambient runtime)
//!
//! The trait is synchronous; the JS plane is async and off-thread. Each method
//! bridges via [`block_on_off_ambient`](crate::runner_impl::block_on_off_ambient)
//! — the same `exec-tools-async-vs-sync` pattern the `ExtensionRunner` /
//! `ExtensionLoader` impls use — so the blocking drive never nests inside an
//! ambient tokio runtime.
//!
//! # `login` is a documented error-stub
//!
//! `get_api_key` (pi sync → string) and `refresh_token` (pi async, credential →
//! credential) are simple one-shot forward invokes and are fully implemented
//! here. `login`, by contrast, needs a JS closure to re-enter Rust mid-execution
//! and await a Rust-supplied prompt reply (`onPrompt`/`onManualCodeInput`/
//! `onSelect` return awaited Promises) — the reentrant suspend/resume primitive
//! the one-shot invoke-stored primitive deliberately does NOT provide. Until that
//! future wave lands, [`login`](DenoExtensionOAuthLogin::login) returns a
//! documented [`AuthFlowError`] rather than a silent no-op. See the parked plan
//! `[[ext-oauth-login-reentrant-primitive-parked.md]]`.

// straitjacket-allow-file:duplication -- `get_api_key` and `refresh_token` are
// deliberate parallel structure (serialize the credential, one-shot invoke the
// stored provider `oauth` closure, isolate the failure, map the result); they
// mirror pi's `adaptOAuth` `toAuth` / `refresh` members one-for-one, not an
// accident to hoist away.

use std::sync::Arc;

use serde_json::{json, Value};

use atilla_ai::auth::error::AuthFlowError;
use atilla_ai::auth::oauth::extension::{ExtensionOAuthLogin, OAuthLoginCallbacks};
use atilla_ai::auth::types::OAuthCredential;

use crate::runner_impl::block_on_off_ambient;
use crate::runtime::JsPlaneHandle;

/// The error [`DenoExtensionOAuthLogin::login`] yields until the reentrant
/// suspend/resume callback primitive is built (a future wave; see
/// `[[ext-oauth-login-reentrant-primitive-parked.md]]`).
const LOGIN_PENDING: &str =
    "extension OAuth login pending reentrant primitive (interactive callback \
     suspend/resume not yet built; see ext-oauth-login-reentrant-primitive-parked)";

/// The deno-backed [`ExtensionOAuthLogin`] implementation.
///
/// Holds a shared handle to the JS plane and the provider name its `oauth`
/// closures were captured under (`pi.registerProvider(config)` →
/// `reg.providers[name]`).
pub struct DenoExtensionOAuthLogin {
    /// The shared off-thread JS plane the provider's `oauth` closures live in.
    plane: Arc<JsPlaneHandle>,
    /// The provider name the `oauth` closures were registered under.
    provider_name: String,
}

impl DenoExtensionOAuthLogin {
    /// Build a login callable for the provider captured as `provider_name` on
    /// `plane`.
    pub fn new(plane: Arc<JsPlaneHandle>, provider_name: impl Into<String>) -> Self {
        Self {
            plane,
            provider_name: provider_name.into(),
        }
    }

    /// Wrap as the `Arc<dyn ExtensionOAuthLogin>` the composer's
    /// `ExtensionOAuthConfig.login` field expects (the injection point).
    pub fn into_login(self) -> Arc<dyn ExtensionOAuthLogin> {
        Arc::new(self)
    }

    /// Serialize a credential to the JSON the JS closures receive, mapping a
    /// serialization failure to an [`AuthFlowError`].
    fn credential_json(&self, credential: &OAuthCredential) -> Result<Value, AuthFlowError> {
        serde_json::to_value(credential)
            .map_err(|error| AuthFlowError::new(format!("serialize credential: {error}")))
    }
}

impl ExtensionOAuthLogin for DenoExtensionOAuthLogin {
    fn login(&self, _callbacks: &dyn OAuthLoginCallbacks) -> Result<OAuthCredential, AuthFlowError> {
        // Documented error-stub: interactive login requires the reentrant
        // callback suspend/resume primitive (a future wave), which the one-shot
        // invoke-stored primitive does not provide. Never a silent no-op.
        // See `[[ext-oauth-login-reentrant-primitive-parked.md]]`.
        Err(AuthFlowError::new(LOGIN_PENDING))
    }

    fn refresh_token(
        &self,
        credential: &OAuthCredential,
    ) -> Result<OAuthCredential, AuthFlowError> {
        let cred = self.credential_json(credential)?;
        let invocation = block_on_off_ambient(self.plane.invoke_stored(
            "providerRefreshToken",
            self.provider_name.clone(),
            &json!([cred]),
        ))
        .map_err(|error| {
            AuthFlowError::new(format!(
                "provider '{}' refreshToken invocation failed: {error}",
                self.provider_name
            ))
        })?;
        if !invocation.ok {
            return Err(AuthFlowError::new(invocation.error.unwrap_or_else(|| {
                format!("provider '{}' refreshToken failed", self.provider_name)
            })));
        }
        serde_json::from_value::<OAuthCredential>(invocation.result).map_err(|error| {
            AuthFlowError::new(format!(
                "provider '{}' refreshToken returned an unparseable credential: {error}",
                self.provider_name
            ))
        })
    }

    fn get_api_key(&self, credential: &OAuthCredential) -> Result<String, AuthFlowError> {
        let cred = self.credential_json(credential)?;
        let invocation = block_on_off_ambient(self.plane.invoke_stored(
            "providerGetApiKey",
            self.provider_name.clone(),
            &json!([cred]),
        ))
        .map_err(|error| {
            AuthFlowError::new(format!(
                "provider '{}' getApiKey invocation failed: {error}",
                self.provider_name
            ))
        })?;
        if !invocation.ok {
            return Err(AuthFlowError::new(invocation.error.unwrap_or_else(|| {
                format!("provider '{}' getApiKey failed", self.provider_name)
            })));
        }
        match invocation.result {
            Value::String(key) => Ok(key),
            other => Err(AuthFlowError::new(format!(
                "provider '{}' getApiKey returned a non-string result: {other}",
                self.provider_name
            ))),
        }
    }
}
