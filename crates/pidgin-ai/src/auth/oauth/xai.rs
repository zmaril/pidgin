// straitjacket-allow-file:duplication — this module mirrors pi's parallel
// provider layout: the `OAuthAuth` impl skeleton (name/login_label/login_machine/
// refresh_machine/to_auth) and the `to_auth` mapping are shared verbatim with the
// sibling provider modules by design, and the form-encoding, URL-scheme, interval,
// and JSON-field helpers are faithful transcriptions of pi's `xai.ts` /
// `device-code.ts` helpers that necessarily resemble the anthropic and
// device_code ports. The clone detector reads these mirrored members as
// duplicates; the repetition is the intended per-provider layout kept faithful to
// pi's source.
//! xAI OAuth device-code flow (Grok/X subscription).
//!
//! Ported from pi-ai's `packages/ai/src/auth/oauth/xai.ts` at pinned commit
//! `3da591ab`. Both login and refresh are modelled as [`OAuthFlowMachine`]s that
//! yield [`Step`]s and consume [`StepInput`]s, so the JS shim and the pure-Rust
//! [`super::flow::run_flow`] driver advance the exact same logic.
//!
//! # Device-code login as a state machine
//!
//! pi's `loginXai` requests a device code, notifies the user, then delegates to
//! [`super::device_code::poll_oauth_device_code_flow`] to poll the token endpoint
//! until the user authorizes (`xai.ts:198-207`). Across the one-way napi boundary
//! the machine cannot own the poll loop — it yields [`Step::Wait`] (sleep then
//! poll) for each poll and re-enters on the response, reimplementing the poller's
//! deadline / interval / `slow_down` arithmetic (`device-code.ts:46-98`) against
//! the `now_ms` the driver threads in.
//!
//! # Poll deadline boundary (behavior (b))
//!
//! pi's poller re-checks `while (now < deadline)` *after* each inter-poll sleep,
//! so a sleep that lands exactly on the deadline skips the final poll. Because a
//! [`Step::Wait`] couples the sleep to the request it fires unconditionally, this
//! machine **pre-checks** the deadline: when the next inter-poll sleep would land
//! on or after it (`now_ms + interval_ms >= deadline`) it emits the timeout
//! [`Step::Error`] immediately with no trailing poll, so the request count matches
//! pi's "break before the final poll" exactly (mirrors `github_copilot.rs`). Only
//! the wall-clock instant of the error moves earlier; the shared
//! [`super::device_code`] poller retains pi's exact semantics.

use serde_json::{Map, Value};

use crate::auth::error::AuthFlowError;
use crate::auth::types::{AuthEvent, ModelAuth, OAuthAuth, OAuthCredential};
use crate::seams::http::{HttpRequest, HttpResponse};

use super::device_code::{
    CANCEL_MESSAGE, DEFAULT_POLL_INTERVAL_SECONDS, MINIMUM_INTERVAL_MS,
    SLOW_DOWN_INTERVAL_INCREMENT_MS, SLOW_DOWN_TIMEOUT_MESSAGE, TIMEOUT_MESSAGE,
};
use super::flow::{OAuthFlowMachine, Step, StepInput};

/// OAuth client id (`xai.ts:8`).
pub const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
/// OAuth scope (`xai.ts:9`).
pub const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
/// Device-code endpoint (`xai.ts:10`).
pub const XAI_DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
/// Token endpoint (`xai.ts:11`).
pub const XAI_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
/// Refresh skew, in ms: refresh slightly before reported expiry (`xai.ts:13`).
pub const REFRESH_SKEW_MS: i64 = 5 * 60 * 1000;
/// Default token lifetime when the server omits `expires_in` (`xai.ts:14`).
pub const DEFAULT_TOKEN_LIFETIME_SECONDS: f64 = 3600.0;

/// The device-grant `grant_type` value (`xai.ts:170`).
const DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Message for a verification URI that is not a well-formed https URL
/// (`xai.ts:55,58`).
const UNTRUSTED_URI_MESSAGE: &str = "Untrusted verification URI in xAI OAuth response";

// ---------------------------------------------------------------------------
// Form encoding — mirrors `new URLSearchParams(fields)` (`xai.ts:72`).
// ---------------------------------------------------------------------------

/// Encode `pairs` as an `application/x-www-form-urlencoded` body, mirroring
/// `URLSearchParams.toString()` (space→`+`, `*-._` and alphanumerics stay,
/// everything else percent-encoded).
fn form_urlencode(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (key, value)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        encode_component(key, &mut out);
        out.push('=');
        encode_component(value, &mut out);
    }
    out
}

fn encode_component(value: &str, out: &mut String) {
    for &byte in value.as_bytes() {
        match byte {
            b' ' => out.push('+'),
            b'*' | b'-' | b'.' | b'_' => out.push(byte as char),
            b if b.is_ascii_alphanumeric() => out.push(b as char),
            b => {
                out.push('%');
                out.push(nibble(b >> 4));
                out.push(nibble(b & 0x0f));
            }
        }
    }
}

fn nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + (n - 10)) as char,
    }
}

/// Build a form-encoded POST with pi's headers (`Accept` + `Content-Type`;
/// `xai.ts:68-72`).
fn post_form(url: &str, pairs: &[(&str, &str)]) -> HttpRequest {
    HttpRequest::post(url, form_urlencode(pairs))
        .with_header("accept", "application/json")
        .with_header("content-type", "application/x-www-form-urlencoded")
}

// ---------------------------------------------------------------------------
// JSON field readers — mirror pi's `requiredString` / `positiveNumber`
// (`xai.ts:32-46`).
// ---------------------------------------------------------------------------

/// Read a required non-empty string field, else pi's field error (`xai.ts:32-38`).
fn required_string(body: &Map<String, Value>, field: &str) -> Result<String, String> {
    match body.get(field).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Ok(value.to_string()),
        _ => Err(format!("Invalid xAI OAuth response field: {field}")),
    }
}

/// Read a required finite positive number field, else pi's field error
/// (`xai.ts:40-46`).
fn positive_number(body: &Map<String, Value>, field: &str) -> Result<f64, String> {
    match body.get(field).and_then(Value::as_f64) {
        Some(value) if value.is_finite() && value > 0.0 => Ok(value),
        _ => Err(format!("Invalid xAI OAuth response field: {field}")),
    }
}

/// The verification URI is opened in the user's browser; force it to be a
/// well-formed https URL so a malicious response cannot launch something else
/// (`xai.ts:50-61`).
fn validate_verification_uri(raw: &str) -> Result<String, String> {
    match url_scheme(raw) {
        Some(scheme) if scheme.eq_ignore_ascii_case("https") => Ok(raw.to_string()),
        _ => Err(UNTRUSTED_URI_MESSAGE.to_string()),
    }
}

/// The URL scheme (chars before the first `:`), or `None` when `value` does not
/// begin with a `scheme:` prefix — the inputs for which JS `new URL(value)`
/// fails or yields a non-https protocol.
fn url_scheme(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    for (i, &c) in bytes.iter().enumerate() {
        if c == b':' {
            return Some(&value[..i]);
        }
        if !(c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.') {
            return None;
        }
    }
    None
}

/// Parse a response body into a JSON object, mirroring pi's `postForm` body
/// handling: valid non-object JSON collapses to `{}`, invalid JSON is an error
/// (`xai.ts:82-91`).
fn parse_json_object(response: &HttpResponse) -> Result<Map<String, Value>, String> {
    match serde_json::from_str::<Value>(&response.body) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Ok(Map::new()),
        Err(_) => Err(format!(
            "xAI OAuth returned invalid JSON (HTTP {})",
            response.status
        )),
    }
}

/// Build pi's `requestFailure` message: `xAI OAuth {action} failed (HTTP {status})`
/// with an optional `error: error_description` detail (`xai.ts:99-105`).
fn request_failure(action: &str, status: u16, body: &Map<String, Value>) -> String {
    let error = body.get("error").and_then(Value::as_str);
    let description = body.get("error_description").and_then(Value::as_str);
    let detail = [error, description]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(": ");
    let suffix = if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    };
    format!("xAI OAuth {action} failed (HTTP {status}){suffix}")
}

// ---------------------------------------------------------------------------
// Device-code parsing (`xai.ts:107-125`).
// ---------------------------------------------------------------------------

/// A parsed device-authorization response (`xai.ts:23-30`).
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval_seconds: Option<f64>,
    expires_in_seconds: f64,
}

/// Parse a device-authorization response body (`xai.ts:107-125`).
///
/// RFC 8628 allows `interval` 0 (no minimum wait); a non-positive or malformed
/// value falls back to the poller default rather than failing. `verification_uri`
/// (and `verification_uri_complete` when present) must be https.
fn parse_device_code(body: &Map<String, Value>) -> Result<DeviceCode, String> {
    let interval_seconds = match body.get("interval").and_then(Value::as_f64) {
        Some(value) if value.is_finite() && value > 0.0 => Some(value),
        _ => None,
    };
    let verification_uri_complete = match body
        .get("verification_uri_complete")
        .and_then(Value::as_str)
    {
        Some(value) if !value.is_empty() => Some(validate_verification_uri(value)?),
        _ => None,
    };
    let device_code = required_string(body, "device_code")?;
    let user_code = required_string(body, "user_code")?;
    let verification_uri = validate_verification_uri(&required_string(body, "verification_uri")?)?;
    let expires_in_seconds = positive_number(body, "expires_in")?;
    Ok(DeviceCode {
        device_code,
        user_code,
        // The complete URI (pre-filled with the user code) is preferred for the
        // user-facing notify (`xai.ts:203`).
        verification_uri: verification_uri_complete.unwrap_or(verification_uri),
        interval_seconds,
        expires_in_seconds,
    })
}

/// Turn a token-endpoint response body into a credential, applying pi's
/// expiry formula `now + expires_in*1000 - skew` (`xai.ts:127-142`).
///
/// `previous_refresh` is reused when the server omits `refresh_token` (an
/// unrotated refresh on the refresh grant).
fn credentials_from_token_response(
    body: &Map<String, Value>,
    previous_refresh: Option<&str>,
    now_ms: i64,
) -> Result<OAuthCredential, String> {
    let access = required_string(body, "access_token")?;
    let refresh = match (body.get("refresh_token"), previous_refresh) {
        // xAI may omit refresh_token on refresh when the token is not rotated.
        (None, Some(previous)) => previous.to_string(),
        _ => required_string(body, "refresh_token")?,
    };
    let expires_in_seconds = if body.get("expires_in").is_none() {
        DEFAULT_TOKEN_LIFETIME_SECONDS
    } else {
        positive_number(body, "expires_in")?
    };
    Ok(OAuthCredential {
        refresh,
        access,
        expires: now_ms + (expires_in_seconds * 1000.0) as i64 - REFRESH_SKEW_MS,
        extra: Map::new(),
    })
}

// ---------------------------------------------------------------------------
// Poll interval / deadline arithmetic (`device-code.ts:125-192`).
// ---------------------------------------------------------------------------

/// The initial poll interval in ms: at least [`MINIMUM_INTERVAL_MS`], defaulting
/// to [`DEFAULT_POLL_INTERVAL_SECONDS`] when unset (`device-code.ts:125-128`).
fn initial_interval_ms(interval_seconds: Option<f64>) -> i64 {
    MINIMUM_INTERVAL_MS
        .max((interval_seconds.unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS) * 1000.0).floor() as i64)
}

/// The interval after a `slow_down`: trust a finite positive server interval,
/// else RFC 8628 §3.5 — increase by 5 seconds (`device-code.ts:164-174`).
fn interval_after_slow_down(current_ms: i64, server_seconds: Option<f64>) -> i64 {
    match server_seconds {
        Some(seconds) if seconds.is_finite() && seconds > 0.0 => {
            MINIMUM_INTERVAL_MS.max((seconds * 1000.0).floor() as i64)
        }
        _ => MINIMUM_INTERVAL_MS.max(current_ms + SLOW_DOWN_INTERVAL_INCREMENT_MS),
    }
}

// ---------------------------------------------------------------------------
// Login machine (`xai.ts:198-207`).
// ---------------------------------------------------------------------------

/// The phases of the xAI login machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LoginPhase {
    /// Not yet started.
    Start,
    /// Emitted the device-code request; awaiting its response.
    AwaitingDeviceCode,
    /// Emitted the `device_code` notify; awaiting its ack.
    AwaitingNotifyAck,
    /// Emitted a poll `Wait`; awaiting its token response.
    Polling,
    /// Terminal.
    Done,
}

/// The xAI device-code login flow machine (`xai.ts:198-207`).
///
/// Step sequence: `start` → `Request(POST device/code)`; on `Response` →
/// `Notify(device_code)`; on `Ack` → `Wait(sleep, POST token)` (poll); on each
/// `Response` → `Wait` again (pending/slow_down) or `Done`/`Error`.
pub struct XaiLoginMachine {
    phase: LoginPhase,
    device_code: String,
    interval_ms: i64,
    deadline_ms: i64,
    slow_down_responses: usize,
}

impl XaiLoginMachine {
    /// A fresh login machine.
    pub fn new() -> Self {
        Self {
            phase: LoginPhase::Start,
            device_code: String::new(),
            interval_ms: 0,
            deadline_ms: 0,
            slow_down_responses: 0,
        }
    }

    /// Build the device-authorization request (`xai.ts:144-158`).
    fn device_code_request() -> Step {
        Step::Request {
            request: post_form(
                XAI_DEVICE_CODE_URL,
                &[
                    ("client_id", XAI_CLIENT_ID),
                    ("scope", XAI_SCOPE),
                    ("referrer", "pi"),
                ],
            ),
        }
    }

    /// Build the device-grant poll request (`xai.ts:167-174`).
    fn poll_request(&self) -> HttpRequest {
        post_form(
            XAI_TOKEN_URL,
            &[
                ("grant_type", DEVICE_CODE_GRANT_TYPE),
                ("client_id", XAI_CLIENT_ID),
                ("device_code", &self.device_code),
            ],
        )
    }

    /// Yield the next poll `Wait` at `now_ms`, or the timeout error when the next
    /// inter-poll sleep would land on or after the deadline (`device-code.ts:148-191`).
    ///
    /// Deadline is pre-checked (behavior (b)): when `now_ms + interval_ms >=
    /// deadline` the timeout `Step::Error` is emitted immediately with no trailing
    /// poll, matching pi's "break before the final poll" (`github_copilot.rs`).
    fn next_poll(&self, now_ms: i64) -> Step {
        let remaining = self.deadline_ms - now_ms;
        if remaining > 0 && self.interval_ms < remaining {
            Step::Wait {
                delay_ms: self.interval_ms as u64,
                request: self.poll_request(),
            }
        } else {
            Step::Error {
                message: if self.slow_down_responses > 0 {
                    SLOW_DOWN_TIMEOUT_MESSAGE
                } else {
                    TIMEOUT_MESSAGE
                }
                .to_string(),
            }
        }
    }

    /// Consume the device-authorization response and yield the `device_code`
    /// notify (`xai.ts:154-205`).
    fn on_device_code_response(&mut self, response: HttpResponse, now_ms: i64) -> Step {
        let body = match parse_json_object(&response) {
            Ok(body) => body,
            Err(message) => return self.fail(message),
        };
        if !response.is_ok() {
            return self.fail(request_failure(
                "device authorization",
                response.status,
                &body,
            ));
        }
        let device = match parse_device_code(&body) {
            Ok(device) => device,
            Err(message) => return self.fail(message),
        };
        self.device_code = device.device_code;
        self.interval_ms = initial_interval_ms(device.interval_seconds);
        // Deadline base: `Date.now()` at the start of polling (`device-code.ts:141`).
        self.deadline_ms = now_ms + (device.expires_in_seconds * 1000.0) as i64;
        self.phase = LoginPhase::AwaitingNotifyAck;
        Step::Notify {
            event: AuthEvent::DeviceCode {
                user_code: device.user_code,
                verification_uri: device.verification_uri,
                interval_seconds: device.interval_seconds,
                expires_in_seconds: Some(device.expires_in_seconds),
            },
        }
    }

    /// Consume a token poll response: complete, keep polling, or fail
    /// (`xai.ts:176-195`).
    fn on_poll_response(&mut self, response: HttpResponse, now_ms: i64) -> Step {
        let body = match parse_json_object(&response) {
            Ok(body) => body,
            Err(message) => return self.fail(message),
        };
        if response.is_ok() {
            return match credentials_from_token_response(&body, None, now_ms) {
                Ok(credential) => {
                    self.phase = LoginPhase::Done;
                    Step::Done { credential }
                }
                Err(message) => self.fail(message),
            };
        }
        match body.get("error").and_then(Value::as_str) {
            Some("authorization_pending") => self.next_poll(now_ms),
            Some("slow_down") => {
                let server = body.get("interval").and_then(Value::as_f64);
                self.interval_ms = interval_after_slow_down(self.interval_ms, server);
                self.slow_down_responses += 1;
                self.next_poll(now_ms)
            }
            Some("access_denied") | Some("authorization_denied") => {
                self.fail("xAI device authorization was denied".to_string())
            }
            Some("expired_token") => self.fail("xAI device code expired".to_string()),
            _ => self.fail(request_failure(
                "device token polling",
                response.status,
                &body,
            )),
        }
    }

    /// Move to the terminal phase and yield an error step.
    fn fail(&mut self, message: String) -> Step {
        self.phase = LoginPhase::Done;
        Step::Error { message }
    }
}

impl Default for XaiLoginMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl OAuthFlowMachine for XaiLoginMachine {
    fn start(&mut self, _now_ms: i64) -> Step {
        self.phase = LoginPhase::AwaitingDeviceCode;
        Self::device_code_request()
    }

    fn advance(&mut self, input: StepInput, now_ms: i64) -> Step {
        if matches!(input, StepInput::Aborted) {
            return self.fail(CANCEL_MESSAGE.to_string());
        }
        match (&self.phase, input) {
            (LoginPhase::AwaitingDeviceCode, StepInput::Response(response)) => {
                self.on_device_code_response(response, now_ms)
            }
            (LoginPhase::AwaitingNotifyAck, StepInput::Ack) => {
                self.phase = LoginPhase::Polling;
                self.next_poll(now_ms)
            }
            (LoginPhase::Polling, StepInput::Response(response)) => {
                self.on_poll_response(response, now_ms)
            }
            _ => self.fail("xAI login flow received an unexpected input".to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Refresh machine (`xai.ts:209-223`).
// ---------------------------------------------------------------------------

/// The phases of the xAI refresh machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RefreshPhase {
    /// Not yet started.
    Start,
    /// Emitted the refresh request; awaiting the response.
    AwaitingToken,
    /// Terminal.
    Done,
}

/// The xAI refresh flow machine (`xai.ts:209-223`).
///
/// Step sequence: `start` → `Request(POST token, grant_type=refresh_token)`; on
/// `Response` → `Done` (reusing the prior refresh token when the server omits a
/// rotation) or `Error`.
pub struct XaiRefreshMachine {
    refresh_token: String,
    phase: RefreshPhase,
}

impl XaiRefreshMachine {
    /// A refresh machine for `refresh_token`.
    pub fn new(refresh_token: impl Into<String>) -> Self {
        Self {
            refresh_token: refresh_token.into(),
            phase: RefreshPhase::Start,
        }
    }
}

impl OAuthFlowMachine for XaiRefreshMachine {
    fn start(&mut self, _now_ms: i64) -> Step {
        self.phase = RefreshPhase::AwaitingToken;
        Step::Request {
            request: post_form(
                XAI_TOKEN_URL,
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", XAI_CLIENT_ID),
                    ("refresh_token", &self.refresh_token),
                ],
            ),
        }
    }

    fn advance(&mut self, input: StepInput, now_ms: i64) -> Step {
        if matches!(input, StepInput::Aborted) {
            self.phase = RefreshPhase::Done;
            return Step::Error {
                message: CANCEL_MESSAGE.to_string(),
            };
        }
        match (&self.phase, input) {
            (RefreshPhase::AwaitingToken, StepInput::Response(response)) => {
                self.phase = RefreshPhase::Done;
                let body = match parse_json_object(&response) {
                    Ok(body) => body,
                    Err(message) => return Step::Error { message },
                };
                if !response.is_ok() {
                    return Step::Error {
                        message: request_failure("token refresh", response.status, &body),
                    };
                }
                match credentials_from_token_response(&body, Some(&self.refresh_token), now_ms) {
                    Ok(credential) => Step::Done { credential },
                    Err(message) => Step::Error { message },
                }
            }
            _ => {
                self.phase = RefreshPhase::Done;
                Step::Error {
                    message: "xAI refresh flow received an unexpected input".to_string(),
                }
            }
        }
    }
}

/// xAI OAuth flow handler (`xai.ts:225-233`).
#[derive(Debug, Default, Clone)]
pub struct XaiOAuth;

impl XaiOAuth {
    /// Construct the handler.
    pub fn new() -> Self {
        Self
    }
}

impl OAuthAuth for XaiOAuth {
    fn name(&self) -> &str {
        "xAI (Grok/X subscription)"
    }

    fn login_label(&self) -> Option<&str> {
        // `loginLabel` (`xai.ts:226`).
        Some("Sign in with SuperGrok or X Premium")
    }

    fn login_machine(&self) -> Box<dyn OAuthFlowMachine> {
        Box::new(XaiLoginMachine::new())
    }

    fn refresh_machine(&self, credential: &OAuthCredential) -> Box<dyn OAuthFlowMachine> {
        Box::new(XaiRefreshMachine::new(credential.refresh.clone()))
    }

    fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError> {
        // `toAuth` returns `{ apiKey: credential.access }` (`xai.ts:230-232`).
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

#[cfg(test)]
mod tests;
