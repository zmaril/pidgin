// straitjacket-allow-file:duplication — these tests transcribe pi's Bedrock
// fixtures and payload-capture assertions. The model/context builders and the
// per-case event objects are near-identical by design; the clone detector reads
// them as duplicates, but they are distinct, load-bearing wire fixtures kept
// faithful to pi's `bedrock-custom-headers.test.ts`,
// `bedrock-thinking-payload.test.ts`, `bedrock-convert-messages.test.ts`,
// `bedrock-models.test.ts`, and `bedrock-endpoint-resolution.test.ts`.
//! Unit tests for the Bedrock ConverseStream driver, mirroring pi's
//! `packages/ai/test/bedrock-*.test.ts` and the `ConverseStream` decode contract.

use super::*;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Message, StopReason, ToolResultMessage,
    ToolResultRole, Usage, UsageCost, UserContent, UserMessage, UserRole,
};
use serde_json::json;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Bridge a catalog `Model` to a [`BedrockModel`], mirroring pi's `getModel`.
fn base_model(id: &str) -> BedrockModel {
    let model = crate::providers::builtin_models()
        .get_model("amazon-bedrock", id)
        .unwrap_or_else(|| panic!("catalog has amazon-bedrock/{id}"));
    let value = serde_json::to_value(&model).expect("serialize catalog model");
    serde_json::from_value(value).expect("deserialize BedrockModel")
}

fn empty_env() -> ProviderEnv {
    ProviderEnv::new()
}

fn provider_headers(pairs: &[(&str, &str)]) -> ProviderHeaders {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), Some((*v).to_string())))
        .collect()
}

// ---------------------------------------------------------------------------
// bedrock-custom-headers.test.ts
// ---------------------------------------------------------------------------

#[test]
fn vc1_registers_build_step_middleware_and_injects_caller_header() {
    // The registration is gated on a non-empty header record.
    let options = BedrockOptions {
        cache_retention: Some(CacheRetention::None),
        headers: Some(provider_headers(&[("x-custom", "v")])),
        ..Default::default()
    };
    let record = custom_headers_record(&options).expect("registration happens");

    // opts.step / priority / name.
    assert_eq!(CUSTOM_HEADERS_MIDDLEWARE_STEP, "build");
    assert_eq!(CUSTOM_HEADERS_MIDDLEWARE_PRIORITY, "low");
    assert_eq!(CUSTOM_HEADERS_MIDDLEWARE_NAME, "pi-ai-custom-headers");

    let mut request_headers: BTreeMap<String, String> = BTreeMap::new();
    apply_custom_headers(&mut request_headers, &record);
    assert_eq!(
        request_headers.get("x-custom").map(String::as_str),
        Some("v")
    );
}

#[test]
fn vc2_skips_reserved_headers_case_insensitively() {
    let options = BedrockOptions {
        cache_retention: Some(CacheRetention::None),
        headers: Some(provider_headers(&[
            ("authorization", "evil"),
            ("x-amz-date", "evil"),
            ("x-allowed", "ok"),
            ("Authorization", "evil2"),
            ("X-Amz-Date", "evil2"),
            ("HOST", "evil3"),
        ])),
        ..Default::default()
    };
    let record = custom_headers_record(&options).expect("registration happens");

    let mut request_headers: BTreeMap<String, String> = BTreeMap::new();
    request_headers.insert("authorization".to_string(), "real-auth".to_string());
    request_headers.insert("x-amz-date".to_string(), "real-date".to_string());
    request_headers.insert("host".to_string(), "real-host".to_string());
    apply_custom_headers(&mut request_headers, &record);

    assert_eq!(
        request_headers.get("authorization").map(String::as_str),
        Some("real-auth")
    );
    assert_eq!(
        request_headers.get("x-amz-date").map(String::as_str),
        Some("real-date")
    );
    assert_eq!(
        request_headers.get("host").map(String::as_str),
        Some("real-host")
    );
    assert_eq!(
        request_headers.get("x-allowed").map(String::as_str),
        Some("ok")
    );
    // Mixed-case reserved keys must be skipped (never re-added as distinct keys).
    assert!(!request_headers.contains_key("Authorization"));
    assert!(!request_headers.contains_key("X-Amz-Date"));
    assert!(!request_headers.contains_key("HOST"));
    let mut keys: Vec<&str> = request_headers.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["authorization", "host", "x-allowed", "x-amz-date"]);
}

#[test]
fn vc3_no_registration_when_headers_undefined_or_empty() {
    let undefined = BedrockOptions {
        cache_retention: Some(CacheRetention::None),
        ..Default::default()
    };
    assert!(custom_headers_record(&undefined).is_none());

    let empty = BedrockOptions {
        cache_retention: Some(CacheRetention::None),
        headers: Some(ProviderHeaders::new()),
        ..Default::default()
    };
    assert!(custom_headers_record(&empty).is_none());
}

#[test]
fn vc3_structural_guard_no_op_when_no_custom_headers_apply() {
    // The middleware body is a pure header-map mutation; applying an empty record
    // (the "request has no headers to inject" analog) leaves the map untouched.
    let mut request_headers: BTreeMap<String, String> = BTreeMap::new();
    request_headers.insert("existing".to_string(), "value".to_string());
    apply_custom_headers(&mut request_headers, &BTreeMap::new());
    assert_eq!(request_headers.len(), 1);
    assert_eq!(
        request_headers.get("existing").map(String::as_str),
        Some("value")
    );
}

#[test]
fn vc4_stream_simple_forwards_headers_end_to_end() {
    // streamSimple only threads `headers` through to BedrockOptions, so the
    // registration decision is identical to the direct-stream path.
    let options = BedrockOptions {
        headers: Some(provider_headers(&[("x-custom", "v")])),
        ..Default::default()
    };
    let record = custom_headers_record(&options).expect("registration happens");
    let mut request_headers: BTreeMap<String, String> = BTreeMap::new();
    apply_custom_headers(&mut request_headers, &record);
    assert_eq!(
        request_headers.get("x-custom").map(String::as_str),
        Some("v")
    );
}

// ---------------------------------------------------------------------------
// bedrock-thinking-payload.test.ts
// ---------------------------------------------------------------------------

/// Override id/name on a catalog base model (pi's `{...baseModel, id, name}`).
fn with_identity(mut model: BedrockModel, id: &str, name: &str) -> BedrockModel {
    model.id = id.to_string();
    model.name = Some(name.to_string());
    model
}

/// Mirror pi's `capturePayload`: build the additional-model-request-fields with
/// `reasoning` defaulting to "high".
fn additional_fields(model: &BedrockModel, options: BedrockOptions) -> Value {
    let options = BedrockOptions {
        reasoning: options.reasoning.or(Some(ThinkingLevel::High)),
        ..options
    };
    build_additional_model_request_fields(model, &options, &empty_env())
        .expect("payload captured before request abort")
}

#[test]
fn adaptive_thinking_for_claude_opus_4_8() {
    let model = with_identity(
        base_model("global.anthropic.claude-opus-4-6-v1"),
        "global.anthropic.claude-opus-4-8-v1",
        "Claude Opus 4.8 (Global)",
    );
    let fields = additional_fields(&model, BedrockOptions::default());
    assert_eq!(
        fields["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(fields["output_config"], json!({ "effort": "high" }));
    assert!(fields.get("anthropic_beta").is_none());
}

#[test]
fn xhigh_reasoning_maps_to_effort_xhigh_for_opus_4_8() {
    let model = with_identity(
        base_model("global.anthropic.claude-opus-4-6-v1"),
        "global.anthropic.claude-opus-4-8-v1",
        "Claude Opus 4.8 (Global)",
    );
    let fields = additional_fields(
        &model,
        BedrockOptions {
            reasoning: Some(ThinkingLevel::Xhigh),
            ..Default::default()
        },
    );
    assert_eq!(
        fields["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(fields["output_config"], json!({ "effort": "xhigh" }));
    assert!(fields.get("anthropic_beta").is_none());
}

#[test]
fn adaptive_thinking_for_claude_fable_5() {
    let model = base_model("global.anthropic.claude-fable-5");
    let fields = additional_fields(&model, BedrockOptions::default());
    assert_eq!(
        fields["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(fields["output_config"], json!({ "effort": "high" }));
    assert!(fields.get("anthropic_beta").is_none());
}

#[test]
fn adaptive_thinking_for_claude_sonnet_5() {
    let model = base_model("global.anthropic.claude-sonnet-5");
    let fields = additional_fields(&model, BedrockOptions::default());
    assert_eq!(
        fields["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(fields["output_config"], json!({ "effort": "high" }));
    assert!(fields.get("anthropic_beta").is_none());
}

#[test]
fn xhigh_reasoning_maps_to_effort_xhigh_for_fable_5() {
    let model = base_model("global.anthropic.claude-fable-5");
    let fields = additional_fields(
        &model,
        BedrockOptions {
            reasoning: Some(ThinkingLevel::Xhigh),
            ..Default::default()
        },
    );
    assert_eq!(
        fields["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(fields["output_config"], json!({ "effort": "xhigh" }));
}

#[test]
fn omits_display_for_govcloud_ids_on_non_adaptive_claude_thinking() {
    let model = with_identity(
        base_model("us.anthropic.claude-sonnet-4-5-20250929-v1:0"),
        "us-gov.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "Claude Sonnet 4.5 (GovCloud)",
    );
    let fields = additional_fields(&model, BedrockOptions::default());
    assert_eq!(
        fields["thinking"],
        json!({ "type": "enabled", "budget_tokens": 16384 })
    );
    assert_eq!(
        fields["anthropic_beta"],
        json!(["interleaved-thinking-2025-05-14"])
    );
}

#[test]
fn omits_display_for_govcloud_regions_on_adaptive_claude_thinking() {
    let model = with_identity(
        base_model("global.anthropic.claude-opus-4-6-v1"),
        "global.anthropic.claude-opus-4-8-v1",
        "Claude Opus 4.8 (Global)",
    );
    let fields = additional_fields(
        &model,
        BedrockOptions {
            region: Some("us-gov-west-1".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(fields["thinking"], json!({ "type": "adaptive" }));
    assert_eq!(fields["output_config"], json!({ "effort": "high" }));
    assert!(fields.get("anthropic_beta").is_none());
}

#[test]
fn adaptive_thinking_when_model_name_identifies_family_but_arn_does_not() {
    let model = with_identity(
        base_model("global.anthropic.claude-opus-4-6-v1"),
        "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/my-profile",
        "Claude Opus 4.6",
    );
    let fields = additional_fields(&model, BedrockOptions::default());
    assert_eq!(
        fields["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(fields["output_config"], json!({ "effort": "high" }));
}

#[test]
fn injects_cache_points_when_model_name_identifies_supported_claude() {
    let model = with_identity(
        base_model("global.anthropic.claude-opus-4-6-v1"),
        "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/my-profile",
        "Claude Sonnet 4.6",
    );
    let context = Context {
        system_prompt: Some("You are helpful.".to_string()),
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("Hello".to_string()),
            timestamp: 0,
        })],
        tools: None,
    };
    // No reasoning option here, matching pi's second application-profile case.
    let payload = build_command_input(&model, &context, &BedrockOptions::default(), &empty_env());

    // System prompt should have a cache point (text + cachePoint).
    let system = payload["system"].as_array().expect("system present");
    assert_eq!(system.len(), 2);
    assert!(system[1].get("cachePoint").is_some());

    // Last user message should have a cache point.
    let messages = payload["messages"].as_array().expect("messages present");
    let last = messages.last().expect("a message");
    let last_content = last["content"].as_array().expect("content array");
    assert!(last_content
        .last()
        .expect("a block")
        .get("cachePoint")
        .is_some());
}

#[test]
fn falls_back_to_fixed_budget_thinking_for_non_adaptive_claude_via_name() {
    let model = with_identity(
        base_model("us.anthropic.claude-sonnet-4-5-20250929-v1:0"),
        "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/my-profile",
        "Claude Sonnet 4.5",
    );
    let fields = additional_fields(&model, BedrockOptions::default());
    assert_eq!(fields["thinking"]["type"], json!("enabled"));
    assert!(fields["thinking"]["budget_tokens"].is_number());
    assert_eq!(
        fields["anthropic_beta"],
        json!(["interleaved-thinking-2025-05-14"])
    );
}

// ---------------------------------------------------------------------------
// bedrock-convert-messages.test.ts
// ---------------------------------------------------------------------------

fn convert_model() -> BedrockModel {
    base_model("us.anthropic.claude-sonnet-4-5-20250929-v1:0")
}

/// Mirror pi's `capturePayload`: build the command input with caching disabled.
fn capture_messages_payload(context: &Context) -> Value {
    let options = BedrockOptions {
        cache_retention: Some(CacheRetention::None),
        ..Default::default()
    };
    build_command_input(&convert_model(), context, &options, &empty_env())
}

fn assistant_fixture(content: Vec<ContentBlock>) -> Message {
    Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: "bedrock-converse-stream".to_string(),
        provider: "amazon-bedrock".to_string(),
        model: convert_model().id,
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: UsageCost::default(),
        },
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: 0,
    })
}

#[test]
fn skips_unknown_user_content_blocks() {
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![
                ContentBlock::Text {
                    text: "hello".to_string(),
                    text_signature: None,
                },
                ContentBlock::Unknown,
            ]),
            timestamp: 0,
        })],
        tools: None,
    };
    let payload = capture_messages_payload(&context);
    let messages = payload["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["content"].as_array().unwrap().len(), 1);
    assert_eq!(messages[0]["content"][0], json!({ "text": "hello" }));
}

#[test]
fn skips_unknown_assistant_content_blocks() {
    let context = Context {
        system_prompt: None,
        messages: vec![assistant_fixture(vec![
            ContentBlock::Text {
                text: "hello".to_string(),
                text_signature: None,
            },
            ContentBlock::Unknown,
        ])],
        tools: None,
    };
    let payload = capture_messages_payload(&context);
    let messages = payload["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["content"].as_array().unwrap().len(), 1);
    assert_eq!(messages[0]["content"][0], json!({ "text": "hello" }));
}

#[test]
fn replaces_user_messages_with_only_unknown_content_with_placeholder() {
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![ContentBlock::Unknown]),
            timestamp: 0,
        })],
        tools: None,
    };
    let payload = capture_messages_payload(&context);
    let messages = payload["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["content"], json!([{ "text": "<empty>" }]));
}

#[test]
fn replaces_blank_user_string_content_with_placeholder() {
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("   ".to_string()),
            timestamp: 0,
        })],
        tools: None,
    };
    let payload = capture_messages_payload(&context);
    let messages = payload["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["content"], json!([{ "text": "<empty>" }]));
}

#[test]
fn filters_blank_user_text_blocks_when_other_content_remains() {
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![
                ContentBlock::Text {
                    text: String::new(),
                    text_signature: None,
                },
                ContentBlock::Text {
                    text: "hello".to_string(),
                    text_signature: None,
                },
            ]),
            timestamp: 0,
        })],
        tools: None,
    };
    let payload = capture_messages_payload(&context);
    let messages = payload["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["content"], json!([{ "text": "hello" }]));
}

#[test]
fn replaces_user_content_emptied_by_sanitization_with_placeholder() {
    // pi feeds a lone UTF-16 surrogate that `sanitizeSurrogates` strips to "".
    // A Rust `&str` cannot hold a lone surrogate (see utils/sanitize_unicode.rs),
    // so the reachable equivalent is content that sanitizes to empty — asserted
    // here via a whitespace-only string, which yields the same placeholder.
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(" ".to_string()),
            timestamp: 0,
        })],
        tools: None,
    };
    let payload = capture_messages_payload(&context);
    let messages = payload["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["content"], json!([{ "text": "<empty>" }]));
}

#[test]
fn skips_assistant_text_blocks_emptied_by_sanitization() {
    // Reachable analog of pi's lone-surrogate assistant text (see the note above):
    // a blank text block is filtered, leaving no content, so the message is
    // dropped entirely.
    let context = Context {
        system_prompt: None,
        messages: vec![assistant_fixture(vec![ContentBlock::Text {
            text: " ".to_string(),
            text_signature: None,
        }])],
        tools: None,
    };
    let payload = capture_messages_payload(&context);
    let messages = payload["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 0);
}

#[test]
fn replaces_blank_tool_result_content_with_placeholder() {
    let context = Context {
        system_prompt: None,
        messages: vec![Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: "tool-1".to_string(),
            tool_name: "tool".to_string(),
            content: vec![ContentBlock::Text {
                text: String::new(),
                text_signature: None,
            }],
            details: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 0,
        })],
        tools: None,
    };
    let payload = capture_messages_payload(&context);
    let messages = payload["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0]["content"][0]["toolResult"]["content"],
        json!([{ "text": "<empty>" }])
    );
}

#[test]
fn skips_assistant_messages_with_only_unknown_content() {
    let context = Context {
        system_prompt: None,
        messages: vec![assistant_fixture(vec![ContentBlock::Unknown])],
        tools: None,
    };
    let payload = capture_messages_payload(&context);
    let messages = payload["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 0);
}

// ---------------------------------------------------------------------------
// bedrock-models.test.ts
// ---------------------------------------------------------------------------

#[test]
fn gets_all_available_bedrock_models() {
    let models = crate::providers::builtin_models().get_models(Some("amazon-bedrock"));
    assert!(!models.is_empty());
}

// ---------------------------------------------------------------------------
// bedrock-endpoint-resolution.test.ts
// ---------------------------------------------------------------------------

fn env_of(pairs: &[(&str, &str)]) -> ProviderEnv {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn assigns_eu_central_1_runtime_urls_to_built_in_eu_inference_profiles() {
    let model = crate::providers::builtin_models()
        .get_model(
            "amazon-bedrock",
            "eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
        )
        .expect("catalog has the EU model");
    assert_eq!(
        model.base_url,
        "https://bedrock-runtime.eu-central-1.amazonaws.com"
    );
}

#[test]
fn does_not_pin_standard_aws_endpoints_when_region_is_configured() {
    let model = base_model("us.anthropic.claude-opus-4-8");
    let process_env = env_of(&[("AWS_REGION", "us-east-2")]);
    let config = build_client_config(&model, &BedrockOptions::default(), &process_env);
    assert_eq!(config["region"], json!("us-east-2"));
    assert!(config.get("endpoint").is_none());
}

#[test]
fn derives_region_from_built_in_eu_endpoint_when_no_region_or_profile() {
    let model = base_model("eu.anthropic.claude-sonnet-4-5-20250929-v1:0");
    let config = build_client_config(&model, &BedrockOptions::default(), &empty_env());
    assert_eq!(
        config["endpoint"],
        json!("https://bedrock-runtime.eu-central-1.amazonaws.com")
    );
    assert_eq!(config["region"], json!("eu-central-1"));
}

#[test]
fn handles_missing_regions_for_explicit_scoped_and_ambient_profiles() {
    let model = base_model("eu.anthropic.claude-sonnet-4-5-20250929-v1:0");

    // Explicit profile option.
    let config = build_client_config(
        &model,
        &BedrockOptions {
            profile: Some("bedrock-profile".to_string()),
            ..Default::default()
        },
        &empty_env(),
    );
    assert_eq!(config["profile"], json!("bedrock-profile"));
    assert_eq!(
        config["endpoint"],
        json!("https://bedrock-runtime.eu-central-1.amazonaws.com")
    );
    assert_eq!(config["region"], json!("eu-central-1"));

    // Scoped env AWS_PROFILE (does not count as an ambient profile).
    let config = build_client_config(
        &model,
        &BedrockOptions {
            env: Some(env_of(&[("AWS_PROFILE", "scoped-bedrock-profile")])),
            ..Default::default()
        },
        &empty_env(),
    );
    assert_eq!(config["profile"], json!("scoped-bedrock-profile"));
    assert_eq!(
        config["endpoint"],
        json!("https://bedrock-runtime.eu-central-1.amazonaws.com")
    );
    assert_eq!(config["region"], json!("eu-central-1"));

    // Ambient AWS_PROFILE suppresses endpoint pinning and default region.
    let process_env = env_of(&[("AWS_PROFILE", "ambient-bedrock-profile")]);
    let config = build_client_config(&model, &BedrockOptions::default(), &process_env);
    assert_eq!(config["profile"], json!("ambient-bedrock-profile"));
    assert!(config.get("endpoint").is_none());
    assert!(config.get("region").is_none());
}

#[test]
fn still_passes_custom_bedrock_endpoints_through_to_the_client() {
    let mut model = base_model("us.anthropic.claude-opus-4-8");
    model.base_url = Some("https://bedrock-vpc.example.com".to_string());
    let process_env = env_of(&[("AWS_REGION", "us-west-2")]);
    let config = build_client_config(&model, &BedrockOptions::default(), &process_env);
    assert_eq!(config["endpoint"], json!("https://bedrock-vpc.example.com"));
    assert_eq!(config["region"], json!("us-west-2"));
}

#[test]
fn extracts_region_from_inference_profile_arn_regardless_of_region() {
    let mut model = base_model("us.anthropic.claude-opus-4-8");
    model.id =
        "arn:aws:bedrock:us-west-2:123456789012:application-inference-profile/abc123".to_string();
    let process_env = env_of(&[("AWS_REGION", "us-east-1")]);
    let config = build_client_config(&model, &BedrockOptions::default(), &process_env);
    assert_eq!(config["region"], json!("us-west-2"));
}

#[test]
fn extracts_region_from_govcloud_inference_profile_arn() {
    let mut model = base_model("us.anthropic.claude-opus-4-8");
    model.id =
        "arn:aws-us-gov:bedrock:us-gov-west-1:123456789012:application-inference-profile/abc123"
            .to_string();
    let process_env = env_of(&[("AWS_REGION", "us-east-1")]);
    let config = build_client_config(&model, &BedrockOptions::default(), &process_env);
    assert_eq!(config["region"], json!("us-gov-west-1"));
}

#[test]
fn preserves_ambient_aws_auth_for_custom_model_ids() {
    let mut model = base_model("us.anthropic.claude-opus-4-8");
    model.id =
        "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/example".to_string();
    let process_env = env_of(&[("AWS_PROFILE", "bedrock-profile")]);
    let config = build_client_config(&model, &BedrockOptions::default(), &process_env);
    assert_eq!(config["profile"], json!("bedrock-profile"));
    assert!(config.get("token").is_none());
    assert!(config.get("authSchemePreference").is_none());
}

#[test]
fn uses_generic_api_key_option_as_bedrock_bearer_token() {
    let model = base_model("us.anthropic.claude-opus-4-8");
    let config = build_client_config(
        &model,
        &BedrockOptions {
            api_key: Some("bedrock-api-key".to_string()),
            ..Default::default()
        },
        &empty_env(),
    );
    assert_eq!(config["token"], json!({ "token": "bedrock-api-key" }));
    assert_eq!(config["authSchemePreference"], json!(["httpBearerAuth"]));
}

// ---------------------------------------------------------------------------
// tool config (convertToolConfig)
// ---------------------------------------------------------------------------

#[test]
fn tool_config_builds_tool_specs_and_choice() {
    let tools = vec![json!({
        "name": "search",
        "description": "Search the web",
        "parameters": { "type": "object", "properties": {} }
    })];
    let config = convert_tool_config(Some(&tools), Some(&BedrockToolChoice::Auto)).expect("config");
    assert_eq!(config["tools"][0]["toolSpec"]["name"], json!("search"));
    assert_eq!(
        config["tools"][0]["toolSpec"]["description"],
        json!("Search the web")
    );
    assert_eq!(
        config["tools"][0]["toolSpec"]["inputSchema"]["json"],
        json!({ "type": "object", "properties": {} })
    );
    assert_eq!(config["toolChoice"], json!({ "auto": {} }));
}

#[test]
fn tool_config_is_none_for_tool_choice_none() {
    let tools = vec![json!({ "name": "search", "parameters": {} })];
    assert!(convert_tool_config(Some(&tools), Some(&BedrockToolChoice::None)).is_none());
}

#[test]
fn tool_config_specific_tool_choice() {
    let tools = vec![json!({ "name": "search", "parameters": {} })];
    let config = convert_tool_config(
        Some(&tools),
        Some(&BedrockToolChoice::Tool {
            name: "search".to_string(),
        }),
    )
    .expect("config");
    assert_eq!(
        config["toolChoice"],
        json!({ "tool": { "name": "search" } })
    );
}

// ---------------------------------------------------------------------------
// ConverseStream decode — streaming into events + final message
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

fn decode_model() -> BedrockModel {
    convert_model()
}

#[test]
fn decodes_text_stream_with_usage_and_cost() {
    let items = vec![
        json!({ "messageStart": { "role": "assistant" } }),
        json!({ "contentBlockDelta": { "contentBlockIndex": 0, "delta": { "text": "Hello" } } }),
        json!({ "contentBlockDelta": { "contentBlockIndex": 0, "delta": { "text": " world" } } }),
        json!({ "contentBlockStop": { "contentBlockIndex": 0 } }),
        json!({ "messageStop": { "stopReason": "end_turn" } }),
        json!({ "metadata": { "usage": { "inputTokens": 10, "outputTokens": 5, "totalTokens": 15 } } }),
    ];
    let outcome = parse_converse_stream(&items, &decode_model(), 0);
    assert_eq!(
        event_kinds(&outcome),
        [
            "start",
            "text_start",
            "text_delta",
            "text_delta",
            "text_end",
            "done"
        ]
    );
    assert_eq!(outcome.message.stop_reason, StopReason::Stop);
    assert_eq!(
        outcome.message.content,
        vec![ContentBlock::Text {
            text: "Hello world".to_string(),
            text_signature: None,
        }]
    );
    assert_eq!(outcome.message.usage.input, 10);
    assert_eq!(outcome.message.usage.output, 5);
    assert_eq!(outcome.message.usage.total_tokens, 15);
    assert!(outcome.message.usage.cost.total > 0.0);
}

#[test]
fn decodes_thinking_then_text() {
    let items = vec![
        json!({ "messageStart": { "role": "assistant" } }),
        json!({ "contentBlockDelta": { "contentBlockIndex": 0, "delta": { "reasoningContent": { "text": "pondering" } } } }),
        json!({ "contentBlockDelta": { "contentBlockIndex": 0, "delta": { "reasoningContent": { "signature": "sig" } } } }),
        json!({ "contentBlockStop": { "contentBlockIndex": 0 } }),
        json!({ "contentBlockDelta": { "contentBlockIndex": 1, "delta": { "text": "answer" } } }),
        json!({ "contentBlockStop": { "contentBlockIndex": 1 } }),
        json!({ "messageStop": { "stopReason": "end_turn" } }),
    ];
    let outcome = parse_converse_stream(&items, &decode_model(), 0);
    assert_eq!(
        event_kinds(&outcome),
        [
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_end",
            "text_start",
            "text_delta",
            "text_end",
            "done"
        ]
    );
    match &outcome.message.content[0] {
        ContentBlock::Thinking {
            thinking,
            thinking_signature,
            ..
        } => {
            assert_eq!(thinking, "pondering");
            assert_eq!(thinking_signature.as_deref(), Some("sig"));
        }
        other => panic!("expected thinking block, got {other:?}"),
    }
    match &outcome.message.content[1] {
        ContentBlock::Text { text, .. } => assert_eq!(text, "answer"),
        other => panic!("expected text block, got {other:?}"),
    }
}

#[test]
fn decodes_tool_call_stream() {
    let items = vec![
        json!({ "messageStart": { "role": "assistant" } }),
        json!({ "contentBlockStart": { "contentBlockIndex": 0, "start": { "toolUse": { "toolUseId": "call_ABC", "name": "echo" } } } }),
        json!({ "contentBlockDelta": { "contentBlockIndex": 0, "delta": { "toolUse": { "input": "{\"text\":" } } } }),
        json!({ "contentBlockDelta": { "contentBlockIndex": 0, "delta": { "toolUse": { "input": "\"hi\"}" } } } }),
        json!({ "contentBlockStop": { "contentBlockIndex": 0 } }),
        json!({ "messageStop": { "stopReason": "tool_use" } }),
    ];
    let outcome = parse_converse_stream(&items, &decode_model(), 0);
    assert_eq!(
        event_kinds(&outcome),
        [
            "start",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_end",
            "done"
        ]
    );
    assert_eq!(outcome.message.stop_reason, StopReason::ToolUse);
    match &outcome.message.content[0] {
        ContentBlock::ToolCall {
            id,
            name,
            arguments,
            ..
        } => {
            assert_eq!(id, "call_ABC");
            assert_eq!(name, "echo");
            assert_eq!(arguments, &json!({ "text": "hi" }));
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}

#[test]
fn reads_cached_prompt_tokens_from_metadata() {
    let items = vec![
        json!({ "messageStart": { "role": "assistant" } }),
        json!({ "contentBlockDelta": { "contentBlockIndex": 0, "delta": { "text": "hi" } } }),
        json!({ "contentBlockStop": { "contentBlockIndex": 0 } }),
        json!({ "messageStop": { "stopReason": "end_turn" } }),
        json!({ "metadata": { "usage": { "inputTokens": 100, "outputTokens": 4, "cacheReadInputTokens": 40, "totalTokens": 144 } } }),
    ];
    let outcome = parse_converse_stream(&items, &decode_model(), 0);
    assert_eq!(outcome.message.usage.input, 100);
    assert_eq!(outcome.message.usage.cache_read, 40);
    assert_eq!(outcome.message.usage.output, 4);
    assert_eq!(outcome.message.usage.total_tokens, 144);
}

#[test]
fn maps_max_tokens_stop_reason_to_length() {
    let items = vec![
        json!({ "messageStart": { "role": "assistant" } }),
        json!({ "contentBlockDelta": { "contentBlockIndex": 0, "delta": { "text": "x" } } }),
        json!({ "contentBlockStop": { "contentBlockIndex": 0 } }),
        json!({ "messageStop": { "stopReason": "max_tokens" } }),
    ];
    let outcome = parse_converse_stream(&items, &decode_model(), 0);
    assert_eq!(outcome.message.stop_reason, StopReason::Length);
    assert!(matches!(
        outcome.events.last(),
        Some(AssistantMessageEvent::Done { .. })
    ));
}

#[test]
fn unknown_stop_reason_produces_error_event() {
    let items = vec![
        json!({ "messageStart": { "role": "assistant" } }),
        json!({ "messageStop": { "stopReason": "guardrail_intervened" } }),
    ];
    let outcome = parse_converse_stream(&items, &decode_model(), 0);
    assert_eq!(outcome.message.stop_reason, StopReason::Error);
    assert_eq!(
        outcome.message.error_message.as_deref(),
        Some("guardrail_intervened")
    );
    assert!(matches!(
        outcome.events.last(),
        Some(AssistantMessageEvent::Error { .. })
    ));
}

#[test]
fn exception_item_produces_prefixed_error_event() {
    let items = vec![
        json!({ "messageStart": { "role": "assistant" } }),
        json!({ "validationException": { "message": "bad input" } }),
    ];
    let outcome = parse_converse_stream(&items, &decode_model(), 0);
    assert_eq!(outcome.message.stop_reason, StopReason::Error);
    assert_eq!(
        outcome.message.error_message.as_deref(),
        Some("Validation error: bad input")
    );
    assert!(matches!(
        outcome.events.last(),
        Some(AssistantMessageEvent::Error { .. })
    ));
}

#[test]
fn json_boundary_roundtrips() {
    let items_json = json!([
        { "messageStart": { "role": "assistant" } },
        { "contentBlockDelta": { "contentBlockIndex": 0, "delta": { "text": "hi" } } },
        { "contentBlockStop": { "contentBlockIndex": 0 } },
        { "messageStop": { "stopReason": "end_turn" } }
    ])
    .to_string();
    let model_json = json!({
        "id": "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "api": "bedrock-converse-stream",
        "provider": "amazon-bedrock",
        "cost": { "input": 3.0, "output": 15.0, "cacheRead": 0.3, "cacheWrite": 3.75 }
    })
    .to_string();
    let out = parse_converse_stream_to_json(&items_json, &model_json, 0).expect("valid");
    assert!(out.contains("\"type\":\"done\""));
    assert!(out.contains("\"text\":\"hi\""));
}

// ---------------------------------------------------------------------------
// tool-id normalization (normalizeToolCallId)
// ---------------------------------------------------------------------------

#[test]
fn normalize_tool_call_id_sanitizes_and_truncates() {
    assert_eq!(normalize_tool_call_id("call_ABC-123"), "call_ABC-123");
    assert_eq!(normalize_tool_call_id("weird id!*"), "weird_id__");
    let long: String = "x".repeat(80);
    assert_eq!(normalize_tool_call_id(&long).chars().count(), 64);
}
