// straitjacket-allow-file[:duplication] — the five OAuth provider stubs share
// one faithful `OAuthAuth` impl skeleton (name/login_label/login/refresh/to_auth
// with `todo!()` bodies pending the provider workers) plus parallel constant
// blocks. The clone detector reads these mirrored skeletons across the provider
// files as duplicates; the repetition is the intended per-provider layout.
//! OpenAI Codex (ChatGPT OAuth) flow — STUB.
//!
//! Ported skeleton of pi-ai's `packages/ai/src/auth/oauth/openai-codex.ts` at
//! pinned commit `3da591ab`. Public constants and the [`OAuthAuth`] surface are
//! laid down here; the browser + device-code login and refresh bodies are ported
//! by the OpenAI Codex provider worker.
//!
//! # Scope
//!
//! Binding the real TCP loopback callback listener (`node:http` on port
//! [`CALLBACK_PORT`]) is out of scope — there is no socket seam among the five.
//! The callback request-handling (path/state validation) and
//! [`parse_authorization_input`] will be ported as pure functions; the JWT
//! `accountId` extraction is likewise a pure helper.

use crate::auth::error::AuthFlowError;
use crate::auth::types::{ModelAuth, OAuthAuth, OAuthCredential};

use super::flow::{OAuthFlowMachine, Step, StepInput};

/// OAuth client id (`openai-codex.ts:26`).
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Auth base URL (`openai-codex.ts:27`).
pub const AUTH_BASE_URL: &str = "https://auth.openai.com";
/// Authorization endpoint (`openai-codex.ts:28`).
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
/// Token endpoint (`openai-codex.ts:29`).
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Loopback redirect URI (`openai-codex.ts:30`).
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
/// Loopback callback port. Binding the real socket is out of scope
/// (`openai-codex.ts:369`).
pub const CALLBACK_PORT: u16 = 1455;
/// Callback path (`openai-codex.ts:337`).
pub const CALLBACK_PATH: &str = "/auth/callback";
/// Device user-code endpoint (`openai-codex.ts:31`).
pub const DEVICE_USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
/// Device token endpoint (`openai-codex.ts:32`).
pub const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
/// Device verification URI shown to the user (`openai-codex.ts:33`).
pub const DEVICE_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
/// Device redirect URI used in the device-code token exchange
/// (`openai-codex.ts:34`).
pub const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
/// Device-code lifetime, in seconds (`openai-codex.ts:35`).
pub const DEVICE_CODE_TIMEOUT_SECONDS: f64 = 15.0 * 60.0;
/// Browser login-method id (`openai-codex.ts:36`).
pub const BROWSER_LOGIN_METHOD: &str = "browser";
/// Device-code login-method id (`openai-codex.ts:37`).
pub const DEVICE_CODE_LOGIN_METHOD: &str = "device_code";
/// OAuth scope (`openai-codex.ts:38`).
pub const SCOPE: &str = "openid profile email offline_access";
/// JWT claim path carrying the ChatGPT account id (`openai-codex.ts:39`).
pub const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

/// Parsed authorization input (code / state) from a pasted redirect URL or code
/// (`openai-codex.ts:73-101`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedAuthInput {
    /// The authorization code, if any.
    pub code: Option<String>,
    /// The OAuth state, if any.
    pub state: Option<String>,
}

/// Parse a pasted authorization code / redirect URL into code + state
/// (`openai-codex.ts:73-101`).
// TODO(port): body pending — provider worker (pure function).
pub fn parse_authorization_input(_input: &str) -> ParsedAuthInput {
    todo!("OpenAI Codex parse_authorization_input — provider worker")
}

/// OpenAI Codex OAuth flow handler (`openai-codex.ts:510-538`).
#[derive(Debug, Default, Clone)]
pub struct OpenAICodexOAuth;

impl OpenAICodexOAuth {
    /// Construct the handler.
    pub fn new() -> Self {
        Self
    }
}

impl OAuthAuth for OpenAICodexOAuth {
    fn name(&self) -> &str {
        "OpenAI (ChatGPT Plus/Pro)"
    }

    // TODO(port): body pending — provider worker (login-method select, then
    // browser or device-code flow; `openai-codex.ts:513-531`).
    fn login_machine(&self) -> Box<dyn OAuthFlowMachine> {
        Box::new(OpenAICodexStubMachine)
    }

    // TODO(port): body pending — provider worker (refresh_token grant;
    // `openai-codex.ts:171-188,506-508`).
    fn refresh_machine(&self, _credential: &OAuthCredential) -> Box<dyn OAuthFlowMachine> {
        Box::new(OpenAICodexStubMachine)
    }

    fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError> {
        // `toAuth` returns `{ apiKey: credential.access }` (`openai-codex.ts:535-537`).
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

/// Stub flow machine — the browser + device-code login/refresh state machines
/// are ported by the OpenAI Codex provider worker.
struct OpenAICodexStubMachine;

impl OAuthFlowMachine for OpenAICodexStubMachine {
    fn start(&mut self, _now_ms: i64) -> Step {
        todo!("OpenAI Codex OAuth flow machine — provider worker")
    }
    fn advance(&mut self, _input: StepInput, _now_ms: i64) -> Step {
        todo!("OpenAI Codex OAuth flow machine — provider worker")
    }
}
