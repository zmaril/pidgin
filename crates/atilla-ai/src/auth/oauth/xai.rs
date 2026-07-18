// straitjacket-allow-file[:duplication] — the five OAuth provider stubs share
// one faithful `OAuthAuth` impl skeleton (name/login_label/login/refresh/to_auth
// with `todo!()` bodies pending the provider workers) plus parallel constant
// blocks. The clone detector reads these mirrored skeletons across the provider
// files as duplicates; the repetition is the intended per-provider layout.
//! xAI OAuth device-code flow (Grok/X subscription) — STUB.
//!
//! Ported skeleton of pi-ai's `packages/ai/src/auth/oauth/xai.ts` at pinned
//! commit `3da591ab`. Public constants and the [`OAuthAuth`] surface are laid
//! down here; the device-code login/refresh bodies (built on
//! [`super::device_code::poll_oauth_device_code_flow`]) are ported by the xAI
//! provider worker.

use crate::auth::error::AuthFlowError;
use crate::auth::types::{AuthInteraction, ModelAuth, OAuthAuth, OAuthCredential, OAuthFlow};

/// OAuth client id (`xai.ts:8`).
pub const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
/// OAuth scope (`xai.ts:9`).
pub const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
/// Device-code endpoint (`xai.ts:10`).
pub const XAI_DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
/// Token endpoint (`xai.ts:11`).
pub const XAI_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
/// Refresh skew, in ms: refresh slightly before reported expiry (`xai.ts:13`).
pub const REFRESH_SKEW_MS: i64 = 5 * 60 * 1000;
/// Default token lifetime when the server omits `expires_in` (`xai.ts:14`).
pub const DEFAULT_TOKEN_LIFETIME_SECONDS: f64 = 3600.0;

/// xAI OAuth flow handler (`xai.ts:229-238`).
#[derive(Debug, Default, Clone)]
pub struct XaiOAuth;

impl XaiOAuth {
    /// Construct the handler.
    pub fn new() -> Self {
        Self
    }
}

impl OAuthAuth for XaiOAuth {
    fn name(&self) -> &str {
        "xAI (Grok/X subscription)"
    }

    fn login_label(&self) -> Option<&str> {
        // `loginLabel` (`xai.ts:231`).
        Some("Sign in with SuperGrok or X Premium")
    }

    // TODO(port): body pending — provider worker (device-code request + poll;
    // `xai.ts:201-211`).
    fn login(
        &self,
        _interaction: &dyn AuthInteraction,
        _flow: &OAuthFlow,
    ) -> Result<OAuthCredential, AuthFlowError> {
        todo!("xAI OAuth login — provider worker")
    }

    // TODO(port): body pending — provider worker (refresh_token grant;
    // `xai.ts:213-227`).
    fn refresh(
        &self,
        _credential: &OAuthCredential,
        _flow: &OAuthFlow,
    ) -> Result<OAuthCredential, AuthFlowError> {
        todo!("xAI OAuth refresh — provider worker")
    }

    fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError> {
        // `toAuth` returns `{ apiKey: credential.access }` (`xai.ts:235-237`).
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}
