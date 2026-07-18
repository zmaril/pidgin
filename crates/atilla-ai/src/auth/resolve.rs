//! Auth resolution, ported from pi-ai's `packages/ai/src/auth/resolve.ts` at
//! pinned commit `3da591ab`.
//!
//! This is the shared auth-resolution path: a stored credential owns its
//! provider (ambient/env is consulted only when nothing is stored), and there is
//! no silent env fallback after a failed refresh or for a credential type
//! without a matching handler (`resolve.ts:31-69`).
//!
//! The centerpiece is [`resolve_stored_oauth`], the double-checked-locking token
//! refresh (`resolve.ts:84-118`): a valid token costs zero locks; an expired one
//! locks, re-checks expiry under the lock, refreshes once globally, and persists
//! the rotated credential before release.

use super::credential_store::{CredentialStore, ModifyError};
use super::error::ModelsError;
use super::types::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthProvider, AuthResolutionOverrides, AuthResult,
    Credential, OAuthAuth, OAuthCredential, OAuthFlow, ProviderEnv,
};

/// Auth resolution shared by the model collections (`resolve.ts:37-69`).
///
/// A stored credential owns the provider; ambient/env is consulted only when
/// nothing is stored. `Ok(None)` means the provider is not configured.
pub fn resolve_provider_auth(
    provider: &AuthProvider,
    credentials: &dyn CredentialStore,
    auth_context: &dyn AuthContext,
    flow: &OAuthFlow,
    overrides: Option<&AuthResolutionOverrides>,
) -> Result<Option<AuthResult>, ModelsError> {
    let override_env = overrides.and_then(|o| o.env.as_ref());

    // A request-scoped context that overlays `overrides.env` onto the base.
    let overlay;
    let request_ctx: &dyn AuthContext = match override_env {
        Some(env) => {
            overlay = OverlayAuthContext {
                base: auth_context,
                env: env.clone(),
            };
            &overlay
        }
        None => auth_context,
    };

    // Explicit api-key override wins when the provider supports api-key auth.
    if let Some(overrides) = overrides {
        if let Some(api_key_override) = &overrides.api_key {
            if let Some(api_key) = &provider.auth.api_key {
                let credential = ApiKeyCredential {
                    key: Some(api_key_override.clone()),
                    env: overrides.env.clone(),
                };
                return resolve_api_key(
                    request_ctx,
                    api_key.as_ref(),
                    &provider.id,
                    Some(credential),
                );
            }
        }
    }

    let stored = read_credential(credentials, &provider.id)?;
    if let Some(stored) = stored {
        match stored {
            Credential::OAuth(oauth_credential) => {
                if let Some(oauth) = &provider.auth.oauth {
                    return resolve_stored_oauth(
                        credentials,
                        &provider.id,
                        oauth.as_ref(),
                        oauth_credential,
                        flow,
                    );
                }
                return Ok(None);
            }
            Credential::ApiKey(api_key_credential) => {
                if let Some(api_key) = &provider.auth.api_key {
                    // Overlay `overrides.env` onto the stored credential's env.
                    let credential = match override_env {
                        Some(env) => {
                            let mut merged = api_key_credential.env.clone().unwrap_or_default();
                            for (k, v) in env {
                                merged.insert(k.clone(), v.clone());
                            }
                            ApiKeyCredential {
                                key: api_key_credential.key.clone(),
                                env: Some(merged),
                            }
                        }
                        None => api_key_credential,
                    };
                    return resolve_api_key(
                        request_ctx,
                        api_key.as_ref(),
                        &provider.id,
                        Some(credential),
                    );
                }
                return Ok(None);
            }
        }
    }

    // Ambient (env vars, AWS profiles, ADC files).
    match &provider.auth.api_key {
        Some(api_key) => resolve_api_key(request_ctx, api_key.as_ref(), &provider.id, None),
        None => Ok(None),
    }
}

/// OAuth resolution with double-checked locking (`resolve.ts:84-118`).
///
/// Valid tokens cost zero locks; expired tokens lock, re-check expiry under the
/// lock, refresh once globally, and persist the rotated credential before
/// release. Refresh failures surface as [`ModelsError::oauth`]; a store failure
/// surfaces as [`ModelsError::auth`].
pub fn resolve_stored_oauth(
    credentials: &dyn CredentialStore,
    provider_id: &str,
    oauth: &dyn OAuthAuth,
    stored: OAuthCredential,
    flow: &OAuthFlow,
) -> Result<Option<AuthResult>, ModelsError> {
    let mut credential = stored;

    if flow.clock.now_ms() >= credential.expires {
        // Optimistic check said expired; the authoritative check runs under the lock.
        let mut modify_fn =
            |current: Option<Credential>| -> Result<Option<Credential>, ModelsError> {
                let current = match current {
                    Some(Credential::OAuth(current)) => current,
                    _ => return Ok(None), // logged out meanwhile
                };
                if flow.clock.now_ms() < current.expires {
                    return Ok(None); // another process/request refreshed
                }
                match oauth.refresh(&current, flow) {
                    Ok(refreshed) => Ok(Some(Credential::OAuth(refreshed))),
                    Err(error) => Err(ModelsError::oauth(format!(
                        "OAuth refresh failed for {provider_id}"
                    ))
                    .with_cause(error.to_string())),
                }
            };

        let post = match credentials.modify(provider_id, &mut modify_fn) {
            Ok(post) => post,
            Err(ModifyError::Callback(error)) => return Err(error),
            Err(ModifyError::Store(error)) => {
                return Err(ModelsError::auth(format!(
                    "Credential store modify failed for {provider_id}"
                ))
                .with_cause(error.message))
            }
        };

        match post {
            Some(Credential::OAuth(refreshed)) => credential = refreshed,
            _ => return Ok(None), // logged out meanwhile
        }
    }

    match oauth.to_auth(&credential) {
        Ok(auth) => Ok(Some(AuthResult {
            auth,
            env: None,
            source: Some("OAuth".into()),
        })),
        Err(error) => Err(ModelsError::oauth(format!(
            "OAuth auth derivation failed for {provider_id}"
        ))
        .with_cause(error.to_string())),
    }
}

fn resolve_api_key(
    auth_context: &dyn AuthContext,
    api_key: &dyn ApiKeyAuth,
    provider_id: &str,
    credential: Option<ApiKeyCredential>,
) -> Result<Option<AuthResult>, ModelsError> {
    api_key
        .resolve(auth_context, credential.as_ref())
        .map_err(|error| {
            ModelsError::auth(format!("API key auth failed for provider {provider_id}"))
                .with_cause(error.to_string())
        })
}

fn read_credential(
    credentials: &dyn CredentialStore,
    provider_id: &str,
) -> Result<Option<Credential>, ModelsError> {
    credentials.read(provider_id).map_err(|error| {
        ModelsError::auth(format!("Credential store read failed for {provider_id}"))
            .with_cause(error.message)
    })
}

/// An [`AuthContext`] that overlays a [`ProviderEnv`] onto a base context
/// (`resolve.ts:71-76`): `env[name] || base.env(name)`, where an empty overlay
/// value falls through (JS `||` semantics).
struct OverlayAuthContext<'a> {
    base: &'a dyn AuthContext,
    env: ProviderEnv,
}

impl AuthContext for OverlayAuthContext<'_> {
    fn env(&self, name: &str) -> Option<String> {
        self.env
            .get(name)
            .filter(|value| !value.is_empty())
            .cloned()
            .or_else(|| self.base.env(name))
    }

    fn file_exists(&self, path: &str) -> bool {
        self.base.file_exists(path)
    }
}

#[cfg(test)]
mod tests;
