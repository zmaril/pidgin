// straitjacket-allow-file[:duplication] — these tests mirror pi's
// `openai-codex-oauth.test.ts` case-by-case: each `#[test]` rebuilds a similar
// machine-driving scaffold (select → usercode → notify → poll → exchange, or
// select → auth_url → prompt → exchange) so the browser/device/refresh paths are
// exercised in isolation. The clone detector reads the repeated scaffolding, and
// the token/device response fixtures shared with the other provider test modules,
// as duplication; it is deliberate, load-bearing per-case fixtures kept parallel
// to the pi test they transcribe.
//! Unit tests for the OpenAI Codex OAuth flow, mirroring pi-ai's
//! `packages/ai/test/openai-codex-oauth.test.ts` at pinned commit `3da591ab`.
//!
//! The pi test drives async `fetch` + Vitest fake timers; the machine port drives
//! the same fetch/timer sequences deterministically by stepping the machine (the
//! `pollTimes`/`expires`/`accountId` assertions carry over one-for-one), plus one
//! end-to-end `run_flow` browser-path test over [`ScriptedTransport`] +
//! [`FakeClock`] + a recording [`AuthInteraction`].

use base64::Engine;
use serde_json::{json, Value};

use super::{
    parse_authorization_input, OpenAICodexLoginMachine, OpenAICodexOAuth,
    OpenAICodexRefreshMachine, ParsedAuthInput, BROWSER_LOGIN_METHOD, CLIENT_ID,
    DEVICE_CODE_LOGIN_METHOD, DEVICE_REDIRECT_URI, DEVICE_TOKEN_URL, DEVICE_USER_CODE_URL,
    DEVICE_VERIFICATION_URI, REDIRECT_URI, TOKEN_URL,
};
use crate::auth::error::AuthFlowError;
use crate::auth::oauth::device_code::{CANCEL_MESSAGE, TIMEOUT_MESSAGE};
use crate::auth::oauth::flow::{run_login, OAuthFlowMachine, Step, StepInput};
use crate::auth::types::{
    AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, AuthSelectOption, OAuthCredential,
};
use crate::seams::clock::FakeClock;
use crate::seams::http::{HttpResponse, ScriptedTransport};

/// pi pins `startTime = 2026-05-20T00:00:00Z`
/// (`openai-codex-oauth.test.ts:77-78`).
const START_MS: i64 = 1_774_339_200_000;

/// Build an unsigned JWT whose payload carries `chatgpt_account_id`, exactly like
/// the pi test's `createAccessToken` (`openai-codex-oauth.test.ts:18-28`).
fn create_access_token(account_id: &str) -> String {
    let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
    let header = b64(&json!({ "alg": "none" }).to_string());
    let payload = b64(&json!({
        "https://api.openai.com/auth": { "chatgpt_account_id": account_id },
    })
    .to_string());
    format!("{header}.{payload}.signature")
}

/// pi's `deviceAuthPendingResponse`: a 403 with an
/// `deviceauth_authorization_pending` error code (`openai-codex-oauth.test.ts:30-42`).
fn device_pending_response() -> HttpResponse {
    HttpResponse {
        status: 403,
        headers: Default::default(),
        body: json!({
            "error": {
                "message": "Device authorization is pending. Please try again.",
                "type": "invalid_request_error",
                "param": null,
                "code": "deviceauth_authorization_pending",
            }
        })
        .to_string(),
    }
}

fn json_body(request_body: &Option<String>) -> Value {
    serde_json::from_str(request_body.as_deref().expect("request has a body")).unwrap()
}

/// Decode a form-urlencoded component (`+`→space, then percent-decode), the
/// test-side analog of `URLSearchParams.get`.
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

fn form_param(request_body: &Option<String>, key: &str) -> Option<String> {
    let body = request_body.as_deref()?;
    for pair in body.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(percent_decode(v));
            }
        }
    }
    None
}

/// Drive the login machine through the select prompt into the device sub-flow,
/// returning the machine positioned right after the `device_code` notify Ack (the
/// first poll request), plus the recorded device_code notify. `now_ms` is the
/// fake clock time in force throughout the pre-poll steps.
fn start_device_flow(
    machine: &mut OpenAICodexLoginMachine,
    now_ms: i64,
    interval: &str,
    user_code: &str,
) -> AuthEvent {
    // start → select prompt.
    match machine.start(now_ms) {
        Step::Prompt {
            prompt:
                AuthPrompt {
                    kind: AuthPromptKind::Select { .. },
                    ..
                },
        } => {}
        other => panic!("expected select prompt, got {other:?}"),
    }

    // Input(device_code) → usercode request.
    let request = match machine.advance(
        StepInput::Input {
            value: DEVICE_CODE_LOGIN_METHOD.to_string(),
        },
        now_ms,
    ) {
        Step::Request { request } => request,
        other => panic!("expected usercode request, got {other:?}"),
    };
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, DEVICE_USER_CODE_URL);
    assert_eq!(
        request.headers.get("content-type").map(String::as_str),
        Some("application/json")
    );
    assert_eq!(json_body(&request.body), json!({ "client_id": CLIENT_ID }));

    // Response(usercode) → device_code notify.
    let usercode = HttpResponse::ok(
        json!({
            "device_auth_id": "device-auth-id",
            "user_code": user_code,
            "interval": interval,
        })
        .to_string(),
    );
    let event = match machine.advance(StepInput::Response(usercode), now_ms) {
        Step::Notify { event } => event,
        other => panic!("expected device_code notify, got {other:?}"),
    };
    event
}

/// pi `openai-codex-oauth.test.ts:75-176`: device happy path with poll timing.
/// The first poll is gated at `startTime`, the next at `startTime + interval`,
/// and `expires` is `startTime + interval + expires_in*1000`.
#[test]
fn device_code_happy_path_with_timing() {
    let access_token = create_access_token("account-123");
    let mut machine = OpenAICodexLoginMachine::with_pkce_bytes([7u8; 32]);
    let mut poll_times: Vec<i64> = Vec::new();
    let mut now = START_MS;

    let event = start_device_flow(&mut machine, now, "5", "ABCD-1234");
    assert_eq!(
        event,
        AuthEvent::DeviceCode {
            user_code: "ABCD-1234".to_string(),
            verification_uri: DEVICE_VERIFICATION_URI.to_string(),
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(900.0),
        }
    );

    // Ack → first poll request (gated at start).
    let poll1 = match machine.advance(StepInput::Ack, now) {
        Step::Request { request } => request,
        other => panic!("expected first poll request, got {other:?}"),
    };
    poll_times.push(now);
    assert_eq!(poll1.method, "POST");
    assert_eq!(poll1.url, DEVICE_TOKEN_URL);
    assert_eq!(
        poll1.headers.get("content-type").map(String::as_str),
        Some("application/json")
    );
    assert_eq!(
        json_body(&poll1.body),
        json!({ "device_auth_id": "device-auth-id", "user_code": "ABCD-1234" })
    );

    // First poll pending → Wait(5000) then the same poll request.
    let (delay, poll2) = match machine.advance(StepInput::Response(device_pending_response()), now)
    {
        Step::Wait { delay_ms, request } => (delay_ms, request),
        other => panic!("expected wait, got {other:?}"),
    };
    assert_eq!(delay, 5000);
    assert_eq!(poll2.url, DEVICE_TOKEN_URL);

    // The shim sleeps the interval, then polls again — success.
    now += delay as i64;
    poll_times.push(now);
    let success = HttpResponse::ok(
        json!({
            "authorization_code": "oauth-code",
            "code_challenge": "device-code-challenge",
            "code_verifier": "device-code-verifier",
        })
        .to_string(),
    );
    let exchange = match machine.advance(StepInput::Response(success), now) {
        Step::Request { request } => request,
        other => panic!("expected exchange request, got {other:?}"),
    };
    assert_eq!(exchange.method, "POST");
    assert_eq!(exchange.url, TOKEN_URL);
    assert_eq!(
        exchange.headers.get("content-type").map(String::as_str),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(
        form_param(&exchange.body, "grant_type").as_deref(),
        Some("authorization_code")
    );
    assert_eq!(
        form_param(&exchange.body, "client_id").as_deref(),
        Some(CLIENT_ID)
    );
    assert_eq!(
        form_param(&exchange.body, "code").as_deref(),
        Some("oauth-code")
    );
    assert_eq!(
        form_param(&exchange.body, "redirect_uri").as_deref(),
        Some(DEVICE_REDIRECT_URI)
    );
    assert_eq!(
        form_param(&exchange.body, "code_verifier").as_deref(),
        Some("device-code-verifier")
    );

    // Exchange response → Done with expiry (no skew) and accountId from the JWT.
    let token = HttpResponse::ok(
        json!({
            "access_token": access_token,
            "refresh_token": "refresh-token",
            "expires_in": 3600,
        })
        .to_string(),
    );
    let credential = match machine.advance(StepInput::Response(token), now) {
        Step::Done { credential } => credential,
        other => panic!("expected done, got {other:?}"),
    };
    assert_eq!(credential.access, access_token);
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(credential.expires, START_MS + 5000 + 3600 * 1000);
    assert_eq!(credential.extra.get("accountId").unwrap(), "account-123");
    assert_eq!(poll_times, vec![START_MS, START_MS + 5000]);
}

/// pi `openai-codex-oauth.test.ts:178-261`: the login machine offers the browser
/// option first, then runs the selected device sub-flow.
#[test]
fn offers_browser_login_first_then_runs_selected_device_flow() {
    let mut machine = OpenAICodexLoginMachine::with_pkce_bytes([1u8; 32]);
    let select = match machine.start(START_MS) {
        Step::Prompt { prompt } => prompt.kind,
        other => panic!("expected select prompt, got {other:?}"),
    };
    match select {
        AuthPromptKind::Select { message, options } => {
            assert_eq!(message, "Select OpenAI Codex login method:");
            assert_eq!(
                options,
                vec![
                    AuthSelectOption {
                        id: "browser".to_string(),
                        label: "Browser login (default)".to_string(),
                        description: None,
                    },
                    AuthSelectOption {
                        id: "device_code".to_string(),
                        label: "Device code login (headless)".to_string(),
                        description: None,
                    },
                ]
            );
        }
        other => panic!("expected select, got {other:?}"),
    }

    // Selecting device_code drives the device sub-flow (usercode → notify).
    let event = {
        // Rebuild from the select we already consumed: continue on this machine.
        let request = match machine.advance(
            StepInput::Input {
                value: DEVICE_CODE_LOGIN_METHOD.to_string(),
            },
            START_MS,
        ) {
            Step::Request { request } => request,
            other => panic!("expected usercode request, got {other:?}"),
        };
        assert_eq!(request.url, DEVICE_USER_CODE_URL);
        let usercode = HttpResponse::ok(
            json!({
                "device_auth_id": "device-auth-id",
                "user_code": "WXYZ-7890",
                "interval": "5",
            })
            .to_string(),
        );
        match machine.advance(StepInput::Response(usercode), START_MS) {
            Step::Notify { event } => event,
            other => panic!("expected device_code notify, got {other:?}"),
        }
    };
    assert_eq!(
        event,
        AuthEvent::DeviceCode {
            user_code: "WXYZ-7890".to_string(),
            verification_uri: DEVICE_VERIFICATION_URI.to_string(),
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(900.0),
        }
    );
}

/// pi `openai-codex-oauth.test.ts:263-272`: cancelling the method-selection
/// prompt surfaces "Login cancelled". Driven via [`run_login`] with a prompt that
/// errors, which the driver has no abort for — so the flow errors with the
/// prompt's message.
#[test]
fn cancels_when_method_selection_is_cancelled() {
    struct CancellingInteraction;
    impl AuthInteraction for CancellingInteraction {
        fn prompt(&self, _prompt: AuthPrompt) -> Result<String, AuthFlowError> {
            Err(AuthFlowError::new("Login cancelled"))
        }
        fn notify(&self, _event: AuthEvent) {}
    }

    let auth = OpenAICodexOAuth::new();
    let transport = ScriptedTransport::new();
    let clock = FakeClock::new(START_MS);
    let err = run_login(
        &auth,
        &transport,
        &clock,
        &clock,
        &CancellingInteraction,
        None,
    )
    .unwrap_err();
    assert_eq!(err.message, "Login cancelled");
}

/// pi `openai-codex-oauth.test.ts:274-317`: aborting while waiting between polls
/// surfaces "Login cancelled". Mirrors the shim feeding [`StepInput::Aborted`]
/// during the wait after the first pending poll.
#[test]
fn cancels_device_flow_while_waiting() {
    let mut machine = OpenAICodexLoginMachine::with_pkce_bytes([2u8; 32]);
    let now = START_MS;
    start_device_flow(&mut machine, now, "5", "ABCD-1234");

    // Ack → first poll; pending → Wait.
    match machine.advance(StepInput::Ack, now) {
        Step::Request { .. } => {}
        other => panic!("expected first poll, got {other:?}"),
    }
    match machine.advance(StepInput::Response(device_pending_response()), now) {
        Step::Wait { .. } => {}
        other => panic!("expected wait, got {other:?}"),
    }

    // Aborted mid-wait → Login cancelled.
    match machine.advance(StepInput::Aborted, now) {
        Step::Error { message } => assert_eq!(message, CANCEL_MESSAGE),
        other => panic!("expected cancel error, got {other:?}"),
    }
}

/// pi `openai-codex-oauth.test.ts:319-360`: the device flow times out after 15
/// minutes with every poll pending (interval 60). Steps the machine through the
/// full deadline, advancing the simulated clock by each Wait's delay.
#[test]
fn device_flow_times_out_after_fifteen_minutes() {
    let mut machine = OpenAICodexLoginMachine::with_pkce_bytes([3u8; 32]);
    let mut now = START_MS;
    start_device_flow(&mut machine, now, "60", "ABCD-1234");

    // Ack → first poll (at start).
    match machine.advance(StepInput::Ack, now) {
        Step::Request { .. } => {}
        other => panic!("expected first poll, got {other:?}"),
    }

    // Feed pending responses; each Wait advances the clock by its delay until the
    // deadline is reached.
    let mut poll_count = 1;
    let error_message = loop {
        match machine.advance(StepInput::Response(device_pending_response()), now) {
            Step::Wait { delay_ms, .. } => {
                assert_eq!(delay_ms, 60_000);
                now += delay_ms as i64;
                poll_count += 1;
            }
            Step::Error { message } => break message,
            other => panic!("unexpected step: {other:?}"),
        }
    };
    assert_eq!(error_message, TIMEOUT_MESSAGE);
    // 900s / 60s = 15 waits; the poll at the exact deadline then times out.
    assert!(poll_count >= 15, "poll_count = {poll_count}");
    assert_eq!(now, START_MS + 900_000);
}

/// pi `openai-codex-oauth.test.ts:362-422`: 403 and 404 poll responses are both
/// treated as pending (interval 1s), then a success completes the flow.
#[test]
fn treats_403_and_404_poll_responses_as_pending() {
    let access_token = create_access_token("account-403-404");
    let mut machine = OpenAICodexLoginMachine::with_pkce_bytes([4u8; 32]);
    let mut now = START_MS;
    let mut poll_times: Vec<i64> = Vec::new();

    start_device_flow(&mut machine, now, "1", "ABCD-1234");
    match machine.advance(StepInput::Ack, now) {
        Step::Request { .. } => {}
        other => panic!("expected first poll, got {other:?}"),
    }
    poll_times.push(now);

    // Poll 1: 403 with a JSON body → pending → Wait(1000).
    let resp_403 = HttpResponse {
        status: 403,
        headers: Default::default(),
        body: json!({ "error": "access_denied", "error_description": "denied" }).to_string(),
    };
    let delay = match machine.advance(StepInput::Response(resp_403), now) {
        Step::Wait { delay_ms, .. } => delay_ms,
        other => panic!("expected wait after 403, got {other:?}"),
    };
    assert_eq!(delay, 1000);
    now += delay as i64;
    poll_times.push(now);

    // Poll 2: 404 with a plain-text body → pending → Wait(1000).
    let resp_404 = HttpResponse {
        status: 404,
        headers: Default::default(),
        body: "not ready".to_string(),
    };
    let delay = match machine.advance(StepInput::Response(resp_404), now) {
        Step::Wait { delay_ms, .. } => delay_ms,
        other => panic!("expected wait after 404, got {other:?}"),
    };
    assert_eq!(delay, 1000);
    now += delay as i64;
    poll_times.push(now);

    // Poll 3: success → exchange → Done.
    let success = HttpResponse::ok(
        json!({
            "authorization_code": "oauth-code",
            "code_challenge": "device-code-challenge",
            "code_verifier": "device-code-verifier",
        })
        .to_string(),
    );
    match machine.advance(StepInput::Response(success), now) {
        Step::Request { request } => assert_eq!(request.url, TOKEN_URL),
        other => panic!("expected exchange request, got {other:?}"),
    }
    let token = HttpResponse::ok(
        json!({
            "access_token": access_token,
            "refresh_token": "refresh-token",
            "expires_in": 3600,
        })
        .to_string(),
    );
    let credential = match machine.advance(StepInput::Response(token), now) {
        Step::Done { credential } => credential,
        other => panic!("expected done, got {other:?}"),
    };
    assert_eq!(credential.access, access_token);
    assert_eq!(
        credential.extra.get("accountId").unwrap(),
        "account-403-404"
    );
    assert_eq!(poll_times.len(), 3);
}

/// pi `openai-codex-oauth.test.ts:424-450`: a fatal (500) poll response surfaces
/// the status and body verbatim.
#[test]
fn includes_response_body_in_poll_failures() {
    let mut machine = OpenAICodexLoginMachine::with_pkce_bytes([5u8; 32]);
    let now = START_MS;
    start_device_flow(&mut machine, now, "5", "ABCD-1234");
    match machine.advance(StepInput::Ack, now) {
        Step::Request { .. } => {}
        other => panic!("expected first poll, got {other:?}"),
    }

    let resp_500 = HttpResponse {
        status: 500,
        headers: Default::default(),
        body: json!({ "error": "server_error", "error_description": "try again later" })
            .to_string(),
    };
    match machine.advance(StepInput::Response(resp_500), now) {
        Step::Error { message } => assert_eq!(
            message,
            "OpenAI Codex device auth failed with status 500: {\"error\":\"server_error\",\"error_description\":\"try again later\"}"
        ),
        other => panic!("expected fatal error, got {other:?}"),
    }
}

/// pi `openai-codex-oauth.test.ts:452-478`: a 401 refresh surfaces pi's exact
/// message. The port never writes to stderr (no logging on this path at all).
#[test]
fn refresh_401_surfaces_message_quietly() {
    let mut machine = OpenAICodexRefreshMachine::new("invalid-refresh-token");
    let request = match machine.start(START_MS) {
        Step::Request { request } => request,
        other => panic!("expected refresh request, got {other:?}"),
    };
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, TOKEN_URL);
    assert_eq!(
        form_param(&request.body, "grant_type").as_deref(),
        Some("refresh_token")
    );
    assert_eq!(
        form_param(&request.body, "refresh_token").as_deref(),
        Some("invalid-refresh-token")
    );
    assert_eq!(
        form_param(&request.body, "client_id").as_deref(),
        Some(CLIENT_ID)
    );

    let resp_401 = HttpResponse {
        status: 401,
        headers: Default::default(),
        body: json!({
            "error": {
                "message": "Could not validate your token. Please try signing in again.",
                "type": "invalid_request_error",
            }
        })
        .to_string(),
    };
    match machine.advance(StepInput::Response(resp_401), START_MS) {
        Step::Error { message } => {
            assert!(
                message.contains("OpenAI Codex token refresh failed (401)"),
                "message: {message}"
            );
            assert!(
                message.contains("Could not validate your token"),
                "message: {message}"
            );
        }
        other => panic!("expected 401 error, got {other:?}"),
    }
}

/// A refresh that succeeds extracts the accountId from the new access token and
/// applies pi's no-skew expiry (`openai-codex.ts:506-508,402-415`).
#[test]
fn refresh_success_extracts_account_id() {
    let access_token = create_access_token("account-456");
    let mut machine = OpenAICodexRefreshMachine::new("refresh-token");
    machine.start(START_MS);
    let token = HttpResponse::ok(
        json!({
            "access_token": access_token,
            "refresh_token": "new-refresh",
            "expires_in": 3600,
        })
        .to_string(),
    );
    let credential = match machine.advance(StepInput::Response(token), START_MS) {
        Step::Done { credential } => credential,
        other => panic!("expected done, got {other:?}"),
    };
    assert_eq!(credential.refresh, "new-refresh");
    assert_eq!(credential.expires, START_MS + 3600 * 1000);
    assert_eq!(credential.extra.get("accountId").unwrap(), "account-456");
}

/// A token response whose access token lacks the account claim errors with pi's
/// exact message (`openai-codex.ts:404-406`).
#[test]
fn missing_account_id_errors() {
    let no_account = {
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
        let header = b64(&json!({ "alg": "none" }).to_string());
        let payload = b64(&json!({ "sub": "user" }).to_string());
        format!("{header}.{payload}.sig")
    };
    let mut machine = OpenAICodexRefreshMachine::new("refresh-token");
    machine.start(START_MS);
    let token = HttpResponse::ok(
        json!({
            "access_token": no_account,
            "refresh_token": "r",
            "expires_in": 3600,
        })
        .to_string(),
    );
    match machine.advance(StepInput::Response(token), START_MS) {
        Step::Error { message } => assert_eq!(message, "Failed to extract accountId from token"),
        other => panic!("expected accountId error, got {other:?}"),
    }
}

/// A 404 user-code response reports that device login is not enabled
/// (`openai-codex.ts:198-208`).
#[test]
fn usercode_404_reports_device_login_disabled() {
    let mut machine = OpenAICodexLoginMachine::with_pkce_bytes([6u8; 32]);
    let now = START_MS;
    match machine.start(now) {
        Step::Prompt { .. } => {}
        other => panic!("expected select, got {other:?}"),
    }
    match machine.advance(
        StepInput::Input {
            value: DEVICE_CODE_LOGIN_METHOD.to_string(),
        },
        now,
    ) {
        Step::Request { .. } => {}
        other => panic!("expected usercode request, got {other:?}"),
    }
    let resp_404 = HttpResponse {
        status: 404,
        headers: Default::default(),
        body: String::new(),
    };
    match machine.advance(StepInput::Response(resp_404), now) {
        Step::Error { message } => assert!(
            message.contains("device code login is not enabled"),
            "message: {message}"
        ),
        other => panic!("expected disabled error, got {other:?}"),
    }
}

/// An [`AuthInteraction`] that selects the browser method and answers the
/// `manual_code` prompt with a callback URL built from the recorded `auth_url` —
/// mirroring pi's manual-code browser path.
struct BrowserInteraction {
    events: std::sync::Mutex<Vec<AuthEvent>>,
    auth_url: std::sync::Mutex<Option<String>>,
}

impl BrowserInteraction {
    fn new() -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
            auth_url: std::sync::Mutex::new(None),
        }
    }
}

impl AuthInteraction for BrowserInteraction {
    fn prompt(&self, prompt: AuthPrompt) -> Result<String, AuthFlowError> {
        match prompt.kind {
            AuthPromptKind::Select { .. } => Ok(BROWSER_LOGIN_METHOD.to_string()),
            AuthPromptKind::ManualCode { .. } => {
                let url = self
                    .auth_url
                    .lock()
                    .unwrap()
                    .clone()
                    .expect("auth_url first");
                let state = query_param(&url, "state").expect("state");
                let redirect = query_param(&url, "redirect_uri").expect("redirect_uri");
                Ok(format!("{redirect}?code=browser-code&state={state}"))
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

/// The native [`run_login`] driver reaches `Done` end-to-end over the browser
/// manual-code path with [`ScriptedTransport`] + [`FakeClock`] + a recording
/// [`AuthInteraction`]. This browser path has no `Wait`, so it drives fully
/// synchronously (the device path's timing is covered by the stepping tests).
#[test]
fn run_login_browser_path_reaches_done_end_to_end() {
    let access_token = create_access_token("account-browser");
    let auth = OpenAICodexOAuth::new();
    let transport = ScriptedTransport::new();
    transport.push_ok(
        json!({
            "access_token": access_token,
            "refresh_token": "refresh",
            "expires_in": 3600,
        })
        .to_string(),
    );
    let clock = FakeClock::new(START_MS);
    let interaction = BrowserInteraction::new();

    let credential = run_login(&auth, &transport, &clock, &clock, &interaction, None).unwrap();

    assert_eq!(credential.access, access_token);
    assert_eq!(credential.refresh, "refresh");
    assert_eq!(credential.expires, START_MS + 3600 * 1000);
    assert_eq!(
        credential.extra.get("accountId").unwrap(),
        "account-browser"
    );

    // Exactly one token request went out, to the browser redirect URI.
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, TOKEN_URL);
    assert_eq!(
        form_param(&requests[0].body, "code").as_deref(),
        Some("browser-code")
    );
    assert_eq!(
        form_param(&requests[0].body, "redirect_uri").as_deref(),
        Some(REDIRECT_URI)
    );

    // The auth_url event was surfaced with pi's extra query params.
    let events = interaction.events.lock().unwrap();
    let auth_url = events.iter().find_map(|e| match e {
        AuthEvent::AuthUrl { url, .. } => Some(url.clone()),
        _ => None,
    });
    let auth_url = auth_url.expect("auth_url event");
    assert_eq!(query_param(&auth_url, "originator").as_deref(), Some("pi"));
    assert_eq!(
        query_param(&auth_url, "codex_cli_simplified_flow").as_deref(),
        Some("true")
    );
    assert_eq!(
        query_param(&auth_url, "id_token_add_organizations").as_deref(),
        Some("true")
    );
}

/// A browser paste whose state does not match aborts with "State mismatch"
/// (`openai-codex.ts:481`).
#[test]
fn browser_state_mismatch_errors() {
    let mut machine = OpenAICodexLoginMachine::with_pkce_bytes([9u8; 32]);
    machine.start(START_MS);
    machine.advance(
        StepInput::Input {
            value: BROWSER_LOGIN_METHOD.to_string(),
        },
        START_MS,
    );
    match machine.advance(StepInput::Ack, START_MS) {
        Step::Prompt { .. } => {}
        other => panic!("expected manual_code prompt, got {other:?}"),
    }
    let pasted = format!("{REDIRECT_URI}?code=c&state=not-the-state");
    match machine.advance(StepInput::Input { value: pasted }, START_MS) {
        Step::Error { message } => assert_eq!(message, "State mismatch"),
        other => panic!("expected state-mismatch error, got {other:?}"),
    }
}

/// An unknown login method errors with pi's message (`openai-codex.ts:527`).
#[test]
fn unknown_login_method_errors() {
    let mut machine = OpenAICodexLoginMachine::with_pkce_bytes([10u8; 32]);
    machine.start(START_MS);
    match machine.advance(
        StepInput::Input {
            value: "carrier-pigeon".to_string(),
        },
        START_MS,
    ) {
        Step::Error { message } => {
            assert_eq!(message, "Unknown OpenAI Codex login method: carrier-pigeon")
        }
        other => panic!("expected unknown-method error, got {other:?}"),
    }
}

/// [`parse_authorization_input`] across all four pi branches
/// (`openai-codex.ts:73-101`).
#[test]
fn parse_authorization_input_covers_all_branches() {
    assert_eq!(
        parse_authorization_input("http://localhost:1455/auth/callback?code=abc&state=xyz"),
        ParsedAuthInput {
            code: Some("abc".into()),
            state: Some("xyz".into()),
        }
    );
    assert_eq!(
        parse_authorization_input("http://localhost:1455/auth/callback"),
        ParsedAuthInput::default()
    );
    assert_eq!(
        parse_authorization_input("the-code#the-state"),
        ParsedAuthInput {
            code: Some("the-code".into()),
            state: Some("the-state".into()),
        }
    );
    assert_eq!(
        parse_authorization_input("code=c1&state=s1"),
        ParsedAuthInput {
            code: Some("c1".into()),
            state: Some("s1".into()),
        }
    );
    assert_eq!(
        parse_authorization_input("  just-a-code  "),
        ParsedAuthInput {
            code: Some("just-a-code".into()),
            state: None,
        }
    );
    assert_eq!(parse_authorization_input("   "), ParsedAuthInput::default());
}

/// `to_auth` maps the access token to `apiKey` (`openai-codex.ts:535-537`).
#[test]
fn to_auth_maps_access_to_api_key() {
    let auth = OpenAICodexOAuth::new();
    let credential = OAuthCredential {
        refresh: "r".into(),
        access: "the-access-token".into(),
        expires: 0,
        extra: Default::default(),
    };
    let model_auth = crate::auth::types::OAuthAuth::to_auth(&auth, &credential).unwrap();
    assert_eq!(model_auth.api_key.as_deref(), Some("the-access-token"));
}
