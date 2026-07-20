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
//! # What this port owns (BEARER-TOKEN path only)
//!
//! - **Request URL.** `POST {endpoint}/model/{modelId}/converse-stream` (AWS
//!   Bedrock Runtime `ConverseStream` REST contract). `endpoint` is the resolved
//!   client endpoint from [`build_client_config`](super::build_client_config):
//!   the explicit `config.endpoint` (`model.baseUrl`) when pinned, else the
//!   SDK-standard `https://bedrock-runtime.{region}.amazonaws.com`. `modelId` is
//!   percent-encoded exactly as the SDK's `extendedEncodeURIComponent` (the
//!   unreserved set `A-Za-z0-9-_.~`).
//! - **Bearer auth.** pi's Bedrock API-key bypass: `config.token = { token }` +
//!   `config.authSchemePreference = ["httpBearerAuth"]`
//!   (`bedrock-converse-stream.ts:210-213`) is put on the wire by the SDK's
//!   `httpBearerAuth` scheme as `Authorization: Bearer <token>`. The token is
//!   resolved by [`build_client_config`](super::build_client_config) from
//!   `options.bearerToken || options.apiKey || AWS_BEARER_TOKEN_BEDROCK`.
//! - **Request body.** [`build_command_input`](super::build_command_input)
//!   serialized as JSON (`content-type: application/json`).
//! - **Response decode.** The binary body is decoded by
//!   [`decode_event_stream`](super::decode_event_stream) into the `Value` items
//!   [`parse_converse_stream`](super::parse_converse_stream) accumulates.
//!
//! # Buffered + bearer only; SigV4 and true-incremental are follow-ups
//!
//! Like the sibling ports, this is the buffered analogue of pi's async
//! `stream()`: it drives the request, collects the whole response body, and
//! produces the entire event sequence eagerly.
//!
//! - Follow-up (SigV4): the non-bearer AWS-credentials path (SigV4 request
//!   signing over resolved access-key/secret/session credentials + region, and
//!   the `AWS_BEDROCK_SKIP_AUTH` proxy path) is not implemented. When no bearer
//!   token resolves, the driver returns a clean pre-start "not configured" error
//!   rather than attempting to sign — it never panics and never sends an
//!   unsigned request.
//! - Follow-up (incremental): true token-by-token streaming would feed the
//!   [`send_streaming`](crate::seams::http::HttpTransport::send_streaming) byte
//!   chunks into the eventstream decoder incrementally (per frame) instead of
//!   collecting-then-decoding. This port collects first, so it is buffered.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::seams::http::{HttpRequest, HttpTransport};
use crate::seams::provider::StreamResult;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, StopReason, Usage, UsageCost,
};
use crate::utils::provider_env::ProviderEnv;

use super::{
    apply_custom_headers, build_client_config, build_command_input, custom_headers_record,
    decode_event_stream, parse_converse_stream, BedrockModel, BedrockOptions,
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

/// The resolved endpoint + bearer token the request needs, extracted from the
/// ported [`build_client_config`](super::build_client_config) output.
struct ResolvedClient {
    endpoint: String,
    bearer_token: String,
}

/// Resolve the request endpoint and bearer token from the client config, or an
/// error explaining why the bearer path is not available (no token resolved, or
/// no endpoint/region to target). SigV4 credentials are intentionally not a
/// fallback here (documented follow-up).
fn resolve_client(config: &Value) -> Result<ResolvedClient, String> {
    // pi sets `config.token = { token }` only on the bearer path; its absence
    // means SigV4 credentials or skip-auth, neither of which this port drives.
    let bearer_token = config
        .get("token")
        .and_then(|token| token.get("token"))
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty());
    let Some(bearer_token) = bearer_token else {
        return Err(
            "Amazon Bedrock is not configured for the bearer-token path: no bearer token \
             resolved (set options.apiKey / options.bearerToken / AWS_BEARER_TOKEN_BEDROCK). \
             SigV4 credential signing is a follow-up and is not attempted."
                .to_string(),
        );
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

    Ok(ResolvedClient {
        endpoint,
        bearer_token: bearer_token.to_string(),
    })
}

/// Assemble the [`HttpRequest`] for the streaming `ConverseStream` call: the
/// bearer `Authorization` header, `content-type: application/json`, and any
/// caller headers (`apply_custom_headers` skips the reserved `authorization` /
/// `host` / `x-amz-*` keys), plus the serialized command-input body.
fn assemble_request(
    model: &BedrockModel,
    resolved: &ResolvedClient,
    options: &BedrockOptions,
    body: String,
) -> HttpRequest {
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    headers.insert(
        "authorization".to_string(),
        format!("Bearer {}", resolved.bearer_token),
    );
    if let Some(custom) = custom_headers_record(options) {
        apply_custom_headers(&mut headers, &custom);
    }

    HttpRequest {
        method: "POST".to_string(),
        url: request_url(&resolved.endpoint, &model.id),
        headers,
        body: Some(body),
    }
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
    let request = assemble_request(model, &resolved, options, serialize_body(&command_input));

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
    fn missing_bearer_token_is_a_clean_error_without_request() {
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
        assert!(result
            .message
            .error_message
            .as_deref()
            .unwrap()
            .contains("not configured for the bearer-token path"));
        assert_eq!(result.events.len(), 1);
        assert!(transport.requests().is_empty());
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
