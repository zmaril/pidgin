// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// auth `types.ts`: the credential/result/prompt/event structs are walls of
// near-identical optional fields sharing the same skip-serializing serde shape,
// and the api-key / OAuth auth traits mirror pi's parallel interface members by
// design. The clone detector reads these as duplicates; they are distinct,
// load-bearing boundary declarations kept verbatim to mirror pi's auth surface.
//! Auth boundary types, ported from pi-ai's `packages/ai/src/auth/types.ts` at
//! pinned commit `3da591ab`.
//!
//! This is the provider-agnostic auth surface: request auth ([`ModelAuth`]),
//! stored credentials ([`Credential`] and its api-key / OAuth variants), the
//! injectable [`AuthContext`], resolution results, the interactive login
//! interaction ([`AuthInteraction`], [`AuthPrompt`], [`AuthEvent`]), and the
//! per-provider auth handlers ([`ApiKeyAuth`], [`OAuthAuth`], [`ProviderAuth`]).
//!
//! # Sync port deviations from pi
//!
//! pi's auth is async and does network via `fetch` / time via `Date.now()`.
//! This port is synchronous: the OAuth flows take the [`crate::seams`] HTTP
//! transport and clock as an injected [`OAuthFlow`] bundle instead of calling
//! `fetch`/`Date.now()` themselves, mirroring how `api/anthropic.rs` keeps I/O
//! out of the core. pi's `type: "api_key" | "oauth"` discriminant on each
//! credential is represented once, on the [`Credential`] enum's serde tag, so
//! the inner [`ApiKeyCredential`] / [`OAuthCredential`] structs carry no `type`
//! field of their own (`types.ts:17-37`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::seams::clock::{Clock, Timers};
use crate::seams::http::HttpTransport;
use crate::seams::provider::AbortSignal;

use super::error::AuthFlowError;
use super::oauth::flow::OAuthFlowMachine;

/// Provider-scoped environment/config values (pi's `ProviderEnv`, a
/// `Record<string, string>`; `../types.ts:104`).
pub type ProviderEnv = BTreeMap<String, String>;

/// Provider request headers (pi's `ProviderHeaders`,
/// `Record<string, string | null>`; `../types.ts:105`). A `None` value marks a
/// header the provider strips.
pub type ProviderHeaders = BTreeMap<String, Option<String>>;

/// Request auth for a single model request (`types.ts:7-11`).
///
/// If a value cannot be expressed as `apiKey`, `headers`, or `baseUrl`, it is
/// provider config, not auth.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelAuth {
    /// The request api key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Extra request headers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<ProviderHeaders>,
    /// A per-credential base URL override (e.g. GitHub Copilot's proxy).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

/// Stored api-key credential (`types.ts:17-21`).
///
/// `env` holds provider-scoped environment/config values such as Cloudflare
/// account/gateway ids. The `type: "api_key"` discriminant lives on the
/// [`Credential`] enum tag.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ApiKeyCredential {
    /// The stored api key, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Provider-scoped environment/config values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<ProviderEnv>,
}

/// Stored canonical OAuth credential (`types.ts:24-34`).
///
/// pi's `OAuthCredentials` carries `refresh`/`access`/`expires` plus an open
/// index signature (`[key: string]: unknown`) so per-provider extras like
/// `enterpriseUrl`, `availableModelIds`, `accountId`, and `scope` round-trip.
/// The open signature is preserved here via [`serde(flatten)`] into [`extra`].
/// The `type: "oauth"` discriminant lives on the [`Credential`] enum tag.
///
/// [`extra`]: OAuthCredential::extra
/// [`serde(flatten)`]: https://serde.rs/field-attrs.html#flatten
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthCredential {
    /// The refresh token.
    pub refresh: String,
    /// The access token.
    pub access: String,
    /// Absolute expiry, in ms since the Unix epoch (pi's `expires`).
    pub expires: i64,
    /// Per-provider extras preserved from the open index signature
    /// (`enterpriseUrl`, `availableModelIds`, `accountId`, `scope`, ...).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// pi's `OAuthCredentials` (`types.ts:24-29`) — the token data returned by
/// extension compatibility flows. Structurally identical to [`OAuthCredential`]
/// in this port because the `type` discriminant lives on the [`Credential`]
/// enum, so they share one struct.
pub type OAuthCredentials = OAuthCredential;

/// One type-tagged credential per provider — the shape of today's `auth.json`
/// (`types.ts:37`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Credential {
    /// An api-key credential.
    #[serde(rename = "api_key")]
    ApiKey(ApiKeyCredential),
    /// An OAuth credential.
    #[serde(rename = "oauth")]
    OAuth(OAuthCredential),
}

impl Credential {
    /// The credential's type discriminant (`Credential["type"]`).
    pub fn auth_type(&self) -> AuthType {
        match self {
            Credential::ApiKey(_) => AuthType::ApiKey,
            Credential::OAuth(_) => AuthType::Oauth,
        }
    }
}

/// The credential type discriminant, `"api_key" | "oauth"` (`types.ts:111`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthType {
    /// An api-key credential.
    ApiKey,
    /// An OAuth credential.
    Oauth,
}

/// Non-secret credential metadata for account/status enumeration
/// (`types.ts:40-43`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialInfo {
    /// The `Provider.id` this credential is keyed by.
    #[serde(rename = "providerId")]
    pub provider_id: String,
    /// The credential type, without exposing any secret.
    #[serde(rename = "type")]
    pub credential_type: AuthType,
}

/// Environment access for auth resolution, injectable for tests and browsers
/// (`types.ts:91-95`).
///
/// The async pi signatures (`Promise<...>`) become synchronous here.
pub trait AuthContext {
    /// Look up an environment variable, returning `None` for unset/blank values.
    fn env(&self, name: &str) -> Option<String>;
    /// Whether a file exists. Supports a leading `~`. Always `false` in browsers.
    fn file_exists(&self, path: &str) -> bool;
}

/// Result of resolving auth for a model (`types.ts:98-104`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthResult {
    /// The resolved request auth.
    pub auth: ModelAuth,
    /// Provider-scoped env/config resolved from credentials and ambient context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<ProviderEnv>,
    /// Human-readable label for status UI ("ANTHROPIC_API_KEY", "OAuth", ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// A side-effect-free availability check result (`types.ts:106-109`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthCheck {
    /// The auth source label, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The auth type.
    #[serde(rename = "type")]
    pub check_type: AuthType,
}

/// A selectable option in a `select` prompt (`types.ts:122`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthSelectOption {
    /// The option id (returned by `prompt` when this option is chosen).
    pub id: String,
    /// The option label.
    pub label: String,
    /// An optional description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The kind of prompt shown to the user during login (`types.ts:119-124`).
///
/// Serialized internally-tagged on `type` (`text` / `secret` / `select` /
/// `manual_code`), matching pi's prompt discriminant, so the [`flow::Step::Prompt`]
/// step round-trips across the napi boundary.
///
/// [`flow::Step::Prompt`]: super::oauth::flow::Step::Prompt
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthPromptKind {
    /// A free-text prompt.
    Text {
        /// The prompt message.
        message: String,
        /// An optional placeholder.
        placeholder: Option<String>,
    },
    /// A masked secret prompt.
    Secret {
        /// The prompt message.
        message: String,
        /// An optional placeholder.
        placeholder: Option<String>,
    },
    /// A single-choice selection prompt.
    Select {
        /// The prompt message.
        message: String,
        /// The selectable options.
        options: Vec<AuthSelectOption>,
    },
    /// A manual authorization-code / redirect-URL prompt.
    ManualCode {
        /// The prompt message.
        message: String,
        /// An optional placeholder.
        placeholder: Option<String>,
    },
}

/// A prompt shown to the user during login (`types.ts:119-124`).
///
/// `signal` lets the flow cancel a pending prompt when an out-of-band event
/// (e.g. a callback server) resolves the step first. It is a live, in-process
/// handle, so it is skipped when the prompt is serialized across the napi
/// boundary (the shim drives cancellation via [`flow::StepInput::Aborted`]); the
/// `kind` is flattened so a prompt serializes as
/// `{"type":"manual_code","message":..,"placeholder":..}`.
///
/// [`flow::StepInput::Aborted`]: super::oauth::flow::StepInput::Aborted
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthPrompt {
    /// Per-prompt cancellation signal (not serialized).
    #[serde(skip)]
    pub signal: Option<AbortSignal>,
    /// The prompt kind.
    #[serde(flatten)]
    pub kind: AuthPromptKind,
}

/// A hyperlink attached to an [`AuthEvent::Info`] (`types.ts:126-129`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthInfoLink {
    /// The link URL.
    pub url: String,
    /// An optional label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A login-progress event surfaced to the UI (`types.ts:131-141`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthEvent {
    /// Informational message with optional links.
    Info {
        /// The message.
        message: String,
        /// Optional links.
        #[serde(skip_serializing_if = "Option::is_none")]
        links: Option<Vec<AuthInfoLink>>,
    },
    /// An authorization URL for the user to open.
    AuthUrl {
        /// The URL.
        url: String,
        /// Optional instructions.
        #[serde(skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    /// A device-code display for the RFC 8628 flow.
    DeviceCode {
        /// The user code.
        #[serde(rename = "userCode")]
        user_code: String,
        /// The verification URI.
        #[serde(rename = "verificationUri")]
        verification_uri: String,
        /// The poll interval, in seconds.
        #[serde(rename = "intervalSeconds", skip_serializing_if = "Option::is_none")]
        interval_seconds: Option<f64>,
        /// The device-code lifetime, in seconds.
        #[serde(rename = "expiresInSeconds", skip_serializing_if = "Option::is_none")]
        expires_in_seconds: Option<f64>,
    },
    /// A free-form progress message.
    Progress {
        /// The message.
        message: String,
    },
}

/// Login interaction callbacks serving both api-key and OAuth flows
/// (`types.ts:150-155`).
///
/// `prompt` returns the entered/selected string (a `select` returns the option
/// id) and errors on cancel/abort. `signal` aborts the whole login flow;
/// per-prompt cancellation uses [`AuthPrompt::signal`].
pub trait AuthInteraction {
    /// The flow-wide abort signal, if any.
    fn signal(&self) -> Option<&AbortSignal> {
        None
    }
    /// Prompt the user, returning the entered/selected string.
    fn prompt(&self, prompt: AuthPrompt) -> Result<String, AuthFlowError>;
    /// Surface a login-progress event.
    fn notify(&self, event: AuthEvent);
}

/// The injected seams an OAuth resolution needs: network, time, timers, and an
/// optional abort signal.
///
/// This is the sync-port stand-in for pi's ambient `fetch` / `Date.now()` /
/// `setTimeout` / `AbortSignal`. [`resolve`](super::resolve) bundles the seams
/// here and hands them to
/// [`run_refresh`](super::oauth::flow::run_refresh) when driving a provider's
/// refresh machine under the store lock; `clock` also drives the expiry checks.
pub struct OAuthFlow<'a> {
    /// The HTTP transport (pi's `fetch`).
    pub http: &'a dyn HttpTransport,
    /// The wall clock (pi's `Date.now()`).
    pub clock: &'a dyn Clock,
    /// The timer scheduler (pi's `setTimeout`, used by device-code polling).
    pub timers: &'a dyn Timers,
    /// A flow-wide abort signal, if any.
    pub signal: Option<&'a AbortSignal>,
}

/// Api-key auth: stored key/provider env plus ambient sources (`types.ts:161-182`).
///
/// Ambient-only providers omit `login` (the default returns `None`).
pub trait ApiKeyAuth: Send + Sync {
    /// Display name, e.g. "Anthropic API key".
    fn name(&self) -> &str;

    /// Interactive setup (prompt for key/provider env). `None` means the
    /// provider is ambient-only (pi's absent `login`).
    fn login(
        &self,
        _interaction: &dyn AuthInteraction,
    ) -> Option<Result<ApiKeyCredential, AuthFlowError>> {
        None
    }

    /// Optional side-effect-free availability check (pi's `check?`).
    fn check(
        &self,
        _ctx: &dyn AuthContext,
        _credential: Option<&ApiKeyCredential>,
    ) -> Option<AuthCheck> {
        None
    }

    /// Resolve auth from the stored credential and/or ambient sources. `None`
    /// means the provider is not configured.
    fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, AuthFlowError>;
}

/// OAuth auth (`types.ts:189-210`).
///
/// Login and refresh are multi-step flows that must not perform effects on the
/// conformance path (the JS shim owns `fetch`/`setTimeout`/prompts across the
/// one-way napi boundary), so instead of Rust-driving the network they each
/// return an [`OAuthFlowMachine`] the shim or the pure-Rust
/// [`run_flow`](super::oauth::flow::run_flow) driver advances. The
/// `refresh`/`to_auth` split lets `resolve` own the locked refresh pattern: the
/// refresh machine produces a credential, `to_auth` derives request auth from
/// whatever credential ends up stored.
pub trait OAuthAuth: Send + Sync {
    /// Display name, e.g. "Anthropic (Claude Pro/Max)".
    fn name(&self) -> &str;

    /// Selector label for the subscription login option (pi's `loginLabel?`).
    fn login_label(&self) -> Option<&str> {
        None
    }

    /// Build the interactive login flow machine. Driving it to completion
    /// produces a fresh OAuth credential.
    fn login_machine(&self) -> Box<dyn OAuthFlowMachine>;

    /// Build the refresh flow machine for `credential`. Driving it exchanges the
    /// refresh token; the flow errors on failure (invalid_grant etc.).
    /// `resolve` runs this under the store lock via
    /// [`run_refresh`](super::oauth::flow::run_refresh).
    fn refresh_machine(&self, credential: &OAuthCredential) -> Box<dyn OAuthFlowMachine>;

    /// Side-effect-free derivation of request auth from a valid credential.
    fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError>;
}

/// Provider auth (`types.ts:217-220`).
///
/// At least one of `api_key` / `oauth` must be present.
#[derive(Default)]
pub struct ProviderAuth {
    /// The api-key auth handler, if any.
    pub api_key: Option<Box<dyn ApiKeyAuth>>,
    /// The OAuth auth handler, if any.
    pub oauth: Option<Box<dyn OAuthAuth>>,
}

/// The minimal provider slice `resolve_provider_auth` needs: an id plus its
/// [`ProviderAuth`] (pi passes `{ id, auth }`; `resolve.ts:38`).
pub struct AuthProvider {
    /// The `Provider.id`, the credential-store key.
    pub id: String,
    /// The provider's auth handlers.
    pub auth: ProviderAuth,
}

/// Overrides for a single resolution (`resolve.ts:16-19`).
#[derive(Debug, Clone, Default)]
pub struct AuthResolutionOverrides {
    /// An explicit api-key override.
    pub api_key: Option<String>,
    /// Provider-scoped env overrides overlaid onto the auth context.
    pub env: Option<ProviderEnv>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn oauth_credential_round_trips_extras() {
        let json = json!({
            "type": "oauth",
            "refresh": "r",
            "access": "a",
            "expires": 1_700_000_000_000_i64,
            "enterpriseUrl": "company.ghe.com",
            "availableModelIds": ["gpt-5", "claude"],
            "accountId": "acct_1",
            "scope": "openid profile",
        });
        let cred: Credential = serde_json::from_value(json.clone()).unwrap();
        match &cred {
            Credential::OAuth(o) => {
                assert_eq!(o.refresh, "r");
                assert_eq!(o.access, "a");
                assert_eq!(o.expires, 1_700_000_000_000);
                assert_eq!(o.extra.get("enterpriseUrl").unwrap(), "company.ghe.com");
                assert_eq!(o.extra.get("accountId").unwrap(), "acct_1");
                assert_eq!(o.extra.get("scope").unwrap(), "openid profile");
                assert!(o.extra.get("availableModelIds").unwrap().is_array());
            }
            _ => panic!("expected oauth"),
        }
        // Round-trips back to the same JSON (extras preserved, tag re-emitted).
        assert_eq!(serde_json::to_value(&cred).unwrap(), json);
        assert_eq!(cred.auth_type(), AuthType::Oauth);
    }

    #[test]
    fn api_key_credential_round_trips() {
        let json = json!({ "type": "api_key", "key": "sk-123" });
        let cred: Credential = serde_json::from_value(json.clone()).unwrap();
        assert!(matches!(cred.auth_type(), AuthType::ApiKey));
        assert_eq!(serde_json::to_value(&cred).unwrap(), json);
    }

    #[test]
    fn model_auth_uses_camel_case_wire_names() {
        let auth = ModelAuth {
            api_key: Some("k".into()),
            headers: None,
            base_url: Some("https://api.example".into()),
        };
        let value = serde_json::to_value(&auth).unwrap();
        assert_eq!(value["apiKey"], "k");
        assert_eq!(value["baseUrl"], "https://api.example");
        assert!(value.get("headers").is_none());
    }

    #[test]
    fn auth_event_matches_pi_wire_tags() {
        let event = AuthEvent::DeviceCode {
            user_code: "WDJB-MJHT".into(),
            verification_uri: "https://example.com/device".into(),
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(900.0),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "device_code");
        assert_eq!(value["userCode"], "WDJB-MJHT");
        assert_eq!(value["verificationUri"], "https://example.com/device");
        assert_eq!(value["intervalSeconds"], 5.0);

        let url_event = AuthEvent::AuthUrl {
            url: "https://auth".into(),
            instructions: None,
        };
        assert_eq!(
            serde_json::to_value(&url_event).unwrap()["type"],
            "auth_url"
        );
    }

    #[test]
    fn credential_info_uses_provider_id_wire_name() {
        let info = CredentialInfo {
            provider_id: "anthropic".into(),
            credential_type: AuthType::Oauth,
        };
        let value = serde_json::to_value(&info).unwrap();
        assert_eq!(value["providerId"], "anthropic");
        assert_eq!(value["type"], "oauth");
    }
}
