// straitjacket-allow-file:duplication — the `to_auth` mapping
// (`{ apiKey: credential.access }`) and the `OAuthAuth` impl skeleton
// (name/login_machine/refresh_machine/to_auth) are shared verbatim with the four
// provider stubs by design; the clone detector reads those mirrored members
// across the provider files as duplicates. The repetition is the intended
// per-provider layout, kept faithful to pi's parallel provider modules.
//! Anthropic OAuth flow (Claude Pro/Max) — reference provider implementation.
//!
//! Ported from pi-ai's `packages/ai/src/auth/oauth/anthropic.ts` at pinned
//! commit `3da591ab`. This is the reference [`OAuthFlowMachine`] port: both
//! login and refresh are modelled as state machines that yield [`Step`]s and
//! consume [`StepInput`]s, so the JS shim and the pure-Rust
//! [`super::flow::run_flow`] driver advance the exact same logic.
//!
//! # Scope
//!
//! Binding the real TCP loopback callback listener (`node:http.createServer` on
//! port [`CALLBACK_PORT`]) is out of scope — there is no socket seam among the
//! five providers. The login machine drives the **manual-code path** the pi test
//! exercises (`anthropic-oauth.test.ts`): notify the authorize URL, prompt for
//! the pasted code / redirect URL, then exchange it. The callback
//! request-handling constants and [`parse_authorization_input`] are ported as
//! pure Rust; the browser HTML lives in [`super::oauth_page`].

use serde::Deserialize;
use serde_json::{json, Map};

use crate::auth::types::{
    AuthEvent, AuthPrompt, AuthPromptKind, ModelAuth, OAuthAuth, OAuthCredential,
};
use crate::seams::http::{HttpRequest, HttpResponse};

use super::device_code::CANCEL_MESSAGE;
use super::flow::{OAuthFlowMachine, Step, StepInput};
use super::pkce::{generate_pkce, generate_pkce_from_bytes, Pkce};

/// OAuth client id (pi decodes this from base64; `anthropic.ts:29`).
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Authorization endpoint (`anthropic.ts:30`).
pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
/// Token endpoint (`anthropic.ts:31`).
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Default loopback callback host (`PI_OAUTH_CALLBACK_HOST` or this;
/// `anthropic.ts:32`).
pub const DEFAULT_CALLBACK_HOST: &str = "127.0.0.1";
/// Loopback callback port. Binding the real socket is out of scope
/// (`anthropic.ts:33`).
pub const CALLBACK_PORT: u16 = 53692;
/// Callback path (`anthropic.ts:34`).
pub const CALLBACK_PATH: &str = "/callback";
/// Redirect URI (`anthropic.ts:35`).
pub const REDIRECT_URI: &str = "http://localhost:53692/callback";
/// OAuth scopes (`anthropic.ts:36-37`).
pub const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
/// Refresh skew: tokens expire `expires_in*1000 - 5min` (`anthropic.ts:225,338`).
pub const REFRESH_SKEW_MS: i64 = 5 * 60 * 1000;

/// The `auth_url` notify instructions (`anthropic.ts:252-253`).
const AUTH_URL_INSTRUCTIONS: &str =
    "Complete login in your browser. If the browser is on another machine, paste the final redirect URL here.";
/// The `manual_code` prompt message (`anthropic.ts:259`).
const MANUAL_CODE_MESSAGE: &str =
    "Complete login in your browser, or paste the authorization code / redirect URL here:";

/// Parsed authorization input (code / state) from a pasted redirect URL or code
/// (`anthropic.ts:52-80`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedAuthInput {
    /// The authorization code, if any.
    pub code: Option<String>,
    /// The OAuth state, if any.
    pub state: Option<String>,
}

/// Parse a pasted authorization code / redirect URL into code + state
/// (`anthropic.ts:52-80`).
///
/// Mirrors pi's four branches in order: an absolute URL (read `code`/`state`
/// query params), a `code#state` fragment split, a bare `code=...&state=...`
/// query string, else the whole trimmed value as the code.
pub fn parse_authorization_input(input: &str) -> ParsedAuthInput {
    let value = input.trim();
    if value.is_empty() {
        return ParsedAuthInput::default();
    }

    // Branch 1: a valid absolute URL — read its query params (`anthropic.ts:56-64`).
    if let Some(query) = url_query(value) {
        let params = parse_query(query);
        return ParsedAuthInput {
            code: params.get("code").cloned(),
            state: params.get("state").cloned(),
        };
    }

    // Branch 2: `code#state` (`anthropic.ts:66-69`).
    if value.contains('#') {
        let mut parts = value.splitn(2, '#');
        let code = parts.next().unwrap_or("").to_string();
        let state = parts.next().unwrap_or("").to_string();
        return ParsedAuthInput {
            code: Some(code),
            state: Some(state),
        };
    }

    // Branch 3: a bare query string containing `code=` (`anthropic.ts:71-77`).
    if value.contains("code=") {
        let params = parse_query(value);
        return ParsedAuthInput {
            code: params.get("code").cloned(),
            state: params.get("state").cloned(),
        };
    }

    // Branch 4: the whole value is the code (`anthropic.ts:79`).
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

/// The token endpoint's JSON response shape (`anthropic.ts:212,320`).
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

/// Build the token-endpoint POST carrying `body`, with pi's headers
/// (`anthropic.ts:171-179`).
fn token_request(body: serde_json::Value) -> HttpRequest {
    HttpRequest::post(TOKEN_URL, body.to_string())
        .with_header("content-type", "application/json")
        .with_header("accept", "application/json")
}

/// Turn a token-endpoint response into a [`Step`], applying pi's expiry formula
/// `now + expires_in*1000 - 5min` (`anthropic.ts:221-226,334-339`).
fn token_response_step(response: HttpResponse, now_ms: i64) -> Step {
    if !response.is_ok() {
        return Step::Error {
            message: format!(
                "HTTP request failed. status={}; url={}; body={}",
                response.status, TOKEN_URL, response.body
            ),
        };
    }
    match serde_json::from_str::<TokenResponse>(&response.body) {
        Ok(token) => Step::Done {
            credential: OAuthCredential {
                refresh: token.refresh_token,
                access: token.access_token,
                expires: now_ms + token.expires_in * 1000 - REFRESH_SKEW_MS,
                extra: Map::new(),
            },
        },
        Err(error) => Step::Error {
            message: format!(
                "Token exchange returned invalid JSON. url={}; body={}; details={}",
                TOKEN_URL, response.body, error
            ),
        },
    }
}

/// The phases of the Anthropic login machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LoginPhase {
    /// Not yet started.
    Start,
    /// Emitted the `auth_url` notify; awaiting its ack.
    AwaitingAck,
    /// Emitted the `manual_code` prompt; awaiting the pasted code / URL.
    AwaitingInput,
    /// Emitted the token-exchange request; awaiting the response.
    AwaitingToken,
    /// Terminal.
    Done,
}

/// The Anthropic login flow machine (manual-code path; `anthropic.ts:229-303`).
///
/// Step sequence: `start` → `Notify(auth_url)`; on `Ack` →
/// `Prompt(manual_code)`; on `Input` → validate state, then
/// `Request(POST token)`; on `Response` → `Done` (or `Error`).
pub struct AnthropicLoginMachine {
    pkce: Pkce,
    phase: LoginPhase,
}

impl AnthropicLoginMachine {
    /// A login machine with a freshly generated PKCE pair.
    pub fn new() -> Self {
        Self {
            pkce: generate_pkce(),
            phase: LoginPhase::Start,
        }
    }

    /// A login machine with a deterministic PKCE pair from a fixed 32-byte seed,
    /// for reproducible tests.
    pub fn with_pkce_bytes(bytes: [u8; 32]) -> Self {
        Self {
            pkce: generate_pkce_from_bytes(bytes),
            phase: LoginPhase::Start,
        }
    }

    /// The PKCE verifier, which doubles as the OAuth `state` (`anthropic.ts:247`).
    fn verifier(&self) -> &str {
        &self.pkce.verifier
    }

    /// Build the authorize URL (`anthropic.ts:239-251`).
    fn authorize_url(&self) -> String {
        let query = form_urlencode(&[
            ("code", "true"),
            ("client_id", CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPES),
            ("code_challenge", &self.pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", self.verifier()),
        ]);
        format!("{AUTHORIZE_URL}?{query}")
    }

    /// Validate the pasted input against the expected state and build the
    /// authorization-code token request (`anthropic.ts:277-298`).
    fn exchange_step(&self, input: &str) -> Step {
        let parsed = parse_authorization_input(input);

        // `if (parsed.state && parsed.state !== verifier) throw ...`.
        if let Some(state) = &parsed.state {
            if !state.is_empty() && state != self.verifier() {
                return Step::Error {
                    message: "OAuth state mismatch".to_string(),
                };
            }
        }

        let code = parsed.code.unwrap_or_default();
        // `state = parsed.state ?? verifier` (nullish: keeps an empty string).
        let state = parsed.state.unwrap_or_else(|| self.verifier().to_string());

        if code.is_empty() {
            return Step::Error {
                message: "Missing authorization code".to_string(),
            };
        }
        if state.is_empty() {
            return Step::Error {
                message: "Missing OAuth state".to_string(),
            };
        }

        Step::Request {
            request: token_request(json!({
                "grant_type": "authorization_code",
                "client_id": CLIENT_ID,
                "code": code,
                "state": state,
                "redirect_uri": REDIRECT_URI,
                "code_verifier": self.verifier(),
            })),
        }
    }
}

impl Default for AnthropicLoginMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl OAuthFlowMachine for AnthropicLoginMachine {
    fn start(&mut self, _now_ms: i64) -> Step {
        self.phase = LoginPhase::AwaitingAck;
        Step::Notify {
            event: AuthEvent::AuthUrl {
                url: self.authorize_url(),
                instructions: Some(AUTH_URL_INSTRUCTIONS.to_string()),
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
            (LoginPhase::AwaitingAck, StepInput::Ack) => {
                self.phase = LoginPhase::AwaitingInput;
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
            (LoginPhase::AwaitingInput, StepInput::Input { value }) => {
                let step = self.exchange_step(&value);
                self.phase = match step {
                    Step::Request { .. } => LoginPhase::AwaitingToken,
                    _ => LoginPhase::Done,
                };
                step
            }
            (LoginPhase::AwaitingToken, StepInput::Response(response)) => {
                self.phase = LoginPhase::Done;
                token_response_step(response, now_ms)
            }
            _ => {
                self.phase = LoginPhase::Done;
                Step::Error {
                    message: "Anthropic login flow received an unexpected input".to_string(),
                }
            }
        }
    }
}

/// The phases of the Anthropic refresh machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RefreshPhase {
    /// Not yet started.
    Start,
    /// Emitted the refresh request; awaiting the response.
    AwaitingToken,
    /// Terminal.
    Done,
}

/// The Anthropic refresh flow machine (`anthropic.ts:308-340`).
///
/// Step sequence: `start` → `Request(POST token, grant_type=refresh_token)`
/// (never sends `scope`); on `Response` → `Done` (or `Error`).
pub struct AnthropicRefreshMachine {
    refresh_token: String,
    phase: RefreshPhase,
}

impl AnthropicRefreshMachine {
    /// A refresh machine for `refresh_token`.
    pub fn new(refresh_token: impl Into<String>) -> Self {
        Self {
            refresh_token: refresh_token.into(),
            phase: RefreshPhase::Start,
        }
    }
}

impl OAuthFlowMachine for AnthropicRefreshMachine {
    fn start(&mut self, _now_ms: i64) -> Step {
        self.phase = RefreshPhase::AwaitingToken;
        Step::Request {
            request: token_request(json!({
                "grant_type": "refresh_token",
                "client_id": CLIENT_ID,
                "refresh_token": self.refresh_token,
            })),
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
                token_response_step(response, now_ms)
            }
            _ => {
                self.phase = RefreshPhase::Done;
                Step::Error {
                    message: "Anthropic refresh flow received an unexpected input".to_string(),
                }
            }
        }
    }
}

/// Anthropic OAuth flow handler (`anthropic.ts:342-350`).
#[derive(Debug, Default, Clone)]
pub struct AnthropicOAuth;

impl AnthropicOAuth {
    /// Construct the handler.
    pub fn new() -> Self {
        Self
    }
}

impl OAuthAuth for AnthropicOAuth {
    fn name(&self) -> &str {
        "Anthropic (Claude Pro/Max)"
    }

    fn login_machine(&self) -> Box<dyn OAuthFlowMachine> {
        Box::new(AnthropicLoginMachine::new())
    }

    fn refresh_machine(&self, credential: &OAuthCredential) -> Box<dyn OAuthFlowMachine> {
        Box::new(AnthropicRefreshMachine::new(credential.refresh.clone()))
    }

    fn to_auth(
        &self,
        credential: &OAuthCredential,
    ) -> Result<ModelAuth, crate::auth::error::AuthFlowError> {
        // `toAuth` returns `{ apiKey: credential.access }` (`anthropic.ts:347-349`).
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

#[cfg(test)]
mod tests;
