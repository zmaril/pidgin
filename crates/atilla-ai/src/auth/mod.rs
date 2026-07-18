//! Auth / OAuth subsystem, ported from pi-ai's `packages/ai/src/auth/` at pinned
//! commit `3da591ab`.
//!
//! This is the shared foundation of pi's AI auth: credential types
//! ([`types`]), the coded error surface ([`error`]), the default auth context
//! ([`context`]), the locked credential store ([`credential_store`]), api-key /
//! lazy-OAuth helpers ([`helpers`]), the double-checked-locking resolver
//! ([`resolve`]), and the OAuth flow foundation plus per-provider stubs
//! ([`oauth`]).
//!
//! # Sync port
//!
//! pi's auth is async and does network via `fetch` / time via `Date.now()`. This
//! port is synchronous: network goes through the [`crate::seams::http`] transport
//! and time through the [`crate::seams::clock`] seam, bundled for the OAuth flows
//! as [`types::OAuthFlow`]. This mirrors how [`crate::api::anthropic`] keeps I/O
//! out of the core. Real TCP loopback listeners (the OAuth callback servers) are
//! out of scope; their request-handling and page HTML are ported as pure
//! functions instead.

pub mod context;
pub mod credential_store;
pub mod error;
pub mod helpers;
pub mod oauth;
pub mod resolve;
pub mod types;

pub use context::DefaultAuthContext;
pub use credential_store::{
    CredentialStore, InMemoryCredentialStore, ModifyError, ModifyFn, StoreError,
};
pub use error::{AuthFlowError, ModelsError, ModelsErrorCode};
pub use helpers::{env_api_key_auth, EnvApiKeyAuth, LazyOAuth};
pub use resolve::{resolve_provider_auth, resolve_stored_oauth};
pub use types::{
    ApiKeyAuth, ApiKeyCredential, AuthCheck, AuthContext, AuthEvent, AuthInfoLink, AuthInteraction,
    AuthPrompt, AuthPromptKind, AuthProvider, AuthResolutionOverrides, AuthResult,
    AuthSelectOption, AuthType, Credential, CredentialInfo, ModelAuth, OAuthAuth, OAuthCredential,
    OAuthCredentials, OAuthFlow, ProviderAuth, ProviderEnv, ProviderHeaders,
};
