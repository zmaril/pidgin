// straitjacket-allow-file[:duplication] — the five OAuth provider stubs share
// one faithful `OAuthAuth` impl skeleton (name/login_label/login/refresh/to_auth
// with `todo!()` bodies pending the provider workers) plus parallel constant
// blocks. The clone detector reads these mirrored skeletons across the provider
// files as duplicates; the repetition is the intended per-provider layout.
//! GitHub Copilot OAuth device-code flow — STUB.
//!
//! Ported skeleton of pi-ai's `packages/ai/src/auth/oauth/github-copilot.ts` at
//! pinned commit `3da591ab`. Public constants and the [`OAuthAuth`] surface are
//! laid down here; the device-code login/refresh + proxy-endpoint derivation are
//! ported by the GitHub Copilot provider worker.
//!
//! # TODO(port dep)
//!
//! `refresh`/`login` also need `GITHUB_COPILOT_MODELS` (from
//! `providers/github-copilot.models.ts`, not yet ported) to enable models after
//! login; the provider worker wires that once the providers module lands.

use crate::auth::error::AuthFlowError;
use crate::auth::types::{ModelAuth, OAuthAuth, OAuthCredential};

use super::flow::{OAuthFlowMachine, Step, StepInput};

/// OAuth client id (pi decodes this from base64; `github-copilot.ts:9-10`).
pub const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
/// Copilot API version header value (`github-copilot.ts:18`).
pub const COPILOT_API_VERSION: &str = "2026-06-01";
/// Default (non-enterprise) Copilot API base URL (`github-copilot.ts:85`).
pub const DEFAULT_BASE_URL: &str = "https://api.individual.githubcopilot.com";
/// Refresh skew, in ms: `expires_at*1000 - 5min` (`github-copilot.ts:274`).
pub const REFRESH_SKEW_MS: i64 = 5 * 60 * 1000;

/// Fixed Copilot request headers (`github-copilot.ts:12-17`).
pub const COPILOT_USER_AGENT: &str = "GitHubCopilotChat/0.35.0";
/// Copilot editor-version header (`github-copilot.ts:14`).
pub const COPILOT_EDITOR_VERSION: &str = "vscode/1.107.0";
/// Copilot editor-plugin-version header (`github-copilot.ts:15`).
pub const COPILOT_EDITOR_PLUGIN_VERSION: &str = "copilot-chat/0.35.0";
/// Copilot integration-id header (`github-copilot.ts:16`).
pub const COPILOT_INTEGRATION_ID: &str = "vscode-chat";

/// GitHub Copilot OAuth flow handler (`github-copilot.ts:367-379`).
#[derive(Debug, Default, Clone)]
pub struct GitHubCopilotOAuth;

impl GitHubCopilotOAuth {
    /// Construct the handler.
    pub fn new() -> Self {
        Self
    }
}

impl OAuthAuth for GitHubCopilotOAuth {
    fn name(&self) -> &str {
        "GitHub Copilot"
    }

    // TODO(port): body pending — provider worker (enterprise-domain prompt +
    // device-code flow + model enablement; `github-copilot.ts:329-359`).
    fn login_machine(&self) -> Box<dyn OAuthFlowMachine> {
        Box::new(GitHubCopilotStubMachine)
    }

    // TODO(port): body pending — provider worker (copilot_internal token +
    // available-model-id fetch; `github-copilot.ts:244-288`).
    fn refresh_machine(&self, _credential: &OAuthCredential) -> Box<dyn OAuthFlowMachine> {
        Box::new(GitHubCopilotStubMachine)
    }

    // TODO(port): body pending — provider worker. `toAuth` returns
    // `{ apiKey: access, baseUrl: getGitHubCopilotBaseUrl(access, enterprise) }`
    // (`github-copilot.ts:373-377`); the proxy-endpoint derivation from the token
    // (`proxy-ep=...` -> `api.*`) is ported with the login/refresh bodies.
    fn to_auth(&self, _credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError> {
        todo!("GitHub Copilot to_auth (base-URL derivation) — provider worker")
    }
}

/// Stub flow machine — the device-code login/refresh state machines are ported
/// by the GitHub Copilot provider worker.
struct GitHubCopilotStubMachine;

impl OAuthFlowMachine for GitHubCopilotStubMachine {
    fn start(&mut self, _now_ms: i64) -> Step {
        todo!("GitHub Copilot OAuth flow machine — provider worker")
    }
    fn advance(&mut self, _input: StepInput, _now_ms: i64) -> Step {
        todo!("GitHub Copilot OAuth flow machine — provider worker")
    }
}
