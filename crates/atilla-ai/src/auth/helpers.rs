//! Auth helpers, ported from pi-ai's `packages/ai/src/auth/helpers.ts` at
//! pinned commit `3da591ab`.
//!
//! - [`env_api_key_auth`] — standard api-key auth: a stored key wins, otherwise
//!   the first set env var resolves (`helpers.ts:9-25`).
//! - [`LazyOAuth`] — wraps an [`OAuthAuth`] loaded on first use, so provider
//!   definitions can advertise OAuth without eagerly constructing the flow
//!   (`helpers.ts:34-47`). pi loads through a bundler-opaque dynamic import; the
//!   sync port loads through an injected closure memoized in a [`OnceLock`].

use std::sync::OnceLock;

use super::error::AuthFlowError;
use super::oauth::flow::OAuthFlowMachine;
use super::types::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthInteraction, AuthPrompt, AuthPromptKind,
    AuthResult, ModelAuth, OAuthAuth, OAuthCredential,
};

/// Standard api-key auth (`helpers.ts:9-25`).
///
/// `login` prompts for the key; `resolve` returns the stored key if present,
/// otherwise the first set env var among `env_vars`.
pub fn env_api_key_auth(name: impl Into<String>, env_vars: &[&str]) -> EnvApiKeyAuth {
    EnvApiKeyAuth {
        name: name.into(),
        env_vars: env_vars.iter().map(|s| s.to_string()).collect(),
    }
}

/// The [`ApiKeyAuth`] returned by [`env_api_key_auth`].
pub struct EnvApiKeyAuth {
    name: String,
    env_vars: Vec<String>,
}

impl ApiKeyAuth for EnvApiKeyAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Option<Result<ApiKeyCredential, AuthFlowError>> {
        // pi: prompt for a secret, then store `{ type: "api_key", key }`.
        let result = interaction
            .prompt(AuthPrompt {
                signal: None,
                kind: AuthPromptKind::Secret {
                    message: format!("Enter {}", self.name),
                    placeholder: None,
                },
            })
            .map(|key| ApiKeyCredential {
                key: Some(key),
                env: None,
            });
        Some(result)
    }

    fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, AuthFlowError> {
        // Stored credential key wins.
        if let Some(key) = credential.and_then(|c| c.key.as_ref()) {
            return Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some(key.clone()),
                    ..ModelAuth::default()
                },
                env: None,
                source: Some("stored credential".into()),
            }));
        }
        // Otherwise the first set env var resolves.
        for env_var in &self.env_vars {
            if let Some(value) = ctx.env(env_var) {
                return Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(value),
                        ..ModelAuth::default()
                    },
                    env: None,
                    source: Some(env_var.clone()),
                }));
            }
        }
        Ok(None)
    }
}

/// A lazily-loaded [`OAuthAuth`] (`helpers.ts:34-47`).
///
/// The wrapped implementation loads on the first `login`/`refresh`/`to_auth`
/// call and is memoized. `name`/`login_label` are known up front so the wrapper
/// can advertise the flow without loading it.
pub struct LazyOAuth {
    name: String,
    login_label: Option<String>,
    load: Box<dyn Fn() -> Box<dyn OAuthAuth> + Send + Sync>,
    cached: OnceLock<Box<dyn OAuthAuth>>,
}

impl LazyOAuth {
    /// Wrap `load` behind an [`OAuthAuth`] that reports `name`/`login_label`
    /// eagerly and loads the implementation on first use.
    pub fn new(
        name: impl Into<String>,
        login_label: Option<String>,
        load: impl Fn() -> Box<dyn OAuthAuth> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            login_label,
            load: Box::new(load),
            cached: OnceLock::new(),
        }
    }

    fn loaded(&self) -> &dyn OAuthAuth {
        self.cached.get_or_init(|| (self.load)()).as_ref()
    }
}

impl OAuthAuth for LazyOAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn login_label(&self) -> Option<&str> {
        self.login_label.as_deref()
    }

    fn login_machine(&self) -> Box<dyn OAuthFlowMachine> {
        self.loaded().login_machine()
    }

    fn refresh_machine(&self, credential: &OAuthCredential) -> Box<dyn OAuthFlowMachine> {
        self.loaded().refresh_machine(credential)
    }

    fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError> {
        self.loaded().to_auth(credential)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::context::DefaultAuthContext;
    use crate::seams::storage::MemoryEnv;

    #[test]
    fn resolve_prefers_stored_key_over_env() {
        let auth = env_api_key_auth("Anthropic API key", &["ANTHROPIC_API_KEY"]);
        let env = MemoryEnv::new().with_env("ANTHROPIC_API_KEY", "env-key");
        let ctx = DefaultAuthContext::new(env);
        let credential = ApiKeyCredential {
            key: Some("stored-key".into()),
            env: None,
        };
        let result = auth.resolve(&ctx, Some(&credential)).unwrap().unwrap();
        assert_eq!(result.auth.api_key.as_deref(), Some("stored-key"));
        assert_eq!(result.source.as_deref(), Some("stored credential"));
    }

    #[test]
    fn resolve_falls_back_to_first_set_env_var() {
        let auth = env_api_key_auth("Key", &["FIRST", "SECOND"]);
        let env = MemoryEnv::new().with_env("SECOND", "second-value");
        let ctx = DefaultAuthContext::new(env);
        let result = auth.resolve(&ctx, None).unwrap().unwrap();
        assert_eq!(result.auth.api_key.as_deref(), Some("second-value"));
        assert_eq!(result.source.as_deref(), Some("SECOND"));
    }

    #[test]
    fn resolve_none_when_unconfigured() {
        let auth = env_api_key_auth("Key", &["UNSET"]);
        let ctx = DefaultAuthContext::new(MemoryEnv::new());
        assert!(auth.resolve(&ctx, None).unwrap().is_none());
    }
}
