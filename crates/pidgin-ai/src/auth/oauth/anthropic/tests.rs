// straitjacket-allow-file[:duplication] — these tests mirror pi's
// `anthropic-oauth.test.ts` case-by-case: each `#[test]` rebuilds a similar
// machine-driving scaffold (start → notify → prompt → request → response) so
// the login/refresh paths are exercised in isolation. The clone detector reads
// the repeated scaffolding as duplication; it is deliberate, load-bearing
// per-case fixtures kept parallel to the pi test they transcribe.
//! Unit tests for the Anthropic OAuth flow, mirroring pi-ai's
//! `packages/ai/test/anthropic-oauth.test.ts` at pinned commit `3da591ab`
//! byte-faithfully, plus branch coverage for [`parse_authorization_input`].

use serde_json::{json, Value};

use super::{
    parse_authorization_input, AnthropicLoginMachine, AnthropicOAuth, AnthropicRefreshMachine,
    ParsedAuthInput, REDIRECT_URI, REFRESH_SKEW_MS, TOKEN_URL,
};
use crate::auth::error::AuthFlowError;
use crate::auth::oauth::flow::{run_login, OAuthFlowMachine, Step, StepInput};
use crate::auth::types::{AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, OAuthCredential};
use crate::seams::clock::FakeClock;
use crate::seams::http::{HttpResponse, ScriptedTransport};

/// Bytes 0..32 — the deterministic PKCE seed (see `pkce::tests`). The resulting
/// verifier (which doubles as the OAuth `state`) is stable across runs.
const PKCE_SEED: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31,
];
const EXPECTED_VERIFIER: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
const NOW_MS: i64 = 1_700_000_000_000;

/// Extract a query param from a URL, percent-decoding the value — the test-side
/// analog of pi's `new URL(authUrl).searchParams.get(key)`.
fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?').map(|(_, q)| q)?;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(percent_decode(v));
            }
        }
    }
    None
}

fn percent_decode(value: &str) -> String {
    let bytes = value.replace('+', " ").into_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                b'A'..=b'F' => Some(b - b'A' + 10),
                _ => None,
            };
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn json_body(request_body: &Option<String>) -> Value {
    serde_json::from_str(request_body.as_deref().expect("request has a body")).unwrap()
}

/// pi `anthropic-oauth.test.ts:39-73`: manual-callback login keeps the localhost
/// `redirect_uri` and exchanges the pasted code.
#[test]
fn manual_callback_login_keeps_redirect_uri_and_exchanges_code() {
    let mut machine = AnthropicLoginMachine::with_pkce_bytes(PKCE_SEED);

    // start → Notify(auth_url).
    let (url, instructions) = match machine.start(NOW_MS) {
        Step::Notify {
            event: AuthEvent::AuthUrl { url, instructions },
        } => (url, instructions),
        other => panic!("expected auth_url notify, got {other:?}"),
    };
    assert!(instructions.is_some());
    // The state carried in the URL is the deterministic verifier.
    let state = query_param(&url, "state").expect("state param");
    let redirect_uri = query_param(&url, "redirect_uri").expect("redirect_uri param");
    assert_eq!(state, EXPECTED_VERIFIER);
    assert_eq!(redirect_uri, REDIRECT_URI);
    assert_eq!(
        query_param(&url, "code_challenge_method").as_deref(),
        Some("S256")
    );

    // Ack → Prompt(manual_code).
    match machine.advance(StepInput::Ack, NOW_MS) {
        Step::Prompt { prompt } => {
            assert!(matches!(prompt.kind, AuthPromptKind::ManualCode { .. }));
        }
        other => panic!("expected manual_code prompt, got {other:?}"),
    }

    // Paste the callback URL exactly as the pi test does.
    let pasted = format!("{redirect_uri}?code=manual-code&state={state}");
    let request = match machine.advance(StepInput::Input { value: pasted }, NOW_MS) {
        Step::Request { request } => request,
        other => panic!("expected token request, got {other:?}"),
    };
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, TOKEN_URL);
    let body = json_body(&request.body);
    assert_eq!(body["grant_type"], "authorization_code");
    assert_eq!(body["code"], "manual-code");
    assert_eq!(body["redirect_uri"], "http://localhost:53692/callback");
    assert_eq!(body["code_verifier"], EXPECTED_VERIFIER);

    // Feed the token response.
    let response = HttpResponse::ok(
        json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "expires_in": 3600,
        })
        .to_string(),
    );
    let credential = match machine.advance(StepInput::Response(response), NOW_MS) {
        Step::Done { credential } => credential,
        other => panic!("expected done, got {other:?}"),
    };
    assert_eq!(credential.access, "access-token");
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(credential.expires, NOW_MS + 3600 * 1000 - REFRESH_SKEW_MS);
}

/// pi `anthropic-oauth.test.ts:75-102`: the refresh request omits `scope`.
#[test]
fn refresh_omits_scope_and_rotates_tokens() {
    let mut machine = AnthropicRefreshMachine::new("refresh-token");

    let request = match machine.start(NOW_MS) {
        Step::Request { request } => request,
        other => panic!("expected refresh request, got {other:?}"),
    };
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, TOKEN_URL);
    let body = json_body(&request.body);
    assert_eq!(body["grant_type"], "refresh_token");
    assert!(body["client_id"].as_str().is_some_and(|c| !c.is_empty()));
    assert_eq!(body["refresh_token"], "refresh-token");
    // The distinguishing assertion: no `scope` key on refresh.
    assert!(body.get("scope").is_none());

    let response = HttpResponse::ok(
        json!({
            "access_token": "new-access-token",
            "refresh_token": "new-refresh-token",
            "expires_in": 3600,
        })
        .to_string(),
    );
    let credential = match machine.advance(StepInput::Response(response), NOW_MS) {
        Step::Done { credential } => credential,
        other => panic!("expected done, got {other:?}"),
    };
    assert_eq!(credential.access, "new-access-token");
    assert_eq!(credential.refresh, "new-refresh-token");
    assert_eq!(credential.expires, NOW_MS + 3600 * 1000 - REFRESH_SKEW_MS);
}

/// An [`AuthInteraction`] that records notifies and answers the `manual_code`
/// prompt with a callback URL built from the recorded `auth_url` — mirroring the
/// pi test's inline interaction (`anthropic-oauth.test.ts:56-68`).
struct RecordingInteraction {
    events: std::sync::Mutex<Vec<AuthEvent>>,
    auth_url: std::sync::Mutex<Option<String>>,
}

impl RecordingInteraction {
    fn new() -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
            auth_url: std::sync::Mutex::new(None),
        }
    }
}

impl AuthInteraction for RecordingInteraction {
    fn prompt(&self, prompt: AuthPrompt) -> Result<String, AuthFlowError> {
        match prompt.kind {
            AuthPromptKind::ManualCode { .. } => {
                let url = self
                    .auth_url
                    .lock()
                    .unwrap()
                    .clone()
                    .expect("auth_url first");
                let state = query_param(&url, "state").expect("state");
                let redirect = query_param(&url, "redirect_uri").expect("redirect_uri");
                Ok(format!("{redirect}?code=manual-code&state={state}"))
            }
            other => Err(AuthFlowError::new(format!("unexpected prompt: {other:?}"))),
        }
    }

    fn notify(&self, event: AuthEvent) {
        if let AuthEvent::AuthUrl { url, .. } = &event {
            *self.auth_url.lock().unwrap() = Some(url.clone());
        }
        self.events.lock().unwrap().push(event);
    }
}

/// The native [`run_flow`] driver reaches `Done` end-to-end over
/// [`ScriptedTransport`] + [`FakeClock`] + a fake interaction (no deterministic
/// PKCE needed — the interaction reads the state back out of the auth URL).
#[test]
fn run_login_driver_reaches_done_end_to_end() {
    let auth = AnthropicOAuth::new();
    let transport = ScriptedTransport::new();
    transport.push_ok(
        json!({
            "access_token": "access",
            "refresh_token": "refresh",
            "expires_in": 3600,
        })
        .to_string(),
    );
    let clock = FakeClock::new(NOW_MS);
    let interaction = RecordingInteraction::new();

    let credential = run_login(&auth, &transport, &clock, &clock, &interaction, None).unwrap();

    assert_eq!(credential.access, "access");
    assert_eq!(credential.refresh, "refresh");
    assert_eq!(credential.expires, NOW_MS + 3600 * 1000 - REFRESH_SKEW_MS);

    // The auth_url event was surfaced, and exactly one token request went out.
    assert!(interaction
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, AuthEvent::AuthUrl { .. })));
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, TOKEN_URL);
    let body = json_body(&requests[0].body);
    assert_eq!(body["grant_type"], "authorization_code");
    assert_eq!(body["code"], "manual-code");
}

/// A pasted state that does not match the verifier aborts with pi's exact
/// message (`anthropic.ts:279,289`).
#[test]
fn login_rejects_state_mismatch() {
    let mut machine = AnthropicLoginMachine::with_pkce_bytes(PKCE_SEED);
    machine.start(NOW_MS);
    machine.advance(StepInput::Ack, NOW_MS);
    let pasted = format!("{REDIRECT_URI}?code=manual-code&state=not-the-verifier");
    match machine.advance(StepInput::Input { value: pasted }, NOW_MS) {
        Step::Error { message } => assert_eq!(message, "OAuth state mismatch"),
        other => panic!("expected state-mismatch error, got {other:?}"),
    }
}

/// A non-2xx token response surfaces an error carrying the status and body.
#[test]
fn login_surfaces_token_http_error() {
    let mut machine = AnthropicRefreshMachine::new("refresh-token");
    machine.start(NOW_MS);
    let response = HttpResponse {
        status: 400,
        headers: Default::default(),
        body: "{\"error\":\"invalid_grant\"}".to_string(),
    };
    match machine.advance(StepInput::Response(response), NOW_MS) {
        Step::Error { message } => {
            assert!(message.contains("status=400"), "message: {message}");
            assert!(message.contains("invalid_grant"), "message: {message}");
        }
        other => panic!("expected http error, got {other:?}"),
    }
}

/// An `Aborted` input mid-flow yields the "Login cancelled" error.
#[test]
fn login_aborts_with_cancel_message() {
    let mut machine = AnthropicLoginMachine::with_pkce_bytes(PKCE_SEED);
    machine.start(NOW_MS);
    match machine.advance(StepInput::Aborted, NOW_MS) {
        Step::Error { message } => assert_eq!(message, "Login cancelled"),
        other => panic!("expected cancel error, got {other:?}"),
    }
}

/// [`parse_authorization_input`] across all four pi branches
/// (`anthropic.ts:52-80`).
#[test]
fn parse_authorization_input_covers_all_branches() {
    // Branch 1: absolute URL with query params.
    assert_eq!(
        parse_authorization_input("http://localhost:53692/callback?code=abc&state=xyz"),
        ParsedAuthInput {
            code: Some("abc".into()),
            state: Some("xyz".into()),
        }
    );
    // Branch 1: URL with no query → both absent.
    assert_eq!(
        parse_authorization_input("http://localhost:53692/callback"),
        ParsedAuthInput::default()
    );
    // Branch 1: URL with only state.
    assert_eq!(
        parse_authorization_input("http://x/cb?state=s"),
        ParsedAuthInput {
            code: None,
            state: Some("s".into()),
        }
    );
    // Branch 1: percent-encoded value is decoded like URLSearchParams.
    assert_eq!(
        parse_authorization_input("http://x/cb?code=a%20b"),
        ParsedAuthInput {
            code: Some("a b".into()),
            state: None,
        }
    );
    // Branch 2: `code#state` fragment split.
    assert_eq!(
        parse_authorization_input("the-code#the-state"),
        ParsedAuthInput {
            code: Some("the-code".into()),
            state: Some("the-state".into()),
        }
    );
    // Branch 3: a bare `code=...&state=...` query string (no scheme, no `#`).
    assert_eq!(
        parse_authorization_input("code=c1&state=s1"),
        ParsedAuthInput {
            code: Some("c1".into()),
            state: Some("s1".into()),
        }
    );
    // Branch 4: the whole trimmed value is the code.
    assert_eq!(
        parse_authorization_input("  just-a-code  "),
        ParsedAuthInput {
            code: Some("just-a-code".into()),
            state: None,
        }
    );
    // Empty input → empty result.
    assert_eq!(parse_authorization_input("   "), ParsedAuthInput::default());
}

/// `to_auth` maps the access token to `apiKey` (`anthropic.ts:347-349`).
#[test]
fn to_auth_maps_access_to_api_key() {
    let auth = AnthropicOAuth::new();
    let credential = OAuthCredential {
        refresh: "r".into(),
        access: "the-access-token".into(),
        expires: 0,
        extra: Default::default(),
    };
    let model_auth = crate::auth::types::OAuthAuth::to_auth(&auth, &credential).unwrap();
    assert_eq!(model_auth.api_key.as_deref(), Some("the-access-token"));
}
