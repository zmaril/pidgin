// straitjacket-allow-file[:duplication] — this provider mirrors pi's parallel
// OAuth provider modules: the `to_auth` mapping (`{ apiKey: credential.access }`),
// the `OAuthAuth` impl skeleton (name/login_machine/refresh_machine/to_auth), the
// `parse_authorization_input` branches, and the form-urlencode / percent-decode
// helpers are shared verbatim with `anthropic.rs` (the reference provider) and the
// other provider files by design. The clone detector reads those mirrored members
// across the provider files as duplicates; the repetition is the intended
// per-provider layout, kept faithful to pi's parallel provider modules.
//! OpenAI Codex (ChatGPT OAuth) flow.
//!
//! Ported from pi-ai's `packages/ai/src/auth/oauth/openai-codex.ts` at pinned
//! commit `3da591ab`. Login is modelled as a single [`OAuthFlowMachine`] that
//! first prompts for a login method (`browser` / `device_code`) and then drives
//! the selected sub-flow; refresh is a second machine. The JS shim and the
//! pure-Rust [`super::flow::run_flow`] driver advance the exact same logic.
//!
//! # Scope
//!
//! Binding the real TCP loopback callback listener (`node:http` on port
//! [`CALLBACK_PORT`]) is out of scope — there is no socket seam among the five
//! providers. The browser login machine drives the **manual-code path** the pi
//! test exercises (notify the authorize URL, prompt for the pasted code /
//! redirect URL, then exchange it); the callback path/state validation and
//! [`parse_authorization_input`] are ported as pure Rust. The device-code flow is
//! ported in full: user-code request, device_code notify, RFC 8628 poll loop
//! (via [`Step::Wait`]), then the final code exchange whose verifier comes from
//! the server.
//!
//! # Poll deadline boundary (behavior (b))
//!
//! The device-code deadline is **pre-checked**: when the next inter-poll sleep
//! would land on or after the deadline (`now_ms + interval_ms >= deadline`), the
//! poll step emits the timeout [`Step::Error`] immediately with no trailing poll,
//! matching pi's "break before the final poll" (mirrors `github_copilot.rs`). Only
//! the wall-clock instant of the error moves earlier; the request count matches pi.

use serde_json::{json, Map, Value};

use base64::Engine;

use crate::auth::error::AuthFlowError;
use crate::auth::types::{
    AuthEvent, AuthPrompt, AuthPromptKind, AuthSelectOption, ModelAuth, OAuthAuth, OAuthCredential,
};
use crate::seams::http::{HttpRequest, HttpResponse};

use super::device_code::{
    CANCEL_MESSAGE, MINIMUM_INTERVAL_MS, SLOW_DOWN_INTERVAL_INCREMENT_MS,
    SLOW_DOWN_TIMEOUT_MESSAGE, TIMEOUT_MESSAGE,
};
use super::flow::{OAuthFlowMachine, Step, StepInput};
use super::pkce::{generate_pkce, generate_pkce_from_bytes, Pkce};

/// OAuth client id (`openai-codex.ts:26`).
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Auth base URL (`openai-codex.ts:27`).
pub const AUTH_BASE_URL: &str = "https://auth.openai.com";
/// Authorization endpoint (`openai-codex.ts:28`).
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
/// Token endpoint (`openai-codex.ts:29`).
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Loopback redirect URI (`openai-codex.ts:30`).
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
/// Loopback callback port. Binding the real socket is out of scope
/// (`openai-codex.ts:369`).
pub const CALLBACK_PORT: u16 = 1455;
/// Callback path (`openai-codex.ts:337`).
pub const CALLBACK_PATH: &str = "/auth/callback";
/// Device user-code endpoint (`openai-codex.ts:31`).
pub const DEVICE_USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
/// Device token endpoint (`openai-codex.ts:32`).
pub const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
/// Device verification URI shown to the user (`openai-codex.ts:33`).
pub const DEVICE_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
/// Device redirect URI used in the device-code token exchange
/// (`openai-codex.ts:34`).
pub const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
/// Device-code lifetime, in seconds (`openai-codex.ts:35`).
pub const DEVICE_CODE_TIMEOUT_SECONDS: f64 = 15.0 * 60.0;
/// Browser login-method id (`openai-codex.ts:36`).
pub const BROWSER_LOGIN_METHOD: &str = "browser";
/// Device-code login-method id (`openai-codex.ts:37`).
pub const DEVICE_CODE_LOGIN_METHOD: &str = "device_code";
/// OAuth scope (`openai-codex.ts:38`).
pub const SCOPE: &str = "openid profile email offline_access";
/// JWT claim path carrying the ChatGPT account id (`openai-codex.ts:39`).
pub const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

/// The login-method select prompt message (`openai-codex.ts:516`).
const SELECT_METHOD_MESSAGE: &str = "Select OpenAI Codex login method:";
/// The browser login-method option label (`openai-codex.ts:518`).
const BROWSER_OPTION_LABEL: &str = "Browser login (default)";
/// The device-code login-method option label (`openai-codex.ts:519`).
const DEVICE_OPTION_LABEL: &str = "Device code login (headless)";
/// The browser `auth_url` notify instructions (`openai-codex.ts:455`).
const AUTH_URL_INSTRUCTIONS: &str = "A browser window should open. Complete login to finish.";
/// The browser `manual_code` prompt message (`openai-codex.ts:462`).
const MANUAL_CODE_MESSAGE: &str =
    "Complete login in your browser, or paste the authorization code / redirect URL here:";

/// Parsed authorization input (code / state) from a pasted redirect URL or code
/// (`openai-codex.ts:73-101`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedAuthInput {
    /// The authorization code, if any.
    pub code: Option<String>,
    /// The OAuth state, if any.
    pub state: Option<String>,
}

/// Parse a pasted authorization code / redirect URL into code + state
/// (`openai-codex.ts:73-101`).
///
/// Mirrors pi's four branches in order: an absolute URL (read `code`/`state`
/// query params), a `code#state` fragment split, a bare `code=...&state=...`
/// query string, else the whole trimmed value as the code.
pub fn parse_authorization_input(input: &str) -> ParsedAuthInput {
    let value = input.trim();
    if value.is_empty() {
        return ParsedAuthInput::default();
    }

    // Branch 1: a valid absolute URL — read its query params (`openai-codex.ts:77-84`).
    if let Some(query) = url_query(value) {
        let params = parse_query(query);
        return ParsedAuthInput {
            code: params.get("code").cloned(),
            state: params.get("state").cloned(),
        };
    }

    // Branch 2: `code#state` (`openai-codex.ts:87-90`).
    if value.contains('#') {
        let mut parts = value.splitn(2, '#');
        let code = parts.next().unwrap_or("").to_string();
        let state = parts.next().unwrap_or("").to_string();
        return ParsedAuthInput {
            code: Some(code),
            state: Some(state),
        };
    }

    // Branch 3: a bare query string containing `code=` (`openai-codex.ts:92-98`).
    if value.contains("code=") {
        let params = parse_query(value);
        return ParsedAuthInput {
            code: params.get("code").cloned(),
            state: params.get("state").cloned(),
        };
    }

    // Branch 4: the whole value is the code (`openai-codex.ts:100`).
    ParsedAuthInput {
        code: Some(value.to_string()),
        state: None,
    }
}

/// If `value` is an absolute URL (has a scheme, like pi's `new URL(value)`
/// succeeding), return its query string (between the first `?` and the first
/// `#`), else `None`.
fn url_query(value: &str) -> Option<&str> {
    if !has_scheme(value) {
        return None;
    }
    let before_fragment = match value.find('#') {
        Some(i) => &value[..i],
        None => value,
    };
    match before_fragment.find('?') {
        Some(i) => Some(&before_fragment[i + 1..]),
        None => Some(""),
    }
}

/// Whether `value` starts with a URL scheme (`scheme:`), matching the inputs for
/// which JS `new URL(value)` succeeds for our redirect-URL use.
fn has_scheme(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    for &c in &bytes[1..] {
        if c == b':' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.') {
            return false;
        }
    }
    false
}

/// Parse an `application/x-www-form-urlencoded` query string, mirroring
/// `URLSearchParams`: strip a leading `?`, split on `&`, `+`→space, then
/// percent-decode, keeping the first value seen per key.
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

/// Decode one form-urlencoded component: `+`→space, then percent-decode.
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

/// Encode `pairs` as an `application/x-www-form-urlencoded` query string,
/// mirroring `URLSearchParams.toString()` (space→`+`, `*-._` and alphanumerics
/// stay, everything else percent-encoded).
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

/// Lowercase hex of `bytes`, mirroring `Buffer.from(bytes).toString("hex")`
/// (used to derive the browser OAuth `state`; `openai-codex.ts:70`).
fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(nibble(b >> 4));
        out.push(nibble(b & 0x0f));
    }
    out.to_lowercase()
}

/// Decode a JWT payload (`openai-codex.ts:103-113`): split on `.`, base64-decode
/// part `[1]`, JSON-parse. Returns `None` on any structural failure.
fn decode_jwt_payload(token: &str) -> Option<Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let decoded = base64_decode_lenient(parts[1])?;
    serde_json::from_slice(&decoded).ok()
}

/// Decode base64 tolerating both standard and URL-safe alphabets, with or
/// without padding — pi decodes via `atob`; real OpenAI tokens are base64url
/// while the test fixtures are `Buffer.toString("base64")` (standard).
fn base64_decode_lenient(input: &str) -> Option<Vec<u8>> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    STANDARD
        .decode(input)
        .or_else(|_| STANDARD_NO_PAD.decode(input))
        .or_else(|_| URL_SAFE.decode(input))
        .or_else(|_| URL_SAFE_NO_PAD.decode(input))
        .ok()
}

/// Extract the ChatGPT account id from an access token's JWT payload
/// (`openai-codex.ts:395-400`): read
/// `payload["https://api.openai.com/auth"].chatgpt_account_id`, requiring a
/// non-empty string.
fn get_account_id(access_token: &str) -> Option<String> {
    let payload = decode_jwt_payload(access_token)?;
    let account_id = payload
        .get(JWT_CLAIM_PATH)?
        .get("chatgpt_account_id")?
        .as_str()?;
    if account_id.is_empty() {
        None
    } else {
        Some(account_id.to_string())
    }
}

/// Which token operation is in flight, for error-message wording
/// (`openai-codex.ts:42,129`).
#[derive(Debug, Clone, Copy)]
enum TokenOperation {
    Exchange,
    Refresh,
}

impl TokenOperation {
    fn as_str(self) -> &'static str {
        match self {
            TokenOperation::Exchange => "exchange",
            TokenOperation::Refresh => "refresh",
        }
    }
}

/// Build a form-encoded token-endpoint POST (`openai-codex.ts:155-166,174-182`).
fn form_token_request(pairs: &[(&str, &str)]) -> HttpRequest {
    HttpRequest::post(TOKEN_URL, form_urlencode(pairs))
        .with_header("content-type", "application/x-www-form-urlencoded")
}

/// Build a JSON device-endpoint POST (`openai-codex.ts:191-196,240-248`).
fn json_request(url: &str, body: Value) -> HttpRequest {
    HttpRequest::post(url, body.to_string()).with_header("content-type", "application/json")
}

/// Turn a token-endpoint response into a terminal [`Step`], applying pi's expiry
/// formula `now + expires_in*1000` (no skew) and requiring an extractable
/// `accountId` (`openai-codex.ts:126-147,402-415`).
fn token_credential_step(response: &HttpResponse, operation: TokenOperation, now_ms: i64) -> Step {
    let op = operation.as_str();
    if !response.is_ok() {
        // `text || response.statusText`; the seam response carries only the body.
        return Step::Error {
            message: format!(
                "OpenAI Codex token {op} failed ({}): {}",
                response.status, response.body
            ),
        };
    }

    let json: Value = match serde_json::from_str(&response.body) {
        Ok(json) => json,
        Err(_) => {
            return Step::Error {
                message: format!(
                    "OpenAI Codex token {op} response missing fields: {}",
                    response.body
                ),
            }
        }
    };

    let access = json.get("access_token").and_then(Value::as_str);
    let refresh = json.get("refresh_token").and_then(Value::as_str);
    let expires_in = json.get("expires_in").and_then(Value::as_f64);
    let (access, refresh, expires_in) = match (access, refresh, expires_in) {
        (Some(access), Some(refresh), Some(expires_in))
            if !access.is_empty() && !refresh.is_empty() =>
        {
            (access, refresh, expires_in)
        }
        _ => {
            return Step::Error {
                message: format!(
                    "OpenAI Codex token {op} response missing fields: {}",
                    response.body
                ),
            }
        }
    };

    let account_id = match get_account_id(access) {
        Some(id) => id,
        None => {
            return Step::Error {
                message: "Failed to extract accountId from token".to_string(),
            }
        }
    };

    let mut extra = Map::new();
    extra.insert("accountId".to_string(), Value::String(account_id));
    Step::Done {
        credential: OAuthCredential {
            refresh: refresh.to_string(),
            access: access.to_string(),
            expires: now_ms + (expires_in * 1000.0) as i64,
            extra,
        },
    }
}

/// The device user-code response, parsed from the `deviceauth/usercode` endpoint
/// (`openai-codex.ts:210-231`).
struct DeviceAuthInfo {
    device_auth_id: String,
    user_code: String,
    interval_seconds: f64,
}

/// Parse the device user-code response, or an error [`Step`]
/// (`openai-codex.ts:198-231`).
fn parse_device_auth(response: &HttpResponse) -> Result<DeviceAuthInfo, Step> {
    if !response.is_ok() {
        if response.status == 404 {
            return Err(Step::Error {
                message:
                    "OpenAI Codex device code login is not enabled for this server. Use browser login or verify the server URL."
                        .to_string(),
            });
        }
        return Err(Step::Error {
            message: format!(
                "OpenAI Codex device code request failed with status {}{}",
                response.status,
                if response.body.is_empty() {
                    String::new()
                } else {
                    format!(": {}", response.body)
                }
            ),
        });
    }

    let json: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
    let device_auth_id = json.get("device_auth_id").and_then(Value::as_str);
    let user_code = json.get("user_code").and_then(Value::as_str);
    // `interval` may be a string ("5") or a number.
    let interval_seconds = match json.get("interval") {
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    };

    match (device_auth_id, user_code, interval_seconds) {
        (Some(device_auth_id), Some(user_code), Some(interval_seconds))
            if !device_auth_id.is_empty()
                && !user_code.is_empty()
                && interval_seconds.is_finite()
                && interval_seconds >= 0.0 =>
        {
            Ok(DeviceAuthInfo {
                device_auth_id: device_auth_id.to_string(),
                user_code: user_code.to_string(),
                interval_seconds,
            })
        }
        _ => Err(Step::Error {
            message: format!(
                "Invalid OpenAI Codex device code response: {}",
                response.body
            ),
        }),
    }
}

/// A single device-token poll classification (`openai-codex.ts:250-288`).
enum PollOutcome {
    /// Server issued the authorization code + verifier — proceed to exchange.
    Complete { code: String, verifier: String },
    /// Authorization still pending; keep polling.
    Pending,
    /// Server asked the client to slow down.
    SlowDown,
    /// Fatal failure.
    Failed { message: String },
}

/// Classify a device-token poll response (`openai-codex.ts:250-288`).
fn classify_poll(response: &HttpResponse) -> PollOutcome {
    if response.is_ok() {
        let json: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
        let code = json.get("authorization_code").and_then(Value::as_str);
        let verifier = json.get("code_verifier").and_then(Value::as_str);
        return match (code, verifier) {
            (Some(code), Some(verifier)) if !code.is_empty() && !verifier.is_empty() => {
                PollOutcome::Complete {
                    code: code.to_string(),
                    verifier: verifier.to_string(),
                }
            }
            _ => PollOutcome::Failed {
                message: format!(
                    "Invalid OpenAI Codex device auth token response: {}",
                    response.body
                ),
            },
        };
    }

    // 403 and 404 are both treated as pending (`openai-codex.ts:265-267`).
    if response.status == 403 || response.status == 404 {
        return PollOutcome::Pending;
    }

    // Otherwise inspect the body `error` code (string, or `{ code }` object).
    let error_code = serde_json::from_str::<Value>(&response.body)
        .ok()
        .and_then(|json| match json.get("error") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Object(o)) => o.get("code").and_then(Value::as_str).map(str::to_string),
            _ => None,
        });

    match error_code.as_deref() {
        Some("deviceauth_authorization_pending") => PollOutcome::Pending,
        Some("slow_down") => PollOutcome::SlowDown,
        _ => PollOutcome::Failed {
            message: format!(
                "OpenAI Codex device auth failed with status {}{}",
                response.status,
                if response.body.is_empty() {
                    String::new()
                } else {
                    format!(": {}", response.body)
                }
            ),
        },
    }
}

/// The initial poll interval, in ms (`device-code.ts:125-128` `initial_interval_ms`):
/// `max(1000, floor(interval_seconds*1000))`. The device user-code response
/// always carries an interval, so there is no default to apply here.
fn initial_interval_ms(interval_seconds: f64) -> i64 {
    MINIMUM_INTERVAL_MS.max((interval_seconds * 1000.0).floor() as i64)
}

/// The device-code poll loop state, carried between poll [`Step`]s
/// (`device-code.ts:130-192`).
struct DevicePollState {
    device_auth_id: String,
    user_code: String,
    /// Absolute poll deadline, in Unix ms (`now + expires_in*1000`).
    deadline_ms: i64,
    /// Current poll interval, in ms.
    interval_ms: i64,
    /// Count of `slow_down` responses seen (selects the timeout message).
    slow_down_responses: usize,
}

impl DevicePollState {
    /// The device-token poll request (`openai-codex.ts:240-248`).
    fn poll_request(&self) -> HttpRequest {
        json_request(
            DEVICE_TOKEN_URL,
            json!({
                "device_auth_id": self.device_auth_id,
                "user_code": self.user_code,
            }),
        )
    }

    /// After a pending/slow_down poll at `now_ms`, either the next timed poll or
    /// the deadline-exceeded error (`device-code.ts:156-191`).
    ///
    /// The deadline is pre-checked (behavior (b)): when the next inter-poll sleep
    /// would land on or after the deadline (`now_ms + interval_ms >= deadline`) the
    /// timeout `Step::Error` is emitted immediately with no trailing poll, matching
    /// pi's "break before the final poll" (mirrors `github_copilot.rs`).
    fn next_wait_or_timeout(&self, now_ms: i64) -> Step {
        let remaining_ms = self.deadline_ms - now_ms;
        if remaining_ms > 0 && self.interval_ms < remaining_ms {
            Step::Wait {
                delay_ms: self.interval_ms as u64,
                request: self.poll_request(),
            }
        } else {
            Step::Error {
                message: if self.slow_down_responses > 0 {
                    SLOW_DOWN_TIMEOUT_MESSAGE.to_string()
                } else {
                    TIMEOUT_MESSAGE.to_string()
                },
            }
        }
    }
}

/// The phases of the OpenAI Codex login machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LoginPhase {
    /// Not yet started.
    Start,
    /// Emitted the login-method select prompt; awaiting the chosen method.
    AwaitingMethod,
    /// Browser: emitted the `auth_url` notify; awaiting its ack.
    BrowserAwaitingAck,
    /// Browser: emitted the `manual_code` prompt; awaiting the pasted code / URL.
    BrowserAwaitingInput,
    /// Browser: emitted the token-exchange request; awaiting the response.
    BrowserAwaitingToken,
    /// Device: emitted the user-code request; awaiting the response.
    DeviceAwaitingUserCode,
    /// Device: emitted the `device_code` notify; awaiting its ack.
    DeviceAwaitingAck,
    /// Device: polling the device-token endpoint; awaiting each response.
    DevicePolling,
    /// Device: emitted the final token-exchange request; awaiting the response.
    DeviceAwaitingToken,
    /// Terminal.
    Done,
}

/// The OpenAI Codex login flow machine (`openai-codex.ts:444-531`).
///
/// Step sequence — the select gates the two sub-flows:
/// - `start` → `Prompt(select browser|device_code)`.
/// - browser: `Input(browser)` → `Notify(auth_url)`; `Ack` →
///   `Prompt(manual_code)`; `Input(code)` → `Request(POST token, form)`;
///   `Response` → `Done`.
/// - device: `Input(device_code)` → `Request(POST usercode)`; `Response` →
///   `Notify(device_code)`; `Ack` → `Request(POST device token)`; `Response`
///   pending → `Wait(interval, POST device token)`, success →
///   `Request(POST token, form)`; `Response` → `Done`.
pub struct OpenAICodexLoginMachine {
    pkce: Pkce,
    /// The browser OAuth `state` (16 random bytes, hex).
    state: String,
    phase: LoginPhase,
    device: Option<DevicePollState>,
}

impl OpenAICodexLoginMachine {
    /// A login machine with a freshly generated PKCE pair and random state.
    pub fn new() -> Self {
        let mut state_bytes = [0u8; 16];
        getrandom::fill(&mut state_bytes).expect("OS CSPRNG unavailable");
        Self {
            pkce: generate_pkce(),
            state: to_hex(&state_bytes),
            phase: LoginPhase::Start,
            device: None,
        }
    }

    /// A login machine with a deterministic PKCE pair and state from a fixed
    /// 32-byte seed, for reproducible tests. The state is the hex of the seed's
    /// first 16 bytes.
    pub fn with_pkce_bytes(bytes: [u8; 32]) -> Self {
        Self {
            pkce: generate_pkce_from_bytes(bytes),
            state: to_hex(&bytes[..16]),
            phase: LoginPhase::Start,
            device: None,
        }
    }

    /// Build the browser authorize URL (`openai-codex.ts:292-311`).
    fn authorize_url(&self) -> String {
        let query = form_urlencode(&[
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPE),
            ("code_challenge", &self.pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", &self.state),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "pi"),
        ]);
        format!("{AUTHORIZE_URL}?{query}")
    }

    /// The login-method select prompt (`openai-codex.ts:514-521`).
    fn select_method_step(&self) -> Step {
        Step::Prompt {
            prompt: AuthPrompt {
                signal: None,
                kind: AuthPromptKind::Select {
                    message: SELECT_METHOD_MESSAGE.to_string(),
                    options: vec![
                        AuthSelectOption {
                            id: BROWSER_LOGIN_METHOD.to_string(),
                            label: BROWSER_OPTION_LABEL.to_string(),
                            description: None,
                        },
                        AuthSelectOption {
                            id: DEVICE_CODE_LOGIN_METHOD.to_string(),
                            label: DEVICE_OPTION_LABEL.to_string(),
                            description: None,
                        },
                    ],
                },
            },
        }
    }

    /// Validate the pasted input against the expected state and build the browser
    /// authorization-code token request (`openai-codex.ts:475-496`).
    fn browser_exchange_step(&self, input: &str) -> Step {
        let parsed = parse_authorization_input(input);

        // `if (parsed.state && parsed.state !== state) throw "State mismatch"`.
        if let Some(state) = &parsed.state {
            if !state.is_empty() && state != &self.state {
                return Step::Error {
                    message: "State mismatch".to_string(),
                };
            }
        }

        let code = parsed.code.unwrap_or_default();
        if code.is_empty() {
            return Step::Error {
                message: "Missing authorization code".to_string(),
            };
        }

        Step::Request {
            request: form_token_request(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", &code),
                ("code_verifier", &self.pkce.verifier),
                ("redirect_uri", REDIRECT_URI),
            ]),
        }
    }

    /// Advance the device sub-flow's polling phase from a poll response
    /// (`openai-codex.ts:234-290`, `device-code.ts:156-191`).
    fn advance_device_poll(&mut self, response: &HttpResponse, now_ms: i64) -> Step {
        match classify_poll(response) {
            PollOutcome::Complete { code, verifier } => {
                self.phase = LoginPhase::DeviceAwaitingToken;
                Step::Request {
                    request: form_token_request(&[
                        ("grant_type", "authorization_code"),
                        ("client_id", CLIENT_ID),
                        ("code", &code),
                        ("code_verifier", &verifier),
                        ("redirect_uri", DEVICE_REDIRECT_URI),
                    ]),
                }
            }
            PollOutcome::Failed { message } => {
                self.phase = LoginPhase::Done;
                Step::Error { message }
            }
            PollOutcome::Pending => {
                let Some(device) = self.device.as_ref() else {
                    return self.unexpected();
                };
                device.next_wait_or_timeout(now_ms)
            }
            PollOutcome::SlowDown => {
                let Some(device) = self.device.as_mut() else {
                    return self.unexpected();
                };
                device.slow_down_responses += 1;
                // No server interval on this poll → RFC 8628 §3.5 +5s.
                device.interval_ms =
                    MINIMUM_INTERVAL_MS.max(device.interval_ms + SLOW_DOWN_INTERVAL_INCREMENT_MS);
                device.next_wait_or_timeout(now_ms)
            }
        }
    }

    fn unexpected(&mut self) -> Step {
        self.phase = LoginPhase::Done;
        Step::Error {
            message: "OpenAI Codex login flow received an unexpected input".to_string(),
        }
    }
}

impl Default for OpenAICodexLoginMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl OAuthFlowMachine for OpenAICodexLoginMachine {
    fn start(&mut self, _now_ms: i64) -> Step {
        self.phase = LoginPhase::AwaitingMethod;
        self.select_method_step()
    }

    fn advance(&mut self, input: StepInput, now_ms: i64) -> Step {
        if matches!(input, StepInput::Aborted) {
            self.phase = LoginPhase::Done;
            return Step::Error {
                message: CANCEL_MESSAGE.to_string(),
            };
        }

        match (&self.phase, input) {
            (LoginPhase::AwaitingMethod, StepInput::Input { value }) => {
                if value == DEVICE_CODE_LOGIN_METHOD {
                    self.phase = LoginPhase::DeviceAwaitingUserCode;
                    Step::Request {
                        request: json_request(
                            DEVICE_USER_CODE_URL,
                            json!({ "client_id": CLIENT_ID }),
                        ),
                    }
                } else if value == BROWSER_LOGIN_METHOD {
                    self.phase = LoginPhase::BrowserAwaitingAck;
                    Step::Notify {
                        event: AuthEvent::AuthUrl {
                            url: self.authorize_url(),
                            instructions: Some(AUTH_URL_INSTRUCTIONS.to_string()),
                        },
                    }
                } else {
                    self.phase = LoginPhase::Done;
                    Step::Error {
                        message: format!("Unknown OpenAI Codex login method: {value}"),
                    }
                }
            }
            (LoginPhase::BrowserAwaitingAck, StepInput::Ack) => {
                self.phase = LoginPhase::BrowserAwaitingInput;
                Step::Prompt {
                    prompt: AuthPrompt {
                        signal: None,
                        kind: AuthPromptKind::ManualCode {
                            message: MANUAL_CODE_MESSAGE.to_string(),
                            placeholder: Some(REDIRECT_URI.to_string()),
                        },
                    },
                }
            }
            (LoginPhase::BrowserAwaitingInput, StepInput::Input { value }) => {
                let step = self.browser_exchange_step(&value);
                self.phase = match step {
                    Step::Request { .. } => LoginPhase::BrowserAwaitingToken,
                    _ => LoginPhase::Done,
                };
                step
            }
            (LoginPhase::BrowserAwaitingToken, StepInput::Response(response)) => {
                self.phase = LoginPhase::Done;
                token_credential_step(&response, TokenOperation::Exchange, now_ms)
            }
            (LoginPhase::DeviceAwaitingUserCode, StepInput::Response(response)) => {
                match parse_device_auth(&response) {
                    Ok(info) => {
                        let event = AuthEvent::DeviceCode {
                            user_code: info.user_code.clone(),
                            verification_uri: DEVICE_VERIFICATION_URI.to_string(),
                            interval_seconds: Some(info.interval_seconds),
                            expires_in_seconds: Some(DEVICE_CODE_TIMEOUT_SECONDS),
                        };
                        self.device = Some(DevicePollState {
                            device_auth_id: info.device_auth_id,
                            user_code: info.user_code,
                            // Deadline is finalized when polling begins (on Ack).
                            deadline_ms: 0,
                            interval_ms: initial_interval_ms(info.interval_seconds),
                            slow_down_responses: 0,
                        });
                        self.phase = LoginPhase::DeviceAwaitingAck;
                        Step::Notify { event }
                    }
                    Err(error) => {
                        self.phase = LoginPhase::Done;
                        error
                    }
                }
            }
            (LoginPhase::DeviceAwaitingAck, StepInput::Ack) => match self.device.as_mut() {
                Some(device) => {
                    // Deadline from the moment polling starts; first poll is
                    // immediate (`wait_before_first_poll` is false).
                    device.deadline_ms = now_ms + (DEVICE_CODE_TIMEOUT_SECONDS * 1000.0) as i64;
                    let request = device.poll_request();
                    self.phase = LoginPhase::DevicePolling;
                    Step::Request { request }
                }
                None => self.unexpected(),
            },
            (LoginPhase::DevicePolling, StepInput::Response(response)) => {
                self.advance_device_poll(&response, now_ms)
            }
            (LoginPhase::DeviceAwaitingToken, StepInput::Response(response)) => {
                self.phase = LoginPhase::Done;
                token_credential_step(&response, TokenOperation::Exchange, now_ms)
            }
            _ => self.unexpected(),
        }
    }
}

/// The phases of the OpenAI Codex refresh machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RefreshPhase {
    /// Not yet started.
    Start,
    /// Emitted the refresh request; awaiting the response.
    AwaitingToken,
    /// Terminal.
    Done,
}

/// The OpenAI Codex refresh flow machine (`openai-codex.ts:171-188,506-508`).
///
/// Step sequence: `start` → `Request(POST token, grant_type=refresh_token)`; on
/// `Response` → `Done` (or `Error`). A 401 surfaces pi's exact message and never
/// writes to stderr.
pub struct OpenAICodexRefreshMachine {
    refresh_token: String,
    phase: RefreshPhase,
}

impl OpenAICodexRefreshMachine {
    /// A refresh machine for `refresh_token`.
    pub fn new(refresh_token: impl Into<String>) -> Self {
        Self {
            refresh_token: refresh_token.into(),
            phase: RefreshPhase::Start,
        }
    }
}

impl OAuthFlowMachine for OpenAICodexRefreshMachine {
    fn start(&mut self, _now_ms: i64) -> Step {
        self.phase = RefreshPhase::AwaitingToken;
        Step::Request {
            request: form_token_request(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &self.refresh_token),
                ("client_id", CLIENT_ID),
            ]),
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
                token_credential_step(&response, TokenOperation::Refresh, now_ms)
            }
            _ => {
                self.phase = RefreshPhase::Done;
                Step::Error {
                    message: "OpenAI Codex refresh flow received an unexpected input".to_string(),
                }
            }
        }
    }
}

/// OpenAI Codex OAuth flow handler (`openai-codex.ts:510-538`).
#[derive(Debug, Default, Clone)]
pub struct OpenAICodexOAuth;

impl OpenAICodexOAuth {
    /// Construct the handler.
    pub fn new() -> Self {
        Self
    }
}

impl OAuthAuth for OpenAICodexOAuth {
    fn name(&self) -> &str {
        "OpenAI (ChatGPT Plus/Pro)"
    }

    fn login_machine(&self) -> Box<dyn OAuthFlowMachine> {
        Box::new(OpenAICodexLoginMachine::new())
    }

    fn refresh_machine(&self, credential: &OAuthCredential) -> Box<dyn OAuthFlowMachine> {
        Box::new(OpenAICodexRefreshMachine::new(credential.refresh.clone()))
    }

    fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError> {
        // `toAuth` returns `{ apiKey: credential.access }` (`openai-codex.ts:535-537`).
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

#[cfg(test)]
mod tests;
