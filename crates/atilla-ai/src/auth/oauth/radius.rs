// straitjacket-allow-file[:duplication] — the five OAuth provider stubs share
// one faithful `OAuthAuth` impl skeleton (name/login_label/login/refresh/to_auth
// with `todo!()` bodies pending the provider workers) plus parallel constant
// blocks. The clone detector reads these mirrored skeletons across the provider
// files as duplicates; the repetition is the intended per-provider layout.
//! Radius gateway OAuth flow — STUB.
//!
//! Ported skeleton of pi-ai's `packages/ai/src/auth/oauth/radius.ts` at pinned
//! commit `3da591ab`. Radius is a pi-messages gateway whose OAuth endpoints are
//! discovered from the gateway (`/v1/oauth`). Public constants and the
//! [`OAuthAuth`] surface are laid down here; the browser + device-code login and
//! refresh bodies are ported by the Radius provider worker.
//!
//! # Scope
//!
//! Binding the real TCP loopback callback listener (`node:http` on port
//! [`CALLBACK_PORT`]) is out of scope — there is no socket seam among the five.
//! The callback request-handling (path/state validation) will be ported as a
//! pure function; the browser HTML lives in [`super::oauth_page`].
//!
//! # TODO(port dep)
//!
//! [`RadiusOAuth::new`] normalizes the gateway URL via `normalizeRadiusGatewayUrl`
//! (from `providers/radius-config.ts`, not yet ported). Until that lands the
//! gateway is stored verbatim; the provider worker wires normalization in.

use crate::auth::error::AuthFlowError;
use crate::auth::types::{AuthInteraction, ModelAuth, OAuthAuth, OAuthCredential, OAuthFlow};

/// Loopback callback host (`radius.ts:25`).
pub const CALLBACK_HOST: &str = "127.0.0.1";
/// Loopback callback port. Binding the real socket is out of scope
/// (`radius.ts:26`).
pub const CALLBACK_PORT: u16 = 1456;
/// Callback path (`radius.ts:27`).
pub const CALLBACK_PATH: &str = "/oauth/callback";
/// Redirect URI (`radius.ts:28`).
pub const REDIRECT_URI: &str = "http://127.0.0.1:1456/oauth/callback";
/// Token-expiry skew, in ms (`radius.ts:29`).
pub const TOKEN_EXPIRY_SKEW_MS: i64 = 60_000;
/// Browser login-method id (`radius.ts:30`).
pub const LOGIN_METHOD_BROWSER: &str = "browser";
/// Device-code login-method id (`radius.ts:31`).
pub const LOGIN_METHOD_DEVICE_CODE: &str = "device-code";

/// Radius OAuth flow handler, parameterized by gateway (`radius.ts:360-410`).
#[derive(Debug, Clone)]
pub struct RadiusOAuth {
    name: String,
    /// The normalized gateway URL. See the module `TODO(port dep)`.
    gateway: String,
}

impl RadiusOAuth {
    /// Construct a Radius handler for `gateway` under display `name`
    /// (`createRadiusOAuth`; `radius.ts:360-362`).
    pub fn new(name: impl Into<String>, gateway: impl Into<String>) -> Self {
        // TODO(port dep): gateway = normalize_radius_gateway_url(gateway).
        Self {
            name: name.into(),
            gateway: gateway.into(),
        }
    }

    /// The configured gateway URL.
    pub fn gateway(&self) -> &str {
        &self.gateway
    }
}

impl OAuthAuth for RadiusOAuth {
    fn name(&self) -> &str {
        &self.name
    }

    // TODO(port): body pending — provider worker (config discovery + login-method
    // select + browser/device-code flow; `radius.ts:366-390`).
    fn login(
        &self,
        _interaction: &dyn AuthInteraction,
        _flow: &OAuthFlow,
    ) -> Result<OAuthCredential, AuthFlowError> {
        todo!("Radius OAuth login — provider worker")
    }

    // TODO(port): body pending — provider worker (config discovery +
    // refresh_token grant; `radius.ts:392-404`).
    fn refresh(
        &self,
        _credential: &OAuthCredential,
        _flow: &OAuthFlow,
    ) -> Result<OAuthCredential, AuthFlowError> {
        todo!("Radius OAuth refresh — provider worker")
    }

    fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError> {
        // `toAuth` returns `{ apiKey: credential.access }` (`radius.ts:406-408`).
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}
