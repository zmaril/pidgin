// straitjacket-allow-file[:duplication] — pi ships no dedicated Radius OAuth
// test at the pinned commit, so these are original unit tests. Each `#[test]`
// rebuilds a similar machine-driving scaffold (start → config → method →
// request/response) so the browser, device-code, refresh, discovery, and
// error-mapping paths are exercised in isolation. The clone detector reads the
// repeated scaffolding as duplication; it is deliberate, load-bearing per-case
// fixtures.
//! Unit tests for the Radius OAuth flow. pi has no `radius-oauth.test.ts` at the
//! pinned commit `3da591ab` (a fetch of that path 404s), so these are original
//! Rust tests covering config discovery, the browser exchange, the device-code
//! poll, refresh, and the OAuth error mappings, driven through the machine and
//! the pure-Rust [`run_login`] / [`run_refresh`] drivers.

use serde_json::{json, Value};

use super::{
    RadiusLoginMachine, RadiusOAuth, RadiusRefreshMachine, LOGIN_METHOD_BROWSER,
    LOGIN_METHOD_DEVICE_CODE, TOKEN_EXPIRY_SKEW_MS,
};
use crate::auth::error::AuthFlowError;
use crate::auth::oauth::device_code::{SLOW_DOWN_TIMEOUT_MESSAGE, TIMEOUT_MESSAGE};
use crate::auth::oauth::flow::{run_login, run_refresh, OAuthFlowMachine, Step, StepInput};
use crate::auth::types::{
    AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, OAuthAuth, OAuthCredential,
};
use crate::seams::clock::FakeClock;
use crate::seams::http::{HttpResponse, ScriptedTransport};

const GATEWAY: &str = "https://gw.example.com";
const NOW_MS: i64 = 1_700_000_000_000;
const STATE: &str = "test-state-123";
const PKCE_SEED: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31,
];

/// The discovered-config JSON the gateway returns from `GET /v1/oauth`.
fn config_json() -> Value {
    json!({
        "issuer": "https://gw.example.com",
        "authorizationEndpoint": "https://gw.example.com/oauth/authorize",
        "tokenEndpoint": "https://gw.example.com/oauth/token",
        "deviceAuthorizationEndpoint": "https://gw.example.com/oauth/device",
        "deviceAuthorizationEventsEndpoint": "https://gw.example.com/oauth/device/events",
        "verificationEndpoint": "https://gw.example.com/device",
        "clientId": "radius-client",
        "scope": "openid profile",
        "deviceCodeGrantType": "urn:ietf:params:oauth:grant-type:device_code",
    })
}

/// A form-urlencoded body parsed into first-value-wins pairs (percent-decoded).
fn form_pairs(body: &Option<String>) -> std::collections::BTreeMap<String, String> {
    let body = body.as_deref().expect("request has a body");
    let mut map = std::collections::BTreeMap::new();
    for pair in body.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            map.entry(k.to_string())
                .or_insert_with(|| percent_decode(v));
        }
    }
    map
}

/// Decode a form-urlencoded component: `+`→space, then percent-decode.
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

fn expect_request(step: Step) -> crate::seams::http::HttpRequest {
    match step {
        Step::Request { request } => request,
        other => panic!("expected request, got {other:?}"),
    }
}

/// `new(name, gateway)` normalizes the gateway: scheme prefixed, trailing
/// slashes stripped.
#[test]
fn constructor_normalizes_gateway() {
    assert_eq!(
        RadiusOAuth::new("Radius", "gw.example.com/").gateway(),
        "https://gw.example.com"
    );
    assert_eq!(
        RadiusOAuth::new("Radius", "http://local:8080///").gateway(),
        "http://local:8080"
    );
    assert_eq!(
        RadiusOAuth::new("Radius", "https://gw.example.com").gateway(),
        "https://gw.example.com"
    );
}

/// Login and refresh both begin with a config-discovery GET to
/// `<gateway>/v1/oauth`, resolving the path against the gateway origin.
#[test]
fn first_step_is_config_discovery() {
    let mut machine = RadiusLoginMachine::with_seed(GATEWAY, "Radius", PKCE_SEED, STATE);
    let request = expect_request(machine.start(NOW_MS));
    assert_eq!(request.method, "GET");
    assert_eq!(request.url, "https://gw.example.com/v1/oauth");
    assert_eq!(request.headers.get("accept").unwrap(), "application/json");

    let mut refresh = RadiusRefreshMachine::new(GATEWAY, "refresh-token");
    let request = expect_request(refresh.start(NOW_MS));
    assert_eq!(request.url, "https://gw.example.com/v1/oauth");
}

/// A failed config-discovery response surfaces pi's exact error message.
#[test]
fn config_discovery_failure_errors() {
    let mut machine = RadiusLoginMachine::with_seed(GATEWAY, "Radius", PKCE_SEED, STATE);
    machine.start(NOW_MS);
    let response = HttpResponse {
        status: 500,
        headers: Default::default(),
        body: "boom".to_string(),
    };
    match machine.advance(StepInput::Response(response), NOW_MS) {
        Step::Error { message } => {
            assert_eq!(
                message,
                "Could not load Radius OAuth config from https://gw.example.com: 500 boom"
            );
        }
        other => panic!("expected config error, got {other:?}"),
    }
}

/// Browser login: config → select browser → progress notify → auth_url notify →
/// manual-code prompt → token exchange → done. The authorize URL carries the
/// PKCE challenge, S256, the handoff flag, and the state.
#[test]
fn browser_login_exchanges_callback_code() {
    let mut machine = RadiusLoginMachine::with_seed(GATEWAY, "Radius", PKCE_SEED, STATE);
    machine.start(NOW_MS);

    // Config response → select prompt.
    match machine.advance(
        StepInput::Response(HttpResponse::ok(config_json().to_string())),
        NOW_MS,
    ) {
        Step::Prompt { prompt } => match prompt.kind {
            AuthPromptKind::Select { options, .. } => {
                assert_eq!(options[0].id, LOGIN_METHOD_BROWSER);
                assert_eq!(options[1].id, LOGIN_METHOD_DEVICE_CODE);
            }
            other => panic!("expected select, got {other:?}"),
        },
        other => panic!("expected select prompt, got {other:?}"),
    }

    // Select browser → progress notify.
    match machine.advance(
        StepInput::Input {
            value: LOGIN_METHOD_BROWSER.into(),
        },
        NOW_MS,
    ) {
        Step::Notify {
            event: AuthEvent::Progress { message },
        } => {
            assert!(
                message.contains("127.0.0.1:1456/oauth/callback"),
                "{message}"
            );
        }
        other => panic!("expected progress notify, got {other:?}"),
    }

    // Ack → auth_url notify.
    let url = match machine.advance(StepInput::Ack, NOW_MS) {
        Step::Notify {
            event: AuthEvent::AuthUrl { url, instructions },
        } => {
            assert_eq!(instructions.as_deref(), Some("Continue in your browser."));
            url
        }
        other => panic!("expected auth_url notify, got {other:?}"),
    };
    assert!(
        url.starts_with("https://gw.example.com/oauth/authorize?"),
        "{url}"
    );
    assert!(url.contains("code_challenge_method=S256"), "{url}");
    assert!(url.contains("handoff=url"), "{url}");
    assert!(url.contains(&format!("state={STATE}")), "{url}");
    assert!(
        url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1456%2Foauth%2Fcallback"),
        "{url}"
    );

    // Ack → manual-code prompt.
    match machine.advance(StepInput::Ack, NOW_MS) {
        Step::Prompt { prompt } => {
            assert!(matches!(prompt.kind, AuthPromptKind::ManualCode { .. }))
        }
        other => panic!("expected manual_code prompt, got {other:?}"),
    }

    // Paste the callback URL → token exchange request.
    let pasted = format!("http://127.0.0.1:1456/oauth/callback?code=auth-code&state={STATE}");
    let request = expect_request(machine.advance(StepInput::Input { value: pasted }, NOW_MS));
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, "https://gw.example.com/oauth/token");
    assert_eq!(
        request.headers.get("content-type").unwrap(),
        "application/x-www-form-urlencoded"
    );
    let pairs = form_pairs(&request.body);
    assert_eq!(pairs.get("grant_type").unwrap(), "authorization_code");
    assert_eq!(pairs.get("code").unwrap(), "auth-code");
    assert_eq!(pairs.get("client_id").unwrap(), "radius-client");
    assert_eq!(
        pairs.get("code_verifier").unwrap(),
        "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"
    );

    // Token response → done; scope stashed on extra, 60s skew applied.
    let response = HttpResponse::ok(
        json!({
            "access_token": "access",
            "refresh_token": "refresh",
            "expires_in": 3600,
            "scope": "openid profile",
        })
        .to_string(),
    );
    match machine.advance(StepInput::Response(response), NOW_MS) {
        Step::Done { credential } => {
            assert_eq!(credential.access, "access");
            assert_eq!(credential.refresh, "refresh");
            assert_eq!(
                credential.expires,
                NOW_MS + 3600 * 1000 - TOKEN_EXPIRY_SKEW_MS
            );
            assert_eq!(credential.extra.get("scope").unwrap(), "openid profile");
        }
        other => panic!("expected done, got {other:?}"),
    }
}

/// A pasted callback whose `state` does not match aborts with the state-mismatch
/// message.
#[test]
fn browser_login_rejects_state_mismatch() {
    let mut machine = RadiusLoginMachine::with_seed(GATEWAY, "Radius", PKCE_SEED, STATE);
    machine.start(NOW_MS);
    machine.advance(
        StepInput::Response(HttpResponse::ok(config_json().to_string())),
        NOW_MS,
    );
    machine.advance(
        StepInput::Input {
            value: LOGIN_METHOD_BROWSER.into(),
        },
        NOW_MS,
    );
    machine.advance(StepInput::Ack, NOW_MS);
    machine.advance(StepInput::Ack, NOW_MS);
    let pasted = "http://127.0.0.1:1456/oauth/callback?code=c&state=wrong".to_string();
    match machine.advance(StepInput::Input { value: pasted }, NOW_MS) {
        Step::Error { message } => assert_eq!(message, "OAuth state mismatch."),
        other => panic!("expected state-mismatch error, got {other:?}"),
    }
}

/// An unknown sign-in method id errors with the provider name.
#[test]
fn unknown_method_errors() {
    let mut machine = RadiusLoginMachine::with_seed(GATEWAY, "Radius", PKCE_SEED, STATE);
    machine.start(NOW_MS);
    machine.advance(
        StepInput::Response(HttpResponse::ok(config_json().to_string())),
        NOW_MS,
    );
    match machine.advance(
        StepInput::Input {
            value: "carrier-pigeon".into(),
        },
        NOW_MS,
    ) {
        Step::Error { message } => {
            assert_eq!(message, "Unknown Radius sign-in method: carrier-pigeon")
        }
        other => panic!("expected unknown-method error, got {other:?}"),
    }
}

/// Drive the device-code machine up through the first poll request.
fn device_up_to_first_poll(machine: &mut RadiusLoginMachine) -> crate::seams::http::HttpRequest {
    machine.start(NOW_MS);
    machine.advance(
        StepInput::Response(HttpResponse::ok(config_json().to_string())),
        NOW_MS,
    );
    // Select device-code → device-authorization request.
    let request = expect_request(machine.advance(
        StepInput::Input {
            value: LOGIN_METHOD_DEVICE_CODE.into(),
        },
        NOW_MS,
    ));
    assert_eq!(request.url, "https://gw.example.com/oauth/device");
    let pairs = form_pairs(&request.body);
    assert_eq!(pairs.get("client_id").unwrap(), "radius-client");
    assert_eq!(pairs.get("scope").unwrap(), "openid profile");

    // Device-authorization response → device_code notify.
    let device_response = HttpResponse::ok(
        json!({
            "device_code": "DEV-CODE",
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://gw.example.com/activate",
            "expires_in": 900,
            "interval": 5,
        })
        .to_string(),
    );
    match machine.advance(StepInput::Response(device_response), NOW_MS) {
        Step::Notify {
            event:
                AuthEvent::DeviceCode {
                    user_code,
                    verification_uri,
                    interval_seconds,
                    expires_in_seconds,
                },
        } => {
            assert_eq!(user_code, "WDJB-MJHT");
            assert_eq!(verification_uri, "https://gw.example.com/activate");
            assert_eq!(interval_seconds, Some(5.0));
            assert_eq!(expires_in_seconds, Some(900.0));
        }
        other => panic!("expected device_code notify, got {other:?}"),
    }

    // Ack → first poll request (no wait before first poll).
    let poll = expect_request(machine.advance(StepInput::Ack, NOW_MS));
    assert_eq!(poll.url, "https://gw.example.com/oauth/token");
    let pairs = form_pairs(&poll.body);
    assert_eq!(
        pairs.get("grant_type").unwrap(),
        "urn:ietf:params:oauth:grant-type:device_code"
    );
    assert_eq!(pairs.get("device_code").unwrap(), "DEV-CODE");
    poll
}

/// Device-code login: authorization_pending then success. The first poll is a
/// bare request; a pending response schedules a `Wait` before the next poll.
#[test]
fn device_login_pending_then_complete() {
    let mut machine = RadiusLoginMachine::with_seed(GATEWAY, "Radius", PKCE_SEED, STATE);
    device_up_to_first_poll(&mut machine);

    // authorization_pending → Wait then poll again.
    let pending = HttpResponse {
        status: 400,
        headers: Default::default(),
        body: json!({ "error": "authorization_pending" }).to_string(),
    };
    let (delay_ms, _request) = match machine.advance(StepInput::Response(pending), NOW_MS) {
        Step::Wait { delay_ms, request } => (delay_ms, request),
        other => panic!("expected wait, got {other:?}"),
    };
    assert_eq!(delay_ms, 5000);

    // Success on the next poll (clock advanced by the interval).
    let ok = HttpResponse::ok(
        json!({
            "access_token": "acc",
            "refresh_token": "ref",
            "expires_in": 7200,
        })
        .to_string(),
    );
    match machine.advance(StepInput::Response(ok), NOW_MS + 5000) {
        Step::Done { credential } => {
            assert_eq!(credential.access, "acc");
            assert_eq!(
                credential.expires,
                NOW_MS + 5000 + 7200 * 1000 - TOKEN_EXPIRY_SKEW_MS
            );
            assert!(credential.extra.get("scope").is_none());
        }
        other => panic!("expected done, got {other:?}"),
    }
}

/// A `slow_down` response bumps the interval by five seconds for the next poll.
#[test]
fn device_login_slow_down_bumps_interval() {
    let mut machine = RadiusLoginMachine::with_seed(GATEWAY, "Radius", PKCE_SEED, STATE);
    device_up_to_first_poll(&mut machine);
    let slow = HttpResponse {
        status: 400,
        headers: Default::default(),
        body: json!({ "error": "slow_down" }).to_string(),
    };
    match machine.advance(StepInput::Response(slow), NOW_MS) {
        Step::Wait { delay_ms, .. } => assert_eq!(delay_ms, 10_000),
        other => panic!("expected wait, got {other:?}"),
    }
}

/// `expired_token` and `access_denied` map to pi's exact terminal messages.
#[test]
fn device_login_expired_and_denied_map_to_messages() {
    for (oauth_error, expected) in [
        ("expired_token", "Device authorization expired."),
        ("access_denied", "Device authorization was denied."),
    ] {
        let mut machine = RadiusLoginMachine::with_seed(GATEWAY, "Radius", PKCE_SEED, STATE);
        device_up_to_first_poll(&mut machine);
        let response = HttpResponse {
            status: 400,
            headers: Default::default(),
            body: json!({ "error": oauth_error }).to_string(),
        };
        match machine.advance(StepInput::Response(response), NOW_MS) {
            Step::Error { message } => assert_eq!(message, expected),
            other => panic!("expected error for {oauth_error}, got {other:?}"),
        }
    }
}

/// An unrecognized OAuth error on a poll is rethrown with the formatted
/// `OAuthResponseError` message.
#[test]
fn device_login_unknown_oauth_error_rethrows() {
    let mut machine = RadiusLoginMachine::with_seed(GATEWAY, "Radius", PKCE_SEED, STATE);
    device_up_to_first_poll(&mut machine);
    let response = HttpResponse {
        status: 400,
        headers: Default::default(),
        body: json!({ "error": "invalid_client", "error_description": "bad client" }).to_string(),
    };
    match machine.advance(StepInput::Response(response), NOW_MS) {
        Step::Error { message } => {
            assert_eq!(
                message,
                "Radius OAuth token request failed: invalid_client: bad client"
            );
        }
        other => panic!("expected rethrown error, got {other:?}"),
    }
}

/// A device-authorization response missing required fields errors.
#[test]
fn device_authorization_missing_fields_errors() {
    let mut machine = RadiusLoginMachine::with_seed(GATEWAY, "Radius", PKCE_SEED, STATE);
    machine.start(NOW_MS);
    machine.advance(
        StepInput::Response(HttpResponse::ok(config_json().to_string())),
        NOW_MS,
    );
    machine.advance(
        StepInput::Input {
            value: LOGIN_METHOD_DEVICE_CODE.into(),
        },
        NOW_MS,
    );
    let response = HttpResponse::ok(json!({ "user_code": "X", "expires_in": 900 }).to_string());
    match machine.advance(StepInput::Response(response), NOW_MS) {
        Step::Error { message } => {
            assert_eq!(
                message,
                "Radius OAuth device authorization response is missing required fields"
            );
        }
        other => panic!("expected missing-fields error, got {other:?}"),
    }
}

/// The device_code notify falls back to the config's verification endpoint when
/// the response omits `verification_uri`.
#[test]
fn device_code_notify_falls_back_to_verification_endpoint() {
    let mut machine = RadiusLoginMachine::with_seed(GATEWAY, "Radius", PKCE_SEED, STATE);
    machine.start(NOW_MS);
    machine.advance(
        StepInput::Response(HttpResponse::ok(config_json().to_string())),
        NOW_MS,
    );
    machine.advance(
        StepInput::Input {
            value: LOGIN_METHOD_DEVICE_CODE.into(),
        },
        NOW_MS,
    );
    let response = HttpResponse::ok(
        json!({ "device_code": "D", "user_code": "U", "expires_in": 900 }).to_string(),
    );
    match machine.advance(StepInput::Response(response), NOW_MS) {
        Step::Notify {
            event: AuthEvent::DeviceCode {
                verification_uri, ..
            },
        } => {
            assert_eq!(verification_uri, "https://gw.example.com/device");
        }
        other => panic!("expected device_code notify, got {other:?}"),
    }
}

/// Device polling times out when the next inter-poll sleep would reach the
/// deadline, and after a slow_down the timeout carries the WSL/VM wording. The
/// deadline is pre-checked (behavior (b)): the boundary poll errors immediately
/// with NO trailing poll, matching pi's "break before the final poll".
#[test]
fn device_login_times_out() {
    // Plain timeout: deadline is 900s, interval 5s.
    let mut machine = RadiusLoginMachine::with_seed(GATEWAY, "Radius", PKCE_SEED, STATE);
    device_up_to_first_poll(&mut machine);
    let pending = || HttpResponse {
        status: 400,
        headers: Default::default(),
        body: json!({ "error": "authorization_pending" }).to_string(),
    };
    // First pending schedules a 5s Wait before the next poll.
    match machine.advance(StepInput::Response(pending()), NOW_MS) {
        Step::Wait { delay_ms, .. } => assert_eq!(delay_ms, 5000),
        other => panic!("expected wait, got {other:?}"),
    }
    // A pending whose next 5s sleep would land on the 900s deadline times out
    // immediately, without scheduling that final poll.
    match machine.advance(StepInput::Response(pending()), NOW_MS + 895_000) {
        Step::Error { message } => assert_eq!(message, TIMEOUT_MESSAGE),
        other => panic!("expected immediate timeout with no trailing poll, got {other:?}"),
    }

    // slow_down timeout wording. The slow_down bumps the interval to 10s, so a
    // pending 5s before the deadline can no longer fit another poll → timeout.
    let mut machine = RadiusLoginMachine::with_seed(GATEWAY, "Radius", PKCE_SEED, STATE);
    device_up_to_first_poll(&mut machine);
    let slow = HttpResponse {
        status: 400,
        headers: Default::default(),
        body: json!({ "error": "slow_down" }).to_string(),
    };
    match machine.advance(StepInput::Response(slow), NOW_MS) {
        Step::Wait { delay_ms, .. } => assert_eq!(delay_ms, 10_000),
        other => panic!("expected wait, got {other:?}"),
    }
    match machine.advance(StepInput::Response(pending()), NOW_MS + 895_000) {
        Step::Error { message } => assert_eq!(message, SLOW_DOWN_TIMEOUT_MESSAGE),
        other => {
            panic!("expected immediate slow_down timeout with no trailing poll, got {other:?}")
        }
    }
}

/// Refresh: config discovery → refresh_token grant → rotated credential.
#[test]
fn refresh_discovers_config_and_rotates_tokens() {
    let mut machine = RadiusRefreshMachine::new(GATEWAY, "old-refresh");
    machine.start(NOW_MS);
    // Config response → refresh token request.
    let request = expect_request(machine.advance(
        StepInput::Response(HttpResponse::ok(config_json().to_string())),
        NOW_MS,
    ));
    assert_eq!(request.url, "https://gw.example.com/oauth/token");
    let pairs = form_pairs(&request.body);
    assert_eq!(pairs.get("grant_type").unwrap(), "refresh_token");
    assert_eq!(pairs.get("refresh_token").unwrap(), "old-refresh");
    assert_eq!(pairs.get("client_id").unwrap(), "radius-client");

    let response = HttpResponse::ok(
        json!({
            "access_token": "new-acc",
            "refresh_token": "new-ref",
            "expires_in": 1800,
        })
        .to_string(),
    );
    match machine.advance(StepInput::Response(response), NOW_MS) {
        Step::Done { credential } => {
            assert_eq!(credential.access, "new-acc");
            assert_eq!(credential.refresh, "new-ref");
            assert_eq!(
                credential.expires,
                NOW_MS + 1800 * 1000 - TOKEN_EXPIRY_SKEW_MS
            );
        }
        other => panic!("expected done, got {other:?}"),
    }
}

/// A non-2xx token response on refresh surfaces the OAuth error message.
#[test]
fn refresh_token_error_surfaces_message() {
    let mut machine = RadiusRefreshMachine::new(GATEWAY, "old-refresh");
    machine.start(NOW_MS);
    machine.advance(
        StepInput::Response(HttpResponse::ok(config_json().to_string())),
        NOW_MS,
    );
    let response = HttpResponse {
        status: 400,
        headers: Default::default(),
        body: json!({ "error": "invalid_grant", "error_description": "expired" }).to_string(),
    };
    match machine.advance(StepInput::Response(response), NOW_MS) {
        Step::Error { message } => {
            assert_eq!(
                message,
                "Radius OAuth token request failed: invalid_grant: expired"
            );
        }
        other => panic!("expected token error, got {other:?}"),
    }
}

/// An `Aborted` input mid-flow yields "Login cancelled" for both machines.
#[test]
fn abort_yields_cancel_message() {
    let mut login = RadiusLoginMachine::with_seed(GATEWAY, "Radius", PKCE_SEED, STATE);
    login.start(NOW_MS);
    match login.advance(StepInput::Aborted, NOW_MS) {
        Step::Error { message } => assert_eq!(message, "Login cancelled"),
        other => panic!("expected cancel, got {other:?}"),
    }
    let mut refresh = RadiusRefreshMachine::new(GATEWAY, "r");
    refresh.start(NOW_MS);
    match refresh.advance(StepInput::Aborted, NOW_MS) {
        Step::Error { message } => assert_eq!(message, "Login cancelled"),
        other => panic!("expected cancel, got {other:?}"),
    }
}

/// `to_auth` maps the access token to `apiKey`.
#[test]
fn to_auth_maps_access_to_api_key() {
    let auth = RadiusOAuth::new("Radius", GATEWAY);
    let credential = OAuthCredential {
        refresh: "r".into(),
        access: "the-access-token".into(),
        expires: 0,
        extra: Default::default(),
    };
    let model_auth = OAuthAuth::to_auth(&auth, &credential).unwrap();
    assert_eq!(model_auth.api_key.as_deref(), Some("the-access-token"));
}

/// The native `run_login` driver reaches `Done` end-to-end over
/// `ScriptedTransport` + `FakeClock` for the browser path. A random `state` is
/// threaded through by reading it back out of the recorded auth_url.
#[test]
fn run_login_browser_reaches_done_end_to_end() {
    let auth = RadiusOAuth::new("Radius", GATEWAY);
    let transport = ScriptedTransport::new();
    transport.push_ok(config_json().to_string());
    transport.push_ok(
        json!({
            "access_token": "access",
            "refresh_token": "refresh",
            "expires_in": 3600,
            "scope": "openid",
        })
        .to_string(),
    );
    let clock = FakeClock::new(NOW_MS);
    let interaction = StateReadingInteraction::default();
    let credential = run_login(&auth, &transport, &clock, &clock, &interaction, None).unwrap();
    assert_eq!(credential.access, "access");
    assert_eq!(credential.refresh, "refresh");
    assert_eq!(
        credential.expires,
        NOW_MS + 3600 * 1000 - TOKEN_EXPIRY_SKEW_MS
    );

    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url, "https://gw.example.com/v1/oauth");
    assert_eq!(requests[1].url, "https://gw.example.com/oauth/token");
    let pairs = form_pairs(&requests[1].body);
    assert_eq!(pairs.get("code").unwrap(), "driver-code");
}

/// An interaction that reads the OAuth `state` out of the recorded auth_url and
/// pastes a matching callback, so it works with a randomly-generated state.
#[derive(Default)]
struct StateReadingInteraction {
    state: std::sync::Mutex<Option<String>>,
}

impl AuthInteraction for StateReadingInteraction {
    fn prompt(&self, prompt: AuthPrompt) -> Result<String, AuthFlowError> {
        match prompt.kind {
            AuthPromptKind::Select { .. } => Ok(LOGIN_METHOD_BROWSER.to_string()),
            AuthPromptKind::ManualCode { .. } => {
                let state = self
                    .state
                    .lock()
                    .unwrap()
                    .clone()
                    .expect("auth_url seen first");
                Ok(format!(
                    "http://127.0.0.1:1456/oauth/callback?code=driver-code&state={state}"
                ))
            }
            other => Err(AuthFlowError::new(format!("unexpected prompt: {other:?}"))),
        }
    }

    fn notify(&self, event: AuthEvent) {
        if let AuthEvent::AuthUrl { url, .. } = &event {
            if let Some((_, query)) = url.split_once('?') {
                for pair in query.split('&') {
                    if let Some(value) = pair.strip_prefix("state=") {
                        *self.state.lock().unwrap() = Some(value.to_string());
                    }
                }
            }
        }
    }
}

/// The native `run_refresh` driver discovers config and rotates the token.
#[test]
fn run_refresh_reaches_done_end_to_end() {
    let auth = RadiusOAuth::new("Radius", GATEWAY);
    let transport = ScriptedTransport::new();
    transport.push_ok(config_json().to_string());
    transport.push_ok(
        json!({
            "access_token": "a2",
            "refresh_token": "r2",
            "expires_in": 3600,
        })
        .to_string(),
    );
    let clock = FakeClock::new(NOW_MS);
    let credential = OAuthCredential {
        refresh: "r1".into(),
        access: "a1".into(),
        expires: 0,
        extra: Default::default(),
    };
    let refreshed = run_refresh(&auth, &credential, &transport, &clock, &clock, None).unwrap();
    assert_eq!(refreshed.access, "a2");
    assert_eq!(refreshed.refresh, "r2");
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url, "https://gw.example.com/v1/oauth");
    assert_eq!(requests[1].url, "https://gw.example.com/oauth/token");
}
