// straitjacket-allow-file:duplication - test resolver mirrors the ported ConfigValueResolver adapter surface
//! Tests for the provider-composer AUTH layer.
//!
//! Assertions are translated from pi's `model-runtime-auth-options.test.ts`
//! (the credential-aware composer behavior) and the `withConfiguredAuth` /
//! `composeModelProvider` throw sites in `provider-composer.ts`. Expected values
//! are derived from those pi tests, not fabricated.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Map;

use super::*;
use crate::auth::context::DefaultAuthContext;
use crate::auth::error::AuthFlowError;
use crate::auth::oauth::flow::OAuthFlowMachine;
use crate::auth::types::{
    AuthEvent, AuthInteraction, AuthPrompt, ModelAuth, OAuthAuth, OAuthCredential,
};
use crate::seams::storage::MemoryEnv;

/// A config-value resolver for tests: literals pass through, `$NAME` interpolates
/// from the resolved env map, `!command` is flagged but not executed. Mirrors the
/// subset of pi's `resolve-config-value.ts` the auth layer exercises.
struct TestResolver;

impl ConfigValueResolver for TestResolver {
    fn get_env_var_names(&self, value: &str) -> Vec<String> {
        match value.strip_prefix('$') {
            Some(name) if !name.starts_with('{') && !name.is_empty() => vec![name.to_string()],
            _ => Vec::new(),
        }
    }

    fn is_command(&self, value: &str) -> bool {
        value.starts_with('!')
    }

    fn resolve_or_throw(
        &self,
        value: &str,
        description: &str,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<String, ConfigValueError> {
        if let Some(name) = value.strip_prefix('$') {
            return env
                .and_then(|env| env.get(name))
                .cloned()
                .ok_or_else(|| ConfigValueError(format!("{description}: {name} is not set")));
        }
        Ok(value.to_string())
    }

    fn resolve_headers_or_throw(
        &self,
        headers: Option<&BTreeMap<String, String>>,
        description: &str,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<Option<BTreeMap<String, String>>, ConfigValueError> {
        let Some(headers) = headers else {
            return Ok(None);
        };
        let mut resolved = BTreeMap::new();
        for (key, value) in headers {
            resolved.insert(key.clone(), self.resolve_or_throw(value, description, env)?);
        }
        Ok(Some(resolved))
    }
}

fn resolver() -> Arc<dyn ConfigValueResolver> {
    Arc::new(TestResolver)
}

/// An [`AuthInteraction`] that answers every prompt with a fixed secret.
struct StubInteraction;

impl AuthInteraction for StubInteraction {
    fn prompt(&self, _prompt: AuthPrompt) -> Result<String, AuthFlowError> {
        Ok("typed-key".to_string())
    }

    fn notify(&self, _event: AuthEvent) {}
}

/// A minimal base OAuth handler whose `to_auth` yields a fixed api key. The flow
/// machines are unused by these tests.
struct StubOAuth;

impl OAuthAuth for StubOAuth {
    fn name(&self) -> &str {
        "Stub OAuth"
    }

    fn login_machine(&self) -> Box<dyn OAuthFlowMachine> {
        unimplemented!("login_machine is not exercised by the composer auth tests")
    }

    fn refresh_machine(&self, _credential: &OAuthCredential) -> Box<dyn OAuthFlowMachine> {
        unimplemented!("refresh_machine is not exercised by the composer auth tests")
    }

    fn to_auth(&self, _credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError> {
        Ok(ModelAuth {
            api_key: Some("oauth-key".to_string()),
            ..ModelAuth::default()
        })
    }
}

fn oauth_credential() -> OAuthCredential {
    OAuthCredential {
        refresh: "refresh".to_string(),
        access: "access".to_string(),
        expires: 0,
        extra: Map::new(),
    }
}

// provider-composer.ts:257-260 — authHeader injects `Authorization: Bearer <key>`
// and preserves configured headers.
#[test]
fn with_configured_auth_injects_bearer() {
    let auth = ModelAuth {
        api_key: Some("generated-key".to_string()),
        ..ModelAuth::default()
    };
    let mut headers = BTreeMap::new();
    headers.insert("x-provider".to_string(), "provider".to_string());
    let resolved = with_configured_auth(auth, Some(&headers), true).unwrap();
    let merged = resolved.headers.unwrap();
    assert_eq!(
        merged.get("Authorization"),
        Some(&Some("Bearer generated-key".to_string()))
    );
    assert_eq!(
        merged.get("x-provider"),
        Some(&Some("provider".to_string()))
    );
}

// provider-composer.ts:258 — authHeader with no resolved api key throws the
// verbatim message.
#[test]
fn with_configured_auth_throws_without_resolved_key() {
    let error = with_configured_auth(ModelAuth::default(), None, true).unwrap_err();
    assert_eq!(error.message, "authHeader requires a resolved API key");
}

// provider-composer.ts:250-256 — without authHeader and no configured headers the
// merged headers are absent.
#[test]
fn with_configured_auth_leaves_headers_absent() {
    let auth = ModelAuth {
        api_key: Some("k".to_string()),
        ..ModelAuth::default()
    };
    let resolved = with_configured_auth(auth, None, false).unwrap();
    assert!(resolved.headers.is_none());
}

// model-runtime-auth-options.test.ts:144-161 — "resolves configured auth from
// request-scoped environment overrides": composeApiKeyAuth resolves the `$ENV`
// api key and header from the auth context.
#[test]
fn compose_api_key_auth_resolves_key_and_headers_from_env() {
    let mut headers = BTreeMap::new();
    headers.insert(
        "x-request-value".to_string(),
        "$REQUEST_SCOPED_HEADER".to_string(),
    );
    let config = ProviderAuthConfig {
        api_key: Some("$REQUEST_SCOPED_API_KEY".to_string()),
        headers: Some(headers),
        auth_header: None,
    };
    let ctx = DefaultAuthContext::new(
        MemoryEnv::new()
            .with_env("REQUEST_SCOPED_API_KEY", "request-key")
            .with_env("REQUEST_SCOPED_HEADER", "request-header"),
    );

    let handler = compose_api_key_auth(
        "request-env-provider",
        None,
        false,
        Some(&config),
        None,
        resolver(),
    )
    .expect("api-key auth composes");

    let result = handler.resolve(&ctx, None).unwrap().unwrap();
    assert_eq!(result.auth.api_key.as_deref(), Some("request-key"));
    assert_eq!(result.source.as_deref(), Some("configured API key"));
    let resolved_headers = result.auth.headers.unwrap();
    assert_eq!(
        resolved_headers.get("x-request-value"),
        Some(&Some("request-header".to_string()))
    );
}

// model-runtime-auth-options.test.ts:190-227 — authHeader providers assemble
// `Authorization: Bearer <key>` alongside the provider headers.
#[test]
fn compose_api_key_auth_applies_auth_header() {
    let mut headers = BTreeMap::new();
    headers.insert("x-provider".to_string(), "provider".to_string());
    let config = ProviderAuthConfig {
        api_key: Some("generated-key".to_string()),
        headers: Some(headers),
        auth_header: Some(true),
    };
    let ctx = DefaultAuthContext::new(MemoryEnv::new());

    let handler = compose_api_key_auth(
        "auth-header-provider",
        None,
        false,
        Some(&config),
        None,
        resolver(),
    )
    .expect("api-key auth composes");
    let result = handler.resolve(&ctx, None).unwrap().unwrap();
    let resolved_headers = result.auth.headers.unwrap();
    assert_eq!(
        resolved_headers.get("Authorization"),
        Some(&Some("Bearer generated-key".to_string()))
    );
    assert_eq!(
        resolved_headers.get("x-provider"),
        Some(&Some("provider".to_string()))
    );
}

// model-runtime-auth-options.test.ts:125-143 — an extension API-key provider
// gets the default "API key" method name and a `login` function.
#[test]
fn compose_api_key_auth_default_name_and_login() {
    let extension = ExtensionAuthConfig {
        api_key: Some("$EXTENSION_TEST_API_KEY".to_string()),
        ..ExtensionAuthConfig::default()
    };
    let handler = compose_api_key_auth(
        "extension-api-key",
        None,
        false,
        None,
        Some(&extension),
        resolver(),
    )
    .expect("api-key auth composes");
    assert_eq!(handler.name(), "API key");
    let credential = handler.login(&StubInteraction).unwrap().unwrap();
    assert_eq!(credential.key.as_deref(), Some("typed-key"));
}

// model-runtime-auth-options.test.ts:229-... — an OAuth-only extension provider
// does not get a fabricated api-key method (composeApiKeyAuth returns None).
#[test]
fn compose_api_key_auth_none_for_oauth_only() {
    let extension = ExtensionAuthConfig {
        oauth: Some(ExtensionOAuthConfig {
            name: "Extension subscription".to_string(),
            uses_callback_server: None,
            login: None,
        }),
        ..ExtensionAuthConfig::default()
    };
    let handler = compose_api_key_auth(
        "extension-oauth",
        None,
        false,
        None,
        Some(&extension),
        resolver(),
    );
    assert!(handler.is_none());
}

// provider-composer.ts:365 — with no extension oauth and no base oauth,
// composeOAuthAuth yields nothing.
#[test]
fn compose_oauth_auth_none_without_source() {
    let handler = compose_oauth_auth("p", None, None, None, resolver());
    assert!(handler.is_none());
}

// provider-composer.ts:371-380 — the composed OAuth `toAuth` layers configured
// headers + authHeader over the base handler's derived auth.
#[test]
fn compose_oauth_auth_layers_headers_and_auth_header() {
    let mut headers = BTreeMap::new();
    headers.insert("x-provider".to_string(), "provider".to_string());
    let config = ProviderAuthConfig {
        api_key: None,
        headers: Some(headers),
        auth_header: Some(true),
    };
    let handler = compose_oauth_auth(
        "oauth-provider",
        Some(Box::new(StubOAuth)),
        Some(&config),
        None,
        resolver(),
    )
    .expect("oauth auth composes");

    let auth = handler.to_auth(&oauth_credential()).unwrap();
    let resolved_headers = auth.headers.unwrap();
    assert_eq!(
        resolved_headers.get("Authorization"),
        Some(&Some("Bearer oauth-key".to_string()))
    );
    assert_eq!(
        resolved_headers.get("x-provider"),
        Some(&Some("provider".to_string()))
    );
}

// provider-composer.ts:412-499 — composeModelProvider assembles the rich auth,
// resolved identity, and (empty) model list.
#[test]
fn compose_model_provider_assembles_auth_and_identity() {
    let config = ProviderAuthConfig {
        api_key: Some("sk-test".to_string()),
        headers: None,
        auth_header: None,
    };
    let provider = compose_model_provider(ComposeModelProviderInput {
        provider_id: "acme".to_string(),
        base: None,
        base_api_key: None,
        base_oauth: None,
        config: Some(config),
        extension: None,
        models: Vec::new(),
        name: "Acme".to_string(),
        base_url: Some("https://acme.test/v1".to_string()),
        headers: None,
        resolver: resolver(),
    })
    .expect("provider composes");

    assert_eq!(provider.id, "acme");
    assert_eq!(provider.name, "Acme");
    assert_eq!(provider.base_url.as_deref(), Some("https://acme.test/v1"));
    assert!(provider.auth.api_key.is_some());
    assert!(provider.auth.oauth.is_none());
    assert!(provider.get_models().is_empty());
}

// provider-composer.ts:443 — the "no authentication method configured" guard
// throws the verbatim message.
#[test]
fn require_auth_method_throws_when_no_method() {
    let error = match require_auth_method("anthropic", None, None) {
        Err(error) => error,
        Ok(_) => panic!("expected a no-auth-method error"),
    };
    assert_eq!(
        error.0,
        "Provider anthropic: no authentication method configured."
    );
}

// The guard passes through when either method is present.
#[test]
fn require_auth_method_accepts_api_key() {
    let config = ProviderAuthConfig {
        api_key: Some("sk-test".to_string()),
        headers: None,
        auth_header: None,
    };
    let api_key = compose_api_key_auth("p", None, false, Some(&config), None, resolver());
    assert!(require_auth_method("p", api_key, None).is_ok());
}
