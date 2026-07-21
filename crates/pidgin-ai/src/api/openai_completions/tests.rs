// straitjacket-allow-file:duplication — these tests transcribe pi's
// OpenAI-completions fixtures verbatim: the request-shaping `compat` models and
// the streamed `ChatCompletionChunk` arrays are walls of near-identical JSON by
// design, and the clone detector reads them as duplicates. They are distinct,
// load-bearing wire fixtures kept faithful to pi's `test/openai-completions-*`
// cases.
// straitjacket-allow-file:file-size — TODO(straitjacket): this file is 1539 lines, over
// the 1500-line ceiling. Declared explicitly so it suppresses only file-size, not every
// rule (the old bracket form was a silent catch-all). Remove once the file is split into a
// directory module (see PR follow-up).
//! Unit tests for the OpenAI-completions request shaper and chunk walker,
//! mirroring representative cases from pi's `packages/ai/test/openai-completions-*`
//! suites (tool-choice, response-model, empty-tools, reasoning-details,
//! cache-control-format, prompt-cache, thinking-as-text, tool-result-images).

use super::*;
use serde_json::json;

use crate::types::{
    AssistantMessage, AssistantRole, CacheRetention, ContentBlock, Context, MaxTokensField,
    Message, Modality, ModelCost, ModelThinkingLevel, OpenAICompletionsCompat, StopReason,
    ThinkingFormat, ThinkingLevel, ToolResultMessage, ToolResultRole, Usage, UsageCost,
    UserContent, UserMessage, UserRole,
};

// ---------------------------------------------------------------------------
// Fixtures
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

fn base_model() -> OpenAICompletionsModel {
    // Mirrors getModel("openai", "gpt-4o-mini") reshaped to openai-completions:
    // a direct OpenAI endpoint, non-reasoning.
    OpenAICompletionsModel {
        id: "gpt-4o-mini".to_string(),
        api: "openai-completions".to_string(),
        provider: "openai".to_string(),
        base_url: "https://api.openai.com/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![Modality::Text],
        cost: zero_cost(),
        compat: None,
    }
}

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 0,
    })
}

fn empty_usage() -> Usage {
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

fn ping_tool() -> Value {
    json!({
        "name": "ping",
        "description": "Ping tool",
        "parameters": { "type": "object", "properties": { "ok": { "type": "boolean" } } }
    })
}

fn context_with(messages: Vec<Message>, tools: Option<Vec<Value>>) -> Context {
    Context {
        system_prompt: None,
        messages,
        tools,
    }
}

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

fn count_kind(outcome: &StreamOutcome, kind: &str) -> usize {
    event_kinds(outcome).iter().filter(|k| **k == kind).count()
}

/// Content indexes for tool-call events, in emission order.
fn tool_content_indexes(outcome: &StreamOutcome) -> Vec<u32> {
    outcome
        .events
        .iter()
        .filter_map(|e| match e {
            AssistantMessageEvent::ToolcallStart { content_index, .. }
            | AssistantMessageEvent::ToolcallDelta { content_index, .. }
            | AssistantMessageEvent::ToolcallEnd { content_index, .. } => Some(*content_index),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Request shaping: tools / tool_choice / strict
// ---------------------------------------------------------------------------

#[test]
fn forwards_tool_choice_and_includes_tools() {
    let context = context_with(
        vec![user("Call ping with ok=true")],
        Some(vec![ping_tool()]),
    );
    let options = OpenAICompletionsOptions {
        tool_choice: Some(json!("required")),
        ..Default::default()
    };
    let params = build_params(&base_model(), &context, &options);

    assert_eq!(params.get("tool_choice"), Some(&json!("required")));
    let tools = params.get("tools").and_then(Value::as_array).unwrap();
    assert!(!tools.is_empty());
}

#[test]
fn includes_strict_false_by_default() {
    let context = context_with(vec![user("hi")], Some(vec![ping_tool()]));
    let params = build_params(
        &base_model(),
        &context,
        &OpenAICompletionsOptions::default(),
    );
    let function = params.get("tools").and_then(Value::as_array).unwrap()[0]
        .get("function")
        .unwrap();
    assert_eq!(function.get("strict"), Some(&json!(false)));
}

#[test]
fn omits_strict_when_compat_disables_strict_mode() {
    let mut model = base_model();
    model.compat = Some(OpenAICompletionsCompat {
        supports_strict_mode: Some(false),
        ..Default::default()
    });
    let context = context_with(vec![user("hi")], Some(vec![ping_tool()]));
    let params = build_params(&model, &context, &OpenAICompletionsOptions::default());
    let function = params.get("tools").and_then(Value::as_array).unwrap()[0]
        .get("function")
        .and_then(Value::as_object)
        .unwrap();
    assert!(!function.contains_key("strict"), "strict must be omitted");
}

#[test]
fn omits_tools_when_empty_or_undefined() {
    let params_empty = build_params(
        &base_model(),
        &context_with(vec![user("hi")], Some(vec![])),
        &OpenAICompletionsOptions::default(),
    );
    assert!(params_empty.get("tools").is_none());

    let params_none = build_params(
        &base_model(),
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions::default(),
    );
    assert!(params_none.get("tools").is_none());
}

#[test]
fn emits_empty_tools_array_when_conversation_has_tool_history() {
    let assistant = Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![ContentBlock::ToolCall {
            id: "t1".to_string(),
            name: "noop".to_string(),
            arguments: json!({}),
            thought_signature: None,
        }],
        api: "openai-completions".to_string(),
        provider: "openai".to_string(),
        model: "gpt-4o-mini".to_string(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: empty_usage(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp: 0,
    });
    let tool_result = Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "t1".to_string(),
        tool_name: "noop".to_string(),
        content: vec![ContentBlock::Text {
            text: "done".to_string(),
            text_signature: None,
        }],
        details: None,
        added_tool_names: None,
        is_error: false,
        timestamp: 0,
    });
    let context = context_with(
        vec![user("use the tool"), assistant, tool_result],
        Some(vec![]),
    );
    let params = build_params(
        &base_model(),
        &context,
        &OpenAICompletionsOptions::default(),
    );
    assert_eq!(params.get("tools"), Some(&json!([])));
}

// ---------------------------------------------------------------------------
// Request shaping: max_tokens field selection
// ---------------------------------------------------------------------------

#[test]
fn selects_max_completion_tokens_by_default() {
    let options = OpenAICompletionsOptions {
        max_tokens: Some(1234),
        ..Default::default()
    };
    let params = build_params(
        &base_model(),
        &context_with(vec![user("hi")], None),
        &options,
    );
    assert_eq!(params.get("max_completion_tokens"), Some(&json!(1234)));
    assert!(params.get("max_tokens").is_none());
}

#[test]
fn selects_max_tokens_when_compat_requests_it() {
    let mut model = base_model();
    model.compat = Some(OpenAICompletionsCompat {
        max_tokens_field: Some(MaxTokensField::MaxTokens),
        ..Default::default()
    });
    let options = OpenAICompletionsOptions {
        max_tokens: Some(123),
        ..Default::default()
    };
    let params = build_params(&model, &context_with(vec![user("hi")], None), &options);
    assert_eq!(params.get("max_tokens"), Some(&json!(123)));
    assert!(params.get("max_completion_tokens").is_none());
}

// ---------------------------------------------------------------------------
// Request shaping: thinkingFormat variants
// ---------------------------------------------------------------------------

fn thinking_map(entries: &[(ModelThinkingLevel, Option<&str>)]) -> crate::types::ThinkingLevelMap {
    entries
        .iter()
        .map(|(k, v)| (*k, v.map(|s| s.to_string())))
        .collect()
}

#[test]
fn zai_thinking_format_enables_and_maps_effort() {
    let mut model = base_model();
    model.provider = "zai".to_string();
    model.base_url = "https://api.z.ai/v1".to_string();
    model.reasoning = true;
    model.thinking_level_map = Some(thinking_map(&[
        (ModelThinkingLevel::Minimal, None),
        (ModelThinkingLevel::Low, Some("high")),
        (ModelThinkingLevel::Medium, Some("high")),
        (ModelThinkingLevel::High, Some("high")),
        (ModelThinkingLevel::Max, Some("max")),
    ]));
    model.compat = Some(OpenAICompletionsCompat {
        supports_reasoning_effort: Some(true),
        ..Default::default()
    });

    let options = OpenAICompletionsOptions {
        reasoning_effort: Some(ThinkingLevel::Max),
        ..Default::default()
    };
    let params = build_params(&model, &context_with(vec![user("hi")], None), &options);
    assert_eq!(
        params.get("thinking"),
        Some(&json!({ "type": "enabled", "clear_thinking": false }))
    );
    assert_eq!(params.get("reasoning_effort"), Some(&json!("max")));

    // Thinking off: disabled, no reasoning_effort.
    let params_off = build_params(
        &model,
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions::default(),
    );
    assert_eq!(
        params_off.get("thinking"),
        Some(&json!({ "type": "disabled" }))
    );
    assert!(params_off.get("reasoning_effort").is_none());
}

#[test]
fn zai_replays_reasoning_content_signature() {
    let mut model = base_model();
    model.provider = "zai".to_string();
    model.base_url = "https://api.z.ai/v1".to_string();
    model.id = "glm-5.2".to_string();
    model.reasoning = true;
    model.thinking_level_map = Some(thinking_map(&[(ModelThinkingLevel::High, Some("high"))]));
    model.compat = Some(OpenAICompletionsCompat {
        supports_reasoning_effort: Some(true),
        ..Default::default()
    });

    let assistant = Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![
            ContentBlock::Thinking {
                thinking: "prior reasoning".to_string(),
                thinking_signature: Some("reasoning_content".to_string()),
                redacted: None,
            },
            ContentBlock::ToolCall {
                id: "call_1".to_string(),
                name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
                thought_signature: None,
            },
        ],
        api: "openai-completions".to_string(),
        provider: "zai".to_string(),
        model: "glm-5.2".to_string(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: empty_usage(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp: 0,
    });
    let tool_result = Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "call_1".to_string(),
        tool_name: "read".to_string(),
        content: vec![ContentBlock::Text {
            text: "contents".to_string(),
            text_signature: None,
        }],
        details: None,
        added_tool_names: None,
        is_error: false,
        timestamp: 0,
    });
    let context = context_with(
        vec![
            user("Read README.md"),
            assistant,
            tool_result,
            user("Continue"),
        ],
        None,
    );
    let options = OpenAICompletionsOptions {
        reasoning_effort: Some(ThinkingLevel::High),
        ..Default::default()
    };
    let params = build_params(&model, &context, &options);
    let messages = params.get("messages").and_then(Value::as_array).unwrap();
    let replayed = messages
        .iter()
        .find(|m| m.get("role") == Some(&json!("assistant")))
        .unwrap();
    assert_eq!(
        replayed.get("reasoning_content"),
        Some(&json!("prior reasoning"))
    );
    assert_eq!(
        params.get("thinking"),
        Some(&json!({ "type": "enabled", "clear_thinking": false }))
    );
}

#[test]
fn openrouter_thinking_format_uses_reasoning_object() {
    let mut model = base_model();
    model.provider = "openrouter".to_string();
    model.base_url = "https://openrouter.ai/api/v1".to_string();
    model.id = "deepseek/deepseek-r1".to_string();
    model.reasoning = true;
    let options = OpenAICompletionsOptions {
        reasoning_effort: Some(ThinkingLevel::High),
        ..Default::default()
    };
    let params = build_params(&model, &context_with(vec![user("hi")], None), &options);
    assert_eq!(params.get("reasoning"), Some(&json!({ "effort": "high" })));
    assert!(params.get("reasoning_effort").is_none());
}

#[test]
fn qwen_chat_template_thinking_kwargs() {
    let mut model = base_model();
    model.provider = "local-vllm".to_string();
    model.base_url = "http://localhost:8000/v1".to_string();
    model.reasoning = true;
    model.compat = Some(OpenAICompletionsCompat {
        thinking_format: Some(ThinkingFormat::QwenChatTemplate),
        supports_reasoning_effort: Some(false),
        ..Default::default()
    });

    let on = build_params(
        &model,
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions {
            reasoning_effort: Some(ThinkingLevel::High),
            ..Default::default()
        },
    );
    assert_eq!(
        on.get("chat_template_kwargs"),
        Some(&json!({ "enable_thinking": true, "preserve_thinking": true }))
    );
    assert!(on.get("reasoning_effort").is_none());

    let off = build_params(
        &model,
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions::default(),
    );
    assert_eq!(
        off.get("chat_template_kwargs"),
        Some(&json!({ "enable_thinking": false, "preserve_thinking": true }))
    );
}

#[test]
fn chat_template_boolean_thinking_kwargs() {
    let mut model = base_model();
    model.provider = "local-vllm".to_string();
    model.base_url = "http://localhost:8000/v1".to_string();
    model.reasoning = true;
    model.compat = Some(OpenAICompletionsCompat {
        thinking_format: Some(ThinkingFormat::ChatTemplate),
        supports_reasoning_effort: Some(false),
        chat_template_kwargs: Some(
            [(
                "thinking".to_string(),
                json!({ "$var": "thinking.enabled" }),
            )]
            .into_iter()
            .collect(),
        ),
        ..Default::default()
    });

    let on = build_params(
        &model,
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions {
            reasoning_effort: Some(ThinkingLevel::High),
            ..Default::default()
        },
    );
    assert_eq!(
        on.get("chat_template_kwargs"),
        Some(&json!({ "thinking": true }))
    );
    assert!(on.get("thinking").is_none());
    assert!(on.get("reasoning_effort").is_none());

    let off = build_params(
        &model,
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions::default(),
    );
    assert_eq!(
        off.get("chat_template_kwargs"),
        Some(&json!({ "thinking": false }))
    );
}

#[test]
fn chat_template_static_effort_kwargs() {
    let mut model = base_model();
    model.provider = "local-vllm".to_string();
    model.base_url = "http://localhost:8000/v1".to_string();
    model.reasoning = true;
    model.thinking_level_map = Some(thinking_map(&[(ModelThinkingLevel::Xhigh, Some("max"))]));
    model.compat = Some(OpenAICompletionsCompat {
        thinking_format: Some(ThinkingFormat::ChatTemplate),
        supports_reasoning_effort: Some(false),
        chat_template_kwargs: Some(
            [
                ("preserve_thinking".to_string(), json!(true)),
                (
                    "reasoning_effort".to_string(),
                    json!({ "$var": "thinking.effort", "omitWhenOff": true }),
                ),
            ]
            .into_iter()
            .collect(),
        ),
        ..Default::default()
    });

    let params = build_params(
        &model,
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions {
            reasoning_effort: Some(ThinkingLevel::Xhigh),
            ..Default::default()
        },
    );
    assert_eq!(
        params.get("chat_template_kwargs"),
        Some(&json!({ "preserve_thinking": true, "reasoning_effort": "max" }))
    );
    assert!(params.get("reasoning_effort").is_none());
}

#[test]
fn deepseek_thinking_format_enabled_and_effort() {
    let mut model = base_model();
    model.provider = "deepseek".to_string();
    model.base_url = "https://api.deepseek.com/v1".to_string();
    model.reasoning = true;
    let params = build_params(
        &model,
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions {
            reasoning_effort: Some(ThinkingLevel::High),
            ..Default::default()
        },
    );
    assert_eq!(params.get("thinking"), Some(&json!({ "type": "enabled" })));
    assert_eq!(params.get("reasoning_effort"), Some(&json!("high")));

    let off = build_params(
        &model,
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions::default(),
    );
    assert_eq!(off.get("thinking"), Some(&json!({ "type": "disabled" })));
    assert!(off.get("reasoning_effort").is_none());
}

#[test]
fn string_thinking_format_maps_effort_and_off() {
    let mut model = base_model();
    model.provider = "local-vllm".to_string();
    model.base_url = "http://localhost:8000/v1".to_string();
    model.reasoning = true;
    model.compat = Some(OpenAICompletionsCompat {
        thinking_format: Some(ThinkingFormat::StringThinking),
        ..Default::default()
    });
    let on = build_params(
        &model,
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions {
            reasoning_effort: Some(ThinkingLevel::High),
            ..Default::default()
        },
    );
    assert_eq!(on.get("thinking"), Some(&json!("high")));

    let off = build_params(
        &model,
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions::default(),
    );
    assert_eq!(off.get("thinking"), Some(&json!("none")));
}

#[test]
fn ant_ling_reasoning_mapped_and_omitted_when_unmapped() {
    let mut model = base_model();
    model.provider = "ant-ling".to_string();
    model.base_url = "https://api.ant-ling.com/v1".to_string();
    model.id = "Ring-2.6-1T".to_string();
    model.reasoning = true;
    model.thinking_level_map = Some(thinking_map(&[(ModelThinkingLevel::High, Some("high"))]));

    // Mapped effort → reasoning {effort}; ant-ling metadata (max_tokens field,
    // no store, no reasoning_effort, no prompt-cache under long retention).
    let mut context = context_with(vec![user("Hi")], None);
    context.system_prompt = Some("Follow instructions.".to_string());
    let params = build_params(
        &model,
        &context,
        &OpenAICompletionsOptions {
            max_tokens: Some(123),
            reasoning_effort: Some(ThinkingLevel::High),
            cache_retention: Some(CacheRetention::Long),
            session_id: Some("ant-ling-session".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(params.get("max_tokens"), Some(&json!(123)));
    assert!(params.get("max_completion_tokens").is_none());
    assert_eq!(params.get("reasoning"), Some(&json!({ "effort": "high" })));
    assert!(params.get("reasoning_effort").is_none());
    assert!(params.get("store").is_none());
    assert!(params.get("prompt_cache_key").is_none());
    assert!(params.get("prompt_cache_retention").is_none());
    let messages = params.get("messages").and_then(Value::as_array).unwrap();
    assert_eq!(messages[0].get("role"), Some(&json!("system")));

    // Unmapped direct effort (medium not in map) → no reasoning param.
    let unmapped = build_params(
        &model,
        &context_with(vec![user("Hi")], None),
        &OpenAICompletionsOptions {
            reasoning_effort: Some(ThinkingLevel::Medium),
            ..Default::default()
        },
    );
    assert!(unmapped.get("reasoning").is_none());
}

#[test]
fn openai_default_reasoning_effort_maps_via_thinking_level_map() {
    let mut model = base_model();
    model.provider = "groq".to_string();
    model.base_url = "https://api.groq.com/openai/v1".to_string();
    model.reasoning = true;
    model.thinking_level_map = Some(thinking_map(&[(
        ModelThinkingLevel::Medium,
        Some("default"),
    )]));
    let params = build_params(
        &model,
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions {
            reasoning_effort: Some(ThinkingLevel::Medium),
            ..Default::default()
        },
    );
    assert_eq!(params.get("reasoning_effort"), Some(&json!("default")));
}

// ---------------------------------------------------------------------------
// Request shaping: prompt cache + cache-control markers
// ---------------------------------------------------------------------------

#[test]
fn sets_prompt_cache_key_for_direct_openai_requests() {
    let params = build_params(
        &base_model(),
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions {
            session_id: Some("session-123".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(params.get("prompt_cache_key"), Some(&json!("session-123")));
    assert!(params.get("prompt_cache_retention").is_none());
}

#[test]
fn sets_prompt_cache_retention_24h_for_long_retention() {
    let params = build_params(
        &base_model(),
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions {
            session_id: Some("session-456".to_string()),
            cache_retention: Some(CacheRetention::Long),
            ..Default::default()
        },
    );
    assert_eq!(params.get("prompt_cache_key"), Some(&json!("session-456")));
    assert_eq!(params.get("prompt_cache_retention"), Some(&json!("24h")));
}

#[test]
fn clamps_prompt_cache_key_to_64_chars() {
    let params = build_params(
        &base_model(),
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions {
            session_id: Some("x".repeat(67)),
            ..Default::default()
        },
    );
    assert_eq!(params.get("prompt_cache_key"), Some(&json!("x".repeat(64))));
}

#[test]
fn omits_prompt_cache_fields_when_retention_none() {
    let params = build_params(
        &base_model(),
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions {
            session_id: Some("session-789".to_string()),
            cache_retention: Some(CacheRetention::None),
            ..Default::default()
        },
    );
    assert!(params.get("prompt_cache_key").is_none());
    assert!(params.get("prompt_cache_retention").is_none());
}

#[test]
fn honors_cache_retention_env_override() {
    let params = build_params(
        &base_model(),
        &context_with(vec![user("hi")], None),
        &OpenAICompletionsOptions {
            session_id: Some("session-env".to_string()),
            cache_retention_env: Some("long".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(params.get("prompt_cache_key"), Some(&json!("session-env")));
    assert_eq!(params.get("prompt_cache_retention"), Some(&json!("24h")));
}

#[test]
fn applies_anthropic_cache_markers_when_compat_enables_them() {
    let mut model = base_model();
    model.provider = "openrouter".to_string();
    model.base_url = "https://example.com/v1".to_string();
    model.id = "custom-qwen".to_string();
    model.reasoning = true;
    model.compat = Some(OpenAICompletionsCompat {
        cache_control_format: Some(crate::types::CacheControlFormat::Anthropic),
        ..Default::default()
    });

    let mut context = context_with(vec![user("Hello")], Some(vec![ping_tool()]));
    context.system_prompt = Some("System prompt".to_string());
    let params = build_params(&model, &context, &OpenAICompletionsOptions::default());
    let messages = params.get("messages").and_then(Value::as_array).unwrap();

    let instruction = messages
        .iter()
        .find(|m| {
            matches!(
                m.get("role").and_then(Value::as_str),
                Some("system") | Some("developer")
            )
        })
        .unwrap();
    let instruction_content = instruction
        .get("content")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(
        instruction_content[0].get("cache_control"),
        Some(&json!({ "type": "ephemeral" }))
    );

    let tools = params.get("tools").and_then(Value::as_array).unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0].get("cache_control"),
        Some(&json!({ "type": "ephemeral" }))
    );

    let last = messages.last().unwrap();
    assert_eq!(last.get("role"), Some(&json!("user")));
    let last_content = last.get("content").and_then(Value::as_array).unwrap();
    assert_eq!(
        last_content[0].get("cache_control"),
        Some(&json!({ "type": "ephemeral" }))
    );
}

#[test]
fn omits_anthropic_cache_markers_when_retention_none() {
    let mut model = base_model();
    model.provider = "openrouter".to_string();
    model.base_url = "https://example.com/v1".to_string();
    model.id = "custom-qwen".to_string();
    model.reasoning = true;
    model.compat = Some(OpenAICompletionsCompat {
        cache_control_format: Some(crate::types::CacheControlFormat::Anthropic),
        ..Default::default()
    });
    let mut context = context_with(vec![user("Hello")], Some(vec![ping_tool()]));
    context.system_prompt = Some("System prompt".to_string());
    let params = build_params(
        &model,
        &context,
        &OpenAICompletionsOptions {
            cache_retention: Some(CacheRetention::None),
            ..Default::default()
        },
    );
    let messages = params.get("messages").and_then(Value::as_array).unwrap();
    let instruction = messages
        .iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("system"))
        .unwrap();
    assert!(instruction.get("content").unwrap().is_string());
    let tools = params.get("tools").and_then(Value::as_array).unwrap();
    assert!(tools[0].get("cache_control").is_none());
    assert!(messages.last().unwrap().get("content").unwrap().is_string());
}

// ---------------------------------------------------------------------------
// convert_messages: thinking-as-text, image batching, opencode-go replay
// ---------------------------------------------------------------------------

fn same_model_assistant(model: &OpenAICompletionsModel, content: Vec<ContentBlock>) -> Message {
    Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: empty_usage(),
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: 0,
    })
}

#[test]
fn serializes_thinking_as_text_replay() {
    let mut model = base_model();
    model.provider = "repro-provider".to_string();
    model.id = "repro-model".to_string();
    model.reasoning = true;
    let compat = ResolvedCompat {
        requires_thinking_as_text: true,
        ..get_compat(&model)
    };
    let assistant = same_model_assistant(
        &model,
        vec![
            ContentBlock::Thinking {
                thinking: "internal reasoning".to_string(),
                thinking_signature: None,
                redacted: None,
            },
            ContentBlock::Text {
                text: "visible answer".to_string(),
                text_signature: None,
            },
        ],
    );
    let context = context_with(vec![user("hello"), assistant, user("continue")], None);
    let messages = convert_messages(&model, &context, &compat);
    assert_eq!(
        messages[1],
        json!({
            "role": "assistant",
            "content": [
                { "type": "text", "text": "internal reasoning" },
                { "type": "text", "text": "visible answer" }
            ]
        })
    );
}

#[test]
fn serializes_thinking_only_replay_as_text() {
    let mut model = base_model();
    model.provider = "repro-provider".to_string();
    model.id = "repro-model".to_string();
    model.reasoning = true;
    let compat = ResolvedCompat {
        requires_thinking_as_text: true,
        ..get_compat(&model)
    };
    let assistant = same_model_assistant(
        &model,
        vec![ContentBlock::Thinking {
            thinking: "internal reasoning".to_string(),
            thinking_signature: None,
            redacted: None,
        }],
    );
    let context = context_with(vec![user("hello"), assistant, user("continue")], None);
    let messages = convert_messages(&model, &context, &compat);
    assert_eq!(
        messages[1],
        json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "internal reasoning" }]
        })
    );
}

#[test]
fn batches_tool_result_images_after_consecutive_tool_results() {
    let mut model = base_model();
    model.input = vec![Modality::Text, Modality::Image];
    let compat = get_compat(&model);

    let assistant = same_model_assistant(
        &model,
        vec![
            ContentBlock::ToolCall {
                id: "tool-1".to_string(),
                name: "read".to_string(),
                arguments: json!({ "path": "img-1.png" }),
                thought_signature: None,
            },
            ContentBlock::ToolCall {
                id: "tool-2".to_string(),
                name: "read".to_string(),
                arguments: json!({ "path": "img-2.png" }),
                thought_signature: None,
            },
        ],
    );
    let tool_result = |id: &str| {
        Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: id.to_string(),
            tool_name: "read".to_string(),
            content: vec![
                ContentBlock::Text {
                    text: "Read image file [image/png]".to_string(),
                    text_signature: None,
                },
                ContentBlock::Image {
                    data: "ZmFrZQ==".to_string(),
                    mime_type: "image/png".to_string(),
                },
            ],
            details: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 0,
        })
    };
    let context = context_with(
        vec![
            user("Read the images"),
            assistant,
            tool_result("tool-1"),
            tool_result("tool-2"),
        ],
        None,
    );
    let messages = convert_messages(&model, &context, &compat);
    let roles: Vec<&str> = messages
        .iter()
        .map(|m| m.get("role").and_then(Value::as_str).unwrap())
        .collect();
    assert_eq!(roles, ["user", "assistant", "tool", "tool", "user"]);

    let image_message = messages.last().unwrap();
    let parts = image_message
        .get("content")
        .and_then(Value::as_array)
        .unwrap();
    let image_parts = parts
        .iter()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("image_url"))
        .count();
    assert_eq!(image_parts, 2);
}

#[test]
fn uses_no_tool_output_placeholder_for_empty_tool_results() {
    let mut model = base_model();
    model.input = vec![Modality::Text, Modality::Image];
    let compat = get_compat(&model);
    let assistant = same_model_assistant(
        &model,
        vec![ContentBlock::ToolCall {
            id: "tool-1".to_string(),
            name: "bash".to_string(),
            arguments: json!({ "command": "true" }),
            thought_signature: None,
        }],
    );
    let tool_result = Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "tool-1".to_string(),
        tool_name: "bash".to_string(),
        content: vec![ContentBlock::Text {
            text: "".to_string(),
            text_signature: None,
        }],
        details: None,
        added_tool_names: None,
        is_error: false,
        timestamp: 0,
    });
    let context = context_with(vec![user("Run the command"), assistant, tool_result], None);
    let messages = convert_messages(&model, &context, &compat);
    let tool_message = messages
        .iter()
        .find(|m| m.get("role") == Some(&json!("tool")))
        .unwrap();
    assert_eq!(
        tool_message.get("content"),
        Some(&json!("(no tool output)"))
    );
}

#[test]
fn opencode_go_replays_reasoning_content_signature() {
    let mut model = base_model();
    model.provider = "opencode-go".to_string();
    model.id = "kimi-k2.6".to_string();
    let compat = get_compat(&model);
    let assistant = same_model_assistant(
        &model,
        vec![
            ContentBlock::Thinking {
                thinking: "think".to_string(),
                thinking_signature: Some("reasoning".to_string()),
                redacted: None,
            },
            ContentBlock::ToolCall {
                id: "call_1".to_string(),
                name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
                thought_signature: None,
            },
        ],
    );
    let context = context_with(vec![assistant], None);
    let messages = convert_messages(&model, &context, &compat);
    assert_eq!(messages[0].get("reasoning_content"), Some(&json!("think")));
    assert!(messages[0].get("reasoning").is_none());
}

// ---------------------------------------------------------------------------
// walk_chunks: coalescing, mixed content, errors, response-model, usage
// ---------------------------------------------------------------------------

fn walk(chunks: &[Value], model: &OpenAICompletionsModel) -> StreamOutcome {
    walk_chunks(chunks, model, &OpenAICompletionsOptions::default(), 0)
}

#[test]
fn coalesces_tool_call_deltas_by_stable_index() {
    let chunks = vec![
        json!({
            "id": "chatcmpl-kimi-bad-stream",
            "choices": [{ "delta": { "tool_calls": [
                { "index": 0, "id": "functions.read:0", "type": "function", "function": { "name": "read", "arguments": "" } }
            ] }, "finish_reason": null }]
        }),
        json!({
            "id": "chatcmpl-kimi-bad-stream",
            "choices": [{ "delta": { "tool_calls": [
                { "index": 0, "id": "chatcmpl-tool-a", "type": "function", "function": { "name": null, "arguments": "{\"path\":\"README" } }
            ] }, "finish_reason": null }]
        }),
        json!({
            "id": "chatcmpl-kimi-bad-stream",
            "choices": [{ "delta": { "tool_calls": [
                { "index": 0, "id": "chatcmpl-tool-b", "type": "function", "function": { "name": null, "arguments": ".md\"}" } }
            ] }, "finish_reason": "tool_calls" }],
            "usage": {
                "prompt_tokens": 10, "completion_tokens": 5,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 }
            }
        }),
    ];
    let outcome = walk(&chunks, &base_model());
    assert_eq!(outcome.message.stop_reason, StopReason::ToolUse);
    assert_eq!(tool_content_indexes(&outcome), [0, 0, 0, 0, 0]);
    assert_eq!(outcome.message.content.len(), 1);
    match &outcome.message.content[0] {
        ContentBlock::ToolCall {
            id,
            name,
            arguments,
            ..
        } => {
            assert_eq!(id, "functions.read:0");
            assert_eq!(name, "read");
            assert_eq!(arguments, &json!({ "path": "README.md" }));
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}

#[test]
fn accumulates_mixed_content_reasoning_and_parallel_tool_calls() {
    let chunks = vec![
        json!({
            "id": "chatcmpl-mixed-deltas",
            "choices": [{ "delta": {
                "content": "answer 1",
                "reasoning_content": "think 1",
                "tool_calls": [
                    { "index": 0, "id": "tc_read_initial", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"README" } },
                    { "index": 1, "id": "tc_grep_initial", "type": "function", "function": { "name": "grep", "arguments": "{\"pattern\":\"TODO" } },
                    { "id": "tc_list_no_index", "type": "function", "function": { "name": "list", "arguments": "{\"path\":\"packages" } },
                    { "id": "tc_write_no_index", "type": "function", "function": { "name": "write", "arguments": "{\"path\":\"out" } }
                ]
            }, "finish_reason": null }]
        }),
        json!({
            "id": "chatcmpl-mixed-deltas",
            "choices": [{ "delta": {
                "content": " answer 2",
                "tool_calls": [
                    { "index": 1, "id": "tc_grep_changed", "type": "function", "function": { "arguments": "\",\"path\":\"src" } },
                    { "id": "tc_write_no_index", "type": "function", "function": { "arguments": ".txt\",\"content\":\"ok\"}" } },
                    { "id": "tc_list_no_index", "type": "function", "function": { "arguments": "/ai\"}" } }
                ]
            }, "finish_reason": null }]
        }),
        json!({
            "id": "chatcmpl-mixed-deltas",
            "choices": [{ "delta": {
                "content": "\n",
                "reasoning_content": " think 2",
                "tool_calls": [
                    { "index": 0, "id": "tc_read_changed", "type": "function", "function": { "arguments": ".md\"}" } },
                    { "index": 1, "type": "function", "function": { "arguments": "\"}" } }
                ]
            }, "finish_reason": "tool_calls" }],
            "usage": {
                "prompt_tokens": 10, "completion_tokens": 8,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 2 }
            }
        }),
    ];
    let outcome = walk(&chunks, &base_model());
    assert_eq!(outcome.message.stop_reason, StopReason::ToolUse);

    assert_eq!(count_kind(&outcome, "text_start"), 1);
    assert_eq!(count_kind(&outcome, "text_delta"), 3);
    assert_eq!(count_kind(&outcome, "text_end"), 1);
    assert_eq!(count_kind(&outcome, "thinking_start"), 1);
    assert_eq!(count_kind(&outcome, "thinking_delta"), 2);
    assert_eq!(count_kind(&outcome, "thinking_end"), 1);
    assert_eq!(count_kind(&outcome, "toolcall_start"), 4);
    assert_eq!(count_kind(&outcome, "toolcall_delta"), 9);
    assert_eq!(count_kind(&outcome, "toolcall_end"), 4);

    assert_eq!(outcome.message.content.len(), 6);
    assert_eq!(
        outcome.message.content[0],
        ContentBlock::Text {
            text: "answer 1 answer 2\n".to_string(),
            text_signature: None,
        }
    );
    assert_eq!(
        outcome.message.content[1],
        ContentBlock::Thinking {
            thinking: "think 1 think 2".to_string(),
            thinking_signature: Some("reasoning_content".to_string()),
            redacted: None,
        }
    );
    let expect_tool = |block: &ContentBlock, eid: &str, ename: &str, args: Value| match block {
        ContentBlock::ToolCall {
            id,
            name,
            arguments,
            ..
        } => {
            assert_eq!(id, eid);
            assert_eq!(name, ename);
            assert_eq!(arguments, &args);
        }
        other => panic!("expected tool call, got {other:?}"),
    };
    expect_tool(
        &outcome.message.content[2],
        "tc_read_initial",
        "read",
        json!({ "path": "README.md" }),
    );
    expect_tool(
        &outcome.message.content[3],
        "tc_grep_initial",
        "grep",
        json!({ "pattern": "TODO", "path": "src" }),
    );
    expect_tool(
        &outcome.message.content[4],
        "tc_list_no_index",
        "list",
        json!({ "path": "packages/ai" }),
    );
    expect_tool(
        &outcome.message.content[5],
        "tc_write_no_index",
        "write",
        json!({ "path": "out.txt", "content": "ok" }),
    );
}

#[test]
fn ignores_null_stream_chunks() {
    let chunks = vec![
        Value::Null,
        json!({ "id": "chatcmpl-test", "choices": [{ "delta": { "content": "OK" }, "finish_reason": null }] }),
        json!({
            "id": "chatcmpl-test",
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 3, "completion_tokens": 1,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 }
            }
        }),
    ];
    let outcome = walk(&chunks, &base_model());
    assert_eq!(outcome.message.stop_reason, StopReason::Stop);
    assert!(outcome.message.error_message.is_none());
    assert_eq!(
        outcome.message.response_id.as_deref(),
        Some("chatcmpl-test")
    );
    assert_eq!(outcome.message.usage.total_tokens, 4);
    assert_eq!(
        outcome.message.content,
        vec![ContentBlock::Text {
            text: "OK".to_string(),
            text_signature: None,
        }]
    );
}

#[test]
fn maps_network_error_finish_reason_to_error() {
    let chunks = vec![
        json!({ "choices": [{ "delta": { "content": "partial" }, "finish_reason": null }] }),
        json!({
            "choices": [{ "delta": {}, "finish_reason": "network_error" }],
            "usage": {
                "prompt_tokens": 1, "completion_tokens": 1,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 }
            }
        }),
    ];
    let outcome = walk(&chunks, &base_model());
    assert_eq!(outcome.message.stop_reason, StopReason::Error);
    assert_eq!(
        outcome.message.error_message.as_deref(),
        Some("Provider finish_reason: network_error")
    );
    assert!(matches!(
        outcome.events.last(),
        Some(AssistantMessageEvent::Error { .. })
    ));
}

#[test]
fn errors_when_stream_ends_without_finish_reason() {
    let chunks = vec![
        json!({ "id": "chatcmpl-truncated", "choices": [{ "delta": { "content": "partial answer" }, "finish_reason": null }] }),
        json!({ "id": "chatcmpl-truncated", "choices": [{ "delta": { "content": "partial answer" }, "finish_reason": null }] }),
    ];
    let outcome = walk(&chunks, &base_model());
    assert_eq!(outcome.message.stop_reason, StopReason::Error);
    assert_eq!(
        outcome.message.error_message.as_deref(),
        Some("Stream ended without finish_reason")
    );
}

#[test]
fn surfaces_routed_response_model() {
    let mut model = base_model();
    model.id = "openrouter/auto".to_string();
    model.provider = "openrouter".to_string();
    model.base_url = "https://openrouter.ai/api/v1".to_string();

    let routed = vec![
        json!({ "id": "chatcmpl-1", "model": "anthropic/claude-opus-4.8", "choices": [{ "index": 0, "delta": { "content": "hi" } }] }),
        json!({
            "id": "chatcmpl-1", "model": "anthropic/claude-opus-4.8",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "prompt_tokens_details": { "cached_tokens": 0 }, "completion_tokens_details": { "reasoning_tokens": 0 } }
        }),
    ];
    let outcome = walk(&routed, &model);
    assert_eq!(outcome.message.model, "openrouter/auto");
    assert_eq!(
        outcome.message.response_model.as_deref(),
        Some("anthropic/claude-opus-4.8")
    );
    assert_eq!(outcome.message.stop_reason, StopReason::Stop);

    // Echoed id → responseModel stays unset.
    let echoed = vec![
        json!({ "id": "chatcmpl-2", "model": "openrouter/auto", "choices": [{ "index": 0, "delta": { "content": "hi" } }] }),
        json!({
            "id": "chatcmpl-2", "model": "openrouter/auto",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "prompt_tokens_details": { "cached_tokens": 0 }, "completion_tokens_details": { "reasoning_tokens": 0 } }
        }),
    ];
    let outcome = walk(&echoed, &model);
    assert!(outcome.message.response_model.is_none());

    // Empty/missing chunk.model → responseModel stays unset.
    let missing = vec![
        json!({ "id": "chatcmpl-3", "choices": [{ "index": 0, "delta": { "content": "hi" } }] }),
        json!({ "id": "chatcmpl-3", "model": "", "choices": [{ "index": 0, "delta": { "content": "!" } }] }),
        json!({
            "id": "chatcmpl-3",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "prompt_tokens_details": { "cached_tokens": 0 }, "completion_tokens_details": { "reasoning_tokens": 0 } }
        }),
    ];
    let outcome = walk(&missing, &model);
    assert!(outcome.message.response_model.is_none());
}

#[test]
fn does_not_double_count_reasoning_tokens() {
    let chunks = vec![json!({
        "id": "chatcmpl-reasoning-usage",
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 10, "completion_tokens": 33,
            "prompt_tokens_details": { "cached_tokens": 0 },
            "completion_tokens_details": { "reasoning_tokens": 21 }
        }
    })];
    let outcome = walk(&chunks, &base_model());
    assert_eq!(outcome.message.usage.input, 10);
    assert_eq!(outcome.message.usage.output, 33);
    assert_eq!(outcome.message.usage.total_tokens, 43);
}

#[test]
fn preserves_cache_read_and_write_from_chunk_usage() {
    let chunks = vec![
        json!({ "id": "chatcmpl-cache-write", "choices": [{ "delta": { "content": "OK" }, "finish_reason": null }] }),
        json!({
            "id": "chatcmpl-cache-write",
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 100, "completion_tokens": 5,
                "prompt_tokens_details": { "cached_tokens": 50, "cache_write_tokens": 30 },
                "completion_tokens_details": { "reasoning_tokens": 0 }
            }
        }),
    ];
    let outcome = walk(&chunks, &base_model());
    assert_eq!(outcome.message.usage.input, 20);
    assert_eq!(outcome.message.usage.cache_read, 50);
    assert_eq!(outcome.message.usage.cache_write, 30);
    assert_eq!(outcome.message.usage.total_tokens, 105);
}

#[test]
fn falls_back_to_choice_usage() {
    let chunks = vec![
        json!({ "id": "c", "choices": [{ "delta": { "content": "OK" }, "finish_reason": null }] }),
        json!({
            "id": "c",
            "choices": [{
                "delta": {}, "finish_reason": "stop",
                "usage": {
                    "prompt_tokens": 100, "completion_tokens": 5,
                    "prompt_tokens_details": { "cached_tokens": 50, "cache_write_tokens": 30 },
                    "completion_tokens_details": { "reasoning_tokens": 0 }
                }
            }]
        }),
    ];
    let outcome = walk(&chunks, &base_model());
    assert_eq!(outcome.message.usage.input, 20);
    assert_eq!(outcome.message.usage.cache_read, 50);
    assert_eq!(outcome.message.usage.cache_write, 30);
    assert_eq!(outcome.message.usage.total_tokens, 105);
}

// ---------------------------------------------------------------------------
// walk_chunks: reasoning_details + opencode-go reasoning remap
// ---------------------------------------------------------------------------

#[test]
fn preserves_reasoning_details_arriving_before_matching_tool_call() {
    let mut model = base_model();
    model.provider = "openrouter".to_string();
    model.base_url = "https://openrouter.ai/api/v1".to_string();
    model.id = "google/gemini-test".to_string();
    model.reasoning = true;

    let detail =
        json!({ "type": "reasoning.encrypted", "id": "call_1", "data": "encrypted-signature" });
    let chunks = vec![
        json!({ "id": "chatcmpl-test", "model": "google/gemini-test", "choices": [{ "index": 0, "delta": { "reasoning_details": [detail] }, "finish_reason": null }] }),
        json!({ "id": "chatcmpl-test", "model": "google/gemini-test", "choices": [{ "index": 0, "delta": { "tool_calls": [
            { "index": 0, "id": "call_1", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"README.md\"}" } }
        ] }, "finish_reason": null }] }),
        json!({ "id": "chatcmpl-test", "model": "google/gemini-test", "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }] }),
    ];
    let outcome = walk(&chunks, &model);
    let tool_call = outcome
        .message
        .content
        .iter()
        .find(|b| matches!(b, ContentBlock::ToolCall { .. }))
        .unwrap();
    let ContentBlock::ToolCall {
        id,
        name,
        arguments,
        thought_signature,
    } = tool_call
    else {
        panic!("expected tool call");
    };
    assert_eq!(id, "call_1");
    assert_eq!(name, "read");
    assert_eq!(arguments, &json!({ "path": "README.md" }));
    // thoughtSignature is the serialized detail; parsing it round-trips.
    let parsed: Value = serde_json::from_str(thought_signature.as_ref().unwrap()).unwrap();
    assert_eq!(parsed, detail);

    // Replaying the produced assistant message re-emits reasoning_details.
    let context = context_with(vec![Message::Assistant(outcome.message.clone())], None);
    let compat = get_compat(&model);
    let messages = convert_messages(&model, &context, &compat);
    let replayed = messages
        .iter()
        .find(|m| m.get("role") == Some(&json!("assistant")))
        .unwrap();
    assert_eq!(replayed.get("reasoning_details"), Some(&json!([detail])));
}

#[test]
fn opencode_go_reasoning_delta_signature_is_reasoning_content() {
    let mut model = base_model();
    model.provider = "opencode-go".to_string();
    model.id = "kimi-k2.6".to_string();
    let chunks = vec![json!({
        "id": "chatcmpl-opencode-go-reasoning",
        "choices": [{ "delta": { "reasoning": "think" }, "finish_reason": "stop" }]
    })];
    let outcome = walk(&chunks, &model);
    assert_eq!(
        outcome.message.content,
        vec![ContentBlock::Thinking {
            thinking: "think".to_string(),
            thinking_signature: Some("reasoning_content".to_string()),
            redacted: None,
        }]
    );
}

#[test]
fn non_opencode_reasoning_delta_keeps_original_field() {
    let chunks = vec![json!({
        "id": "chatcmpl-reasoning",
        "choices": [{ "delta": { "reasoning": "think" }, "finish_reason": "stop" }]
    })];
    let outcome = walk(&chunks, &base_model());
    assert_eq!(
        outcome.message.content,
        vec![ContentBlock::Thinking {
            thinking: "think".to_string(),
            thinking_signature: Some("reasoning".to_string()),
            redacted: None,
        }]
    );
}

// ---------------------------------------------------------------------------
// Raw SSE decode path
// ---------------------------------------------------------------------------

#[test]
fn parse_sse_chunks_decodes_data_lines_and_stops_at_done() {
    let body = concat!(
        "data: {\"id\":\"chatcmpl-repro\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-repro\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n",
    );
    let chunks = parse_sse_chunks(body);
    assert_eq!(chunks.len(), 2);

    let outcome = walk(&chunks, &base_model());
    assert_eq!(outcome.message.stop_reason, StopReason::Stop);
    assert_eq!(
        outcome.message.content,
        vec![ContentBlock::Text {
            text: "ok".to_string(),
            text_signature: None,
        }]
    );
    assert!(matches!(
        outcome.events.last(),
        Some(AssistantMessageEvent::Done { .. })
    ));
}

#[test]
fn full_lifecycle_event_ordering() {
    let chunks = vec![
        json!({ "id": "c", "choices": [{ "delta": { "content": "Hello" }, "finish_reason": null }] }),
        json!({
            "id": "c",
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "prompt_tokens_details": { "cached_tokens": 0 }, "completion_tokens_details": { "reasoning_tokens": 0 } }
        }),
    ];
    let outcome = walk(&chunks, &base_model());
    assert_eq!(
        event_kinds(&outcome),
        ["start", "text_start", "text_delta", "text_end", "done"]
    );
}
