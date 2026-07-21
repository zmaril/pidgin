//! The extension-OAuth *type surface* and the login seam — pi's
//! `OAuthLoginCallbacks` (`packages/ai/src/compat/extension-oauth-types.ts:35`)
//! and the effectful [`ExtensionOAuthLogin`] callable (the members of pi's
//! `ExtensionOAuthConfig`, `packages/coding-agent/src/core/provider-composer.ts:37`).
//!
//! This module holds only the legacy extension-OAuth request/prompt types
//! ([`OAuthAuthInfo`], [`OAuthDeviceCodeInfo`], [`OAuthPrompt`],
//! [`OAuthSelectOption`], [`OAuthSelectPrompt`]), the callback surface handed to
//! an extension login ([`OAuthLoginCallbacks`]), and the login callable
//! ([`ExtensionOAuthLogin`]). Keeping them here lets both consumers reach one
//! copy: the extension plane (pidgin-extensions) implements
//! [`ExtensionOAuthLogin`] over its JS closures, while pidgin-coding holds it in
//! the composer's `ExtensionOAuthConfig.login`.
//!
//! # The adapter lives in pidgin-coding
//!
//! pi's `adaptOAuth` (`provider-composer.ts:230`) — which bridges a **push**
//! extension login (one handed an [`OAuthLoginCallbacks`]) onto pidgin-ai's
//! **pull** [`OAuthAuth`](crate::auth::types::OAuthAuth) flow machine via a `std::thread` + channel
//! re-inversion — lives in pi's coding-agent package, so it lives in
//! pidgin-coding here (`pidgin-coding`'s `core::extension_oauth_adapt`), next to
//! the credential-aware provider-composer. It drives whatever
//! [`ExtensionOAuthLogin`] it is given through this module's callback surface.

use crate::auth::error::AuthFlowError;
use crate::auth::types::OAuthCredential;

/// Legacy extension OAuth authorization link (pi's `OAuthAuthInfo`,
/// `extension-oauth-types.ts:11-14`). Maps to [`AuthEvent::AuthUrl`](crate::auth::types::AuthEvent::AuthUrl).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthAuthInfo {
    /// The authorization URL for the user to open.
    pub url: String,
    /// Optional instructions to show alongside the URL.
    pub instructions: Option<String>,
}

/// Legacy extension OAuth device-code notification (pi's `OAuthDeviceCodeInfo`,
/// `extension-oauth-types.ts:17-22`). Maps to [`AuthEvent::DeviceCode`](crate::auth::types::AuthEvent::DeviceCode).
#[derive(Debug, Clone, PartialEq)]
pub struct OAuthDeviceCodeInfo {
    /// The user code to enter at the verification URI.
    pub user_code: String,
    /// The verification URI.
    pub verification_uri: String,
    /// The poll interval, in seconds.
    pub interval_seconds: Option<f64>,
    /// The device-code lifetime, in seconds.
    pub expires_in_seconds: Option<f64>,
}

/// Legacy extension OAuth prompt (pi's `OAuthPrompt`,
/// `extension-oauth-types.ts:4-8`). Maps to an [`AuthPromptKind::Text`](crate::auth::types::AuthPromptKind::Text) prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthPrompt {
    /// The prompt message.
    pub message: String,
    /// An optional placeholder.
    pub placeholder: Option<String>,
    /// Whether an empty response is allowed (retained from pi; the canonical
    /// `text` prompt carries no such field, so it is not forwarded).
    pub allow_empty: Option<bool>,
}

/// A selectable option in an extension `select` prompt (pi's `OAuthSelectOption`,
/// `extension-oauth-types.ts:24-27`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSelectOption {
    /// The option id (returned when this option is chosen).
    pub id: String,
    /// The option label.
    pub label: String,
}

/// An extension `select` prompt (pi's `OAuthSelectPrompt`,
/// `extension-oauth-types.ts:29-32`). Maps to an [`AuthPromptKind::Select`](crate::auth::types::AuthPromptKind::Select).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSelectPrompt {
    /// The prompt message.
    pub message: String,
    /// The selectable options.
    pub options: Vec<OAuthSelectOption>,
}

/// The callback surface handed to an extension's OAuth login, mirroring pi's
/// `OAuthLoginCallbacks` (`extension-oauth-types.ts:35-43`).
///
/// The fire-and-forget notifications (`on_auth`/`on_device_code`/`on_progress`)
/// return nothing; the reply-shaped prompts (`on_prompt`/`on_manual_code_input`/
/// `on_select`) block until the caller supplies a value and error on
/// cancel/abort. pi's optional `onProgress`/`onManualCodeInput` are required here
/// (the adapter always supplies them).
pub trait OAuthLoginCallbacks {
    /// Surface an authorization URL (pi's `onAuth`).
    fn on_auth(&self, info: OAuthAuthInfo);
    /// Surface a device-code display (pi's `onDeviceCode`).
    fn on_device_code(&self, info: OAuthDeviceCodeInfo);
    /// Prompt for free-text input, blocking for the reply (pi's `onPrompt`).
    fn on_prompt(&self, prompt: OAuthPrompt) -> Result<String, AuthFlowError>;
    /// Surface a free-form progress message (pi's `onProgress`).
    fn on_progress(&self, message: String);
    /// Prompt for a manually pasted authorization code (pi's
    /// `onManualCodeInput`).
    fn on_manual_code_input(&self) -> Result<String, AuthFlowError>;
    /// Prompt for a single choice, blocking for the reply (pi's `onSelect`).
    fn on_select(&self, prompt: OAuthSelectPrompt) -> Result<Option<String>, AuthFlowError>;
}

/// An extension's callback-driven OAuth login, mirroring the effectful members
/// of pi's `ExtensionOAuthConfig` (`provider-composer.ts:37-39`).
///
/// The extension plane (pidgin-extensions) implements this over its JS
/// `login(callbacks)` / `refreshToken` / `getApiKey` closures;
/// pidgin-coding's `adapt_extension_oauth` drives it through a thread + channel bridge.
pub trait ExtensionOAuthLogin: Send + Sync {
    /// Run the interactive login, driving `callbacks` and returning a fresh
    /// credential (pi's `login(callbacks)`).
    fn login(&self, callbacks: &dyn OAuthLoginCallbacks) -> Result<OAuthCredential, AuthFlowError>;

    /// Exchange the refresh token for a fresh credential (pi's `refreshToken`).
    fn refresh_token(&self, credential: &OAuthCredential)
        -> Result<OAuthCredential, AuthFlowError>;

    /// Derive the request api key from a credential (pi's `getApiKey`).
    fn get_api_key(&self, credential: &OAuthCredential) -> Result<String, AuthFlowError>;
}
