// straitjacket-allow-file[:duplication] — this provider mirrors the shared
// `OAuthAuth` impl skeleton (name/login_machine/refresh_machine/to_auth) and the
// per-provider request/response scaffolding that the four sibling provider
// modules also carry; the clone detector reads those mirrored members across the
// provider files as duplicates. The repetition is the intended per-provider
// layout, kept faithful to pi's parallel provider modules. Likewise the small
// form-encode / interval-math helpers echo shapes in `anthropic.rs` /
// `device_code.rs` because they transcribe the same pi primitives.
//! GitHub Copilot OAuth device-code flow.
//!
//! Ported from pi-ai's `packages/ai/src/auth/oauth/github-copilot.ts` at pinned
//! commit `3da591ab`. Both login and refresh are modelled as
//! [`OAuthFlowMachine`]s that yield [`Step`]s and consume [`StepInput`]s, so the
//! JS shim and the pure-Rust [`super::flow::run_flow`] driver advance the exact
//! same logic.
//!
//! # Login chain
//!
//! `prompt(enterprise URL)` → `POST device/code` → `notify(device_code)` →
//! device-code **poll loop** (`POST oauth/access_token`, wait-before-first-poll,
//! honouring `slow_down`) → `GET copilot_internal/v2/token` (copilot token
//! exchange) → `notify("Enabling models...")` → **enable-all** (`POST
//! models/<id>/policy` per model, best-effort) → `GET models` → `Done`.
//!
//! # Refresh chain
//!
//! `GET copilot_internal/v2/token` → `GET models` → `Done` (the refreshed
//! credential carries the freshly filtered `availableModelIds`).
//!
//! # Sync port deviations
//!
//! - The device-code poll loop is expressed as [`Step::Wait`] steps (sleep, then
//!   `POST access_token`). pi's `poll_oauth_device_code_flow` sleeps one final
//!   interval before throwing the timeout error when the server keeps replying
//!   `slow_down`/`pending` past the deadline; because [`Step::Wait`] couples a
//!   sleep with a request, scheduling that trailing sleep would perform one
//!   extra (post-deadline) poll the reference flow never makes. We instead emit
//!   the timeout [`Step::Error`] immediately once the next poll would land on or
//!   after the deadline, so the request count matches pi exactly and only the
//!   wall-clock instant of the error moves earlier. See [`GitHubCopilotLoginMachine::schedule_next_poll`].
//! - `enable-all` is best-effort in pi (`fetch` errors are swallowed per model).
//!   A non-2xx policy response does not fail login here either — the response is
//!   ignored. A hard transport error still aborts, since the shared
//!   [`super::flow::run_flow`] driver surfaces transport failures.
//! - Domain/URL handling (`normalize_domain`, verification-uri normalisation) is
//!   hand-rolled ASCII parsing rather than a full WHATWG URL implementation; it
//!   reproduces `new URL(...).hostname` / `.href` for the inputs the flow sees.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::auth::error::AuthFlowError;
use crate::auth::types::{
    AuthEvent, AuthPrompt, AuthPromptKind, ModelAuth, OAuthAuth, OAuthCredential,
};
use crate::seams::http::{HttpRequest, HttpResponse};

use super::device_code::{
    CANCEL_MESSAGE, DEFAULT_POLL_INTERVAL_SECONDS, MINIMUM_INTERVAL_MS,
    SLOW_DOWN_INTERVAL_INCREMENT_MS, SLOW_DOWN_TIMEOUT_MESSAGE, TIMEOUT_MESSAGE,
};
use super::flow::{OAuthFlowMachine, Step, StepInput};

/// OAuth client id (pi decodes this from base64; `github-copilot.ts:9-10`).
pub const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
/// Copilot API version header value (`github-copilot.ts:18`).
pub const COPILOT_API_VERSION: &str = "2026-06-01";
/// Default (non-enterprise) Copilot API base URL (`github-copilot.ts:85`).
pub const DEFAULT_BASE_URL: &str = "https://api.individual.githubcopilot.com";
/// Refresh skew, in ms: `expires_at*1000 - 5min` (`github-copilot.ts:274`).
pub const REFRESH_SKEW_MS: i64 = 5 * 60 * 1000;

/// Fixed Copilot `User-Agent` header (`github-copilot.ts:12-17`).
pub const COPILOT_USER_AGENT: &str = "GitHubCopilotChat/0.35.0";
/// Copilot editor-version header (`github-copilot.ts:14`).
pub const COPILOT_EDITOR_VERSION: &str = "vscode/1.107.0";
/// Copilot editor-plugin-version header (`github-copilot.ts:15`).
pub const COPILOT_EDITOR_PLUGIN_VERSION: &str = "copilot-chat/0.35.0";
/// Copilot integration-id header (`github-copilot.ts:16`).
pub const COPILOT_INTEGRATION_ID: &str = "vscode-chat";

/// Default GitHub domain when the user does not supply an enterprise host.
const DEFAULT_DOMAIN: &str = "github.com";

/// The enterprise-URL login prompt message (`github-copilot.ts:330`).
const ENTERPRISE_PROMPT_MESSAGE: &str = "GitHub Enterprise URL/domain (blank for github.com)";
/// The enterprise-URL prompt placeholder (`github-copilot.ts:332`).
const ENTERPRISE_PROMPT_PLACEHOLDER: &str = "company.ghe.com";
/// The "enabling models" progress message (`github-copilot.ts:347`).
const ENABLING_MODELS_MESSAGE: &str = "Enabling models...";

/// Known GitHub Copilot model ids whose policy is enabled after login.
///
/// This mirrors `Object.values(GITHUB_COPILOT_MODELS).map(m => m.id)` from
/// `packages/ai/src/providers/github-copilot.models.ts` (pinned commit
/// `3da591ab`), which lives in the providers module (outside this crate's OAuth
/// scope) and is inlined here so the login flow can accept each model's policy.
const GITHUB_COPILOT_MODELS: &[&str] = &[
    "claude-fable-5",
    "claude-haiku-4.5",
    "claude-opus-4.5",
    "claude-opus-4.6",
    "claude-opus-4.7",
    "claude-opus-4.8",
    "claude-sonnet-4",
    "claude-sonnet-4.5",
    "claude-sonnet-4.6",
    "claude-sonnet-5",
    "gemini-2.5-pro",
    "gemini-3-flash-preview",
    "gemini-3.1-pro-preview",
    "gemini-3.5-flash",
    "gpt-4.1",
    "gpt-5-mini",
    "gpt-5.2",
    "gpt-5.2-codex",
    "gpt-5.3-codex",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.4-nano",
    "gpt-5.5",
    "gpt-5.6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "kimi-k2.7-code",
    "mai-code-1-flash-picker",
];

// ---------------------------------------------------------------------------
// URL / domain helpers (`github-copilot.ts:38-90`)
// ---------------------------------------------------------------------------

/// Extract the hostname from a URL-ish string, mirroring `new URL(...).hostname`
/// for the ASCII inputs the flow sees.
fn parse_hostname(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://")?.1;
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host_port.split(':').next().unwrap_or("");
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// Normalise a user-entered enterprise domain to a bare hostname, or `None` when
/// blank/unparseable (`github-copilot.ts:38-49`).
fn normalize_domain(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    parse_hostname(&with_scheme)
}

/// The device-code endpoint for `domain` (`github-copilot.ts:57`).
fn device_code_url(domain: &str) -> String {
    format!("https://{domain}/login/device/code")
}

/// The access-token endpoint for `domain` (`github-copilot.ts:58`).
fn access_token_url(domain: &str) -> String {
    format!("https://{domain}/login/oauth/access_token")
}

/// The copilot-token endpoint for `domain` (`github-copilot.ts:59`).
fn copilot_token_url(domain: &str) -> String {
    format!("https://api.{domain}/copilot_internal/v2/token")
}

/// Parse the `proxy-ep=<host>` claim from a copilot token and swap the leading
/// `proxy.` for `api.`, yielding the API base URL (`github-copilot.ts:66-73`).
fn base_url_from_token(token: &str) -> Option<String> {
    let start = token.find("proxy-ep=")? + "proxy-ep=".len();
    let host = token[start..].split(';').next().unwrap_or("");
    if host.is_empty() {
        return None;
    }
    let api_host = host
        .strip_prefix("proxy.")
        .map_or_else(|| host.to_string(), |rest| format!("api.{rest}"));
    Some(format!("https://{api_host}"))
}

/// Derive the Copilot API base URL from the token's `proxy-ep`, falling back to
/// the enterprise or individual host (`github-copilot.ts:75-83`).
fn get_github_copilot_base_url(token: Option<&str>, enterprise_domain: Option<&str>) -> String {
    if let Some(url) = token.and_then(base_url_from_token) {
        return url;
    }
    if let Some(domain) = enterprise_domain {
        return format!("https://copilot-api.{domain}");
    }
    DEFAULT_BASE_URL.to_string()
}

/// The domain string used for the auth endpoints: the enterprise host, or the
/// public GitHub domain (`github-copilot.ts:245,335`).
fn auth_domain(enterprise_domain: Option<&str>) -> String {
    enterprise_domain.unwrap_or(DEFAULT_DOMAIN).to_string()
}

/// The enterprise domain a stored credential targets, if any
/// (`github-copilot.ts:361-365`).
fn credential_enterprise_domain(credential: &OAuthCredential) -> Option<String> {
    credential
        .extra
        .get("enterpriseUrl")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .and_then(normalize_domain)
}

// ---------------------------------------------------------------------------
// Encoding / header helpers
// ---------------------------------------------------------------------------

/// Whether a byte must be percent-encoded when reconstructing a URL `href`. A
/// pragmatic subset of the WHATWG URL path percent-encode set: C0 controls,
/// DEL, space, `"` `<` `>` `` ` `` `{` `}`, and any non-ASCII byte.
fn should_percent_encode(byte: u8) -> bool {
    matches!(byte, 0x00..=0x20 | 0x7f)
        || byte >= 0x80
        || matches!(byte, b'"' | b'<' | b'>' | b'`' | b'{' | b'}')
}

fn hex_upper(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + (nibble - 10)) as char,
    }
}

/// Whether `scheme` is a syntactically valid URL scheme (`ALPHA *(ALPHA / DIGIT
/// / "+" / "-" / ".")`).
fn is_valid_scheme(scheme: &str) -> bool {
    let mut bytes = scheme.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_alphabetic() => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

/// Reject a non-http(s) `verification_uri` and normalise it to its `href`,
/// mirroring `new URL(verificationUri).href` plus the protocol guard
/// (`github-copilot.ts:166-186`).
fn normalize_verification_uri(raw: &str) -> Result<String, ()> {
    let (scheme, rest) = raw.split_once("://").ok_or(())?;
    let scheme = scheme.to_ascii_lowercase();
    if !is_valid_scheme(&scheme) || (scheme != "http" && scheme != "https") {
        return Err(());
    }
    let mut out = format!("{scheme}://");
    for &byte in rest.as_bytes() {
        if should_percent_encode(byte) {
            out.push('%');
            out.push(hex_upper(byte >> 4));
            out.push(hex_upper(byte & 0x0f));
        } else {
            out.push(byte as char);
        }
    }
    Ok(out)
}

/// Encode `pairs` as an `application/x-www-form-urlencoded` body, mirroring
/// `URLSearchParams.toString()` (space→`+`, `*-._`/alphanumerics stay, else
/// percent-encoded).
fn form_encode(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (key, value)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        encode_form_component(key, &mut out);
        out.push('=');
        encode_form_component(value, &mut out);
    }
    out
}

fn encode_form_component(value: &str, out: &mut String) {
    for &byte in value.as_bytes() {
        match byte {
            b' ' => out.push('+'),
            b'*' | b'-' | b'.' | b'_' => out.push(byte as char),
            b if b.is_ascii_alphanumeric() => out.push(b as char),
            b => {
                out.push('%');
                out.push(hex_upper(b >> 4));
                out.push(hex_upper(b & 0x0f));
            }
        }
    }
}

/// Attach the fixed Copilot request headers (`github-copilot.ts:12-17`).
fn with_copilot_headers(request: HttpRequest) -> HttpRequest {
    request
        .with_header("User-Agent", COPILOT_USER_AGENT)
        .with_header("Editor-Version", COPILOT_EDITOR_VERSION)
        .with_header("Editor-Plugin-Version", COPILOT_EDITOR_PLUGIN_VERSION)
        .with_header("Copilot-Integration-Id", COPILOT_INTEGRATION_ID)
}

// ---------------------------------------------------------------------------
// Request builders shared by login and refresh
// ---------------------------------------------------------------------------

/// The copilot-token exchange request (`github-copilot.ts:244-262`): a GET with
/// the GitHub access token as the bearer.
fn copilot_token_request(domain: &str, github_access_token: &str) -> HttpRequest {
    with_copilot_headers(
        HttpRequest::get(copilot_token_url(domain))
            .with_header("Accept", "application/json")
            .with_header("Authorization", format!("Bearer {github_access_token}")),
    )
}

/// The available-models request (`github-copilot.ts:120-133`): a GET carrying
/// the copilot token plus the API-version header.
fn models_request(copilot_token: &str, enterprise_domain: Option<&str>) -> HttpRequest {
    let base_url = get_github_copilot_base_url(Some(copilot_token), enterprise_domain);
    with_copilot_headers(
        HttpRequest::get(format!("{base_url}/models"))
            .with_header("Accept", "application/json")
            .with_header("Authorization", format!("Bearer {copilot_token}")),
    )
    .with_header("X-GitHub-Api-Version", COPILOT_API_VERSION)
}

/// The per-model policy-enablement request (`github-copilot.ts:296-314`).
fn enable_model_request(
    copilot_token: &str,
    model_id: &str,
    enterprise_domain: Option<&str>,
) -> HttpRequest {
    let base_url = get_github_copilot_base_url(Some(copilot_token), enterprise_domain);
    with_copilot_headers(
        HttpRequest::post(
            format!("{base_url}/models/{model_id}/policy"),
            "{\"state\":\"enabled\"}",
        )
        .with_header("Content-Type", "application/json")
        .with_header("Authorization", format!("Bearer {copilot_token}")),
    )
    .with_header("openai-intent", "chat-policy")
    .with_header("x-interaction-type", "chat-policy")
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

/// The device-code response (`github-copilot.ts:20-27`).
#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    interval: Option<f64>,
    expires_in: f64,
}

/// The copilot-token response (`github-copilot.ts:262-272`).
#[derive(Debug, Deserialize)]
struct CopilotTokenResponse {
    token: String,
    expires_at: f64,
}

/// An HTTP-error message shaped like pi's `fetchJson` throw
/// (`${status} ${statusText}: ${text}`; `github-copilot.ts:135-142`).
fn http_error(response: &HttpResponse) -> String {
    format!("{} request failed: {}", response.status, response.body)
}

/// Interpret a poll response body (`github-copilot.ts:200-232`).
enum PollOutcome {
    Complete(String),
    Pending,
    SlowDown(Option<f64>),
    Failed(String),
}

fn parse_poll_outcome(body: &str) -> PollOutcome {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return PollOutcome::Failed("Invalid device token response".to_string());
    };
    if let Some(token) = value.get("access_token").and_then(Value::as_str) {
        return PollOutcome::Complete(token.to_string());
    }
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return match error {
            "authorization_pending" => PollOutcome::Pending,
            "slow_down" => PollOutcome::SlowDown(value.get("interval").and_then(Value::as_f64)),
            other => {
                let suffix = value
                    .get("error_description")
                    .and_then(Value::as_str)
                    .map_or_else(String::new, |d| format!(": {d}"));
                PollOutcome::Failed(format!("Device flow failed: {other}{suffix}"))
            }
        };
    }
    PollOutcome::Failed("Invalid device token response".to_string())
}

/// Whether a catalog model is user-selectable (`github-copilot.ts:100-105`):
/// `model_picker_enabled === true && policy?.state !== "disabled" &&
/// capabilities?.supports?.tool_calls !== false`.
fn is_selectable_model(item: &Value) -> bool {
    let picker_enabled = item.get("model_picker_enabled") == Some(&Value::Bool(true));
    let policy_ok = item
        .get("policy")
        .and_then(|p| p.get("state"))
        .and_then(Value::as_str)
        != Some("disabled");
    let tool_calls_ok = item
        .get("capabilities")
        .and_then(|c| c.get("supports"))
        .and_then(|s| s.get("tool_calls"))
        != Some(&Value::Bool(false));
    picker_enabled && policy_ok && tool_calls_ok
}

/// Filter the models response to the selectable model ids (`github-copilot.ts:107-118`).
fn parse_available_model_ids(body: &str) -> Result<Vec<String>, String> {
    let value: Value =
        serde_json::from_str(body).map_err(|_| "Invalid Copilot models response".to_string())?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "Invalid Copilot models response".to_string())?;
    let mut ids = Vec::new();
    for item in data {
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            if is_selectable_model(item) {
                ids.push(id.to_string());
            }
        }
    }
    Ok(ids)
}

// ---------------------------------------------------------------------------
// Poll-interval math (mirrors `device-code.ts`)
// ---------------------------------------------------------------------------

/// The initial poll interval in ms, floored at [`MINIMUM_INTERVAL_MS`]
/// (`device-code.ts:5-7`).
fn initial_interval_ms(interval_seconds: Option<f64>) -> i64 {
    MINIMUM_INTERVAL_MS
        .max((interval_seconds.unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS) * 1000.0).floor() as i64)
}

/// The interval after a `slow_down`: adopt the server value when finite/positive,
/// else add [`SLOW_DOWN_INTERVAL_INCREMENT_MS`] (`device-code.ts:9`, poll loop).
fn slow_down_interval_ms(current: i64, server_seconds: Option<f64>) -> i64 {
    match server_seconds {
        Some(seconds) if seconds.is_finite() && seconds > 0.0 => {
            MINIMUM_INTERVAL_MS.max((seconds * 1000.0).floor() as i64)
        }
        _ => MINIMUM_INTERVAL_MS.max(current + SLOW_DOWN_INTERVAL_INCREMENT_MS),
    }
}

/// Build the stored credential with its per-provider extras.
fn build_credential(
    refresh: String,
    access: String,
    expires: i64,
    enterprise_domain: Option<&str>,
    model_ids: Vec<String>,
) -> OAuthCredential {
    let mut extra = Map::new();
    if let Some(domain) = enterprise_domain {
        extra.insert(
            "enterpriseUrl".to_string(),
            Value::String(domain.to_string()),
        );
    }
    extra.insert(
        "availableModelIds".to_string(),
        Value::Array(model_ids.into_iter().map(Value::String).collect()),
    );
    OAuthCredential {
        refresh,
        access,
        expires,
        extra,
    }
}

// ---------------------------------------------------------------------------
// Login machine
// ---------------------------------------------------------------------------

/// The phases of the GitHub Copilot login machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LoginPhase {
    /// Not yet started.
    Start,
    /// Emitted the enterprise-URL prompt; awaiting the entered value.
    AwaitingDomain,
    /// Emitted the device-code request; awaiting the response.
    AwaitingDeviceCode,
    /// Emitted the `device_code` notify; awaiting its ack.
    AwaitingDeviceCodeAck,
    /// Waiting/polling for the GitHub access token.
    Polling,
    /// Emitted the copilot-token request; awaiting the response.
    AwaitingCopilotToken,
    /// Emitted the "Enabling models..." notify; awaiting its ack.
    AwaitingProgressAck,
    /// Emitted the policy request for model `index`; awaiting the response.
    EnablingModels(usize),
    /// Emitted the models request; awaiting the response.
    AwaitingModels,
    /// Terminal.
    Done,
}

/// The GitHub Copilot login flow machine (`github-copilot.ts:329-359`).
pub struct GitHubCopilotLoginMachine {
    phase: LoginPhase,
    enterprise_domain: Option<String>,
    domain: String,
    device_code: String,
    interval_ms: i64,
    deadline_ms: i64,
    slow_down_responses: usize,
    github_access_token: String,
    copilot_access: String,
    expires: i64,
}

impl GitHubCopilotLoginMachine {
    /// A fresh login machine.
    pub fn new() -> Self {
        Self {
            phase: LoginPhase::Start,
            enterprise_domain: None,
            domain: DEFAULT_DOMAIN.to_string(),
            device_code: String::new(),
            interval_ms: 0,
            deadline_ms: 0,
            slow_down_responses: 0,
            github_access_token: String::new(),
            copilot_access: String::new(),
            expires: 0,
        }
    }

    fn enterprise(&self) -> Option<&str> {
        self.enterprise_domain.as_deref()
    }

    /// The access-token poll request (`github-copilot.ts:186-199`).
    fn poll_request(&self) -> HttpRequest {
        HttpRequest::post(
            access_token_url(&self.domain),
            form_encode(&[
                ("client_id", CLIENT_ID),
                ("device_code", &self.device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ]),
        )
        .with_header("Accept", "application/json")
        .with_header("Content-Type", "application/x-www-form-urlencoded")
        .with_header("User-Agent", COPILOT_USER_AGENT)
    }

    /// Either wait one interval before the next poll, or emit the timeout error
    /// when that poll would land on or after the deadline.
    ///
    /// pi sleeps the trailing interval before throwing; we error immediately (see
    /// the module-level sync-port deviation note) so the request count matches.
    fn schedule_next_poll(&self, now_ms: i64) -> Step {
        let remaining = self.deadline_ms - now_ms;
        if remaining > 0 && self.interval_ms < remaining {
            Step::Wait {
                delay_ms: self.interval_ms as u64,
                request: self.poll_request(),
            }
        } else {
            Step::Error {
                message: self.timeout_message().to_string(),
            }
        }
    }

    fn timeout_message(&self) -> &'static str {
        if self.slow_down_responses > 0 {
            SLOW_DOWN_TIMEOUT_MESSAGE
        } else {
            TIMEOUT_MESSAGE
        }
    }

    /// Consume the enterprise-URL input and emit the device-code request
    /// (`github-copilot.ts:334-344`).
    fn begin_device_flow(&mut self, input: &str) -> Step {
        let trimmed = input.trim();
        let enterprise = normalize_domain(input);
        if !trimmed.is_empty() && enterprise.is_none() {
            self.phase = LoginPhase::Done;
            return Step::Error {
                message: "Invalid GitHub Enterprise URL/domain".to_string(),
            };
        }
        self.domain = auth_domain(enterprise.as_deref());
        self.enterprise_domain = enterprise;
        self.phase = LoginPhase::AwaitingDeviceCode;
        Step::Request {
            request: HttpRequest::post(
                device_code_url(&self.domain),
                form_encode(&[("client_id", CLIENT_ID), ("scope", "read:user")]),
            )
            .with_header("Accept", "application/json")
            .with_header("Content-Type", "application/x-www-form-urlencoded")
            .with_header("User-Agent", COPILOT_USER_AGENT),
        }
    }

    /// Consume the device-code response and emit the `device_code` notify
    /// (`github-copilot.ts:144-185,345-346`).
    fn on_device_code(&mut self, response: HttpResponse) -> Step {
        if !response.is_ok() {
            self.phase = LoginPhase::Done;
            return Step::Error {
                message: http_error(&response),
            };
        }
        let device: DeviceCodeResponse = match serde_json::from_str(&response.body) {
            Ok(device) => device,
            Err(_) => {
                self.phase = LoginPhase::Done;
                return Step::Error {
                    message: "Invalid device code response fields".to_string(),
                };
            }
        };
        let verification_uri = match normalize_verification_uri(&device.verification_uri) {
            Ok(uri) => uri,
            Err(()) => {
                self.phase = LoginPhase::Done;
                return Step::Error {
                    message: "Untrusted verification_uri in device code response".to_string(),
                };
            }
        };
        self.device_code = device.device_code;
        self.interval_ms = initial_interval_ms(device.interval);
        // Deadline is anchored when polling begins (on the notify ack); stash the
        // lifetime in ms until then.
        self.deadline_ms = (device.expires_in * 1000.0) as i64;
        self.phase = LoginPhase::AwaitingDeviceCodeAck;
        Step::Notify {
            event: AuthEvent::DeviceCode {
                user_code: device.user_code,
                verification_uri,
                interval_seconds: device.interval,
                expires_in_seconds: Some(device.expires_in),
            },
        }
    }

    /// Consume a poll response, driving the poll loop or advancing to the copilot
    /// token exchange (`github-copilot.ts:186-232`).
    fn on_poll(&mut self, response: HttpResponse, now_ms: i64) -> Step {
        if !response.is_ok() {
            self.phase = LoginPhase::Done;
            return Step::Error {
                message: http_error(&response),
            };
        }
        match parse_poll_outcome(&response.body) {
            PollOutcome::Complete(token) => {
                self.github_access_token = token;
                self.phase = LoginPhase::AwaitingCopilotToken;
                Step::Request {
                    request: copilot_token_request(&self.domain, &self.github_access_token),
                }
            }
            PollOutcome::Pending => self.schedule_next_poll(now_ms),
            PollOutcome::SlowDown(server_seconds) => {
                self.slow_down_responses += 1;
                self.interval_ms = slow_down_interval_ms(self.interval_ms, server_seconds);
                self.schedule_next_poll(now_ms)
            }
            PollOutcome::Failed(message) => {
                self.phase = LoginPhase::Done;
                Step::Error { message }
            }
        }
    }

    /// Consume the copilot-token response and emit the "Enabling models..."
    /// notify (`github-copilot.ts:244-288,346-347`).
    fn on_copilot_token(&mut self, response: HttpResponse) -> Step {
        if !response.is_ok() {
            self.phase = LoginPhase::Done;
            return Step::Error {
                message: http_error(&response),
            };
        }
        let token: CopilotTokenResponse = match serde_json::from_str(&response.body) {
            Ok(token) => token,
            Err(_) => {
                self.phase = LoginPhase::Done;
                return Step::Error {
                    message: "Invalid Copilot token response fields".to_string(),
                };
            }
        };
        self.copilot_access = token.token;
        self.expires = (token.expires_at * 1000.0) as i64 - REFRESH_SKEW_MS;
        self.phase = LoginPhase::AwaitingProgressAck;
        Step::Notify {
            event: AuthEvent::Progress {
                message: ENABLING_MODELS_MESSAGE.to_string(),
            },
        }
    }

    /// Emit the policy request for model `index`, or the models request once the
    /// catalog is exhausted (`github-copilot.ts:290-320,348-359`).
    fn enable_or_finish(&mut self, index: usize) -> Step {
        if index < GITHUB_COPILOT_MODELS.len() {
            self.phase = LoginPhase::EnablingModels(index);
            Step::Request {
                request: enable_model_request(
                    &self.copilot_access,
                    GITHUB_COPILOT_MODELS[index],
                    self.enterprise(),
                ),
            }
        } else {
            self.phase = LoginPhase::AwaitingModels;
            Step::Request {
                request: models_request(&self.copilot_access, self.enterprise()),
            }
        }
    }

    /// Consume the models response and finish (`github-copilot.ts:349-359`).
    fn on_models(&mut self, response: HttpResponse) -> Step {
        self.phase = LoginPhase::Done;
        if !response.is_ok() {
            return Step::Error {
                message: http_error(&response),
            };
        }
        match parse_available_model_ids(&response.body) {
            Ok(model_ids) => Step::Done {
                credential: build_credential(
                    std::mem::take(&mut self.github_access_token),
                    std::mem::take(&mut self.copilot_access),
                    self.expires,
                    self.enterprise(),
                    model_ids,
                ),
            },
            Err(message) => Step::Error { message },
        }
    }
}

impl Default for GitHubCopilotLoginMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl OAuthFlowMachine for GitHubCopilotLoginMachine {
    fn start(&mut self, _now_ms: i64) -> Step {
        self.phase = LoginPhase::AwaitingDomain;
        Step::Prompt {
            prompt: AuthPrompt {
                signal: None,
                kind: AuthPromptKind::Text {
                    message: ENTERPRISE_PROMPT_MESSAGE.to_string(),
                    placeholder: Some(ENTERPRISE_PROMPT_PLACEHOLDER.to_string()),
                },
            },
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
            (LoginPhase::AwaitingDomain, StepInput::Input { value }) => {
                self.begin_device_flow(&value)
            }
            (LoginPhase::AwaitingDeviceCode, StepInput::Response(response)) => {
                self.on_device_code(response)
            }
            (LoginPhase::AwaitingDeviceCodeAck, StepInput::Ack) => {
                // Anchor the poll deadline to now (`device-code.ts` deadline base).
                self.deadline_ms += now_ms;
                self.phase = LoginPhase::Polling;
                self.schedule_next_poll(now_ms)
            }
            (LoginPhase::Polling, StepInput::Response(response)) => self.on_poll(response, now_ms),
            (LoginPhase::AwaitingCopilotToken, StepInput::Response(response)) => {
                self.on_copilot_token(response)
            }
            (LoginPhase::AwaitingProgressAck, StepInput::Ack) => self.enable_or_finish(0),
            (LoginPhase::EnablingModels(index), StepInput::Response(_)) => {
                // Best-effort: the policy response (ok or not) is ignored.
                let next = index + 1;
                self.enable_or_finish(next)
            }
            (LoginPhase::AwaitingModels, StepInput::Response(response)) => self.on_models(response),
            _ => {
                self.phase = LoginPhase::Done;
                Step::Error {
                    message: "GitHub Copilot login flow received an unexpected input".to_string(),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Refresh machine
// ---------------------------------------------------------------------------

/// The phases of the GitHub Copilot refresh machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RefreshPhase {
    /// Not yet started.
    Start,
    /// Emitted the copilot-token request; awaiting the response.
    AwaitingCopilotToken,
    /// Emitted the models request; awaiting the response.
    AwaitingModels,
    /// Terminal.
    Done,
}

/// The GitHub Copilot refresh flow machine (`github-copilot.ts:244-292`).
pub struct GitHubCopilotRefreshMachine {
    phase: RefreshPhase,
    refresh_token: String,
    enterprise_domain: Option<String>,
    domain: String,
    copilot_access: String,
    expires: i64,
}

impl GitHubCopilotRefreshMachine {
    /// A refresh machine for `credential`.
    pub fn new(credential: &OAuthCredential) -> Self {
        let enterprise_domain = credential_enterprise_domain(credential);
        Self {
            phase: RefreshPhase::Start,
            refresh_token: credential.refresh.clone(),
            domain: auth_domain(enterprise_domain.as_deref()),
            enterprise_domain,
            copilot_access: String::new(),
            expires: 0,
        }
    }

    fn enterprise(&self) -> Option<&str> {
        self.enterprise_domain.as_deref()
    }
}

impl OAuthFlowMachine for GitHubCopilotRefreshMachine {
    fn start(&mut self, _now_ms: i64) -> Step {
        self.phase = RefreshPhase::AwaitingCopilotToken;
        Step::Request {
            request: copilot_token_request(&self.domain, &self.refresh_token),
        }
    }

    fn advance(&mut self, input: StepInput, _now_ms: i64) -> Step {
        if matches!(input, StepInput::Aborted) {
            self.phase = RefreshPhase::Done;
            return Step::Error {
                message: CANCEL_MESSAGE.to_string(),
            };
        }
        match (&self.phase, input) {
            (RefreshPhase::AwaitingCopilotToken, StepInput::Response(response)) => {
                if !response.is_ok() {
                    self.phase = RefreshPhase::Done;
                    return Step::Error {
                        message: http_error(&response),
                    };
                }
                let token: CopilotTokenResponse = match serde_json::from_str(&response.body) {
                    Ok(token) => token,
                    Err(_) => {
                        self.phase = RefreshPhase::Done;
                        return Step::Error {
                            message: "Invalid Copilot token response fields".to_string(),
                        };
                    }
                };
                self.copilot_access = token.token;
                self.expires = (token.expires_at * 1000.0) as i64 - REFRESH_SKEW_MS;
                self.phase = RefreshPhase::AwaitingModels;
                Step::Request {
                    request: models_request(&self.copilot_access, self.enterprise()),
                }
            }
            (RefreshPhase::AwaitingModels, StepInput::Response(response)) => {
                self.phase = RefreshPhase::Done;
                if !response.is_ok() {
                    return Step::Error {
                        message: http_error(&response),
                    };
                }
                match parse_available_model_ids(&response.body) {
                    Ok(model_ids) => Step::Done {
                        credential: build_credential(
                            std::mem::take(&mut self.refresh_token),
                            std::mem::take(&mut self.copilot_access),
                            self.expires,
                            self.enterprise(),
                            model_ids,
                        ),
                    },
                    Err(message) => Step::Error { message },
                }
            }
            _ => {
                self.phase = RefreshPhase::Done;
                Step::Error {
                    message: "GitHub Copilot refresh flow received an unexpected input".to_string(),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// OAuthAuth handler
// ---------------------------------------------------------------------------

/// GitHub Copilot OAuth flow handler (`github-copilot.ts:367-379`).
#[derive(Debug, Default, Clone)]
pub struct GitHubCopilotOAuth;

impl GitHubCopilotOAuth {
    /// Construct the handler.
    pub fn new() -> Self {
        Self
    }
}

impl OAuthAuth for GitHubCopilotOAuth {
    fn name(&self) -> &str {
        "GitHub Copilot"
    }

    fn login_machine(&self) -> Box<dyn OAuthFlowMachine> {
        Box::new(GitHubCopilotLoginMachine::new())
    }

    fn refresh_machine(&self, credential: &OAuthCredential) -> Box<dyn OAuthFlowMachine> {
        Box::new(GitHubCopilotRefreshMachine::new(credential))
    }

    fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthFlowError> {
        // `toAuth` returns `{ apiKey: access, baseUrl: getGitHubCopilotBaseUrl(...) }`
        // (`github-copilot.ts:373-377`).
        let enterprise = credential_enterprise_domain(credential);
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            base_url: Some(get_github_copilot_base_url(
                Some(&credential.access),
                enterprise.as_deref(),
            )),
            ..ModelAuth::default()
        })
    }
}

#[cfg(test)]
mod tests;
