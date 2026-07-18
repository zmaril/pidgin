// straitjacket-allow-file[:duplication] — these tests transcribe pi's Google
// fixtures verbatim: the hand-built `Model` / `Context` message lists and the
// per-tool JSON-Schema objects are walls of near-identical struct/JSON literals
// by design, and the clone detector reads them as duplicates. They are distinct,
// load-bearing fixtures kept byte-for-byte with pi's test cases.
//! Unit tests for the shared Google helpers, porting the assertions from pi's
//! `packages/ai/test/google-shared-convert-tools.test.ts`,
//! `google-thinking-signature.test.ts`,
//! `google-shared-image-tool-result-routing.test.ts`, and
//! `google-shared-gemini3-unsigned-tool-call.test.ts`, plus local decode-loop
//! sanity checks for [`parse_google_stream`].

use super::*;
use crate::types::{
    AssistantMessage, AssistantRole, Context, Message, ModelCost, StopReason,
    ToolResultMessage, ToolResultRole, Usage, UsageCost, UserContent, UserMessage, UserRole,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn zero_cost() -> ModelCost {
    ModelCost {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        tiers: None,
    }
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

fn make_model(api: &str, provider: &str, id: &str, input: Vec<Modality>) -> GoogleModel {
    GoogleModel {
        id: id.to_string(),
        api: api.to_string(),
        provider: provider.to_string(),
        base_url: "https://example.com".to_string(),
        reasoning: true,
        input,
        cost: zero_cost(),
        headers: None,
    }
}

fn user_text(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 0,
    })
}

fn assistant(content: Vec<ContentBlock>, api: &str, provider: &str, model: &str) -> Message {
    Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: api.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: zero_usage(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp: 0,
    })
}

fn tool_result(id: &str, name: &str, content: Vec<ContentBlock>, is_error: bool) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: id.to_string(),
        tool_name: name.to_string(),
        content,
        details: None,
        added_tool_names: None,
        is_error,
        timestamp: 0,
    })
}

fn tool_call(id: &str, name: &str, arguments: Value, sig: Option<&str>) -> ContentBlock {
    ContentBlock::ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments,
        thought_signature: sig.map(str::to_string),
    }
}

fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_string(),
        text_signature: None,
    }
}

fn image_block(data: &str, mime: &str) -> ContentBlock {
    ContentBlock::Image {
        data: data.to_string(),
        mime_type: mime.to_string(),
    }
}

fn make_tool(parameters: Value) -> Value {
    json!({ "name": "test_tool", "description": "A test tool", "parameters": parameters })
}

// ---------------------------------------------------------------------------
// google-shared-convert-tools.test.ts
// ---------------------------------------------------------------------------

#[test]
fn strips_meta_keys_from_parameters_when_use_parameters_true() {
    let tools = vec![make_tool(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "$id": "urn:bash-tool",
        "$comment": "A bash tool for demonstration",
        "$defs": { "commandDef": { "type": "string" } },
        "definitions": { "legacyDef": { "type": "number" } },
        "type": "object",
        "properties": { "command": { "type": "string" } },
        "required": ["command"],
    }))];

    let result = convert_tools(&tools, true).expect("declarations");
    let decl = &result[0]["functionDeclarations"][0];

    assert_eq!(
        decl["parameters"],
        json!({
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"],
        })
    );
    for key in ["$schema", "$id", "$comment", "$defs", "definitions"] {
        assert!(decl["parameters"].get(key).is_none(), "expected {key} stripped");
    }
}

#[test]
fn recursively_strips_nested_meta_keys() {
    let tools = vec![make_tool(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "deep": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "$id": "urn:nested",
                "type": "string",
            },
        },
    }))];

    let result = convert_tools(&tools, true).expect("declarations");
    let decl = &result[0]["functionDeclarations"][0];

    assert_eq!(
        decl["parameters"],
        json!({
            "type": "object",
            "properties": { "deep": { "type": "string" } },
        })
    );
}

#[test]
fn preserves_ref_while_stripping_meta_keys() {
    let tools = vec![make_tool(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "refProp": { "$ref": "#/$defs/someDef", "type": "string" },
        },
    }))];

    let result = convert_tools(&tools, true).expect("declarations");
    let decl = &result[0]["functionDeclarations"][0];

    assert_eq!(
        decl["parameters"],
        json!({
            "type": "object",
            "properties": {
                "refProp": { "$ref": "#/$defs/someDef", "type": "string" },
            },
        })
    );
}

#[test]
fn does_not_mutate_the_original_parameters() {
    let original = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "command": { "type": "string" } },
        "required": ["command"],
    });
    let tools = vec![make_tool(original.clone())];

    let _ = convert_tools(&tools, true);

    assert_eq!(tools[0]["parameters"], original);
}

#[test]
fn preserves_schema_in_parameters_json_schema_when_use_parameters_false() {
    let params = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "command": { "type": "string" } },
        "required": ["command"],
    });
    let tools = vec![make_tool(params.clone())];

    let result = convert_tools(&tools, false).expect("declarations");
    let decl = &result[0]["functionDeclarations"][0];

    assert_eq!(decl["parametersJsonSchema"], params);
}

#[test]
fn handles_tools_without_schema_gracefully() {
    let tools = vec![make_tool(json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"],
    }))];

    let result = convert_tools(&tools, true).expect("declarations");
    let decl = &result[0]["functionDeclarations"][0];

    assert_eq!(
        decl["parameters"],
        json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
        })
    );
}

#[test]
fn returns_none_for_empty_tool_list() {
    assert!(convert_tools(&[], false).is_none());
    assert!(convert_tools(&[], true).is_none());
}

// ---------------------------------------------------------------------------
// google-thinking-signature.test.ts
// ---------------------------------------------------------------------------

#[test]
fn treats_thought_true_as_thinking() {
    assert!(is_thinking_part(&json!({ "thought": true })));
    assert!(is_thinking_part(&json!({ "thought": true, "thoughtSignature": "opaque" })));
}

#[test]
fn does_not_treat_signature_alone_as_thinking() {
    assert!(!is_thinking_part(&json!({ "thoughtSignature": "opaque" })));
    assert!(!is_thinking_part(&json!({ "thought": false, "thoughtSignature": "opaque" })));
}

#[test]
fn does_not_treat_empty_or_missing_signatures_as_thinking() {
    assert!(!is_thinking_part(&json!({})));
    assert!(!is_thinking_part(&json!({ "thought": false, "thoughtSignature": "" })));
}

#[test]
fn preserves_existing_signature_when_deltas_omit_it() {
    let first = retain_thought_signature(None, Some("sig-1"));
    assert_eq!(first.as_deref(), Some("sig-1"));

    let second = retain_thought_signature(first, None);
    assert_eq!(second.as_deref(), Some("sig-1"));

    let third = retain_thought_signature(second, Some(""));
    assert_eq!(third.as_deref(), Some("sig-1"));
}

#[test]
fn updates_signature_when_new_non_empty_arrives() {
    let updated = retain_thought_signature(Some("sig-1".to_string()), Some("sig-2"));
    assert_eq!(updated.as_deref(), Some("sig-2"));
}

// ---------------------------------------------------------------------------
// google-shared-image-tool-result-routing.test.ts
// ---------------------------------------------------------------------------

fn image_routing_context(model: &GoogleModel) -> Context {
    Context {
        system_prompt: None,
        messages: vec![
            user_text("read the files"),
            assistant(
                vec![
                    tool_call("call_a", "read", json!({ "path": "a.txt" }), None),
                    tool_call("call_img", "read", json!({ "path": "image.png" }), None),
                    tool_call("call_b", "read", json!({ "path": "b.txt" }), None),
                ],
                &model.api,
                &model.provider,
                &model.id,
            ),
            tool_result("call_a", "read", vec![text_block("alpha text")], false),
            tool_result("call_img", "read", vec![image_block("abc", "image/png")], false),
            tool_result("call_b", "read", vec![text_block("beta text")], false),
        ],
        tools: None,
    }
}

#[test]
fn keeps_separate_synthetic_image_turn_for_gemini_2x() {
    let model = make_model(
        "google-generative-ai",
        "google",
        "gemini-2.5-flash",
        vec![Modality::Text, Modality::Image],
    );
    let contents = convert_messages(&model, &image_routing_context(&model), 0);

    assert_eq!(contents.len(), 5);
    let parts2 = contents[2]["parts"].as_array().unwrap();
    assert!(parts2.iter().all(|p| p.get("functionResponse").is_some()));
    assert_eq!(contents[3]["parts"][0]["text"], json!("Tool result image:"));
    assert!(contents[3]["parts"][1].get("inlineData").is_some());
    assert!(contents[4]["parts"][0].get("functionResponse").is_some());
}

#[test]
fn nests_image_tool_results_for_gemini_3() {
    let model = make_model(
        "google-generative-ai",
        "google",
        "gemini-3-pro-preview",
        vec![Modality::Text, Modality::Image],
    );
    let contents = convert_messages(&model, &image_routing_context(&model), 0);

    assert_eq!(contents.len(), 3);
    let tool_result_turn = &contents[2];
    assert_eq!(tool_result_turn["parts"].as_array().unwrap().len(), 3);
    let image_response = &tool_result_turn["parts"][1]["functionResponse"];
    assert!(!image_response.is_null());
    assert_eq!(image_response["parts"].as_array().unwrap().len(), 1);
    assert!(image_response["parts"][0].get("inlineData").is_some());
}

// ---------------------------------------------------------------------------
// google-shared-gemini3-unsigned-tool-call.test.ts
// ---------------------------------------------------------------------------

fn gemini3_context(api: &str, provider: &str, model_id: &str, sig: Option<&str>) -> Context {
    Context {
        system_prompt: None,
        messages: vec![
            user_text("Hi"),
            assistant(
                vec![
                    tool_call("call_1", "bash", json!({ "command": "echo hi" }), sig),
                    tool_call("call_2", "bash", json!({ "command": "ls -la" }), None),
                ],
                api,
                provider,
                model_id,
            ),
        ],
        tools: None,
    }
}

fn model_turn(contents: &[Value]) -> Value {
    contents
        .iter()
        .find(|c| c["role"] == json!("model"))
        .cloned()
        .expect("model turn present")
}

fn function_call_parts(model_turn: &Value) -> Vec<Value> {
    model_turn["parts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p.get("functionCall").is_some())
        .cloned()
        .collect()
}

#[test]
fn no_skip_validator_for_unsigned_genai_tool_calls() {
    let model = make_model("google-generative-ai", "google", "gemini-3-pro-preview", vec![Modality::Text]);
    // assistant message is from a different model id ("other-model")
    let contents = convert_messages(
        &model,
        &gemini3_context("google-generative-ai", "google", "other-model", None),
        0,
    );

    let turn = model_turn(&contents);
    let fc = function_call_parts(&turn);
    assert_eq!(fc.len(), 2);
    assert!(fc[0].get("thoughtSignature").is_none());
    assert!(fc[1].get("thoughtSignature").is_none());
    assert!(!serde_json::to_string(&turn).unwrap().contains("skip_thought_signature_validator"));

    let historical = turn["parts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| {
            p.get("text")
                .and_then(Value::as_str)
                .map(|t| t.contains("Historical context"))
                .unwrap_or(false)
        })
        .count();
    assert_eq!(historical, 0);
}

#[test]
fn no_skip_validator_for_unsigned_vertex_tool_calls() {
    let model = make_model("google-vertex", "google-vertex", "gemini-3-pro-preview", vec![Modality::Text]);
    let contents = convert_messages(
        &model,
        &gemini3_context("google-vertex", "google-vertex", "gemini-3-pro-preview", None),
        0,
    );

    let turn = model_turn(&contents);
    let fc = function_call_parts(&turn);
    assert_eq!(fc.len(), 2);
    assert!(fc[0].get("thoughtSignature").is_none());
    assert!(fc[1].get("thoughtSignature").is_none());
    assert!(!serde_json::to_string(&turn).unwrap().contains("skip_thought_signature_validator"));
}

#[test]
fn preserves_valid_signature_for_same_provider_and_model() {
    let model = make_model("google-generative-ai", "google", "gemini-3-pro-preview", vec![Modality::Text]);
    let valid_sig = "AAAAAAAAAAAAAAAAAAAAAA==";
    let contents = convert_messages(
        &model,
        &gemini3_context("google-generative-ai", "google", "gemini-3-pro-preview", Some(valid_sig)),
        0,
    );

    let turn = model_turn(&contents);
    let fc = function_call_parts(&turn);
    assert_eq!(fc.len(), 2);
    assert_eq!(fc[0]["thoughtSignature"], json!(valid_sig));
    assert!(fc[1].get("thoughtSignature").is_none());
}

#[test]
fn no_signature_for_non_gemini_3_models() {
    let model = make_model("google-generative-ai", "google", "gemini-2.5-flash", vec![Modality::Text]);
    let contents = convert_messages(
        &model,
        &gemini3_context("google-generative-ai", "google", "other-model", None),
        0,
    );

    let turn = model_turn(&contents);
    let fc = function_call_parts(&turn);
    assert!(fc[0].get("thoughtSignature").is_none());
}

// ---------------------------------------------------------------------------
// local decode-loop sanity checks (parse_google_stream)
// ---------------------------------------------------------------------------

fn event_kinds(outcome: &StreamOutcome) -> Vec<&'static str> {
    outcome
        .events
        .iter()
        .map(|e| match e {
            AssistantMessageEvent::Start { .. } => "start",
            AssistantMessageEvent::TextStart { .. } => "text_start",
            AssistantMessageEvent::TextDelta { .. } => "text_delta",
            AssistantMessageEvent::TextEnd { .. } => "text_end",
            AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
            AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
            AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
            AssistantMessageEvent::ToolcallStart { .. } => "toolcall_start",
            AssistantMessageEvent::ToolcallDelta { .. } => "toolcall_delta",
            AssistantMessageEvent::ToolcallEnd { .. } => "toolcall_end",
            AssistantMessageEvent::Done { .. } => "done",
            AssistantMessageEvent::Error { .. } => "error",
        })
        .collect()
}

fn decode_model() -> GoogleModel {
    make_model("google-generative-ai", "google", "gemini-2.5-flash", vec![Modality::Text])
}

#[test]
fn decodes_text_stream_with_usage() {
    let chunks = vec![json!({
        "responseId": "resp-1",
        "candidates": [{
            "content": { "parts": [{ "text": "ok" }] },
            "finishReason": "STOP",
        }],
        "usageMetadata": {
            "promptTokenCount": 3,
            "cachedContentTokenCount": 1,
            "candidatesTokenCount": 2,
            "thoughtsTokenCount": 0,
            "totalTokenCount": 5,
        },
    })];

    let outcome = parse_google_stream(&chunks, &decode_model(), API, 0);
    assert_eq!(
        event_kinds(&outcome),
        ["start", "text_start", "text_delta", "text_end", "done"]
    );
    let msg = &outcome.message;
    assert_eq!(msg.stop_reason, StopReason::Stop);
    assert_eq!(msg.response_id.as_deref(), Some("resp-1"));
    assert_eq!(msg.usage.input, 2); // 3 - 1
    assert_eq!(msg.usage.output, 2);
    assert_eq!(msg.usage.cache_read, 1);
    assert_eq!(msg.usage.total_tokens, 5);
    assert_eq!(msg.content, vec![text_block("ok")]);
}

const API: &str = "google-generative-ai";

#[test]
fn decodes_tool_call_with_synthesized_id() {
    let chunks = vec![json!({
        "candidates": [{
            "content": {
                "parts": [{
                    "functionCall": { "name": "read", "args": { "path": "a.txt" } },
                }],
            },
            "finishReason": "STOP",
        }],
    })];

    let outcome = parse_google_stream(&chunks, &decode_model(), API, 0);
    assert_eq!(
        event_kinds(&outcome),
        ["start", "toolcall_start", "toolcall_delta", "toolcall_end", "done"]
    );
    let msg = &outcome.message;
    // Any tool call forces stopReason toolUse.
    assert_eq!(msg.stop_reason, StopReason::ToolUse);
    match &msg.content[0] {
        ContentBlock::ToolCall { id, name, arguments, .. } => {
            assert_eq!(name, "read");
            assert_eq!(arguments, &json!({ "path": "a.txt" }));
            // synthesized: `${name}_${now}_${counter}`
            assert_eq!(id, "read_0_1");
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}

#[test]
fn uses_provided_tool_call_id_when_present() {
    let chunks = vec![json!({
        "candidates": [{
            "content": {
                "parts": [{
                    "functionCall": { "id": "call_xyz", "name": "read", "args": {} },
                }],
            },
            "finishReason": "STOP",
        }],
    })];

    let outcome = parse_google_stream(&chunks, &decode_model(), API, 0);
    match &outcome.message.content[0] {
        ContentBlock::ToolCall { id, .. } => assert_eq!(id, "call_xyz"),
        other => panic!("expected tool call, got {other:?}"),
    }
}
