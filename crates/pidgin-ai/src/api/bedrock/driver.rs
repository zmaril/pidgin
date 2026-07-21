// straitjacket-allow-file:duplication — the pre-start error-shell construction
// (empty `AssistantMessage` + zeroed `Usage`) and the buffered send -> decode
// flow mirror the sibling Mistral/Anthropic drivers by design; the clone
// detector reads the shared boundary-type scaffolding as duplicative, but the
// Bedrock request assembly (bearer auth, ConverseStream URL, binary-eventstream
// decode) is a distinct, load-bearing wire path kept verbatim to pi.
//! The Amazon Bedrock `ConverseStream` driver, the transport-facing half of the
//! ported dialect at [`crate::api::bedrock`]. Ported from pi-ai's
//! `packages/ai/src/api/bedrock-converse-stream.ts` `stream` at pinned commit
//! `3da591ab`.
//!
//! pi builds a `BedrockRuntimeClient` per request and lets
//! `@aws-sdk/client-bedrock-runtime` put a `ConverseStreamCommand` on the wire
//! (`bedrock-converse-stream.ts:216-250`). The SDK owns three things this port
//! reproduces without the SDK: the request URL/method it derives for
//! `ConverseStream`, the auth headers it derives from the client config, and the
//! `vnd.amazon.eventstream` binary decode of the response.
//!
//! # What this port owns (bearer-token AND SigV4 auth paths)
//!
//! - **Request URL.** `POST {endpoint}/model/{modelId}/converse-stream` (AWS
//!   Bedrock Runtime `ConverseStream` REST contract). `endpoint` is the resolved
//!   client endpoint from [`build_client_config`](super::build_client_config):
//!   the explicit `config.endpoint` (`model.baseUrl`) when pinned, else the
//!   SDK-standard `https://bedrock-runtime.{region}.amazonaws.com`. `modelId` is
//!   percent-encoded exactly as the SDK's `extendedEncodeURIComponent` (the
//!   unreserved set `A-Za-z0-9-_.~`).
//! - **Bearer auth (precedence: wins when a token resolves).** pi's Bedrock
//!   API-key bypass: `config.token = { token }` +
//!   `config.authSchemePreference = ["httpBearerAuth"]`
//!   (`bedrock-converse-stream.ts:210-213`) is put on the wire by the SDK's
//!   `httpBearerAuth` scheme as `Authorization: Bearer <token>`. The token is
//!   resolved by [`build_client_config`](super::build_client_config) from
//!   `options.bearerToken || options.apiKey || AWS_BEARER_TOKEN_BEDROCK`.
//! - **SigV4 auth (non-bearer path).** When no bearer token resolves but AWS
//!   credentials do, the SDK SigV4-signs the request. This port reproduces that
//!   without the SDK via [`super::sigv4`]: it signs the exact `POST` body +
//!   headers actually sent (service `bedrock`, region from the resolved config),
//!   writing `x-amz-date` / `x-amz-content-sha256` / `host` /
//!   `x-amz-security-token` (session token only) / `authorization`. The
//!   `AWS_BEDROCK_SKIP_AUTH` proxy path (dummy credentials, no token) flows
//!   through the same SigV4 path, matching pi.
//! - **Request body.** [`build_command_input`](super::build_command_input)
//!   serialized as JSON (`content-type: application/json`). On the SigV4 path
//!   the signature covers these exact body bytes and headers.
//! - **Response decode.** The binary body is decoded by
//!   [`decode_event_stream`](super::decode_event_stream) into the `Value` items
//!   [`parse_converse_stream`](super::parse_converse_stream) accumulates.
//!
//! # Buffered port; true-incremental streaming is a follow-up
//!
//! Like the sibling ports, this is the buffered analogue of pi's async
//! `stream()`: it drives the request, collects the whole response body, and
//! produces the entire event sequence eagerly.
//!
//! - Follow-up (incremental): true token-by-token streaming would feed the
//!   [`send_streaming`](crate::seams::http::HttpTransport::send_streaming) byte
//!   chunks into the eventstream decoder incrementally (per frame) instead of
//!   collecting-then-decoding. This port collects first, so it is buffered.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::api::anthropic::simple_options::{adjust_max_tokens_for_thinking, clamp_reasoning};
use crate::seams::http::{HttpRequest, HttpTransport};
use crate::seams::provider::StreamResult;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, StopReason, Usage, UsageCost,
};
use crate::utils::provider_env::ProviderEnv;

use super::{
    apply_custom_headers, build_client_config, build_command_input, clamp_max_tokens_to_context,
    custom_headers_record, decode_event_stream, is_anthropic_claude_model, parse_converse_stream,
    sigv4, supports_adaptive_thinking, to_adjust_budgets, BedrockModel, BedrockOptions,
    ThinkingBudgets,
};

/// Upper bound on the error-body text folded into a non-2xx error message,
/// mirroring the sibling drivers' truncation of an unbounded provider body.
const MAX_BEDROCK_ERROR_BODY_CHARS: usize = 4000;

/// Build the `ConverseStream` request URL: `POST {endpoint}/model/{modelId}/
/// converse-stream` (AWS Bedrock Runtime REST contract), with `modelId`
/// percent-encoded as the SDK's `extendedEncodeURIComponent`.
fn request_url(endpoint: &str, model_id: &str) -> String {
    format!(
        "{}/model/{}/converse-stream",
        endpoint.trim_end_matches('/'),
        encode_model_id(model_id)
    )
}

/// Percent-encode a path segment exactly as the AWS SDK's
/// `extendedEncodeURIComponent`: everything outside the unreserved set
/// `A-Za-z0-9-_.~` becomes `%XX` (uppercase hex), over the UTF-8 bytes. So an
/// on-demand id like `anthropic.claude-3-5-sonnet-20241022-v2:0` encodes its
/// `:` as `%3A`, and an inference-profile ARN encodes its `:` and `/`.
fn encode_model_id(model_id: &str) -> String {
    let mut out = String::with_capacity(model_id.len());
    for &byte in model_id.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// The auth scheme resolved for the request. Bearer wins if a token resolved
/// (pi precedence); otherwise SigV4 over the resolved AWS credentials + region.
enum ResolvedAuth {
    Bearer(String),
    SigV4 {
        credentials: sigv4::AwsCredentials,
        region: String,
    },
}

/// The resolved endpoint + auth the request needs, extracted from the ported
/// [`build_client_config`](super::build_client_config) output.
struct ResolvedClient {
    endpoint: String,
    auth: ResolvedAuth,
}

/// Resolve the request endpoint + auth scheme from the client config, or an
/// error explaining why neither path is available.
///
/// Precedence matches pi (`bedrock-converse-stream.ts`): a resolved bearer token
/// wins; otherwise the SDK SigV4-signs with resolved AWS credentials. When
/// neither a token nor credentials resolve, a clean pre-start error is returned
/// rather than sending an unsigned request.
fn resolve_client(config: &Value) -> Result<ResolvedClient, String> {
    // pi sets `config.token = { token }` only on the bearer path; when present
    // it wins over any resolved credentials.
    let bearer_token = config
        .get("token")
        .and_then(|token| token.get("token"))
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty());

    let auth = if let Some(bearer_token) = bearer_token {
        ResolvedAuth::Bearer(bearer_token.to_string())
    } else if let Some(credentials) = extract_credentials(config) {
        // pi sets `config.credentials` from the AWS_ACCESS_KEY_ID /
        // AWS_SECRET_ACCESS_KEY (+ optional AWS_SESSION_TOKEN) env subset, or the
        // dummy skip-auth credentials; either way the SDK SigV4-signs. The region
        // is resolved by `build_client_config` (ARN > option > env > default).
        let Some(region) = config.get("region").and_then(Value::as_str) else {
            return Err(
                "Amazon Bedrock SigV4 signing could not resolve a region (set options.region / \
                 AWS_REGION / AWS_DEFAULT_REGION, or use a standard regional endpoint)."
                    .to_string(),
            );
        };
        ResolvedAuth::SigV4 {
            credentials,
            region: region.to_string(),
        }
    } else {
        return Err(no_credentials_error());
    };

    // The SDK targets `config.endpoint` when pinned, else derives the standard
    // regional host from `config.region`.
    let endpoint = if let Some(endpoint) = config.get("endpoint").and_then(Value::as_str) {
        endpoint.to_string()
    } else if let Some(region) = config.get("region").and_then(Value::as_str) {
        format!("https://bedrock-runtime.{region}.amazonaws.com")
    } else {
        return Err(
            "Amazon Bedrock endpoint could not be resolved: no explicit endpoint and no region \
             (set model.baseUrl or options.region / AWS_REGION)."
                .to_string(),
        );
    };

    Ok(ResolvedClient { endpoint, auth })
}

/// Extract SigV4 credentials from the resolved client config's `credentials`
/// object (the env subset `build_client_config` resolves, or the skip-auth dummy
/// credentials). Returns `None` when no credentials object resolved.
fn extract_credentials(config: &Value) -> Option<sigv4::AwsCredentials> {
    let credentials = config.get("credentials")?;
    let access_key_id = credentials
        .get("accessKeyId")
        .and_then(Value::as_str)?
        .to_string();
    let secret_access_key = credentials
        .get("secretAccessKey")
        .and_then(Value::as_str)?
        .to_string();
    let session_token = credentials
        .get("sessionToken")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(sigv4::AwsCredentials {
        access_key_id,
        secret_access_key,
        session_token,
    })
}

/// The pre-start error returned when no Bedrock auth resolves. Enumerates the
/// AWS credential-resolution steps the SDK supports that this port does NOT
/// implement, so the failure is faithful rather than silently narrow.
//
// Follow-up (#282): the implemented credential subset is the bearer token
// (AWS_BEARER_TOKEN_BEDROCK) and the static env credentials
// (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY / AWS_SESSION_TOKEN). The following
// AWS default-credential-chain steps the SDK also supports are DEFERRED and not
// yet resolved here: shared config/credentials profiles (`~/.aws/credentials`,
// AWS_PROFILE), SSO / IAM Identity Center, web-identity / AssumeRoleWithWebIdentity
// (AWS_WEB_IDENTITY_TOKEN_FILE), ECS/EKS container credentials
// (AWS_CONTAINER_CREDENTIALS_RELATIVE_URI / _FULL_URI), and EC2 instance metadata
// (IMDS). Add these to the resolution chain in a follow-up.
fn no_credentials_error() -> String {
    "Amazon Bedrock has no usable credentials: no bearer token (options.apiKey / \
     options.bearerToken / AWS_BEARER_TOKEN_BEDROCK) and no static AWS credentials \
     (AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY [+ AWS_SESSION_TOKEN]) resolved. Profiles, SSO, \
     web-identity, ECS/EKS container credentials, and EC2 instance metadata (IMDS) are not yet \
     supported (follow-up)."
        .to_string()
}

/// Assemble the [`HttpRequest`] for the streaming `ConverseStream` call:
/// `content-type: application/json`, any caller headers (`apply_custom_headers`
/// skips the reserved `authorization` / `host` / `x-amz-*` keys), the serialized
/// command-input body, and the auth headers for the resolved scheme.
///
/// Bearer sets `Authorization: Bearer <token>`. SigV4 signs the exact body bytes
/// and headers (caller headers are applied BEFORE signing, matching pi's Smithy
/// `build`-step middleware, so the signature covers them). `timestamp` is pi's
/// `Date.now()`, formatted as the SigV4 `x-amz-date`.
fn assemble_request(
    model: &BedrockModel,
    resolved: &ResolvedClient,
    options: &BedrockOptions,
    body: String,
    timestamp: i64,
) -> Result<HttpRequest, String> {
    let url = request_url(&resolved.endpoint, &model.id);
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    if let Some(custom) = custom_headers_record(options) {
        apply_custom_headers(&mut headers, &custom);
    }

    match &resolved.auth {
        ResolvedAuth::Bearer(token) => {
            headers.insert("authorization".to_string(), format!("Bearer {token}"));
        }
        ResolvedAuth::SigV4 {
            credentials,
            region,
        } => {
            let amz_date = sigv4::amz_date_from_epoch_ms(timestamp);
            sigv4::sign_request(
                "POST",
                &url,
                &mut headers,
                body.as_bytes(),
                credentials,
                region,
                sigv4::BEDROCK_SERVICE,
                &amz_date,
            )?;
        }
    }

    Ok(HttpRequest {
        method: "POST".to_string(),
        url,
        headers,
        body: Some(body),
    })
}

/// Serialize the command input; only defined for a `serde_json::Value` so a
/// serialization failure is impossible.
fn serialize_body(body: &Value) -> String {
    serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string())
}

/// Format a non-2xx response into the terminal error message. The Bedrock error
/// body on the bearer path is plain JSON (not eventstream), so it is surfaced as
/// UTF-8 text, truncated like the sibling drivers.
fn format_api_error(status: u16, body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return format!("Bedrock API error ({status})");
    }
    format!(
        "Bedrock API error ({status}): {}",
        truncate_error_text(trimmed, MAX_BEDROCK_ERROR_BODY_CHARS)
    )
}

/// Truncate an over-long error body to `max_chars`, appending a count of the
/// dropped characters (mirrors the sibling drivers' `truncateErrorText`).
fn truncate_error_text(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!(
        "{truncated}... [truncated {} chars]",
        char_count - max_chars
    )
}

fn zero_usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost::default(),
    }
}

/// A single-`error`-event result for a failure before the stream's `start` event
/// (missing bearer token, an unresolved endpoint, a non-2xx create, a transport
/// error, or a malformed eventstream body), matching pi's pre-`start` `catch`
/// (`bedrock-converse-stream.ts:293-303`).
fn error_result(model: &BedrockModel, timestamp: i64, message: String) -> StreamResult {
    let output = AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: zero_usage(),
        stop_reason: StopReason::Error,
        error_message: Some(message),
        timestamp,
    };
    StreamResult {
        events: vec![AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: output.clone(),
        }],
        message: output,
    }
}

/// Stream a `ConverseStream` response for `model` over the injected `transport`,
/// mirroring pi's `stream()` on the bearer-token path. `process_env` is the
/// ambient environment snapshot the config/command-input resolution reads
/// (scoped overrides live on `options.env`); `timestamp` is pi's `Date.now()`.
///
/// The response body is fetched as RAW BYTES via
/// [`send_streaming`](crate::seams::http::HttpTransport::send_streaming) — never
/// `send`/`.text()`, which would corrupt the binary eventstream (see
/// [`super::decode_event_stream`]) — then collected and decoded (buffered).
pub fn stream<T: HttpTransport + ?Sized>(
    transport: &T,
    model: &BedrockModel,
    context: &Context,
    options: &BedrockOptions,
    process_env: &ProviderEnv,
    timestamp: i64,
) -> StreamResult {
    // Resolve the SDK client config (endpoint / region / bearer token) via the
    // ported pure builder, then extract the bearer-path request parameters.
    let config = build_client_config(model, options, process_env);
    let resolved = match resolve_client(&config) {
        Ok(resolved) => resolved,
        Err(message) => return error_result(model, timestamp, message),
    };

    let command_input = build_command_input(model, context, options, process_env);
    let request = match assemble_request(
        model,
        &resolved,
        options,
        serialize_body(&command_input),
        timestamp,
    ) {
        Ok(request) => request,
        Err(message) => return error_result(model, timestamp, message),
    };

    // Drive the request over the RAW-BYTES streaming path and buffer the whole
    // body. A binary eventstream body cannot survive the String `send()`/`.text()`
    // path, so `send_streaming` (which reads bytes) is mandatory here.
    let response = match transport.send_streaming(&request) {
        Ok(response) => response,
        Err(error) => return error_result(model, timestamp, error.to_string()),
    };

    let status = response.status;
    let mut body = Vec::new();
    for chunk in response.chunks {
        match chunk {
            Ok(bytes) => body.extend_from_slice(&bytes),
            Err(error) => return error_result(model, timestamp, error.to_string()),
        }
    }

    if !(200..300).contains(&status) {
        return error_result(model, timestamp, format_api_error(status, &body));
    }

    // Decode the binary eventstream into the items the semantic decoder reads.
    let items = match decode_event_stream(&body) {
        Ok(items) => items,
        Err(error) => return error_result(model, timestamp, error.to_string()),
    };

    let outcome = parse_converse_stream(&items, model, timestamp);
    StreamResult {
        events: outcome.events,
        message: outcome.message,
    }
}

/// pi's `streamSimple` (`bedrock-converse-stream.ts:392`): lower the unified
/// `reasoning`/`thinkingBudgets` controls into a shaped [`BedrockOptions`], then
/// run the turn through [`stream`].
///
/// The branching is model-family-aware, mirroring pi exactly:
///
/// - **No reasoning** (`:398`): passthrough with `reasoning` cleared — no thinking
///   config is emitted. (The backend short-circuits to the raw [`stream`] before
///   reaching here, so this branch keeps the function a total faithful port.)
/// - **Claude + adaptive** (`:403-408`): pass `reasoning` + `thinkingBudgets`
///   straight through; the driver's [`build_additional_model_request_fields`]
///   (`super`) emits `thinking.type = "adaptive"` + `output_config.effort`.
/// - **Claude, non-adaptive** (`:413-430`): fit thinking inside the output cap via
///   the shared `adjustMaxTokensForThinking`, re-clamp to the context window, and
///   override the clamped level's budget with
///   `min(adjusted, max(0, maxTokens - 1024))`.
/// - **Non-Claude** (`:433-437`): pass `reasoning` through; the driver ignores it
///   (`build_additional_model_request_fields` returns `None` for non-Claude), so
///   no thinking config reaches the request.
///
/// `base` is pi's `buildBaseOptions` output: `maxTokens` is clamped to the context
/// window (`simple-options.ts:29`) in every branch.
pub fn stream_simple<T: HttpTransport + ?Sized>(
    transport: &T,
    model: &BedrockModel,
    context: &Context,
    options: &BedrockOptions,
    process_env: &ProviderEnv,
    timestamp: i64,
) -> StreamResult {
    // pi's `buildBaseOptions` clamps `options.maxTokens ?? model.maxTokens` to the
    // context window (`simple-options.ts:29`); every branch below streams off this
    // clamped base cap.
    let base_max_tokens = clamp_max_tokens_to_context(
        model,
        context,
        options.max_tokens.unwrap_or(model.max_tokens),
    );

    let Some(reasoning) = options.reasoning else {
        // `:398-400` — passthrough, reasoning cleared.
        let shaped = BedrockOptions {
            max_tokens: Some(base_max_tokens),
            reasoning: None,
            ..options.clone()
        };
        return stream(transport, model, context, &shaped, process_env, timestamp);
    };

    if is_anthropic_claude_model(model) {
        if supports_adaptive_thinking(&model.id, model.name_ref()) {
            // `:403-408` — adaptive Claude: reasoning + budgets pass through.
            let shaped = BedrockOptions {
                max_tokens: Some(base_max_tokens),
                reasoning: Some(reasoning),
                thinking_budgets: options.thinking_budgets.clone(),
                ..options.clone()
            };
            return stream(transport, model, context, &shaped, process_env, timestamp);
        }

        // `:413-430` — budget-based Claude: fit thinking inside the output cap,
        // re-clamp to context, and override the clamped level's budget.
        let adjusted = adjust_max_tokens_for_thinking(
            Some(base_max_tokens),
            model.max_tokens,
            reasoning,
            options
                .thinking_budgets
                .as_ref()
                .map(to_adjust_budgets)
                .as_ref(),
        );
        let max_tokens = clamp_max_tokens_to_context(model, context, adjusted.max_tokens);
        // `Math.min(adjusted.thinkingBudget, Math.max(0, maxTokens - 1024))` (`:428`).
        let budget = adjusted
            .thinking_budget
            .min(max_tokens.saturating_sub(1024));

        // `{ ...(options.thinkingBudgets || {}), [clampReasoning(reasoning)!]: budget }` (`:426-428`).
        let mut thinking_budgets: ThinkingBudgets =
            options.thinking_budgets.clone().unwrap_or_default();
        thinking_budgets.insert(clamp_reasoning(reasoning), budget);

        let shaped = BedrockOptions {
            max_tokens: Some(max_tokens),
            reasoning: Some(reasoning),
            thinking_budgets: Some(thinking_budgets),
            ..options.clone()
        };
        return stream(transport, model, context, &shaped, process_env, timestamp);
    }

    // `:433-437` — non-Claude passthrough (the driver ignores reasoning).
    let shaped = BedrockOptions {
        max_tokens: Some(base_max_tokens),
        reasoning: Some(reasoning),
        thinking_budgets: options.thinking_budgets.clone(),
        ..options.clone()
    };
    stream(transport, model, context, &shaped, process_env, timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use crate::api::bedrock::eventstream::test_support::{
        encode_event, encode_exception, ScriptedBytesTransport,
    };
    use crate::types::{ContentBlock, Message, UserContent, UserMessage, UserRole};

    /// A neutral non-reasoning bedrock model targeting `base_url` (a standard
    /// regional host, so `build_client_config` pins it as the explicit endpoint).
    fn model(base_url: &str) -> BedrockModel {
        serde_json::from_value(json!({
            "id": "anthropic.claude-3-5-sonnet-20241022-v2:0",
            "name": "Claude 3.5 Sonnet",
            "api": "bedrock-converse-stream",
            "provider": "amazon-bedrock",
            "cost": { "input": 1.0, "output": 5.0, "cacheRead": 0.1, "cacheWrite": 1.25 },
            "reasoning": false,
            "input": ["text"],
            "baseUrl": base_url,
            "maxTokens": 8192,
        }))
        .unwrap()
    }

    fn user_context() -> Context {
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("Hi".to_string()),
                timestamp: 0,
            })],
            tools: None,
        }
    }

    /// A bearer-token options bundle (the resolved apiKey path).
    fn bearer_options() -> BedrockOptions {
        BedrockOptions {
            api_key: Some("bedrock-bearer-token".to_string()),
            ..BedrockOptions::default()
        }
    }

    /// A `hello world` ConverseStream turn as real binary eventstream frames.
    fn hello_eventstream() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(encode_event(
            "messageStart",
            &json!({ "role": "assistant" }),
        ));
        bytes.extend(encode_event(
            "contentBlockDelta",
            &json!({ "contentBlockIndex": 0, "delta": { "text": "Hello" } }),
        ));
        bytes.extend(encode_event(
            "contentBlockDelta",
            &json!({ "contentBlockIndex": 0, "delta": { "text": " world" } }),
        ));
        bytes.extend(encode_event(
            "contentBlockStop",
            &json!({ "contentBlockIndex": 0 }),
        ));
        bytes.extend(encode_event(
            "messageStop",
            &json!({ "stopReason": "end_turn" }),
        ));
        bytes.extend(encode_event(
            "metadata",
            &json!({ "usage": { "inputTokens": 10, "outputTokens": 5, "totalTokens": 15 } }),
        ));
        bytes
    }

    /// A raw-bytes transport whose (single) `200` response is the eventstream
    /// body. A binary eventstream cannot survive the `String`-bodied
    /// `ScriptedTransport`, so the bedrock driver is exercised over the bytes
    /// path (mirroring the reqwest transport's `send_streaming`).
    fn scripted_bytes(body: Vec<u8>) -> ScriptedBytesTransport {
        let transport = ScriptedBytesTransport::new();
        transport.push(200, body);
        transport
    }

    #[test]
    fn streams_hello_over_eventstream_and_threads_bearer_and_url() {
        let transport = scripted_bytes(hello_eventstream());
        let result = stream(
            &transport,
            &model("https://bedrock-runtime.us-east-1.amazonaws.com"),
            &user_context(),
            &bearer_options(),
            &ProviderEnv::new(),
            0,
        );

        // The decoded turn matches feeding the Values to parse_converse_stream.
        assert_eq!(result.message.stop_reason, StopReason::Stop);
        assert_eq!(
            result.message.content,
            vec![ContentBlock::Text {
                text: "Hello world".to_string(),
                text_signature: None,
            }]
        );
        assert_eq!(result.message.usage.input, 10);
        assert_eq!(result.message.usage.output, 5);

        // The request carries the bearer credential and the ConverseStream URL,
        // with the model id percent-encoded (`:` -> `%3A`).
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(
            requests[0].url,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-3-5-sonnet-20241022-v2%3A0/converse-stream"
        );
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer bedrock-bearer-token")
        );
        assert_eq!(
            requests[0].headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
    }

    #[test]
    fn eventstream_decode_matches_direct_value_decode() {
        // The splitter -> parse_converse_stream path must yield the SAME outcome
        // as feeding the equivalent Values straight to parse_converse_stream.
        let transport = scripted_bytes(hello_eventstream());
        let via_wire = stream(
            &transport,
            &model("https://bedrock.test"),
            &user_context(),
            &bearer_options(),
            &ProviderEnv::new(),
            0,
        );

        let items = vec![
            json!({ "messageStart": { "role": "assistant" } }),
            json!({ "contentBlockDelta": { "contentBlockIndex": 0, "delta": { "text": "Hello" } } }),
            json!({ "contentBlockDelta": { "contentBlockIndex": 0, "delta": { "text": " world" } } }),
            json!({ "contentBlockStop": { "contentBlockIndex": 0 } }),
            json!({ "messageStop": { "stopReason": "end_turn" } }),
            json!({ "metadata": { "usage": { "inputTokens": 10, "outputTokens": 5, "totalTokens": 15 } } }),
        ];
        let direct = parse_converse_stream(&items, &model("https://bedrock.test"), 0);

        assert_eq!(via_wire.message.content, direct.message.content);
        assert_eq!(via_wire.message.stop_reason, direct.message.stop_reason);
        assert_eq!(via_wire.message.usage.input, direct.message.usage.input);
        assert_eq!(via_wire.events.len(), direct.events.len());
    }

    #[test]
    fn temperature_and_max_tokens_land_in_inference_config() {
        let transport = scripted_bytes(hello_eventstream());
        let options = BedrockOptions {
            api_key: Some("bedrock-bearer-token".to_string()),
            temperature: Some(0.4),
            max_tokens: Some(321),
            ..BedrockOptions::default()
        };
        stream(
            &transport,
            &model("https://bedrock.test"),
            &user_context(),
            &options,
            &ProviderEnv::new(),
            0,
        );

        let body: Value =
            serde_json::from_str(transport.requests()[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body["inferenceConfig"]["temperature"], json!(0.4));
        assert_eq!(body["inferenceConfig"]["maxTokens"], json!(321));
        assert_eq!(
            body["modelId"],
            json!("anthropic.claude-3-5-sonnet-20241022-v2:0")
        );
    }

    #[test]
    fn no_credentials_is_a_clean_error_without_request() {
        // Neither a bearer token nor AWS credentials resolve: a clean pre-start
        // error that enumerates the deferred resolution steps, and no request.
        let transport = ScriptedBytesTransport::new();
        let result = stream(
            &transport,
            &model("https://bedrock.test"),
            &user_context(),
            &BedrockOptions::default(),
            &ProviderEnv::new(),
            0,
        );

        assert_eq!(result.message.stop_reason, StopReason::Error);
        let message = result.message.error_message.as_deref().unwrap();
        assert!(message.contains("no usable credentials"));
        assert!(message.contains("IMDS"));
        assert_eq!(result.events.len(), 1);
        assert!(transport.requests().is_empty());
    }

    /// Scoped provider-env carrying static AWS credentials (the SigV4 path).
    fn sigv4_env(with_session: bool) -> ProviderEnv {
        let mut env = ProviderEnv::new();
        env.insert("AWS_ACCESS_KEY_ID".to_string(), "AKIDEXAMPLE".to_string());
        env.insert(
            "AWS_SECRET_ACCESS_KEY".to_string(),
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
        );
        if with_session {
            env.insert("AWS_SESSION_TOKEN".to_string(), "SESSIONTOKEN".to_string());
        }
        env
    }

    #[test]
    fn sigv4_path_signs_the_request_when_no_bearer_token() {
        // No bearer token, but static AWS creds in the scoped env: the request is
        // SigV4-signed over the exact body + headers, and the decoded turn still
        // matches the eventstream response.
        let transport = scripted_bytes(hello_eventstream());
        let options = BedrockOptions {
            env: Some(sigv4_env(true)),
            ..BedrockOptions::default()
        };
        let result = stream(
            &transport,
            &model("https://bedrock-runtime.us-east-1.amazonaws.com"),
            &user_context(),
            &options,
            &ProviderEnv::new(),
            1_440_938_160_000,
        );

        assert_eq!(result.message.stop_reason, StopReason::Stop);

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        let headers = &requests[0].headers;
        // No bearer Authorization; a SigV4 one instead, over the signed header set.
        let auth = headers.get("authorization").expect("authorization present");
        assert!(auth.starts_with(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/bedrock/aws4_request"
        ));
        assert!(auth.contains(
            "SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token"
        ));
        // The SigV4 headers are on the wire and the content hash covers the body.
        assert_eq!(
            headers.get("x-amz-date").map(String::as_str),
            Some("20150830T123600Z")
        );
        assert_eq!(
            headers.get("x-amz-security-token").map(String::as_str),
            Some("SESSIONTOKEN")
        );
        let body = requests[0].body.as_deref().unwrap();
        let expected_hash = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(body.as_bytes());
            hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        assert_eq!(
            headers.get("x-amz-content-sha256").map(String::as_str),
            Some(expected_hash.as_str())
        );
    }

    #[test]
    fn bearer_token_wins_over_aws_credentials() {
        // Both a bearer token and AWS creds resolve: bearer wins (pi precedence),
        // so the request carries a Bearer Authorization and no SigV4 headers.
        let transport = scripted_bytes(hello_eventstream());
        let options = BedrockOptions {
            api_key: Some("bedrock-bearer-token".to_string()),
            env: Some(sigv4_env(true)),
            ..BedrockOptions::default()
        };
        stream(
            &transport,
            &model("https://bedrock-runtime.us-east-1.amazonaws.com"),
            &user_context(),
            &options,
            &ProviderEnv::new(),
            0,
        );

        let requests = transport.requests();
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer bedrock-bearer-token")
        );
        assert!(!requests[0].headers.contains_key("x-amz-date"));
        assert!(!requests[0].headers.contains_key("x-amz-content-sha256"));
    }

    #[test]
    fn non_2xx_surfaces_json_error_body() {
        let transport = ScriptedBytesTransport::new();
        transport.push(403, b"{\"message\":\"forbidden\"}".to_vec());
        let result = stream(
            &transport,
            &model("https://bedrock.test"),
            &user_context(),
            &bearer_options(),
            &ProviderEnv::new(),
            0,
        );

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("Bedrock API error (403): {\"message\":\"forbidden\"}")
        );
        assert_eq!(transport.requests().len(), 1);
    }

    #[test]
    fn in_stream_exception_frame_surfaces_error() {
        // An eventstream that opens then carries a modeled exception frame decodes
        // into an item parse_converse_stream turns into a terminal error.
        let mut bytes = Vec::new();
        bytes.extend(encode_event(
            "messageStart",
            &json!({ "role": "assistant" }),
        ));
        bytes.extend(encode_exception(
            "validationException",
            &json!({ "message": "bad input" }),
        ));
        let transport = scripted_bytes(bytes);

        let result = stream(
            &transport,
            &model("https://bedrock.test"),
            &user_context(),
            &bearer_options(),
            &ProviderEnv::new(),
            0,
        );
        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert!(result
            .message
            .error_message
            .as_deref()
            .unwrap()
            .contains("bad input"));
    }
}
