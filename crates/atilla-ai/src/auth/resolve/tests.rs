// straitjacket-allow-file[:duplication] — each `#[test]` rebuilds the same
// scaffold (a `ScriptedTransport` + `FakeClock` + `OAuthFlow` bundle and a fake
// `OAuthAuth`) by design, so the double-checked-locking paths are each exercised
// in isolation. The clone detector reads the repeated setup as duplication; it is
// deliberate, load-bearing per-case fixtures.
//! Unit tests for auth resolution, exercising the double-checked-locking OAuth
//! refresh (`resolve.ts:84-118`) with a [`FakeClock`], an
//! [`InMemoryCredentialStore`], and a fake [`OAuthAuth`].

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Map;

use super::*;
use crate::auth::context::DefaultAuthContext;
use crate::auth::credential_store::InMemoryCredentialStore;
use crate::auth::error::{AuthFlowError, ModelsErrorCode};
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::{AuthInteraction, ModelAuth, OAuthCredential, ProviderAuth};
use crate::seams::clock::FakeClock;
use crate::seams::http::ScriptedTransport;
use crate::seams::storage::MemoryEnv;

const START_MS: i64 = 1_700_000_000_000;

/// A configurable fake OAuth flow that records refresh/to_auth calls.
struct FakeOAuth {
    refresh_calls: Arc<AtomicUsize>,
    to_auth_calls: Arc<AtomicUsize>,
    outcome: Mutex<RefreshOutcome>,
}

#[derive(Clone)]
enum RefreshOutcome {
    /// Refresh succeeds, rotating to `access` with the given absolute `expires`.
    Ok { access: String, expires: i64 },
    /// Refresh fails with this message.
    Err(String),
}

impl FakeOAuth {
    fn new(outcome: RefreshOutcome) -> Self {
        Self {
            refresh_calls: Arc::new(AtomicUsize::new(0)),
            to_auth_calls: Arc::new(AtomicUsize::new(0)),
            outcome: Mutex::new(outcome),
        }
    }
}

impl OAuthAuth for FakeOAuth {
    fn name(&self) -> &str {
        "Fake OAuth"
    }

    fn login(
        &self,
        _interaction: &dyn AuthInteraction,
        _flow: &OAuthFlow,
    ) -> Result<OAuthCredential, AuthFlowError> {
        unreachable!("login is not exercised by resolution tests")
    }

    fn refresh(
        &self,
        _credential: &OAuthCredential,
        _flow: &OAuthFlow,
    ) -> Result<OAuthCredential, AuthFlowError> {
        self.refresh_calls.fetch_add(1, Ordering::SeqCst);
        match self.outcome.lock().unwrap().clone() {
            RefreshOutcome::Ok { access, expires } => Ok(OAuthCredential {
                refresh: "rotated-refresh".into(),
                access,
                expires,
                extra: Map::new(),
            }),
            RefreshOutcome::Err(message) => Err(AuthFlowError::new(message)),
        }
    }

    fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError> {
        self.to_auth_calls.fetch_add(1, Ordering::SeqCst);
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

fn oauth_credential(access: &str, expires: i64) -> OAuthCredential {
    OAuthCredential {
        refresh: "refresh".into(),
        access: access.into(),
        expires,
        extra: Map::new(),
    }
}

fn store_with_oauth(access: &str, expires: i64) -> InMemoryCredentialStore {
    let store = InMemoryCredentialStore::new();
    let mut set = |_current: Option<Credential>| {
        Ok(Some(Credential::OAuth(oauth_credential(access, expires))))
    };
    store.modify("provider", &mut set).unwrap();
    store
}

#[test]
fn valid_token_costs_zero_refresh() {
    let store = store_with_oauth("live-token", START_MS + 60_000);
    let oauth = FakeOAuth::new(RefreshOutcome::Ok {
        access: "should-not-be-used".into(),
        expires: START_MS + 999_999,
    });
    let transport = ScriptedTransport::new();
    let clock = FakeClock::new(START_MS);
    let flow = OAuthFlow {
        http: &transport,
        clock: &clock,
        timers: &clock,
        signal: None,
    };

    let result = resolve_stored_oauth(
        &store,
        "provider",
        &oauth,
        oauth_credential("live-token", START_MS + 60_000),
        &flow,
    )
    .unwrap()
    .unwrap();

    assert_eq!(oauth.refresh_calls.load(Ordering::SeqCst), 0);
    assert_eq!(result.auth.api_key.as_deref(), Some("live-token"));
    assert_eq!(result.source.as_deref(), Some("OAuth"));
}

#[test]
fn expired_token_refreshes_once_and_persists() {
    let store = store_with_oauth("stale-token", START_MS - 1);
    let oauth = FakeOAuth::new(RefreshOutcome::Ok {
        access: "fresh-token".into(),
        expires: START_MS + 3_600_000,
    });
    let transport = ScriptedTransport::new();
    let clock = FakeClock::new(START_MS);
    let flow = OAuthFlow {
        http: &transport,
        clock: &clock,
        timers: &clock,
        signal: None,
    };

    let result = resolve_stored_oauth(
        &store,
        "provider",
        &oauth,
        oauth_credential("stale-token", START_MS - 1),
        &flow,
    )
    .unwrap()
    .unwrap();

    assert_eq!(oauth.refresh_calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.auth.api_key.as_deref(), Some("fresh-token"));

    // The rotated credential is persisted.
    match store.read("provider").unwrap().unwrap() {
        Credential::OAuth(c) => {
            assert_eq!(c.access, "fresh-token");
            assert_eq!(c.refresh, "rotated-refresh");
            assert_eq!(c.expires, START_MS + 3_600_000);
        }
        _ => panic!("expected oauth"),
    }
}

#[test]
fn double_check_skips_refresh_when_store_already_refreshed() {
    // The store holds an already-fresh credential (another request refreshed it
    // between our optimistic read and taking the lock); the stale `stored` we
    // pass in still looks expired. The under-lock re-check must skip refresh.
    let store = store_with_oauth("already-fresh", START_MS + 3_600_000);
    let oauth = FakeOAuth::new(RefreshOutcome::Ok {
        access: "should-not-refresh".into(),
        expires: START_MS + 999_999_999,
    });
    let transport = ScriptedTransport::new();
    let clock = FakeClock::new(START_MS);
    let flow = OAuthFlow {
        http: &transport,
        clock: &clock,
        timers: &clock,
        signal: None,
    };

    let result = resolve_stored_oauth(
        &store,
        "provider",
        &oauth,
        // Stale view: expired.
        oauth_credential("stale-view", START_MS - 1),
        &flow,
    )
    .unwrap()
    .unwrap();

    assert_eq!(oauth.refresh_calls.load(Ordering::SeqCst), 0);
    // to_auth runs against the store's already-fresh credential.
    assert_eq!(result.auth.api_key.as_deref(), Some("already-fresh"));
}

#[test]
fn refresh_failure_surfaces_as_oauth_error() {
    let store = store_with_oauth("stale", START_MS - 1);
    let oauth = FakeOAuth::new(RefreshOutcome::Err("invalid_grant".into()));
    let transport = ScriptedTransport::new();
    let clock = FakeClock::new(START_MS);
    let flow = OAuthFlow {
        http: &transport,
        clock: &clock,
        timers: &clock,
        signal: None,
    };

    let err = resolve_stored_oauth(
        &store,
        "provider",
        &oauth,
        oauth_credential("stale", START_MS - 1),
        &flow,
    )
    .unwrap_err();

    assert_eq!(err.code, ModelsErrorCode::Oauth);
    assert_eq!(err.message, "OAuth refresh failed for provider");
    assert_eq!(err.cause.as_deref(), Some("invalid_grant"));
    // Nothing rotated in the store.
    match store.read("provider").unwrap().unwrap() {
        Credential::OAuth(c) => assert_eq!(c.access, "stale"),
        _ => panic!("expected oauth"),
    }
}

#[test]
fn logged_out_under_lock_returns_none() {
    // Store empty (logged out) but the stale view still looks expired.
    let store = InMemoryCredentialStore::new();
    let oauth = FakeOAuth::new(RefreshOutcome::Ok {
        access: "x".into(),
        expires: START_MS + 1,
    });
    let transport = ScriptedTransport::new();
    let clock = FakeClock::new(START_MS);
    let flow = OAuthFlow {
        http: &transport,
        clock: &clock,
        timers: &clock,
        signal: None,
    };

    let result = resolve_stored_oauth(
        &store,
        "provider",
        &oauth,
        oauth_credential("stale", START_MS - 1),
        &flow,
    )
    .unwrap();

    assert!(result.is_none());
    assert_eq!(oauth.refresh_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn resolve_provider_auth_routes_oauth_and_refreshes_on_clock_advance() {
    let store = store_with_oauth("t0", START_MS + 10_000);
    let provider = AuthProvider {
        id: "provider".into(),
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(Box::new(FakeOAuth::new(RefreshOutcome::Ok {
                access: "t1".into(),
                expires: START_MS + 3_600_000,
            }))),
        },
    };
    let transport = ScriptedTransport::new();
    let clock = FakeClock::new(START_MS);
    let auth_ctx = DefaultAuthContext::new(MemoryEnv::new());
    let flow = OAuthFlow {
        http: &transport,
        clock: &clock,
        timers: &clock,
        signal: None,
    };

    // Before expiry: live token, no refresh.
    let result = resolve_provider_auth(&provider, &store, &auth_ctx, &flow, None)
        .unwrap()
        .unwrap();
    assert_eq!(result.auth.api_key.as_deref(), Some("t0"));

    // Advance past expiry: refresh runs, new token resolved.
    clock.set_now_ms(START_MS + 20_000);
    let result = resolve_provider_auth(&provider, &store, &auth_ctx, &flow, None)
        .unwrap()
        .unwrap();
    assert_eq!(result.auth.api_key.as_deref(), Some("t1"));
}

#[test]
fn resolve_provider_auth_ambient_env_when_nothing_stored() {
    let store = InMemoryCredentialStore::new();
    let provider = AuthProvider {
        id: "anthropic".into(),
        auth: ProviderAuth {
            api_key: Some(Box::new(env_api_key_auth(
                "Anthropic API key",
                &["ANTHROPIC_API_KEY"],
            ))),
            oauth: None,
        },
    };
    let transport = ScriptedTransport::new();
    let clock = FakeClock::new(START_MS);
    let auth_ctx =
        DefaultAuthContext::new(MemoryEnv::new().with_env("ANTHROPIC_API_KEY", "sk-ambient"));
    let flow = OAuthFlow {
        http: &transport,
        clock: &clock,
        timers: &clock,
        signal: None,
    };

    let result = resolve_provider_auth(&provider, &store, &auth_ctx, &flow, None)
        .unwrap()
        .unwrap();
    assert_eq!(result.auth.api_key.as_deref(), Some("sk-ambient"));
    assert_eq!(result.source.as_deref(), Some("ANTHROPIC_API_KEY"));
}

#[test]
fn resolve_provider_auth_api_key_override_wins() {
    let store = InMemoryCredentialStore::new();
    let provider = AuthProvider {
        id: "anthropic".into(),
        auth: ProviderAuth {
            api_key: Some(Box::new(env_api_key_auth("Key", &["ANTHROPIC_API_KEY"]))),
            oauth: None,
        },
    };
    let transport = ScriptedTransport::new();
    let clock = FakeClock::new(START_MS);
    let auth_ctx =
        DefaultAuthContext::new(MemoryEnv::new().with_env("ANTHROPIC_API_KEY", "sk-ambient"));
    let flow = OAuthFlow {
        http: &transport,
        clock: &clock,
        timers: &clock,
        signal: None,
    };
    let overrides = AuthResolutionOverrides {
        api_key: Some("sk-override".into()),
        env: None,
    };

    let result = resolve_provider_auth(&provider, &store, &auth_ctx, &flow, Some(&overrides))
        .unwrap()
        .unwrap();
    assert_eq!(result.auth.api_key.as_deref(), Some("sk-override"));
    assert_eq!(result.source.as_deref(), Some("stored credential"));
}

#[test]
fn resolve_provider_auth_none_for_unconfigured() {
    let store = InMemoryCredentialStore::new();
    let provider = AuthProvider {
        id: "anthropic".into(),
        auth: ProviderAuth {
            api_key: Some(Box::new(env_api_key_auth("Key", &["UNSET_ENV"]))),
            oauth: None,
        },
    };
    let transport = ScriptedTransport::new();
    let clock = FakeClock::new(START_MS);
    let auth_ctx = DefaultAuthContext::new(MemoryEnv::new());
    let flow = OAuthFlow {
        http: &transport,
        clock: &clock,
        timers: &clock,
        signal: None,
    };

    let result = resolve_provider_auth(&provider, &store, &auth_ctx, &flow, None).unwrap();
    assert!(result.is_none());
}
