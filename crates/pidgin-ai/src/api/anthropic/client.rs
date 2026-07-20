// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `anthropic-messages.ts` `createClient` header-switching (`createClient`,
// `mergeHeaders`, `assertRequestAuth`, `shouldUseFineGrainedToolStreamingBeta`).
// The three auth-mode branches build near-identical `defaultHeaders` maps by
// design; the clone detector reads the mirrored base-header literals as
// duplicates, but each branch is a distinct, load-bearing wire assembly kept
// verbatim to mirror the upstream header composition exactly.
//! Anthropic Messages request/header assembly, ported from pi-ai's
//! `packages/ai/src/api/anthropic-messages.ts` `createClient` at pinned commit
//! `3da591ab`.
//!
//! pi builds an `Anthropic` SDK client per request and lets the SDK put the
//! request on the wire; the auth-mode branch (github-copilot / OAuth / API-key)
//! only shapes the `defaultHeaders` and the auth credential. This seam-targeted
//! port reproduces exactly that: given the model, context, serialized body, and
//! credential, it assembles the [`HttpRequest`] (POST URL + headers + body) the
//! injected [`HttpTransport`](crate::seams::http::HttpTransport) is handed.
//!
//! What this port owns (per pi's `createClient`):
//! - the beta-string composition (`fine-grained-tool-streaming-2025-05-14`,
//!   `interleaved-thinking-2025-05-14`, and the OAuth `claude-code-20250219` /
//!   `oauth-2025-04-20` prefix),
//! - the OAuth identity headers (`user-agent: claude-cli/<version>`, `x-app: cli`),
//! - the `x-session-affinity` header on the API-key path,
//! - the auth credential header (`x-api-key` for API-key, `authorization: Bearer`
//!   for OAuth/copilot), which the Anthropic SDK derives from `apiKey`/`authToken`.
//!
//! The SDK-equivalent defaults pi delegates to the official Anthropic TS SDK
//! (pi's `createClient` never writes them because the SDK injects them before
//! the request hits the wire; our raw transport has no such layer, so they are
//! supplied here to reproduce the same wire request): `content-type:
//! application/json` and `anthropic-version: 2023-06-01` (the SDK's
//! `DEFAULT_ANTHROPIC_VERSION`; the API rejects a request without it with
//! `400 anthropic-version: header is required`). Both are added at low
//! precedence so a caller-supplied `content-type`/`anthropic-version` still
//! wins. `accept: application/json` stays exactly as `createClient` sets it
//! (`anthropic-messages.ts:860/882/901`): the Messages API selects SSE from the
//! body's `stream: true`, not the `accept` header, so the SDK does not override
//! it for streaming.
//!
//! Still left to the SDK and not reproduced here: the non-OAuth `user-agent`
//! (cosmetic — the API does not require it, and the SDK's exact
//! `@anthropic-ai/sdk/<version>` string is not load-bearing). github-copilot
//! dynamic vision headers (`buildCopilotDynamicHeaders`) are a sibling concern
//! and are not assembled here; the copilot branch reproduces only the static
//! header set.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::seams::http::HttpRequest;
use crate::types::{AnthropicMessagesCompat, Context, Model};

use super::compat::get_anthropic_compat;

/// `anthropic-messages.ts:74`.
const CLAUDE_CODE_VERSION: &str = "2.1.75";
/// The Anthropic SDK's `DEFAULT_ANTHROPIC_VERSION`. pi delegates this to the SDK
/// (`createClient` never sets it); supplied here so the raw request carries the
/// header the API requires.
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// `anthropic-messages.ts:168`.
const FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
/// `anthropic-messages.ts:169`.
const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";

/// The three auth modes pi's `createClient` selects between
/// (`anthropic-messages.ts:851/865/889`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// `model.provider === "github-copilot"`: Bearer `authToken`, selective betas.
    Copilot,
    /// `apiKey.includes("sk-ant-oat")`: Bearer `authToken` + Claude Code identity.
    OAuth,
    /// Otherwise: `x-api-key`, optional session-affinity header.
    ApiKey,
}

/// pi's `isOAuthToken` (`anthropic-messages.ts:828`).
pub fn is_oauth_token(api_key: &str) -> bool {
    api_key.contains("sk-ant-oat")
}

/// Resolve the auth mode from the provider and credential, matching the branch
/// order in `createClient` (copilot by provider first, then OAuth by token
/// shape, then API-key).
pub fn resolve_auth_mode(provider: &str, api_key: Option<&str>) -> AuthMode {
    if provider == "github-copilot" {
        return AuthMode::Copilot;
    }
    if api_key.map(is_oauth_token).unwrap_or(false) {
        return AuthMode::OAuth;
    }
    AuthMode::ApiKey
}

/// pi's `shouldUseFineGrainedToolStreamingBeta` (`anthropic-messages.ts:1256`):
/// there are tools and the model does not support eager tool-input streaming.
pub fn should_use_fine_grained_tool_streaming_beta(
    model: &Model<AnthropicMessagesCompat>,
    context: &Context,
) -> bool {
    let has_tools = context
        .tools
        .as_ref()
        .map(|t| !t.is_empty())
        .unwrap_or(false);
    has_tools && !get_anthropic_compat(model).supports_eager_tool_input_streaming
}

/// pi's `assertRequestAuth` (`anthropic-messages.ts:290`): a credential is
/// present, or one of the recognized auth headers is set to a non-empty value.
pub fn assert_request_auth(
    provider: &str,
    api_key: Option<&str>,
    headers: Option<&BTreeMap<String, String>>,
) -> Result<(), String> {
    if api_key.is_some() {
        return Ok(());
    }
    if has_header(headers, "authorization")
        || has_header(headers, "x-api-key")
        || has_header(headers, "cf-aig-authorization")
    {
        return Ok(());
    }
    Err(format!("No API key for provider: {provider}"))
}

/// pi's `hasHeader` (`anthropic-messages.ts:280`): a case-insensitive lookup for
/// a header whose value is non-empty after trimming.
fn has_header(headers: Option<&BTreeMap<String, String>>, name: &str) -> bool {
    let Some(headers) = headers else {
        return false;
    };
    let expected = name.to_ascii_lowercase();
    headers
        .iter()
        .any(|(key, value)| key.to_ascii_lowercase() == expected && !value.trim().is_empty())
}

/// pi's `mergeHeaders` (`anthropic-messages.ts:265`): later sources override
/// earlier ones. Keys are lowercased per the transport seam's convention.
fn merge_into(target: &mut BTreeMap<String, String>, source: &BTreeMap<String, String>) {
    for (key, value) in source {
        target.insert(key.to_ascii_lowercase(), value.clone());
    }
}

/// Compute the `betaFeatures` list shared by all three branches
/// (`anthropic-messages.ts:842-849`). Adaptive-thinking models have interleaved
/// thinking built in, so the interleaved beta is skipped for them.
fn beta_features(
    model: &Model<AnthropicMessagesCompat>,
    context: &Context,
    interleaved_thinking: bool,
) -> Vec<&'static str> {
    let force_adaptive = model
        .compat
        .as_ref()
        .and_then(|c| c.force_adaptive_thinking)
        .unwrap_or(false);
    let needs_interleaved_beta = interleaved_thinking && !force_adaptive;

    let mut betas: Vec<&'static str> = Vec::new();
    if should_use_fine_grained_tool_streaming_beta(model, context) {
        betas.push(FINE_GRAINED_TOOL_STREAMING_BETA);
    }
    if needs_interleaved_beta {
        betas.push(INTERLEAVED_THINKING_BETA);
    }
    betas
}

/// The default request URL the Anthropic SDK derives from `baseURL`: the
/// messages endpoint under the model's base URL.
fn request_url(base_url: &str) -> String {
    format!("{}/v1/messages", base_url.trim_end_matches('/'))
}

/// Assemble the [`HttpRequest`] for a streaming Anthropic Messages call,
/// reproducing pi's `createClient` header switching. `body` is the serialized
/// `MessageCreateParamsStreaming` JSON (from [`super::request::build_params`]).
///
/// `session_id` is the caller's session id already gated on cache retention
/// (pi's `cacheSessionId = cacheRetention === "none" ? undefined : sessionId`);
/// the API-key branch further gates it on `sendSessionAffinityHeaders`.
#[allow(clippy::too_many_arguments)]
pub fn assemble_request(
    mode: AuthMode,
    model: &Model<AnthropicMessagesCompat>,
    context: &Context,
    body: String,
    api_key: Option<&str>,
    options_headers: Option<&BTreeMap<String, String>>,
    interleaved_thinking: bool,
    session_id: Option<&str>,
) -> HttpRequest {
    let betas = beta_features(model, context, interleaved_thinking);
    let mut headers: BTreeMap<String, String> = BTreeMap::new();

    match mode {
        AuthMode::Copilot => {
            headers.insert("accept".to_string(), "application/json".to_string());
            headers.insert(
                "anthropic-dangerous-direct-browser-access".to_string(),
                "true".to_string(),
            );
            if !betas.is_empty() {
                headers.insert("anthropic-beta".to_string(), betas.join(","));
            }
            // mergeHeaders(base, model.headers, dynamicHeaders, optionsHeaders):
            // dynamicHeaders (copilot vision) are a sibling concern, omitted here.
            if let Some(model_headers) = &model.headers {
                merge_into(&mut headers, model_headers);
            }
            if let Some(options_headers) = options_headers {
                merge_into(&mut headers, options_headers);
            }
            set_bearer_auth(&mut headers, api_key);
        }
        AuthMode::OAuth => {
            headers.insert("accept".to_string(), "application/json".to_string());
            headers.insert(
                "anthropic-dangerous-direct-browser-access".to_string(),
                "true".to_string(),
            );
            let mut oauth_betas: Vec<&str> = vec!["claude-code-20250219", "oauth-2025-04-20"];
            oauth_betas.extend(betas.iter().copied());
            headers.insert("anthropic-beta".to_string(), oauth_betas.join(","));
            headers.insert(
                "user-agent".to_string(),
                format!("claude-cli/{CLAUDE_CODE_VERSION}"),
            );
            headers.insert("x-app".to_string(), "cli".to_string());
            // mergeHeaders(base, model.headers, optionsHeaders).
            if let Some(model_headers) = &model.headers {
                merge_into(&mut headers, model_headers);
            }
            if let Some(options_headers) = options_headers {
                merge_into(&mut headers, options_headers);
            }
            set_bearer_auth(&mut headers, api_key);
        }
        AuthMode::ApiKey => {
            headers.insert("accept".to_string(), "application/json".to_string());
            headers.insert(
                "anthropic-dangerous-direct-browser-access".to_string(),
                "true".to_string(),
            );
            if !betas.is_empty() {
                headers.insert("anthropic-beta".to_string(), betas.join(","));
            }
            // sessionAffinityHeaders: gated on sessionId && sendSessionAffinityHeaders.
            if let Some(session_id) = session_id {
                if get_anthropic_compat(model).send_session_affinity_headers {
                    headers.insert("x-session-affinity".to_string(), session_id.to_string());
                }
            }
            // mergeHeaders(base, sessionAffinity, model.headers, optionsHeaders).
            if let Some(model_headers) = &model.headers {
                merge_into(&mut headers, model_headers);
            }
            if let Some(options_headers) = options_headers {
                merge_into(&mut headers, options_headers);
            }
            // The SDK derives x-api-key from apiKey; a caller-supplied header wins.
            if let Some(api_key) = api_key {
                headers
                    .entry("x-api-key".to_string())
                    .or_insert_with(|| api_key.to_string());
            }
        }
    }

    apply_sdk_default_headers(&mut headers);

    HttpRequest {
        method: "POST".to_string(),
        url: request_url(&model.base_url),
        headers,
        body: Some(body),
    }
}

/// Supply the SDK-equivalent defaults pi's `createClient` leaves to the official
/// Anthropic TS SDK (see the module-level boundary note): `content-type:
/// application/json` and `anthropic-version` (`DEFAULT_ANTHROPIC_VERSION`). Both
/// are inserted only when absent, so a caller-supplied header (already merged
/// from `model.headers`/`optionsHeaders`) keeps precedence — matching the SDK,
/// whose built-in defaults sit below `defaultHeaders`.
fn apply_sdk_default_headers(headers: &mut BTreeMap<String, String>) {
    headers
        .entry("anthropic-version".to_string())
        .or_insert_with(|| ANTHROPIC_VERSION.to_string());
    headers
        .entry("content-type".to_string())
        .or_insert_with(|| "application/json".to_string());
}

/// Set `authorization: Bearer <token>` from the credential unless a caller
/// already supplied an `authorization` header (the SDK derives it from
/// `authToken`; user `defaultHeaders` override it).
fn set_bearer_auth(headers: &mut BTreeMap<String, String>, api_key: Option<&str>) {
    if let Some(api_key) = api_key {
        headers
            .entry("authorization".to_string())
            .or_insert_with(|| format!("Bearer {api_key}"));
    }
}

/// Serialize an opaque JSON value; only defined for the request body so a
/// serialization failure is impossible for a `serde_json::Value`.
pub fn serialize_body(body: &Value) -> String {
    serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string())
}
