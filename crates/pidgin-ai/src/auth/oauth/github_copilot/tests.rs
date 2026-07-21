// straitjacket-allow-file:duplication — these tests mirror pi's
// `github-copilot-oauth.test.ts` case-by-case: each `#[test]` rebuilds a similar
// machine-driving scaffold (prompt → device-code → notify → poll loop → copilot
// token → enable-all → models) so the login/refresh paths are exercised in
// isolation. The clone detector reads the repeated scaffolding as duplication; it
// is deliberate, load-bearing per-case fixtures kept parallel to the pi test they
// transcribe.
//! Unit tests for the GitHub Copilot OAuth flow, mirroring pi-ai's
//! `packages/ai/test/github-copilot-oauth.test.ts` at pinned commit `3da591ab`.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::{
    base_url_from_token, get_github_copilot_base_url, normalize_domain, GitHubCopilotLoginMachine,
    GitHubCopilotOAuth, GitHubCopilotRefreshMachine, GITHUB_COPILOT_MODELS, REFRESH_SKEW_MS,
};
use crate::auth::error::AuthFlowError;
use crate::auth::oauth::flow::{run_login, OAuthFlowMachine, Step, StepInput};
use crate::auth::types::{
    AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, OAuthAuth, OAuthCredential,
};
use crate::seams::clock::{Clock, FakeClock, TimerId, Timers};
use crate::seams::http::{HttpResponse, ScriptedTransport};

/// A copilot token whose `proxy-ep` resolves to the individual API host.
const COPILOT_TOKEN: &str = "tid=test;exp=9999999999;proxy-ep=proxy.individual.githubcopilot.com;";
const COPILOT_EXPIRES_AT: i64 = 9_999_999_999;

fn copilot_token_body() -> String {
    json!({ "token": COPILOT_TOKEN, "expires_at": COPILOT_EXPIRES_AT }).to_string()
}

/// The three-model catalog pi uses in the refresh test: only `gpt-4.1` survives
/// the picker/policy/tool-call filter (`github-copilot-oauth.test.ts:76-95`).
fn picker_catalog_body() -> String {
    json!({
        "data": [
            { "id": "gpt-4.1", "model_picker_enabled": true,
              "capabilities": { "supports": { "tool_calls": true } } },
            { "id": "claude-opus-4.7", "model_picker_enabled": true,
              "policy": { "state": "disabled" },
              "capabilities": { "supports": { "tool_calls": true } } },
            { "id": "gpt-5.4-nano", "model_picker_enabled": false,
              "capabilities": { "supports": { "tool_calls": true } } },
        ]
    })
    .to_string()
}

/// The copilot-token + per-model policy + models responses that follow a
/// successful device-code poll, so a login run reaches `Done`.
fn login_tail(models_body: &str) -> Vec<HttpResponse> {
    let mut responses = vec![HttpResponse::ok(copilot_token_body())];
    for _ in 0..GITHUB_COPILOT_MODELS.len() {
        responses.push(HttpResponse::ok(""));
    }
    responses.push(HttpResponse::ok(models_body.to_string()));
    responses
}

/// The result of driving a login machine to a terminal step.
struct DriveOutcome {
    poll_times: Vec<i64>,
    events: Vec<AuthEvent>,
    result: Result<OAuthCredential, String>,
}

/// Drive `machine` to a terminal step against a [`FakeClock`], advancing virtual
/// time by each [`Step::Wait`]'s delay (pi's `advanceTimersByTime`) and recording
/// `Date.now()` at every access-token poll. Notifies are auto-acked and the
/// enterprise prompt is answered with `prompt_value`.
fn drive_login(
    machine: &mut dyn OAuthFlowMachine,
    start_now: i64,
    responses: Vec<HttpResponse>,
    prompt_value: &str,
) -> DriveOutcome {
    let clock = FakeClock::new(start_now);
    let mut responses = responses.into_iter();
    let mut poll_times = Vec::new();
    let mut events = Vec::new();
    let is_poll = |request: &crate::seams::http::HttpRequest| {
        request.url.ends_with("/login/oauth/access_token")
    };
    let mut step = machine.start(clock.now_ms());
    loop {
        step = match step {
            Step::Prompt { .. } => machine.advance(
                StepInput::Input {
                    value: prompt_value.to_string(),
                },
                clock.now_ms(),
            ),
            Step::Notify { event } => {
                events.push(event);
                machine.advance(StepInput::Ack, clock.now_ms())
            }
            Step::Request { request } => {
                if is_poll(&request) {
                    poll_times.push(clock.now_ms());
                }
                let response = responses.next().expect("scripted response for request");
                machine.advance(StepInput::Response(response), clock.now_ms())
            }
            Step::Wait { delay_ms, request } => {
                clock.advance(delay_ms);
                if is_poll(&request) {
                    poll_times.push(clock.now_ms());
                }
                let response = responses.next().expect("scripted response for poll");
                machine.advance(StepInput::Response(response), clock.now_ms())
            }
            Step::Done { credential } => {
                return DriveOutcome {
                    poll_times,
                    events,
                    result: Ok(credential),
                }
            }
            Step::Error { message } => {
                return DriveOutcome {
                    poll_times,
                    events,
                    result: Err(message),
                }
            }
        };
    }
}

fn find_device_code(events: &[AuthEvent]) -> Option<&AuthEvent> {
    events
        .iter()
        .find(|event| matches!(event, AuthEvent::DeviceCode { .. }))
}

/// pi `github-copilot-oauth.test.ts:117-173`: the device-code details reach
/// `onDeviceCode` verbatim.
#[test]
fn login_reports_device_code_details() {
    let mut machine = GitHubCopilotLoginMachine::new();
    let mut responses = vec![
        HttpResponse::ok(
            json!({
                "device_code": "device-code",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://github.com/login/device",
                "interval": 1,
                "expires_in": 900,
            })
            .to_string(),
        ),
        HttpResponse::ok(json!({ "access_token": "ghu_refresh_token" }).to_string()),
    ];
    responses.extend(login_tail("{\"data\":[]}"));

    let outcome = drive_login(&mut machine, 0, responses, "");
    assert!(outcome.result.is_ok(), "login should complete");

    match find_device_code(&outcome.events).expect("device_code event") {
        AuthEvent::DeviceCode {
            user_code,
            verification_uri,
            interval_seconds,
            expires_in_seconds,
        } => {
            assert_eq!(user_code, "ABCD-EFGH");
            assert_eq!(verification_uri, "https://github.com/login/device");
            assert_eq!(*interval_seconds, Some(1.0));
            assert_eq!(*expires_in_seconds, Some(900.0));
        }
        other => panic!("expected device_code, got {other:?}"),
    }
}

/// pi `github-copilot-oauth.test.ts:175-202`: a non-http(s) `verification_uri` is
/// rejected before it reaches `onDeviceCode`.
#[test]
fn login_rejects_untrusted_verification_uri() {
    let mut machine = GitHubCopilotLoginMachine::new();
    let responses = vec![HttpResponse::ok(
        json!({
            "device_code": "device-code",
            "user_code": "ABCD-EFGH",
            "verification_uri": "$(id>/tmp/pwned)",
            "interval": 1,
            "expires_in": 900,
        })
        .to_string(),
    )];

    let outcome = drive_login(&mut machine, 0, responses, "");
    match outcome.result {
        Err(message) => assert!(
            message.contains("Untrusted verification_uri"),
            "message: {message}"
        ),
        Ok(_) => panic!("expected untrusted-uri error"),
    }
    assert!(
        find_device_code(&outcome.events).is_none(),
        "device_code must not be surfaced for an untrusted uri"
    );
}

/// pi `github-copilot-oauth.test.ts:204-264`: the `verification_uri` is
/// normalised to its `href` before it reaches `onDeviceCode`.
#[test]
fn login_normalizes_verification_uri() {
    let raw = "https://github.com/login/\u{1b}]8;;evil";
    let normalized = "https://github.com/login/%1B]8;;evil";
    assert_ne!(raw, normalized, "the raw uri must differ from its href");

    let mut machine = GitHubCopilotLoginMachine::new();
    let mut responses = vec![
        HttpResponse::ok(
            json!({
                "device_code": "device-code",
                "user_code": "ABCD-EFGH",
                "verification_uri": raw,
                "interval": 1,
                "expires_in": 900,
            })
            .to_string(),
        ),
        HttpResponse::ok(json!({ "access_token": "ghu_refresh_token" }).to_string()),
    ];
    responses.extend(login_tail("{\"data\":[]}"));

    let outcome = drive_login(&mut machine, 0, responses, "");
    match find_device_code(&outcome.events).expect("device_code event") {
        AuthEvent::DeviceCode {
            verification_uri, ..
        } => {
            assert_eq!(verification_uri, normalized);
            assert_ne!(verification_uri, raw);
        }
        other => panic!("expected device_code, got {other:?}"),
    }
}

/// pi `github-copilot-oauth.test.ts:266-353`: wait-before-first-poll, then a
/// server `slow_down` of 7s bumps the interval. Poll times: `[+5000, +10000,
/// +17000]`.
#[test]
fn login_waits_before_polling_and_bumps_interval_after_slow_down() {
    let mut machine = GitHubCopilotLoginMachine::new();
    let mut responses = vec![
        HttpResponse::ok(
            json!({
                "device_code": "device-code",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://github.com/login/device",
                "interval": 5,
                "expires_in": 900,
            })
            .to_string(),
        ),
        HttpResponse::ok(
            json!({ "error": "authorization_pending", "error_description": "pending" }).to_string(),
        ),
        HttpResponse::ok(
            json!({ "error": "slow_down", "error_description": "slow down", "interval": 7 })
                .to_string(),
        ),
        HttpResponse::ok(json!({ "access_token": "ghu_refresh_token" }).to_string()),
    ];
    responses.extend(login_tail("{\"data\":[]}"));

    let outcome = drive_login(&mut machine, 0, responses, "");
    assert!(outcome.result.is_ok(), "login should complete");
    assert_eq!(outcome.poll_times, vec![5000, 10000, 17000]);
}

/// pi `github-copilot-oauth.test.ts:355-401`: repeated `slow_down` past
/// `expires_in` times out. Poll times: `[+5000, +15000]`.
#[test]
fn login_times_out_after_repeated_slow_down() {
    let mut machine = GitHubCopilotLoginMachine::new();
    let responses = vec![
        HttpResponse::ok(
            json!({
                "device_code": "device-code",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://github.com/login/device",
                "interval": 5,
                "expires_in": 25,
            })
            .to_string(),
        ),
        HttpResponse::ok(
            json!({ "error": "slow_down", "error_description": "slow down" }).to_string(),
        ),
        HttpResponse::ok(
            json!({ "error": "slow_down", "error_description": "still too fast" }).to_string(),
        ),
    ];

    let outcome = drive_login(&mut machine, 0, responses, "");
    match outcome.result {
        Err(message) => assert!(
            message.contains("Device flow timed out after one or more slow_down responses"),
            "message: {message}"
        ),
        Ok(_) => panic!("expected slow_down timeout"),
    }
    assert_eq!(outcome.poll_times, vec![5000, 15000]);
}

/// pi `github-copilot-oauth.test.ts:61-115`: refresh exchanges the copilot token
/// and filters the catalog to the picker-enabled ids (`["gpt-4.1"]`).
#[test]
fn refresh_filters_models_to_picker_catalog() {
    let credential = OAuthCredential {
        refresh: "ghu_refresh_token".to_string(),
        access: "old-access-token".to_string(),
        expires: 0,
        extra: Default::default(),
    };
    let mut machine = GitHubCopilotRefreshMachine::new(&credential);

    // start → copilot-token GET at the public API host, bearer = refresh token.
    let request = match machine.start(0) {
        Step::Request { request } => request,
        other => panic!("expected copilot-token request, got {other:?}"),
    };
    assert_eq!(request.method, "GET");
    assert_eq!(
        request.url,
        "https://api.github.com/copilot_internal/v2/token"
    );
    assert_eq!(
        request.headers.get("Authorization").map(String::as_str),
        Some("Bearer ghu_refresh_token")
    );

    // copilot-token response → models GET at the proxy-derived host.
    let request = match machine.advance(
        StepInput::Response(HttpResponse::ok(copilot_token_body())),
        0,
    ) {
        Step::Request { request } => request,
        other => panic!("expected models request, got {other:?}"),
    };
    assert_eq!(request.method, "GET");
    assert_eq!(
        request.url,
        "https://api.individual.githubcopilot.com/models"
    );
    assert_eq!(
        request.headers.get("Authorization").map(String::as_str),
        Some(format!("Bearer {COPILOT_TOKEN}").as_str())
    );
    assert_eq!(
        request
            .headers
            .get("X-GitHub-Api-Version")
            .map(String::as_str),
        Some("2026-06-01")
    );

    // models response → Done with the filtered ids.
    let credential = match machine.advance(
        StepInput::Response(HttpResponse::ok(picker_catalog_body())),
        0,
    ) {
        Step::Done { credential } => credential,
        other => panic!("expected done, got {other:?}"),
    };
    assert_eq!(credential.refresh, "ghu_refresh_token");
    assert_eq!(credential.access, COPILOT_TOKEN);
    assert_eq!(
        credential.expires,
        COPILOT_EXPIRES_AT * 1000 - REFRESH_SKEW_MS
    );
    let available = credential
        .extra
        .get("availableModelIds")
        .and_then(Value::as_array)
        .expect("availableModelIds array");
    assert_eq!(available, &vec![Value::String("gpt-4.1".to_string())]);
}

/// A test clock that fires each scheduled timer synchronously, advancing `now`
/// by the timer's delay. This lets [`run_login`] drive the poll loop without a
/// second thread to pump a deterministic clock.
#[derive(Clone)]
struct ImmediateClock {
    now: Arc<Mutex<i64>>,
}

impl ImmediateClock {
    fn new(start_ms: i64) -> Self {
        Self {
            now: Arc::new(Mutex::new(start_ms)),
        }
    }
}

impl Clock for ImmediateClock {
    fn now_ms(&self) -> i64 {
        *self.now.lock().unwrap()
    }
}

impl Timers for ImmediateClock {
    fn set_timeout(&self, delay_ms: u64, callback: Box<dyn FnOnce() + Send>) -> TimerId {
        *self.now.lock().unwrap() += delay_ms as i64;
        callback();
        TimerId(0)
    }
    fn clear(&self, _id: TimerId) {}
}

/// An [`AuthInteraction`] that answers the enterprise prompt blank and records
/// notifies (mirroring the pi test's inline interaction).
struct RecordingInteraction {
    events: Mutex<Vec<AuthEvent>>,
}

impl RecordingInteraction {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }
}

impl AuthInteraction for RecordingInteraction {
    fn prompt(&self, prompt: AuthPrompt) -> Result<String, AuthFlowError> {
        match prompt.kind {
            AuthPromptKind::Text { .. } => Ok(String::new()),
            other => Err(AuthFlowError::new(format!("unexpected prompt: {other:?}"))),
        }
    }
    fn notify(&self, event: AuthEvent) {
        self.events.lock().unwrap().push(event);
    }
}

/// The native [`run_login`] driver reaches `Done` end-to-end over
/// [`ScriptedTransport`] + [`ImmediateClock`] + a recording interaction, covering
/// the full login chain (device → poll loop → copilot token → enable-all →
/// models).
#[test]
fn run_login_driver_reaches_done_end_to_end() {
    let auth = GitHubCopilotOAuth::new();
    let transport = ScriptedTransport::new();

    // Requests in order: device-code, poll (pending, slow_down, success),
    // copilot-token, one policy POST per model, then the models fetch.
    transport.push_ok(
        json!({
            "device_code": "device-code",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://github.com/login/device",
            "interval": 5,
            "expires_in": 900,
        })
        .to_string(),
    );
    transport.push_ok(json!({ "error": "authorization_pending" }).to_string());
    transport.push_ok(json!({ "error": "slow_down", "interval": 7 }).to_string());
    transport.push_ok(json!({ "access_token": "ghu_refresh_token" }).to_string());
    for response in login_tail(&picker_catalog_body()) {
        transport.push_response(Ok(response));
    }

    let clock = ImmediateClock::new(1_700_000_000_000);
    let interaction = RecordingInteraction::new();

    let credential = run_login(&auth, &transport, &clock, &clock, &interaction, None).unwrap();

    assert_eq!(credential.refresh, "ghu_refresh_token");
    assert_eq!(credential.access, COPILOT_TOKEN);
    let available = credential
        .extra
        .get("availableModelIds")
        .and_then(Value::as_array)
        .expect("availableModelIds array");
    assert_eq!(available, &vec![Value::String("gpt-4.1".to_string())]);

    // The device-code and progress notifies were surfaced.
    let events = interaction.events.lock().unwrap();
    assert!(events
        .iter()
        .any(|event| matches!(event, AuthEvent::DeviceCode { .. })));
    assert!(events.iter().any(
        |event| matches!(event, AuthEvent::Progress { message } if message == "Enabling models...")
    ));

    // The device-code POST + 3 polls + copilot-token + N policy POSTs + models.
    let requests = transport.requests();
    assert_eq!(requests.len(), 6 + GITHUB_COPILOT_MODELS.len());
}

/// An `Aborted` input mid-flow yields the "Login cancelled" error.
#[test]
fn login_aborts_with_cancel_message() {
    let mut machine = GitHubCopilotLoginMachine::new();
    machine.start(0);
    match machine.advance(StepInput::Aborted, 0) {
        Step::Error { message } => assert_eq!(message, "Login cancelled"),
        other => panic!("expected cancel error, got {other:?}"),
    }
}

/// An invalid enterprise URL aborts the login before any request.
#[test]
fn login_rejects_invalid_enterprise_domain() {
    let mut machine = GitHubCopilotLoginMachine::new();
    machine.start(0);
    match machine.advance(
        StepInput::Input {
            value: "http://".to_string(),
        },
        0,
    ) {
        Step::Error { message } => assert_eq!(message, "Invalid GitHub Enterprise URL/domain"),
        other => panic!("expected invalid-domain error, got {other:?}"),
    }
}

/// The device-code POST targets the enterprise host when one is supplied.
#[test]
fn login_uses_enterprise_domain_for_endpoints() {
    let mut machine = GitHubCopilotLoginMachine::new();
    machine.start(0);
    let request = match machine.advance(
        StepInput::Input {
            value: "company.ghe.com".to_string(),
        },
        0,
    ) {
        Step::Request { request } => request,
        other => panic!("expected device-code request, got {other:?}"),
    };
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, "https://company.ghe.com/login/device/code");
    assert_eq!(
        request.headers.get("Content-Type").map(String::as_str),
        Some("application/x-www-form-urlencoded")
    );
    let body = request.body.as_deref().unwrap_or_default();
    assert!(body.contains("client_id="), "body: {body}");
    assert!(body.contains("scope=read%3Auser"), "body: {body}");
}

/// The access-token poll body form-encodes the grant type and device code, as pi
/// asserts (`github-copilot-oauth.test.ts:301-303`).
#[test]
fn poll_request_body_matches_pi() {
    let mut machine = GitHubCopilotLoginMachine::new();
    machine.start(0);
    machine.advance(
        StepInput::Input {
            value: String::new(),
        },
        0,
    );
    // Device-code response → notify.
    let notify = machine.advance(
        StepInput::Response(HttpResponse::ok(
            json!({
                "device_code": "device-code",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://github.com/login/device",
                "interval": 5,
                "expires_in": 900,
            })
            .to_string(),
        )),
        0,
    );
    assert!(matches!(notify, Step::Notify { .. }));
    // Ack → first Wait carrying the poll request.
    let request = match machine.advance(StepInput::Ack, 0) {
        Step::Wait { request, delay_ms } => {
            assert_eq!(delay_ms, 5000);
            request
        }
        other => panic!("expected poll wait, got {other:?}"),
    };
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, "https://github.com/login/oauth/access_token");
    let body = request.body.as_deref().unwrap_or_default();
    assert!(body.contains("client_id="), "body: {body}");
    assert!(body.contains("device_code=device-code"), "body: {body}");
    assert!(
        body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"),
        "body: {body}"
    );
}

/// `to_auth` maps the copilot token to `apiKey` and derives the proxy base URL
/// (`github-copilot.ts:373-377`).
#[test]
fn to_auth_maps_access_and_base_url() {
    let auth = GitHubCopilotOAuth::new();
    let credential = OAuthCredential {
        refresh: "ghu_refresh_token".to_string(),
        access: COPILOT_TOKEN.to_string(),
        expires: 0,
        extra: Default::default(),
    };
    let model_auth = OAuthAuth::to_auth(&auth, &credential).unwrap();
    assert_eq!(model_auth.api_key.as_deref(), Some(COPILOT_TOKEN));
    assert_eq!(
        model_auth.base_url.as_deref(),
        Some("https://api.individual.githubcopilot.com")
    );
}

/// `base_url_from_token` and `get_github_copilot_base_url` follow pi's precedence
/// (`github-copilot.ts:66-83`).
#[test]
fn base_url_derivation_covers_token_and_fallbacks() {
    assert_eq!(
        base_url_from_token(COPILOT_TOKEN).as_deref(),
        Some("https://api.individual.githubcopilot.com")
    );
    assert_eq!(base_url_from_token("tid=test;exp=1;"), None);
    // Token proxy-ep wins over everything.
    assert_eq!(
        get_github_copilot_base_url(Some(COPILOT_TOKEN), Some("company.ghe.com")),
        "https://api.individual.githubcopilot.com"
    );
    // No usable token → enterprise host.
    assert_eq!(
        get_github_copilot_base_url(None, Some("company.ghe.com")),
        "https://copilot-api.company.ghe.com"
    );
    // No token, no enterprise → individual default.
    assert_eq!(
        get_github_copilot_base_url(None, None),
        "https://api.individual.githubcopilot.com"
    );
}

/// `normalize_domain` extracts the hostname and rejects blanks.
#[test]
fn normalize_domain_extracts_hostname() {
    assert_eq!(normalize_domain("  "), None);
    assert_eq!(
        normalize_domain("company.ghe.com").as_deref(),
        Some("company.ghe.com")
    );
    assert_eq!(
        normalize_domain("https://Company.GHE.com/path").as_deref(),
        Some("company.ghe.com")
    );
}
