// straitjacket-allow-file[:duplication] — these tests mirror pi's
// `xai-oauth.test.ts` case-by-case: each `#[test]` rebuilds a similar
// machine-driving scaffold (device request → notify → poll waits → response) so
// each device-flow / refresh path is exercised in isolation. The clone detector
// reads the repeated scaffolding and the shared form/JSON test helpers as
// duplication; it is deliberate, load-bearing per-case fixtures kept parallel to
// the pi test they transcribe.
//! Unit tests for the xAI OAuth device-code flow, mirroring pi-ai's
//! `packages/ai/test/xai-oauth.test.ts` at pinned commit `3da591ab`.

use std::collections::BTreeMap;

use serde_json::json;

use super::{
    XaiLoginMachine, XaiOAuth, XaiRefreshMachine, REFRESH_SKEW_MS, XAI_CLIENT_ID,
    XAI_DEVICE_CODE_URL, XAI_TOKEN_URL,
};
use crate::auth::error::AuthFlowError;
use crate::auth::oauth::flow::{run_login, OAuthFlowMachine, Step, StepInput};
use crate::auth::types::{AuthEvent, AuthInteraction, AuthPrompt, OAuthAuth, OAuthCredential};
use crate::seams::clock::{FakeClock, TimerId, Timers};
use crate::seams::http::{HttpRequest, HttpResponse, ScriptedTransport};
use crate::seams::provider::AbortSignal;

/// `2026-07-09T20:00:00Z` in ms — the pi test's pinned `startTime`.
const START_MS: i64 = 1_783_022_400_000;

// ---------------------------------------------------------------------------
// Test helpers.
// ---------------------------------------------------------------------------

/// Decode one form-urlencoded component: `+`→space, then percent-decode.
fn decode_component(value: &str) -> String {
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

/// Parse a request's form-encoded body, the test-side analog of pi's
/// `new URLSearchParams(String(init.body))`.
fn request_form(request: &HttpRequest) -> BTreeMap<String, String> {
    let body = request.body.as_deref().unwrap_or("");
    let mut map = BTreeMap::new();
    for pair in body.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(decode_component(key), decode_component(value));
    }
    map
}

/// A `200 OK` JSON response.
fn json_ok(body: serde_json::Value) -> HttpResponse {
    HttpResponse::ok(body.to_string())
}

/// A JSON response with an explicit status (device/token errors use 400).
fn json_status(body: serde_json::Value, status: u16) -> HttpResponse {
    HttpResponse {
        status,
        headers: Default::default(),
        body: body.to_string(),
    }
}

/// The pi test's `deviceCodeResponse` fixture (`xai-oauth.test.ts:23-32`).
fn device_code_response() -> serde_json::Value {
    json!({
        "device_code": "device-code",
        "user_code": "ABCD-1234",
        "verification_uri": "https://accounts.x.ai/oauth2/device",
        "expires_in": 900,
        "interval": 5,
    })
}

/// The pi test's `tokenResponse` fixture (`xai-oauth.test.ts:34-42`).
fn token_response() -> serde_json::Value {
    json!({
        "access_token": "access-token",
        "refresh_token": "refresh-token",
        "expires_in": 21_600,
        "token_type": "Bearer",
    })
}

/// Drive `machine.start`, asserting the device-authorization request, and return
/// the machine ready for its device-code response.
fn start_login() -> XaiLoginMachine {
    let mut machine = XaiLoginMachine::new();
    let request = match machine.start(START_MS) {
        Step::Request { request } => request,
        other => panic!("expected device-code request, got {other:?}"),
    };
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, XAI_DEVICE_CODE_URL);
    let form = request_form(&request);
    assert_eq!(form.get("client_id").unwrap(), XAI_CLIENT_ID);
    assert_eq!(
        form.get("scope").unwrap(),
        "openid profile email offline_access grok-cli:access api:access"
    );
    assert_eq!(form.get("referrer").unwrap(), "pi");
    machine
}

/// A [`Timers`] that fires each scheduled callback synchronously and advances a
/// shared [`FakeClock`] by the delay, so [`run_login`]'s `Wait` steps do not
/// block on real time — the Rust analog of `vi.advanceTimersByTimeAsync`.
struct ImmediateTimers {
    clock: FakeClock,
}

impl Timers for ImmediateTimers {
    fn set_timeout(&self, delay_ms: u64, callback: Box<dyn FnOnce() + Send>) -> TimerId {
        self.clock.advance(delay_ms);
        callback();
        TimerId(0)
    }
    fn clear(&self, _id: TimerId) {}
}

/// An [`AuthInteraction`] that records `device_code` events and optionally aborts
/// a signal the moment the first one is surfaced.
struct RecordingInteraction {
    events: std::sync::Mutex<Vec<AuthEvent>>,
    abort_on_notify: Option<AbortSignal>,
}

impl RecordingInteraction {
    fn new() -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
            abort_on_notify: None,
        }
    }

    fn aborting(signal: AbortSignal) -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
            abort_on_notify: Some(signal),
        }
    }
}

impl AuthInteraction for RecordingInteraction {
    fn prompt(&self, _prompt: AuthPrompt) -> Result<String, AuthFlowError> {
        Err(AuthFlowError::new("Unexpected prompt"))
    }
    fn notify(&self, event: AuthEvent) {
        if let Some(signal) = &self.abort_on_notify {
            signal.abort();
        }
        self.events.lock().unwrap().push(event);
    }
}

// ---------------------------------------------------------------------------
// Login: device grant, delayed polling, pending + slow_down
// (`xai-oauth.test.ts:79-150`).
// ---------------------------------------------------------------------------

#[test]
fn device_grant_delays_polling_and_handles_pending_and_slow_down() {
    let mut machine = start_login();

    // Device-code response → device_code notify with the parsed fields.
    let event = match machine.advance(
        StepInput::Response(json_ok(device_code_response())),
        START_MS,
    ) {
        Step::Notify { event } => event,
        other => panic!("expected device_code notify, got {other:?}"),
    };
    assert_eq!(
        event,
        AuthEvent::DeviceCode {
            user_code: "ABCD-1234".into(),
            verification_uri: "https://accounts.x.ai/oauth2/device".into(),
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(900.0),
        }
    );

    // Ack → first poll waits one interval (5s) before firing the token request.
    let wait = machine.advance(StepInput::Ack, START_MS);
    let (delay, request) = match wait {
        Step::Wait { delay_ms, request } => (delay_ms, request),
        other => panic!("expected poll wait, got {other:?}"),
    };
    assert_eq!(delay, 5000);
    assert_eq!(request.url, XAI_TOKEN_URL);
    let form = request_form(&request);
    assert_eq!(
        form.get("grant_type").unwrap(),
        "urn:ietf:params:oauth:grant-type:device_code"
    );
    assert_eq!(form.get("client_id").unwrap(), XAI_CLIENT_ID);
    assert_eq!(form.get("device_code").unwrap(), "device-code");

    // First poll (t+5s) is authorization_pending → wait another 5s.
    let pending = json_status(json!({ "error": "authorization_pending" }), 400);
    match machine.advance(StepInput::Response(pending), START_MS + 5000) {
        Step::Wait { delay_ms, .. } => assert_eq!(delay_ms, 5000),
        other => panic!("expected another poll wait, got {other:?}"),
    }

    // Second poll (t+10s) is slow_down interval=10 → next wait rises to 10s.
    let slow_down = json_status(json!({ "error": "slow_down", "interval": 10 }), 400);
    match machine.advance(StepInput::Response(slow_down), START_MS + 10_000) {
        Step::Wait { delay_ms, .. } => assert_eq!(delay_ms, 10_000),
        other => panic!("expected slowed poll wait, got {other:?}"),
    }

    // Third poll (t+20s) completes.
    let credential = match machine.advance(
        StepInput::Response(json_ok(token_response())),
        START_MS + 20_000,
    ) {
        Step::Done { credential } => credential,
        other => panic!("expected done, got {other:?}"),
    };
    assert_eq!(credential.access, "access-token");
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(
        credential.expires,
        START_MS + 20_000 + 21_600_000 - REFRESH_SKEW_MS
    );
}

/// The full login driven end-to-end by [`run_login`] over [`ScriptedTransport`]
/// + [`FakeClock`], exercising the `Wait`/poll path in the driver itself.
#[test]
fn run_login_drives_device_flow_to_done() {
    let auth = XaiOAuth::new();
    let transport = ScriptedTransport::new();
    transport.push_ok(device_code_response().to_string());
    transport.push_response(Ok(json_status(
        json!({ "error": "authorization_pending" }),
        400,
    )));
    transport.push_response(Ok(json_status(
        json!({ "error": "slow_down", "interval": 10 }),
        400,
    )));
    transport.push_ok(token_response().to_string());

    let clock = FakeClock::new(START_MS);
    let timers = ImmediateTimers {
        clock: clock.clone(),
    };
    let interaction = RecordingInteraction::new();

    let credential = run_login(&auth, &transport, &timers, &clock, &interaction, None).unwrap();
    assert_eq!(credential.access, "access-token");
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(
        credential.expires,
        START_MS + 20_000 + 21_600_000 - REFRESH_SKEW_MS
    );

    // One device-code request + three token polls, in order.
    let requests = transport.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].url, XAI_DEVICE_CODE_URL);
    assert!(requests[1..].iter().all(|r| r.url == XAI_TOKEN_URL));
    // The device_code event was surfaced.
    assert!(interaction
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, AuthEvent::DeviceCode { .. })));
}

/// interval 0 falls back to the RFC 8628 default poll interval
/// (`xai-oauth.test.ts:152-172`).
#[test]
fn interval_zero_falls_back_to_default_interval() {
    let mut machine = start_login();
    let mut response = device_code_response();
    response["interval"] = json!(0);

    let event = match machine.advance(StepInput::Response(json_ok(response)), START_MS) {
        Step::Notify { event } => event,
        other => panic!("expected notify, got {other:?}"),
    };
    // interval 0 is dropped, so the notify carries no intervalSeconds.
    assert_eq!(
        event,
        AuthEvent::DeviceCode {
            user_code: "ABCD-1234".into(),
            verification_uri: "https://accounts.x.ai/oauth2/device".into(),
            interval_seconds: None,
            expires_in_seconds: Some(900.0),
        }
    );
    match machine.advance(StepInput::Ack, START_MS) {
        Step::Wait { delay_ms, .. } => assert_eq!(delay_ms, 5000),
        other => panic!("expected default 5s wait, got {other:?}"),
    }
}

/// `verification_uri_complete` is preferred for the notify when present
/// (`xai-oauth.test.ts:174-202`).
#[test]
fn prefers_verification_uri_complete() {
    let mut machine = start_login();
    let mut response = device_code_response();
    response["verification_uri_complete"] =
        json!("https://accounts.x.ai/oauth2/device?user_code=ABCD-1234");

    match machine.advance(StepInput::Response(json_ok(response)), START_MS) {
        Step::Notify {
            event: AuthEvent::DeviceCode {
                verification_uri, ..
            },
        } => assert_eq!(
            verification_uri,
            "https://accounts.x.ai/oauth2/device?user_code=ABCD-1234"
        ),
        other => panic!("expected notify, got {other:?}"),
    }
}

/// A non-https `verification_uri_complete` is rejected
/// (`xai-oauth.test.ts:204-217`).
#[test]
fn rejects_non_https_verification_uri_complete() {
    let mut machine = start_login();
    let mut response = device_code_response();
    response["verification_uri_complete"] =
        json!("http://accounts.x.ai/oauth2/device?user_code=ABCD-1234");
    match machine.advance(StepInput::Response(json_ok(response)), START_MS) {
        Step::Error { message } => assert!(message.contains("Untrusted verification URI")),
        other => panic!("expected untrusted-uri error, got {other:?}"),
    }
}

/// A non-https or malformed `verification_uri` is rejected
/// (`xai-oauth.test.ts:219-229`).
#[test]
fn rejects_non_https_verification_uri() {
    for uri in [
        "http://accounts.x.ai/oauth2/device",
        "file:///etc/passwd",
        "not a url",
    ] {
        let mut machine = start_login();
        let mut response = device_code_response();
        response["verification_uri"] = json!(uri);
        match machine.advance(StepInput::Response(json_ok(response)), START_MS) {
            Step::Error { message } => {
                assert!(message.contains("Untrusted verification URI"), "uri: {uri}")
            }
            other => panic!("expected untrusted-uri error for {uri}, got {other:?}"),
        }
    }
}

/// Device authorization denial fails with pi's message
/// (`xai-oauth.test.ts:231-249`).
#[test]
fn denial_fails_with_message() {
    for error in ["access_denied", "authorization_denied"] {
        let mut machine = start_login();
        let mut device = device_code_response();
        device["interval"] = json!(1);
        machine.advance(StepInput::Response(json_ok(device)), START_MS);
        machine.advance(StepInput::Ack, START_MS);
        let denied = json_status(json!({ "error": error }), 400);
        match machine.advance(StepInput::Response(denied), START_MS + 1000) {
            Step::Error { message } => {
                assert_eq!(message, "xAI device authorization was denied")
            }
            other => panic!("expected denial error for {error}, got {other:?}"),
        }
    }
}

/// An `expired_token` poll response fails with pi's message (`xai.ts:190-191`).
#[test]
fn expired_token_fails_with_message() {
    let mut machine = start_login();
    machine.advance(
        StepInput::Response(json_ok(device_code_response())),
        START_MS,
    );
    machine.advance(StepInput::Ack, START_MS);
    let expired = json_status(json!({ "error": "expired_token" }), 400);
    match machine.advance(StepInput::Response(expired), START_MS + 5000) {
        Step::Error { message } => assert_eq!(message, "xAI device code expired"),
        other => panic!("expected expired error, got {other:?}"),
    }
}

/// An aborted signal while waiting for the first poll cancels with pi's message,
/// firing exactly one request (the device-code fetch)
/// (`xai-oauth.test.ts:251-263`).
#[test]
fn cancels_while_waiting_for_first_poll() {
    let auth = XaiOAuth::new();
    let transport = ScriptedTransport::new();
    transport.push_ok(device_code_response().to_string());

    let clock = FakeClock::new(START_MS);
    let timers = ImmediateTimers {
        clock: clock.clone(),
    };
    let signal = AbortSignal::new();
    let interaction = RecordingInteraction::aborting(signal.clone());

    let err = run_login(
        &auth,
        &transport,
        &timers,
        &clock,
        &interaction,
        Some(&signal),
    )
    .unwrap_err();
    assert_eq!(err.message, "Login cancelled");
    // Only the device-code request went out; the poll never fired.
    assert_eq!(transport.requests().len(), 1);
}

/// An `Aborted` input mid-flow yields the "Login cancelled" error.
#[test]
fn login_aborts_with_cancel_message() {
    let mut machine = start_login();
    match machine.advance(StepInput::Aborted, START_MS) {
        Step::Error { message } => assert_eq!(message, "Login cancelled"),
        other => panic!("expected cancel error, got {other:?}"),
    }
}

/// Polling past the device-code deadline times out with the plain message.
#[test]
fn poll_past_deadline_times_out() {
    let mut machine = start_login();
    let mut device = device_code_response();
    device["expires_in"] = json!(10); // 10s lifetime.
    machine.advance(StepInput::Response(json_ok(device)), START_MS);
    machine.advance(StepInput::Ack, START_MS);
    // A pending poll arriving at/after the deadline times out.
    let pending = json_status(json!({ "error": "authorization_pending" }), 400);
    match machine.advance(StepInput::Response(pending), START_MS + 10_000) {
        Step::Error { message } => assert_eq!(message, "Device flow timed out"),
        other => panic!("expected timeout, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Refresh (`xai-oauth.test.ts:265-324`).
// ---------------------------------------------------------------------------

/// Drive a refresh machine and return its terminal step.
fn run_refresh_once(refresh_token: &str, response: HttpResponse, now_ms: i64) -> Step {
    let mut machine = XaiRefreshMachine::new(refresh_token);
    let request = match machine.start(now_ms) {
        Step::Request { request } => request,
        other => panic!("expected refresh request, got {other:?}"),
    };
    assert_eq!(request.url, XAI_TOKEN_URL);
    let form = request_form(&request);
    assert_eq!(form.get("grant_type").unwrap(), "refresh_token");
    assert_eq!(form.get("client_id").unwrap(), XAI_CLIENT_ID);
    assert_eq!(form.get("refresh_token").unwrap(), refresh_token);
    machine.advance(StepInput::Response(response), now_ms)
}

/// Refresh rotates tokens, and preserves an unrotated refresh token
/// (`xai-oauth.test.ts:265-291`).
#[test]
fn refresh_rotates_and_preserves_unrotated_token() {
    // Rotation: the server returns a new refresh token.
    let rotated = run_refresh_once(
        "old-refresh",
        json_ok(json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "expires_in": 21_600,
        })),
        START_MS,
    );
    match rotated {
        Step::Done { credential } => {
            assert_eq!(credential.refresh, "new-refresh");
            assert_eq!(credential.access, "new-access");
        }
        other => panic!("expected rotated done, got {other:?}"),
    }

    // No rotation: refresh_token omitted → the prior token is preserved.
    let preserved = run_refresh_once(
        "keep-refresh",
        json_ok(json!({
            "access_token": "newer-access",
            "expires_in": 21_600,
        })),
        START_MS,
    );
    let credential = match preserved {
        Step::Done { credential } => credential,
        other => panic!("expected preserved done, got {other:?}"),
    };
    assert_eq!(credential.refresh, "keep-refresh");
    assert_eq!(credential.access, "newer-access");

    // Handler surface: name + toAuth mapping.
    let auth = XaiOAuth::new();
    assert_eq!(OAuthAuth::name(&auth), "xAI (Grok/X subscription)");
    let model_auth = OAuthAuth::to_auth(&auth, &credential).unwrap();
    assert_eq!(model_auth.api_key.as_deref(), Some("newer-access"));
}

/// A missing `expires_in` falls back to a one-hour lifetime
/// (`xai-oauth.test.ts:293-304`).
#[test]
fn refresh_assumes_one_hour_when_expires_in_missing() {
    let step = run_refresh_once(
        "old-refresh",
        json_ok(json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "token_type": "Bearer",
        })),
        START_MS,
    );
    match step {
        Step::Done { credential } => {
            assert_eq!(credential.expires, START_MS + 3_600_000 - REFRESH_SKEW_MS)
        }
        other => panic!("expected done, got {other:?}"),
    }
}

/// A token response missing `access_token` is rejected with pi's field error
/// (`xai-oauth.test.ts:306-313`).
#[test]
fn refresh_rejects_missing_access_token() {
    let step = run_refresh_once(
        "old-refresh",
        json_ok(json!({
            "refresh_token": "refresh-token",
            "expires_in": 21_600,
        })),
        START_MS,
    );
    match step {
        Step::Error { message } => {
            assert_eq!(message, "Invalid xAI OAuth response field: access_token")
        }
        other => panic!("expected field error, got {other:?}"),
    }
}

/// A refresh HTTP failure surfaces the upstream error code and description
/// (`xai-oauth.test.ts:315-324`).
#[test]
fn refresh_surfaces_error_code_and_description() {
    let step = run_refresh_once(
        "old-refresh",
        json_status(
            json!({
                "error": "invalid_grant",
                "error_description": "refresh token revoked",
            }),
            400,
        ),
        START_MS,
    );
    match step {
        Step::Error { message } => assert_eq!(
            message,
            "xAI OAuth token refresh failed (HTTP 400): invalid_grant: refresh token revoked"
        ),
        other => panic!("expected refresh failure, got {other:?}"),
    }
}

/// `to_auth` maps the access token to `apiKey` (`xai.ts:230-232`).
#[test]
fn to_auth_maps_access_to_api_key() {
    let auth = XaiOAuth::new();
    let credential = OAuthCredential {
        refresh: "r".into(),
        access: "the-access-token".into(),
        expires: 0,
        extra: Default::default(),
    };
    let model_auth = OAuthAuth::to_auth(&auth, &credential).unwrap();
    assert_eq!(model_auth.api_key.as_deref(), Some("the-access-token"));
}
