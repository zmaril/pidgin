//! Shim-facing convenience constructor bridging a provider id + mode to a
//! driveable [`OAuthFlowMachine`].
//!
//! The napi core resolves a provider id (one of the flip-test provider ids) and
//! a [`OAuthFlowMode`] into a flow machine it can drive across the one-way napi
//! boundary, without knowing each provider's concrete `OAuthAuth` type. This is
//! the single entry point [`oauth_flow_for`] the core calls.
//!
//! Radius is intentionally not resolvable here: it needs a gateway URL and has
//! no pi flip test. Construct it directly via
//! `RadiusOAuth::new(name, gateway)` (see [`super::radius`]) and call
//! `login_machine()` / `refresh_machine(&cred)` on it.

use crate::auth::error::AuthFlowError;
use crate::auth::types::{OAuthAuth, OAuthCredential};

use super::flow::OAuthFlowMachine;
use super::load::{
    load_anthropic_oauth, load_github_copilot_oauth, load_openai_codex_oauth, load_xai_oauth,
};

/// Which OAuth flow the napi core wants for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthFlowMode {
    /// The interactive login flow (`OAuthAuth::login_machine`).
    Login,
    /// The token-refresh flow (`OAuthAuth::refresh_machine`).
    Refresh,
}

/// Resolve a provider id + mode into a driveable flow machine for the napi core.
///
/// `credential_json` is the serialized [`OAuthCredential`] for
/// [`OAuthFlowMode::Refresh`] (ignored for [`OAuthFlowMode::Login`]).
///
/// Provider ids are 1:1 with the flip-test provider files: `"anthropic"`,
/// `"openai-codex"`, `"github-copilot"`, `"xai"`. `"radius"` is not handled
/// (it needs a gateway URL and has no flip test) — it returns the
/// unknown-provider error; construct Radius directly via
/// `RadiusOAuth::new(name, gateway)`.
///
/// # Errors
///
/// - Unknown provider id → `unknown OAuth provider: <id>`.
/// - [`OAuthFlowMode::Refresh`] with `credential_json` `None` → error.
/// - [`OAuthFlowMode::Refresh`] with malformed credential JSON → error.
pub fn oauth_flow_for(
    provider: &str,
    mode: OAuthFlowMode,
    credential_json: Option<&str>,
) -> Result<Box<dyn OAuthFlowMachine>, AuthFlowError> {
    let auth: Box<dyn OAuthAuth> = match provider {
        "anthropic" => load_anthropic_oauth(),
        "openai-codex" => load_openai_codex_oauth(),
        "github-copilot" => load_github_copilot_oauth(),
        "xai" => load_xai_oauth(),
        other => {
            return Err(AuthFlowError::new(format!(
                "unknown OAuth provider: {other}"
            )))
        }
    };

    match mode {
        OAuthFlowMode::Login => Ok(auth.login_machine()),
        OAuthFlowMode::Refresh => {
            let json = credential_json.ok_or_else(|| {
                AuthFlowError::new("refresh requires a serialized OAuthCredential, got none")
            })?;
            let credential: OAuthCredential = serde_json::from_str(json).map_err(|error| {
                AuthFlowError::new(format!("invalid OAuthCredential JSON: {error}"))
            })?;
            Ok(auth.refresh_machine(&credential))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oauth::flow::Step;
    use crate::auth::types::{AuthEvent, AuthPromptKind};

    const NOW_MS: i64 = 1_700_000_000_000;

    /// A minimal valid `OAuthCredential` JSON for the refresh path.
    fn credential_json() -> String {
        serde_json::json!({
            "refresh": "refresh-token",
            "access": "access-token",
            "expires": NOW_MS,
        })
        .to_string()
    }

    #[test]
    fn anthropic_login_first_step_is_auth_url_notify() {
        let mut machine = oauth_flow_for("anthropic", OAuthFlowMode::Login, None).unwrap();
        match machine.start(NOW_MS) {
            Step::Notify {
                event: AuthEvent::AuthUrl { url, .. },
            } => assert!(url.starts_with("https://claude.ai/oauth/authorize?")),
            other => panic!("expected auth_url notify, got {other:?}"),
        }
    }

    #[test]
    fn openai_codex_login_first_step_is_select_prompt() {
        let mut machine = oauth_flow_for("openai-codex", OAuthFlowMode::Login, None).unwrap();
        match machine.start(NOW_MS) {
            Step::Prompt { prompt } => match prompt.kind {
                AuthPromptKind::Select { options, .. } => {
                    let ids: Vec<_> = options.iter().map(|o| o.id.as_str()).collect();
                    assert_eq!(ids, ["browser", "device_code"]);
                }
                other => panic!("expected select prompt, got {other:?}"),
            },
            other => panic!("expected prompt, got {other:?}"),
        }
    }

    #[test]
    fn github_copilot_login_first_step_is_text_prompt() {
        let mut machine = oauth_flow_for("github-copilot", OAuthFlowMode::Login, None).unwrap();
        match machine.start(NOW_MS) {
            Step::Prompt { prompt } => assert!(matches!(prompt.kind, AuthPromptKind::Text { .. })),
            other => panic!("expected text prompt, got {other:?}"),
        }
    }

    #[test]
    fn xai_login_first_step_is_device_code_request() {
        let mut machine = oauth_flow_for("xai", OAuthFlowMode::Login, None).unwrap();
        match machine.start(NOW_MS) {
            Step::Request { request } => {
                assert_eq!(request.url, "https://auth.x.ai/oauth2/device/code");
            }
            other => panic!("expected request, got {other:?}"),
        }
    }

    #[test]
    fn refresh_first_step_is_request_for_each_provider() {
        for id in ["anthropic", "openai-codex", "github-copilot", "xai"] {
            let json = credential_json();
            let mut machine = oauth_flow_for(id, OAuthFlowMode::Refresh, Some(&json)).unwrap();
            assert!(
                matches!(machine.start(NOW_MS), Step::Request { .. }),
                "provider {id} refresh should start with a request"
            );
        }
    }

    /// The `Err` message of a resolution that must fail (the `Ok` machine is not
    /// `Debug`, so `unwrap_err` is unavailable).
    fn err_message(result: Result<Box<dyn OAuthFlowMachine>, AuthFlowError>) -> String {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(error) => error.message,
        }
    }

    #[test]
    fn unknown_provider_is_error() {
        assert_eq!(
            err_message(oauth_flow_for("nope", OAuthFlowMode::Login, None)),
            "unknown OAuth provider: nope"
        );
    }

    #[test]
    fn radius_is_treated_as_unknown_provider() {
        // Radius needs a gateway and has no flip test; it is not resolvable here.
        assert_eq!(
            err_message(oauth_flow_for("radius", OAuthFlowMode::Login, None)),
            "unknown OAuth provider: radius"
        );
    }

    #[test]
    fn refresh_without_credential_is_error() {
        assert!(oauth_flow_for("anthropic", OAuthFlowMode::Refresh, None).is_err());
    }

    #[test]
    fn refresh_with_malformed_json_is_error() {
        assert!(oauth_flow_for("anthropic", OAuthFlowMode::Refresh, Some("{not json")).is_err());
    }
}
