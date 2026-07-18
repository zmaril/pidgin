// straitjacket-allow-file[:duplication] — the five OAuth provider stubs share
// one faithful `OAuthAuth` impl skeleton (name/login_label/login/refresh/to_auth
// with `todo!()` bodies pending the provider workers) plus parallel constant
// blocks. The clone detector reads these mirrored skeletons across the provider
// files as duplicates; the repetition is the intended per-provider layout.
//! Anthropic OAuth flow (Claude Pro/Max) — STUB.
//!
//! Ported skeleton of pi-ai's `packages/ai/src/auth/oauth/anthropic.ts` at
//! pinned commit `3da591ab`. The public constants and the [`OAuthAuth`] surface
//! are laid down here; the login/refresh bodies are ported by the Anthropic
//! provider worker.
//!
//! # Scope
//!
//! Binding the real TCP loopback callback listener (`node:http.createServer` on
//! port [`CALLBACK_PORT`]) is out of scope — there is no socket seam among the
//! five. The callback request-handling logic (path/state validation) and
//! [`parse_authorization_input`] will be ported as pure functions; the browser
//! HTML lives in [`super::oauth_page`].

use crate::auth::error::AuthFlowError;
use crate::auth::types::{AuthInteraction, ModelAuth, OAuthAuth, OAuthCredential, OAuthFlow};

/// OAuth client id (pi decodes this from base64; `anthropic.ts:29`).
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Authorization endpoint (`anthropic.ts:30`).
pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
/// Token endpoint (`anthropic.ts:31`).
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Default loopback callback host (`PI_OAUTH_CALLBACK_HOST` or this;
/// `anthropic.ts:32`).
pub const DEFAULT_CALLBACK_HOST: &str = "127.0.0.1";
/// Loopback callback port. Binding the real socket is out of scope
/// (`anthropic.ts:33`).
pub const CALLBACK_PORT: u16 = 53692;
/// Callback path (`anthropic.ts:34`).
pub const CALLBACK_PATH: &str = "/callback";
/// Redirect URI (`anthropic.ts:35`).
pub const REDIRECT_URI: &str = "http://localhost:53692/callback";
/// OAuth scopes (`anthropic.ts:36-37`).
pub const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
/// Refresh skew: tokens expire `expires_in*1000 - 5min` (`anthropic.ts:225,338`).
pub const REFRESH_SKEW_MS: i64 = 5 * 60 * 1000;

/// Parsed authorization input (code / state) from a pasted redirect URL or code
/// (`anthropic.ts:52-80`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedAuthInput {
    /// The authorization code, if any.
    pub code: Option<String>,
    /// The OAuth state, if any.
    pub state: Option<String>,
}

/// Parse a pasted authorization code / redirect URL into code + state
/// (`anthropic.ts:52-80`).
// TODO(port): body pending — provider worker. Ports the URL / `#` / `code=` /
// bare-code branches as a pure function.
pub fn parse_authorization_input(_input: &str) -> ParsedAuthInput {
    todo!("Anthropic parse_authorization_input — provider worker")
}

/// Anthropic OAuth flow handler (`anthropic.ts:342-350`).
#[derive(Debug, Default, Clone)]
pub struct AnthropicOAuth;

impl AnthropicOAuth {
    /// Construct the handler.
    pub fn new() -> Self {
        Self
    }
}

impl OAuthAuth for AnthropicOAuth {
    fn name(&self) -> &str {
        "Anthropic (Claude Pro/Max)"
    }

    // TODO(port): body pending — provider worker (PKCE + loopback callback +
    // authorization-code exchange; `anthropic.ts:229-303`).
    fn login(
        &self,
        _interaction: &dyn AuthInteraction,
        _flow: &OAuthFlow,
    ) -> Result<OAuthCredential, AuthFlowError> {
        todo!("Anthropic OAuth login — provider worker")
    }

    // TODO(port): body pending — provider worker (refresh_token grant against
    // TOKEN_URL; `anthropic.ts:308-340`).
    fn refresh(
        &self,
        _credential: &OAuthCredential,
        _flow: &OAuthFlow,
    ) -> Result<OAuthCredential, AuthFlowError> {
        todo!("Anthropic OAuth refresh — provider worker")
    }

    fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError> {
        // `toAuth` returns `{ apiKey: credential.access }` (`anthropic.ts:347-349`).
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}
