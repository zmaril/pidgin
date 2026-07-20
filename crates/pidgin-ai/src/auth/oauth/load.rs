//! OAuth flow registry, ported from pi-ai's
//! `packages/ai/src/auth/oauth/load.ts` at pinned commit `3da591ab`.
//!
//! pi loads each flow through a bundler-opaque dynamic import so Node-only
//! callback-server code stays out of browser bundles (`load.ts:9-56`). The sync
//! Rust port has no bundler indirection, so this is a plain factory returning the
//! right [`OAuthAuth`] per provider.

use crate::auth::types::OAuthAuth;

use super::anthropic::AnthropicOAuth;
use super::github_copilot::GitHubCopilotOAuth;
use super::openai_codex::OpenAICodexOAuth;
use super::radius::RadiusOAuth;
use super::xai::XaiOAuth;

/// Load the Anthropic OAuth flow (`load.ts:29-32`).
pub fn load_anthropic_oauth() -> Box<dyn OAuthAuth> {
    Box::new(AnthropicOAuth::new())
}

/// Load the OpenAI Codex OAuth flow (`load.ts:34-37`).
pub fn load_openai_codex_oauth() -> Box<dyn OAuthAuth> {
    Box::new(OpenAICodexOAuth::new())
}

/// Load the GitHub Copilot OAuth flow (`load.ts:39-42`).
pub fn load_github_copilot_oauth() -> Box<dyn OAuthAuth> {
    Box::new(GitHubCopilotOAuth::new())
}

/// Load the xAI OAuth flow (`load.ts:44-47`).
pub fn load_xai_oauth() -> Box<dyn OAuthAuth> {
    Box::new(XaiOAuth::new())
}

/// Load a Radius OAuth flow for a named gateway (`load.ts:49-56`).
pub fn load_radius_oauth(
    name: impl Into<String>,
    gateway: impl Into<String>,
) -> Box<dyn OAuthAuth> {
    Box::new(RadiusOAuth::new(name, gateway))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_returns_named_flows() {
        assert_eq!(load_anthropic_oauth().name(), "Anthropic (Claude Pro/Max)");
        assert_eq!(
            load_openai_codex_oauth().name(),
            "OpenAI (ChatGPT Plus/Pro)"
        );
        assert_eq!(load_github_copilot_oauth().name(), "GitHub Copilot");
        assert_eq!(load_xai_oauth().name(), "xAI (Grok/X subscription)");
        assert_eq!(
            load_xai_oauth().login_label(),
            Some("Sign in with SuperGrok or X Premium")
        );
        assert_eq!(
            load_radius_oauth("Radius", "https://gw.example").name(),
            "Radius"
        );
    }
}
