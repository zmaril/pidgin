// straitjacket-allow-file[:duplication] — the `to_auth` mapping
// (`{ apiKey: credential.access }`) and the `OAuthAuth` impl skeleton
// (name/login_machine/refresh_machine/to_auth) are shared verbatim with the four
// sibling provider modules by design; the clone detector reads those mirrored
// members across the provider files as duplicates. The form-encoding and
// token-response helpers likewise mirror the shapes in `anthropic.rs`/`xai.rs`
// (each provider keeps its own copy because pi's provider modules are parallel,
// self-contained files). The repetition is the intended per-provider layout.
//! Radius gateway OAuth flow.
//!
//! Ported from pi-ai's `packages/ai/src/auth/oauth/radius.ts` at pinned commit
//! `3da591ab`. Radius is a pi-messages gateway whose OAuth endpoints are
//! **discovered** from the gateway (`GET <gateway>/v1/oauth`), so both the login
//! and refresh machines begin with a config-discovery [`Step::Request`] and then
//! proceed against the discovered endpoints.
//!
//! # Machine structure
//!
//! Login: `start` → `Request(GET /v1/oauth)`; on the config response →
//! `Prompt(select browser | device-code)`; then one of two sub-flows:
//! - **browser**: `Notify(progress)` → `Notify(auth_url)` → `Prompt(manual code)`
//!   → `Request(POST token, authorization_code)` → `Done`.
//! - **device-code**: `Request(POST device-authorization)` →
//!   `Notify(device_code)` → poll `Request`/`Wait(POST token, device grant)` →
//!   `Done`.
//!
//! Refresh: `start` → `Request(GET /v1/oauth)`; on the config response →
//! `Request(POST token, refresh_token)` → `Done`.
//!
//! # Scope
//!
//! Binding the real TCP loopback callback listener (`node:http` on port
//! [`CALLBACK_PORT`]) is out of conformance scope — there is no socket seam among
//! the five providers. The login machine drives the **callback-code path** as a
//! manual prompt (mirroring pi's callback server: validate `state`, read `code`),
//! then exchanges the code exactly as the loopback handler would.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::auth::error::AuthFlowError;
use crate::auth::types::{
    AuthEvent, AuthPrompt, AuthPromptKind, AuthSelectOption, ModelAuth, OAuthAuth, OAuthCredential,
};
use crate::seams::http::{HttpRequest, HttpResponse};

use super::device_code::{
    CANCEL_MESSAGE, DEFAULT_POLL_INTERVAL_SECONDS, MINIMUM_INTERVAL_MS,
    SLOW_DOWN_INTERVAL_INCREMENT_MS, SLOW_DOWN_TIMEOUT_MESSAGE, TIMEOUT_MESSAGE,
};
use super::flow::{OAuthFlowMachine, Step, StepInput};
use super::pkce::{generate_pkce, generate_pkce_from_bytes, Pkce};

/// Loopback callback host (`radius.ts:25`).
pub const CALLBACK_HOST: &str = "127.0.0.1";
/// Loopback callback port. Binding the real socket is out of scope
/// (`radius.ts:26`).
pub const CALLBACK_PORT: u16 = 1456;
/// Callback path (`radius.ts:27`).
pub const CALLBACK_PATH: &str = "/oauth/callback";
/// Redirect URI (`radius.ts:28`).
pub const REDIRECT_URI: &str = "http://127.0.0.1:1456/oauth/callback";
/// Token-expiry skew, in ms — 60s, distinct from the 5-minute providers
/// (`radius.ts:29`).
pub const TOKEN_EXPIRY_SKEW_MS: i64 = 60_000;
/// Browser login-method id (`radius.ts:30`).
pub const LOGIN_METHOD_BROWSER: &str = "browser";
/// Device-code login-method id (`radius.ts:31`).
pub const LOGIN_METHOD_DEVICE_CODE: &str = "device-code";

/// The discovered Radius OAuth configuration (`radius.ts:33-43`), fetched from
/// `GET <gateway>/v1/oauth`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RadiusOAuthConfig {
    #[allow(dead_code)]
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    device_authorization_endpoint: String,
    #[allow(dead_code)]
    device_authorization_events_endpoint: String,
    verification_endpoint: String,
    client_id: String,
    scope: String,
    device_code_grant_type: String,
}

/// The device-authorization response shape (`radius.ts:45-52`).
#[derive(Debug, Clone, Deserialize)]
struct DeviceAuthorizationResponse {
    device_code: Option<String>,
    user_code: Option<String>,
    verification_uri: Option<String>,
    #[allow(dead_code)]
    verification_uri_complete: Option<String>,
    expires_in: Option<f64>,
    interval: Option<f64>,
}

/// The token-endpoint success body (`radius.ts:118-123`).
#[derive(Debug, Deserialize)]
struct TokenResponseBody {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    scope: Option<String>,
}

/// Normalize a gateway URL, mirroring `normalizeRadiusGatewayUrl`
/// (`providers/radius-config.ts`): prepend `https://` when no `http(s)://`
/// scheme is present, then strip any trailing slashes. Inlined here because the
/// provider-side normalizer lives outside this module's scope.
fn normalize_radius_gateway_url(value: &str) -> String {
    let lowered = value.to_ascii_lowercase();
    let with_scheme = if lowered.starts_with("http://") || lowered.starts_with("https://") {
        value.to_string()
    } else {
        format!("https://{value}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

/// Resolve `<gateway>/v1/oauth` the way `new URL("/v1/oauth", gateway)` does: an
/// absolute path replaces the gateway's path, keeping only its origin.
fn config_url(gateway: &str) -> String {
    let origin = match gateway.split_once("://") {
        Some((scheme, after)) => {
            let authority = after.split(['/', '?', '#']).next().unwrap_or(after);
            format!("{scheme}://{authority}")
        }
        None => gateway.to_string(),
    };
    format!("{origin}/v1/oauth")
}

/// Build the config-discovery request (`radius.ts:54-57`).
fn config_request(gateway: &str) -> HttpRequest {
    HttpRequest::get(config_url(gateway)).with_header("accept", "application/json")
}

/// Encode `pairs` as `application/x-www-form-urlencoded`, mirroring
/// `URLSearchParams.toString()` (space→`+`, `*-._` and alphanumerics literal,
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

/// Parse a form-urlencoded query string into first-value-wins pairs, mirroring
/// `URLSearchParams`: `+`→space then percent-decode.
fn parse_query(query: &str) -> std::collections::BTreeMap<String, String> {
    let query = query.strip_prefix('?').unwrap_or(query);
    let mut map = std::collections::BTreeMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        map.entry(decode_component(key))
            .or_insert_with(|| decode_component(value));
    }
    map
}

fn decode_component(value: &str) -> String {
    let bytes = value.replace('+', " ").into_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
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

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Build a token-endpoint POST carrying form-encoded `pairs` with pi's headers
/// (`radius.ts:98-104`).
fn token_request(token_endpoint: &str, pairs: &[(&str, &str)]) -> HttpRequest {
    HttpRequest::post(token_endpoint, form_urlencode(pairs))
        .with_header("accept", "application/json")
        .with_header("content-type", "application/x-www-form-urlencoded")
}

/// The outcome of reading a non-2xx OAuth response body (`radius.ts:60-93`).
struct OAuthResponseError {
    oauth_error: Option<String>,
    message: String,
}

/// Parse an error response into `{error, error_description}` and build pi's
/// `OAuthResponseError` message (`radius.ts:62-93`).
fn read_oauth_error(status: u16, body: &str, prefix: &str) -> OAuthResponseError {
    let mut oauth_error: Option<String> = None;
    let mut description: Option<String> = None;
    if !body.is_empty() {
        match serde_json::from_str::<Value>(body) {
            Ok(data) => {
                oauth_error = data
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                description = data
                    .get("error_description")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            Err(_) => description = Some(body.to_string()),
        }
    }
    // detail = oauthError ? (description ? `${oauthError}: ${description}` :
    //   oauthError) : (description || String(status)).
    let detail = match (&oauth_error, &description) {
        (Some(err), Some(desc)) => format!("{err}: {desc}"),
        (Some(err), None) => err.clone(),
        (None, Some(desc)) => desc.clone(),
        (None, None) => status.to_string(),
    };
    OAuthResponseError {
        oauth_error,
        message: format!("{prefix}: {detail}"),
    }
}

/// The interpretation of a token-endpoint response (`radius.ts:96-124`).
enum TokenResult {
    /// A 2xx response yielding a credential.
    Credential(OAuthCredential),
    /// A non-2xx response carrying an OAuth error.
    Failed(OAuthResponseError),
    /// A 2xx response whose body was not valid token JSON (pi rethrows the
    /// `SyntaxError`).
    Invalid(String),
}

/// Interpret a token-endpoint response into a [`TokenResult`], applying the
/// expiry formula `now + expires_in*1000 - 60_000` and stashing `scope` on the
/// credential's `extra` map (`radius.ts:106-124`).
fn interpret_token_response(response: &HttpResponse, now_ms: i64) -> TokenResult {
    if !response.is_ok() {
        return TokenResult::Failed(read_oauth_error(
            response.status,
            &response.body,
            "Radius OAuth token request failed",
        ));
    }
    match serde_json::from_str::<TokenResponseBody>(&response.body) {
        Ok(token) => {
            let mut extra = Map::new();
            if let Some(scope) = token.scope {
                extra.insert("scope".to_string(), Value::String(scope));
            }
            TokenResult::Credential(OAuthCredential {
                refresh: token.refresh_token,
                access: token.access_token,
                expires: now_ms + token.expires_in * 1000 - TOKEN_EXPIRY_SKEW_MS,
                extra,
            })
        }
        Err(error) => TokenResult::Invalid(format!(
            "Radius OAuth token response returned invalid JSON. body={}; details={error}",
            response.body
        )),
    }
}

/// The initial poll interval, in ms: `max(MIN, floor(interval_seconds*1000))`
/// with the RFC 8628 §3.2 five-second default (`device-code.ts` parity).
fn initial_interval_ms(interval_seconds: Option<f64>) -> i64 {
    MINIMUM_INTERVAL_MS
        .max((interval_seconds.unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS) * 1000.0).floor() as i64)
}

/// Mutable device-code polling state, mirroring `pollOAuthDeviceCodeFlow`'s
/// locals (`device-code.ts:46-98`).
struct DeviceState {
    device_code: String,
    deadline_ms: f64,
    interval_ms: i64,
    slow_down_responses: usize,
}

impl DeviceState {
    /// The timeout message, chosen by whether any `slow_down` was seen.
    fn timeout_message(&self) -> &'static str {
        if self.slow_down_responses > 0 {
            SLOW_DOWN_TIMEOUT_MESSAGE
        } else {
            TIMEOUT_MESSAGE
        }
    }
}

/// The phases of the Radius login machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LoginPhase {
    /// Not yet started.
    Start,
    /// Emitted the config-discovery request; awaiting its response.
    AwaitingConfig,
    /// Emitted the login-method select prompt; awaiting the selected id.
    AwaitingMethod,
    /// Browser: emitted the `progress` notify; awaiting its ack.
    BrowserAwaitingProgressAck,
    /// Browser: emitted the `auth_url` notify; awaiting its ack.
    BrowserAwaitingUrlAck,
    /// Browser: emitted the manual-code prompt; awaiting the pasted callback.
    BrowserAwaitingCode,
    /// Browser: emitted the token exchange; awaiting the response.
    BrowserAwaitingToken,
    /// Device: emitted the device-authorization request; awaiting the response.
    DeviceAwaitingAuth,
    /// Device: emitted the `device_code` notify; awaiting its ack.
    DeviceAwaitingCodeAck,
    /// Device: polling the token endpoint; awaiting each poll response.
    DevicePolling,
    /// Terminal.
    Done,
}

/// The Radius login flow machine (`createRadiusOAuth().login`; `radius.ts:366-390`).
pub struct RadiusLoginMachine {
    gateway: String,
    name: String,
    pkce: Pkce,
    state: String,
    phase: LoginPhase,
    config: Option<RadiusOAuthConfig>,
    device: Option<DeviceState>,
}

impl RadiusLoginMachine {
    /// A login machine for `gateway` under display `name`, with a freshly
    /// generated PKCE pair and random `state`.
    pub fn new(gateway: impl Into<String>, name: impl Into<String>) -> Self {
        Self::build(gateway.into(), name.into(), generate_pkce(), random_state())
    }

    /// A login machine with a deterministic PKCE pair (from a 32-byte seed) and
    /// an explicit `state`, for reproducible tests.
    pub fn with_seed(
        gateway: impl Into<String>,
        name: impl Into<String>,
        pkce_bytes: [u8; 32],
        state: impl Into<String>,
    ) -> Self {
        Self::build(
            gateway.into(),
            name.into(),
            generate_pkce_from_bytes(pkce_bytes),
            state.into(),
        )
    }

    fn build(gateway: String, name: String, pkce: Pkce, state: String) -> Self {
        Self {
            gateway,
            name,
            pkce,
            state,
            phase: LoginPhase::Start,
            config: None,
            device: None,
        }
    }

    fn config(&self) -> &RadiusOAuthConfig {
        self.config.as_ref().expect("config discovered before use")
    }

    /// Build the authorize URL (`radius.ts:258-271`). `authorizeUrl.search` is
    /// replaced with the query below, so any existing query on the endpoint is
    /// dropped.
    fn authorize_url(&self) -> String {
        let config = self.config();
        let base = config
            .authorization_endpoint
            .split_once('?')
            .map(|(base, _)| base)
            .unwrap_or(&config.authorization_endpoint);
        let query = form_urlencode(&[
            ("response_type", "code"),
            ("client_id", &config.client_id),
            ("redirect_uri", REDIRECT_URI),
            ("scope", &config.scope),
            ("code_challenge", &self.pkce.challenge),
            ("code_challenge_method", "S256"),
            ("handoff", "url"),
            ("state", &self.state),
        ]);
        format!("{base}?{query}")
    }

    /// Validate the pasted callback and build the authorization-code token
    /// request, mirroring the callback server's `state`/`code` checks
    /// (`radius.ts:169-206, 278-291`).
    fn browser_exchange_step(&mut self, input: &str) -> Step {
        let params = callback_params(input);
        if let Some(state) = params.get("state") {
            if state != &self.state {
                self.phase = LoginPhase::Done;
                return Step::Error {
                    message: "OAuth state mismatch.".to_string(),
                };
            }
        }
        if params.contains_key("error") || params.get("code").is_none_or(String::is_empty) {
            self.phase = LoginPhase::Done;
            return Step::Error {
                message: "OAuth callback did not complete.".to_string(),
            };
        }
        let code = params.get("code").cloned().unwrap_or_default();
        let config = self.config();
        let request = token_request(
            &config.token_endpoint,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &config.client_id),
                ("redirect_uri", REDIRECT_URI),
                ("code", &code),
                ("code_verifier", &self.pkce.verifier),
            ],
        );
        self.phase = LoginPhase::BrowserAwaitingToken;
        Step::Request { request }
    }

    /// Build the next device-code poll request against the token endpoint
    /// (`radius.ts:311-317`).
    fn poll_request(&self) -> Step {
        let config = self.config();
        let device = self.device.as_ref().expect("device state before poll");
        Step::Request {
            request: token_request(
                &config.token_endpoint,
                &[
                    ("grant_type", &config.device_code_grant_type),
                    ("client_id", &config.client_id),
                    ("device_code", &device.device_code),
                ],
            ),
        }
    }

    /// Enter the poll loop: the top-of-loop deadline guard, then the first poll
    /// (no wait before the first poll; `radius.ts:307-310`).
    fn begin_poll(&mut self, now_ms: i64) -> Step {
        let device = self.device.as_ref().expect("device state before poll");
        if now_ms as f64 >= device.deadline_ms {
            self.phase = LoginPhase::Done;
            return Step::Error {
                message: device.timeout_message().to_string(),
            };
        }
        self.phase = LoginPhase::DevicePolling;
        self.poll_request()
    }

    /// Handle a poll response, mapping the RFC 8628 statuses and pi's device
    /// oauth-error cases, then either finishing or scheduling the next poll
    /// (`radius.ts:319-340`, `device-code.ts:46-98`).
    fn advance_poll(&mut self, response: &HttpResponse, now_ms: i64) -> Step {
        // Top-of-loop `while now < deadline` guard for a poll that followed a
        // Wait (whose sleep advanced the clock). Deviation from pi's driver: the
        // yield-based contract has no bare sleep, so the Wait already performed
        // the trailing poll pi would skip at the exact deadline; the terminal
        // timeout result is identical.
        {
            let device = self.device.as_ref().expect("device state before poll");
            if now_ms as f64 >= device.deadline_ms {
                self.phase = LoginPhase::Done;
                return Step::Error {
                    message: device.timeout_message().to_string(),
                };
            }
        }

        match interpret_token_response(response, now_ms) {
            TokenResult::Credential(credential) => {
                self.phase = LoginPhase::Done;
                return Step::Done { credential };
            }
            TokenResult::Invalid(message) => {
                self.phase = LoginPhase::Done;
                return Step::Error { message };
            }
            TokenResult::Failed(error) => match error.oauth_error.as_deref() {
                Some("authorization_pending") => {}
                Some("slow_down") => {
                    let device = self.device.as_mut().expect("device state");
                    device.slow_down_responses += 1;
                    device.interval_ms = MINIMUM_INTERVAL_MS
                        .max(device.interval_ms + SLOW_DOWN_INTERVAL_INCREMENT_MS);
                }
                Some("expired_token") => {
                    self.phase = LoginPhase::Done;
                    return Step::Error {
                        message: "Device authorization expired.".to_string(),
                    };
                }
                Some("access_denied") => {
                    self.phase = LoginPhase::Done;
                    return Step::Error {
                        message: "Device authorization was denied.".to_string(),
                    };
                }
                _ => {
                    self.phase = LoginPhase::Done;
                    return Step::Error {
                        message: error.message,
                    };
                }
            },
        }

        // Pending / slow_down: schedule the next poll unless the deadline passed.
        let device = self.device.as_ref().expect("device state");
        let remaining_ms = device.deadline_ms - now_ms as f64;
        if remaining_ms <= 0.0 {
            self.phase = LoginPhase::Done;
            return Step::Error {
                message: device.timeout_message().to_string(),
            };
        }
        let delay_ms = (device.interval_ms as f64).min(remaining_ms) as u64;
        let request = match self.poll_request() {
            Step::Request { request } => request,
            _ => unreachable!("poll_request always yields a Request"),
        };
        Step::Wait { delay_ms, request }
    }

    /// The login-method select prompt (`radius.ts:369-380`).
    fn method_prompt(&self) -> Step {
        Step::Prompt {
            prompt: AuthPrompt {
                signal: None,
                kind: AuthPromptKind::Select {
                    message: format!("Sign in to {}:", self.name),
                    options: vec![
                        AuthSelectOption {
                            id: LOGIN_METHOD_BROWSER.to_string(),
                            label: "Sign in with browser (recommended)".to_string(),
                            description: None,
                        },
                        AuthSelectOption {
                            id: LOGIN_METHOD_DEVICE_CODE.to_string(),
                            label: "Sign in with device code (when signing in from another device)"
                                .to_string(),
                            description: None,
                        },
                    ],
                },
            },
        }
    }

    /// Parse the device-authorization response and, on success, set up polling
    /// state and emit the `device_code` notify (`radius.ts:227-256, 293-306`).
    fn on_device_auth(&mut self, response: &HttpResponse, now_ms: i64) -> Step {
        if !response.is_ok() {
            self.phase = LoginPhase::Done;
            return Step::Error {
                message: read_oauth_error(
                    response.status,
                    &response.body,
                    "Radius OAuth device authorization failed",
                )
                .message,
            };
        }
        let device: DeviceAuthorizationResponse = match serde_json::from_str(&response.body) {
            Ok(device) => device,
            Err(error) => {
                self.phase = LoginPhase::Done;
                return Step::Error {
                    message: format!(
                        "Radius OAuth device authorization response is invalid JSON: {error}"
                    ),
                };
            }
        };
        let (device_code, user_code, expires_in) =
            match (device.device_code, device.user_code, device.expires_in) {
                (Some(dc), Some(uc), Some(exp))
                    if !dc.is_empty() && !uc.is_empty() && exp != 0.0 =>
                {
                    (dc, uc, exp)
                }
                _ => {
                    self.phase = LoginPhase::Done;
                    return Step::Error {
                        message:
                            "Radius OAuth device authorization response is missing required fields"
                                .to_string(),
                    };
                }
            };
        let config = self.config();
        let verification_uri = device
            .verification_uri
            .filter(|uri| !uri.is_empty())
            .unwrap_or_else(|| config.verification_endpoint.clone());
        self.device = Some(DeviceState {
            device_code,
            deadline_ms: now_ms as f64 + expires_in * 1000.0,
            interval_ms: initial_interval_ms(device.interval),
            slow_down_responses: 0,
        });
        self.phase = LoginPhase::DeviceAwaitingCodeAck;
        Step::Notify {
            event: AuthEvent::DeviceCode {
                user_code,
                verification_uri,
                interval_seconds: device.interval,
                expires_in_seconds: Some(expires_in),
            },
        }
    }

    /// Handle the config-discovery response: parse the config or error out
    /// (`radius.ts:54-66`).
    fn on_config(&mut self, response: &HttpResponse) -> Option<Step> {
        if !response.is_ok() {
            self.phase = LoginPhase::Done;
            return Some(Step::Error {
                message: format!(
                    "Could not load Radius OAuth config from {}: {} {}",
                    self.gateway, response.status, response.body
                ),
            });
        }
        match serde_json::from_str::<RadiusOAuthConfig>(&response.body) {
            Ok(config) => {
                self.config = Some(config);
                None
            }
            Err(error) => {
                self.phase = LoginPhase::Done;
                Some(Step::Error {
                    message: format!("Radius OAuth config returned invalid JSON: {error}"),
                })
            }
        }
    }
}

impl OAuthFlowMachine for RadiusLoginMachine {
    fn start(&mut self, _now_ms: i64) -> Step {
        self.phase = LoginPhase::AwaitingConfig;
        Step::Request {
            request: config_request(&self.gateway),
        }
    }

    fn advance(&mut self, input: StepInput, now_ms: i64) -> Step {
        if matches!(input, StepInput::Aborted) {
            self.phase = LoginPhase::Done;
            return Step::Error {
                message: CANCEL_MESSAGE.to_string(),
            };
        }

        match (&self.phase, input) {
            (LoginPhase::AwaitingConfig, StepInput::Response(response)) => {
                if let Some(step) = self.on_config(&response) {
                    return step;
                }
                self.phase = LoginPhase::AwaitingMethod;
                self.method_prompt()
            }
            (LoginPhase::AwaitingMethod, StepInput::Input { value }) => {
                if value == LOGIN_METHOD_DEVICE_CODE {
                    let config = self.config();
                    let device_request = HttpRequest::post(
                        &config.device_authorization_endpoint,
                        form_urlencode(&[
                            ("client_id", &config.client_id),
                            ("scope", &config.scope),
                        ]),
                    )
                    .with_header("accept", "application/json")
                    .with_header("content-type", "application/x-www-form-urlencoded");
                    self.phase = LoginPhase::DeviceAwaitingAuth;
                    Step::Request {
                        request: device_request,
                    }
                } else if value == LOGIN_METHOD_BROWSER {
                    self.phase = LoginPhase::BrowserAwaitingProgressAck;
                    Step::Notify {
                        event: AuthEvent::Progress {
                            message: format!("Listening for OAuth callback on {REDIRECT_URI}"),
                        },
                    }
                } else {
                    self.phase = LoginPhase::Done;
                    Step::Error {
                        message: format!("Unknown {} sign-in method: {value}", self.name),
                    }
                }
            }
            (LoginPhase::BrowserAwaitingProgressAck, StepInput::Ack) => {
                self.phase = LoginPhase::BrowserAwaitingUrlAck;
                Step::Notify {
                    event: AuthEvent::AuthUrl {
                        url: self.authorize_url(),
                        instructions: Some("Continue in your browser.".to_string()),
                    },
                }
            }
            (LoginPhase::BrowserAwaitingUrlAck, StepInput::Ack) => {
                self.phase = LoginPhase::BrowserAwaitingCode;
                Step::Prompt {
                    prompt: AuthPrompt {
                        signal: None,
                        kind: AuthPromptKind::ManualCode {
                            message: "Paste the OAuth callback URL (or authorization code) here:"
                                .to_string(),
                            placeholder: Some(REDIRECT_URI.to_string()),
                        },
                    },
                }
            }
            (LoginPhase::BrowserAwaitingCode, StepInput::Input { value }) => {
                self.browser_exchange_step(&value)
            }
            (LoginPhase::BrowserAwaitingToken, StepInput::Response(response)) => {
                self.phase = LoginPhase::Done;
                match interpret_token_response(&response, now_ms) {
                    TokenResult::Credential(credential) => Step::Done { credential },
                    TokenResult::Failed(error) => Step::Error {
                        message: error.message,
                    },
                    TokenResult::Invalid(message) => Step::Error { message },
                }
            }
            (LoginPhase::DeviceAwaitingAuth, StepInput::Response(response)) => {
                self.on_device_auth(&response, now_ms)
            }
            (LoginPhase::DeviceAwaitingCodeAck, StepInput::Ack) => self.begin_poll(now_ms),
            (LoginPhase::DevicePolling, StepInput::Response(response)) => {
                self.advance_poll(&response, now_ms)
            }
            _ => {
                self.phase = LoginPhase::Done;
                Step::Error {
                    message: "Radius login flow received an unexpected input".to_string(),
                }
            }
        }
    }
}

/// The phases of the Radius refresh machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RefreshPhase {
    /// Not yet started.
    Start,
    /// Emitted the config-discovery request; awaiting its response.
    AwaitingConfig,
    /// Emitted the refresh token request; awaiting the response.
    AwaitingToken,
    /// Terminal.
    Done,
}

/// The Radius refresh flow machine (`createRadiusOAuth().refresh`;
/// `radius.ts:392-404`).
///
/// Step sequence: `start` → `Request(GET /v1/oauth)`; on the config response →
/// `Request(POST token, grant_type=refresh_token)`; on the token response →
/// `Done` (or `Error`).
pub struct RadiusRefreshMachine {
    gateway: String,
    refresh_token: String,
    phase: RefreshPhase,
    config: Option<RadiusOAuthConfig>,
}

impl RadiusRefreshMachine {
    /// A refresh machine for `gateway` and `refresh_token`.
    pub fn new(gateway: impl Into<String>, refresh_token: impl Into<String>) -> Self {
        Self {
            gateway: gateway.into(),
            refresh_token: refresh_token.into(),
            phase: RefreshPhase::Start,
            config: None,
        }
    }
}

impl OAuthFlowMachine for RadiusRefreshMachine {
    fn start(&mut self, _now_ms: i64) -> Step {
        self.phase = RefreshPhase::AwaitingConfig;
        Step::Request {
            request: config_request(&self.gateway),
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
            (RefreshPhase::AwaitingConfig, StepInput::Response(response)) => {
                if !response.is_ok() {
                    self.phase = RefreshPhase::Done;
                    return Step::Error {
                        message: format!(
                            "Could not load Radius OAuth config from {}: {} {}",
                            self.gateway, response.status, response.body
                        ),
                    };
                }
                match serde_json::from_str::<RadiusOAuthConfig>(&response.body) {
                    Ok(config) => {
                        let request = token_request(
                            &config.token_endpoint,
                            &[
                                ("grant_type", "refresh_token"),
                                ("client_id", &config.client_id),
                                ("refresh_token", &self.refresh_token),
                            ],
                        );
                        self.config = Some(config);
                        self.phase = RefreshPhase::AwaitingToken;
                        Step::Request { request }
                    }
                    Err(error) => {
                        self.phase = RefreshPhase::Done;
                        Step::Error {
                            message: format!("Radius OAuth config returned invalid JSON: {error}"),
                        }
                    }
                }
            }
            (RefreshPhase::AwaitingToken, StepInput::Response(response)) => {
                self.phase = RefreshPhase::Done;
                match interpret_token_response(&response, now_ms) {
                    TokenResult::Credential(credential) => Step::Done { credential },
                    TokenResult::Failed(error) => Step::Error {
                        message: error.message,
                    },
                    TokenResult::Invalid(message) => Step::Error { message },
                }
            }
            _ => {
                self.phase = RefreshPhase::Done;
                Step::Error {
                    message: "Radius refresh flow received an unexpected input".to_string(),
                }
            }
        }
    }
}

/// Generate a random `state` nonce in UUIDv4 string form, mirroring
/// `crypto.randomUUID()` (`radius.ts:259`). The value is an opaque callback
/// nonce; only its round-trip through the authorize URL / callback matters.
fn random_state() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable");
    // RFC 4122 version 4 / variant bits.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Extract `code` / `state` / `error` from a pasted callback URL or bare query
/// string, mirroring what the loopback callback server reads off the request URL
/// (`radius.ts:169-206`).
fn callback_params(input: &str) -> std::collections::BTreeMap<String, String> {
    let value = input.trim();
    let before_fragment = value.split('#').next().unwrap_or(value);
    let query = match before_fragment.split_once('?') {
        Some((_, query)) => query,
        None => before_fragment,
    };
    parse_query(query)
}

/// Radius OAuth flow handler, parameterized by gateway (`createRadiusOAuth`;
/// `radius.ts:360-410`).
#[derive(Debug, Clone)]
pub struct RadiusOAuth {
    name: String,
    /// The normalized gateway URL.
    gateway: String,
}

impl RadiusOAuth {
    /// Construct a Radius handler for `gateway` under display `name`, normalizing
    /// the gateway URL (`createRadiusOAuth`; `radius.ts:360-362`).
    pub fn new(name: impl Into<String>, gateway: impl Into<String>) -> Self {
        let gateway = normalize_radius_gateway_url(&gateway.into());
        Self {
            name: name.into(),
            gateway,
        }
    }

    /// The normalized gateway URL.
    pub fn gateway(&self) -> &str {
        &self.gateway
    }
}

impl OAuthAuth for RadiusOAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn login_machine(&self) -> Box<dyn OAuthFlowMachine> {
        Box::new(RadiusLoginMachine::new(
            self.gateway.clone(),
            self.name.clone(),
        ))
    }

    fn refresh_machine(&self, credential: &OAuthCredential) -> Box<dyn OAuthFlowMachine> {
        Box::new(RadiusRefreshMachine::new(
            self.gateway.clone(),
            credential.refresh.clone(),
        ))
    }

    fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError> {
        // `toAuth` returns `{ apiKey: credential.access }` (`radius.ts:406-408`).
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

#[cfg(test)]
mod tests;
