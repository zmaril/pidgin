//! Ported from `vendor/pi/packages/agent/test/harness/compaction.test.ts`.
//!
//! Each test cites the pi `it(...)` name it maps to. Deterministic assertions
//! (token math, cut points, preparation, serialization) are ported faithfully.
//! Summarization-path tests drive a `FauxModels` fake standing in for pi's
//! `createModels()` + `fauxProvider()` (atilla-ai does not yet wrap `Models`);
//! it records the completion options and context each call receives, which is
//! how the reasoning/max-tokens/prompt assertions are checked. The branch of
//! pi's suite guarded by a real OAuth token has no counterpart here (there are
//! no real-LLM tests in this file to `#[ignore]`).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

use atilla_ai::providers::faux::{
    faux_assistant_message, faux_text, FauxAssistantOptions, FauxModelDefinition, FauxProvider,
    RegisterFauxProviderOptions,
};
use atilla_ai::{AssistantMessage, Context, Model, StopReason, Usage};

use atilla_agent::harness::compaction::{
    calculate_context_tokens, compact, estimate_context_tokens, estimate_tokens, find_cut_point,
    find_turn_start_index, generate_summary, get_last_assistant_usage, prepare_compaction,
    should_compact, CompactionErrorCode, CompactionPreparation, CompactionSettings,
    CompletionOptions, CutPointResult, FileOperations, Models, DEFAULT_COMPACTION_SETTINGS,
};
use atilla_agent::harness::session::{build_session_context, SessionContextBuildOptions};
use atilla_agent::harness::types::{
    BranchSummaryEntry, CompactionEntry, CustomMessageEntry, MessageEntry, ModelChangeEntry,
    SessionTreeEntry, ThinkingLevelChangeEntry,
};

// ---------------------------------------------------------------------------
// Test builders (mirror the `create*` helpers atop compaction.test.ts).
// ---------------------------------------------------------------------------

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn create_id() -> String {
    format!("entry-{}", NEXT_ID.fetch_add(1, Ordering::SeqCst))
}

const TS: &str = "2024-01-01T00:00:00.000Z";

fn create_mock_usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Value {
    json!({
        "input": input,
        "output": output,
        "cacheRead": cache_read,
        "cacheWrite": cache_write,
        "totalTokens": input + output + cache_read + cache_write,
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
    })
}

fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Usage {
    serde_json::from_value(create_mock_usage(input, output, cache_read, cache_write)).unwrap()
}

fn create_user_message(text: &str) -> Value {
    json!({ "role": "user", "content": [{ "type": "text", "text": text }], "timestamp": 0 })
}

fn create_assistant_message(text: &str, usage: Value) -> Value {
    json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": text }],
        "api": "anthropic-messages",
        "provider": "anthropic",
        "model": "claude-sonnet-4-5",
        "usage": usage,
        "stopReason": "stop",
        "timestamp": 0,
    })
}

fn assistant_default(text: &str) -> Value {
    create_assistant_message(text, create_mock_usage(100, 50, 0, 0))
}

fn message_entry(message: Value, parent_id: Option<&str>) -> SessionTreeEntry {
    SessionTreeEntry::Message(MessageEntry {
        id: create_id(),
        parent_id: parent_id.map(str::to_string),
        timestamp: TS.to_string(),
        message,
    })
}

fn compaction_entry(summary: &str, first_kept: &str, parent_id: Option<&str>) -> SessionTreeEntry {
    SessionTreeEntry::Compaction(CompactionEntry {
        id: create_id(),
        parent_id: parent_id.map(str::to_string),
        timestamp: TS.to_string(),
        summary: summary.to_string(),
        first_kept_entry_id: first_kept.to_string(),
        tokens_before: 1234,
        details: None,
        from_hook: None,
    })
}

fn thinking_entry(level: &str, parent_id: Option<&str>) -> SessionTreeEntry {
    SessionTreeEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
        id: create_id(),
        parent_id: parent_id.map(str::to_string),
        timestamp: TS.to_string(),
        thinking_level: level.to_string(),
    })
}

fn model_change_entry(provider: &str, model_id: &str, parent_id: Option<&str>) -> SessionTreeEntry {
    SessionTreeEntry::ModelChange(ModelChangeEntry {
        id: create_id(),
        parent_id: parent_id.map(str::to_string),
        timestamp: TS.to_string(),
        provider: provider.to_string(),
        model_id: model_id.to_string(),
    })
}

fn entry_id(entry: &SessionTreeEntry) -> String {
    entry.id().to_string()
}

// ---------------------------------------------------------------------------
// FauxModels: the test stand-in for pi's createModels() + fauxProvider().
// ---------------------------------------------------------------------------

type ResponseFn = Box<dyn Fn(&Context, &CompletionOptions) -> AssistantMessage>;

struct FauxModels {
    responses: RefCell<VecDeque<ResponseFn>>,
    seen_options: RefCell<Vec<CompletionOptions>>,
    seen_contexts: RefCell<Vec<Context>>,
}

impl FauxModels {
    fn new() -> Self {
        Self {
            responses: RefCell::new(VecDeque::new()),
            seen_options: RefCell::new(Vec::new()),
            seen_contexts: RefCell::new(Vec::new()),
        }
    }

    fn set_responses(&self, responses: Vec<ResponseFn>) {
        *self.responses.borrow_mut() = responses.into_iter().collect();
    }

    fn text_response(text: &str) -> ResponseFn {
        let text = text.to_string();
        Box::new(move |_ctx, _opts| {
            faux_assistant_message(
                vec![faux_text(text.clone())],
                FauxAssistantOptions::default(),
                0,
            )
        })
    }

    fn error_response(stop: StopReason, message: &str) -> ResponseFn {
        let message = message.to_string();
        Box::new(move |_ctx, _opts| {
            faux_assistant_message(
                vec![],
                FauxAssistantOptions {
                    stop_reason: Some(stop),
                    error_message: Some(message.clone()),
                    ..FauxAssistantOptions::default()
                },
                0,
            )
        })
    }
}

impl Models for FauxModels {
    fn complete_simple(
        &self,
        _model: &Model,
        context: &Context,
        options: &CompletionOptions,
    ) -> AssistantMessage {
        self.seen_options.borrow_mut().push(options.clone());
        self.seen_contexts.borrow_mut().push(context.clone());
        let f = self
            .responses
            .borrow_mut()
            .pop_front()
            .expect("no faux response queued");
        f(context, options)
    }
}

/// Mirror pi's `createFauxModel(reasoning, maxTokens)`.
fn create_faux_model(reasoning: bool, max_tokens: u64) -> Model {
    let faux = FauxProvider::new(RegisterFauxProviderOptions {
        models: Some(vec![FauxModelDefinition {
            id: if reasoning {
                "reasoning-model".to_string()
            } else {
                "non-reasoning-model".to_string()
            },
            name: None,
            reasoning: Some(reasoning),
            input: None,
            cost: None,
            context_window: Some(200000),
            max_tokens: Some(max_tokens),
        }]),
        ..RegisterFauxProviderOptions::default()
    });
    faux.get_model(None).expect("faux model")
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// pi: "calculates total context tokens from usage"
#[test]
fn calculates_total_context_tokens_from_usage() {
    assert_eq!(calculate_context_tokens(&usage(1000, 500, 200, 100)), 1800);
    assert_eq!(calculate_context_tokens(&usage(0, 0, 0, 0)), 0);
}

/// pi: "checks compaction threshold"
#[test]
fn checks_compaction_threshold() {
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 10000,
        keep_recent_tokens: 20000,
    };
    assert!(should_compact(95000, 100000, &settings));
    assert!(!should_compact(89000, 100000, &settings));
    assert!(!should_compact(
        95000,
        100000,
        &CompactionSettings {
            enabled: false,
            ..settings.clone()
        }
    ));
}

/// pi: "finds a cut point based on token differences"
#[test]
fn finds_a_cut_point_based_on_token_differences() {
    let mut entries: Vec<SessionTreeEntry> = Vec::new();
    let mut parent: Option<String> = None;
    for i in 0..10 {
        let user = message_entry(create_user_message(&format!("User {i}")), parent.as_deref());
        let user_id = entry_id(&user);
        entries.push(user);
        let assistant = message_entry(
            create_assistant_message(
                &format!("Assistant {i}"),
                create_mock_usage(0, 100, (i + 1) * 1000, 0),
            ),
            Some(&user_id),
        );
        parent = Some(entry_id(&assistant));
        entries.push(assistant);
    }

    let result = find_cut_point(&entries, 0, entries.len(), 2500);
    assert_eq!(entries[result.first_kept_entry_index].type_str(), "message");
}

/// pi: "covers cut-point and turn-start edge cases"
#[test]
fn covers_cut_point_and_turn_start_edge_cases() {
    let thinking = thinking_entry("high", None);
    let thinking_id = entry_id(&thinking);
    let model_change = model_change_entry("openai", "gpt-4", Some(&thinking_id));
    assert_eq!(
        find_cut_point(&[thinking.clone(), model_change.clone()], 0, 2, 1),
        CutPointResult {
            first_kept_entry_index: 0,
            turn_start_index: -1,
            is_split_turn: false,
        }
    );

    let branch_summary = SessionTreeEntry::BranchSummary(BranchSummaryEntry {
        id: create_id(),
        parent_id: Some(entry_id(&model_change)),
        timestamp: TS.to_string(),
        from_id: "branch".to_string(),
        summary: "branch summary".to_string(),
        details: None,
        from_hook: None,
    });
    let custom_message = SessionTreeEntry::CustomMessage(CustomMessageEntry {
        id: create_id(),
        parent_id: Some(entry_id(&branch_summary)),
        timestamp: TS.to_string(),
        custom_type: "note".to_string(),
        content: json!("custom content"),
        display: true,
        details: None,
    });

    assert_eq!(
        find_turn_start_index(&[thinking.clone(), branch_summary.clone()], 1, 0),
        1
    );
    assert_eq!(
        find_turn_start_index(&[thinking.clone(), custom_message.clone()], 1, 0),
        1
    );
    assert_eq!(
        find_turn_start_index(&[thinking.clone(), model_change.clone()], 1, 0),
        -1
    );

    let result = find_cut_point(&[thinking.clone(), branch_summary, custom_message], 0, 3, 1);
    assert_eq!(result.first_kept_entry_index, 0);

    let tool_result = message_entry(
        json!({
            "role": "toolResult",
            "toolCallId": "call-1",
            "toolName": "read",
            "content": [{ "type": "text", "text": "tool output" }],
            "isError": false,
            "timestamp": 0,
        }),
        None,
    );
    assert_eq!(
        find_cut_point(&[tool_result], 0, 1, 1),
        CutPointResult {
            first_kept_entry_index: 0,
            turn_start_index: -1,
            is_split_turn: false,
        }
    );

    let user = message_entry(create_user_message("user"), None);
    let user_id = entry_id(&user);
    let compaction = compaction_entry("summary", &user_id, Some(&user_id));
    let assistant = message_entry(assistant_default("assistant"), Some(&entry_id(&compaction)));
    assert_eq!(
        find_cut_point(&[user, compaction, assistant], 0, 3, 1).first_kept_entry_index,
        2
    );
}

/// pi: "estimates tokens and context usage across supported message roles"
#[test]
fn estimates_tokens_and_context_usage_across_supported_message_roles() {
    let mock_usage = create_mock_usage(10, 5, 3, 2);
    let assistant = create_assistant_message("assistant", mock_usage.clone());

    let mut assistant_with_thinking_and_tool = assistant.clone();
    assistant_with_thinking_and_tool["content"] = json!([
        { "type": "thinking", "thinking": "thinking" },
        { "type": "toolCall", "id": "call-1", "name": "read", "arguments": { "path": "file.ts" } },
    ]);

    let custom_string = json!({
        "role": "custom", "customType": "note", "content": "custom text", "display": true, "timestamp": 0,
    });
    let tool_result_with_image = json!({
        "role": "toolResult", "toolCallId": "call-1", "toolName": "read",
        "content": [
            { "type": "text", "text": "tool text" },
            { "type": "image", "mimeType": "image/png", "data": "abc" },
        ],
        "isError": false, "timestamp": 0,
    });
    let bash_execution = json!({
        "role": "bashExecution", "command": "npm run check", "output": "ok",
        "exitCode": 0, "cancelled": false, "truncated": false, "timestamp": 0,
    });
    let branch_summary_message =
        json!({ "role": "branchSummary", "summary": "branch", "fromId": "x", "timestamp": 0 });
    let compaction_summary_message = json!({ "role": "compactionSummary", "summary": "compact", "tokensBefore": 123, "timestamp": 0 });

    assert!(estimate_tokens(&create_user_message("plain user")) > 0);
    assert!(estimate_tokens(&assistant_with_thinking_and_tool) > 0);
    assert!(estimate_tokens(&custom_string) > 0);
    assert!(estimate_tokens(&tool_result_with_image) > 1000);
    assert!(estimate_tokens(&bash_execution) > 0);
    assert!(estimate_tokens(&branch_summary_message) > 0);
    assert!(estimate_tokens(&compaction_summary_message) > 0);
    assert_eq!(
        estimate_tokens(&json!({ "role": "unknown", "timestamp": 0 })),
        0
    );

    let expected_usage = usage(10, 5, 3, 2);
    assert_eq!(
        get_last_assistant_usage(&[
            message_entry(create_user_message("user"), None),
            message_entry(assistant.clone(), None),
        ]),
        Some(expected_usage.clone())
    );

    let mut aborted = assistant.clone();
    aborted["stopReason"] = json!("aborted");
    let mut errored = assistant.clone();
    errored["stopReason"] = json!("error");
    assert_eq!(
        get_last_assistant_usage(&[message_entry(aborted, None), message_entry(errored, None)]),
        None
    );

    assert_eq!(
        get_last_assistant_usage(&[
            message_entry(create_user_message("user"), None),
            message_entry(assistant.clone(), None),
            message_entry(
                create_assistant_message("partial", create_mock_usage(0, 0, 0, 0)),
                None
            ),
        ]),
        Some(expected_usage)
    );

    assert_eq!(
        estimate_context_tokens(&[create_user_message("no usage")]).last_usage_index,
        None
    );

    let estimate = estimate_context_tokens(&[assistant.clone(), create_user_message("tail")]);
    assert_eq!(estimate.usage_tokens, 20);
    assert_eq!(estimate.last_usage_index, Some(0));

    let estimate = estimate_context_tokens(&[
        create_user_message("Hello"),
        assistant.clone(),
        create_user_message("continue"),
        create_assistant_message("Partial thinking", create_mock_usage(0, 0, 0, 0)),
    ]);
    assert_eq!(estimate.usage_tokens, 20);
    assert_eq!(estimate.last_usage_index, Some(1));
    assert!(estimate.trailing_tokens > 0);
    assert_eq!(estimate.tokens, 20 + estimate.trailing_tokens);
}

/// pi: "builds session context with a compaction entry"
#[test]
fn builds_session_context_with_a_compaction_entry() {
    let u1 = message_entry(create_user_message("1"), None);
    let a1 = message_entry(assistant_default("a"), Some(&entry_id(&u1)));
    let u2 = message_entry(create_user_message("2"), Some(&entry_id(&a1)));
    let a2 = message_entry(assistant_default("b"), Some(&entry_id(&u2)));
    let compaction = compaction_entry("Summary of 1,a,2,b", &entry_id(&u2), Some(&entry_id(&a2)));
    let u3 = message_entry(create_user_message("3"), Some(&entry_id(&compaction)));
    let a3 = message_entry(assistant_default("c"), Some(&entry_id(&u3)));
    let loaded = build_session_context(
        &[u1, a1, u2, a2, compaction, u3, a3],
        &SessionContextBuildOptions::default(),
    );
    assert_eq!(loaded.messages.len(), 5);
    assert_eq!(
        loaded.messages[0].get("role").and_then(Value::as_str),
        Some("compactionSummary")
    );
}

/// pi: "tracks model and thinking level changes in built context"
#[test]
fn tracks_model_and_thinking_level_changes_in_built_context() {
    let user = message_entry(create_user_message("1"), None);
    let model_change = model_change_entry("openai", "gpt-4", Some(&entry_id(&user)));
    let assistant = message_entry(assistant_default("a"), Some(&entry_id(&model_change)));
    let thinking = thinking_entry("high", Some(&entry_id(&assistant)));
    let loaded = build_session_context(
        &[user, model_change, assistant, thinking],
        &SessionContextBuildOptions::default(),
    );
    let model = loaded.model.expect("model");
    assert_eq!(model.provider, "anthropic");
    assert_eq!(model.model_id, "claude-sonnet-4-5");
    assert_eq!(loaded.thinking_level, "high");
}

/// pi: "prepares compaction using the latest compaction summary as previousSummary"
#[test]
fn prepares_compaction_using_latest_summary_as_previous_summary() {
    let u1 = message_entry(create_user_message("user msg 1"), None);
    let a1 = message_entry(assistant_default("assistant msg 1"), Some(&entry_id(&u1)));
    let u2 = message_entry(create_user_message("user msg 2"), Some(&entry_id(&a1)));
    let a2 = message_entry(
        create_assistant_message("assistant msg 2", create_mock_usage(5000, 1000, 0, 0)),
        Some(&entry_id(&u2)),
    );
    let compaction1 = compaction_entry("First summary", &entry_id(&u2), Some(&entry_id(&a2)));
    let u3 = message_entry(
        create_user_message("user msg 3"),
        Some(&entry_id(&compaction1)),
    );
    let a3 = message_entry(
        create_assistant_message("assistant msg 3", create_mock_usage(8000, 2000, 0, 0)),
        Some(&entry_id(&u3)),
    );
    let path = vec![u1, a1, u2, a2, compaction1, u3, a3];
    let preparation = prepare_compaction(&path, &DEFAULT_COMPACTION_SETTINGS)
        .unwrap()
        .expect("preparation");
    assert_eq!(
        preparation.previous_summary.as_deref(),
        Some("First summary")
    );
    assert!(!preparation.first_kept_entry_id.is_empty());
    let expected_tokens = estimate_context_tokens(
        &build_session_context(&path, &SessionContextBuildOptions::default()).messages,
    )
    .tokens;
    assert_eq!(preparation.tokens_before, expected_tokens);
}

/// pi: "prepares split-turn compaction with prior file-operation details"
#[test]
fn prepares_split_turn_compaction_with_prior_file_operation_details() {
    let u1 = message_entry(create_user_message("user msg 1"), None);
    let mut assistant_message = assistant_default("assistant msg 1");
    assistant_message["content"] = json!([{ "type": "toolCall", "id": "tool-1", "name": "write", "arguments": { "path": "written.ts" } }]);
    let a1 = message_entry(assistant_message, Some(&entry_id(&u1)));
    let compaction1 = SessionTreeEntry::Compaction(CompactionEntry {
        id: create_id(),
        parent_id: Some(entry_id(&a1)),
        timestamp: TS.to_string(),
        summary: "First summary".to_string(),
        first_kept_entry_id: entry_id(&u1),
        tokens_before: 1234,
        details: Some(json!({ "readFiles": ["old-read.ts"], "modifiedFiles": ["old-edit.ts"] })),
        from_hook: None,
    });
    let u2 = message_entry(
        create_user_message("large turn"),
        Some(&entry_id(&compaction1)),
    );
    let a2 = message_entry(
        assistant_default("large assistant message"),
        Some(&entry_id(&u2)),
    );

    let preparation = prepare_compaction(
        &[u1, a1, compaction1, u2, a2],
        &CompactionSettings {
            enabled: true,
            reserve_tokens: 100,
            keep_recent_tokens: 1,
        },
    )
    .unwrap()
    .expect("preparation");

    assert_eq!(
        preparation.previous_summary.as_deref(),
        Some("First summary")
    );
    assert!(preparation.is_split_turn);
    let roles: Vec<&str> = preparation
        .turn_prefix_messages
        .iter()
        .map(|m| m.get("role").and_then(Value::as_str).unwrap_or(""))
        .collect();
    assert_eq!(roles, vec!["user"]);
    assert!(preparation.file_ops.read.contains("old-read.ts"));
    assert!(preparation.file_ops.edited.contains("old-edit.ts"));
    assert!(preparation.file_ops.written.contains("written.ts"));
}

/// pi: "prepares custom and branch summary entries for summarization"
#[test]
fn prepares_custom_and_branch_summary_entries_for_summarization() {
    let branch_summary = SessionTreeEntry::BranchSummary(BranchSummaryEntry {
        id: create_id(),
        parent_id: None,
        timestamp: TS.to_string(),
        from_id: "branch".to_string(),
        summary: "branch summary".to_string(),
        details: None,
        from_hook: None,
    });
    let custom_message = SessionTreeEntry::CustomMessage(CustomMessageEntry {
        id: create_id(),
        parent_id: Some(entry_id(&branch_summary)),
        timestamp: TS.to_string(),
        custom_type: "note".to_string(),
        content: json!("custom content"),
        display: true,
        details: None,
    });
    let user = message_entry(
        create_user_message("keep"),
        Some(&entry_id(&custom_message)),
    );
    let assistant = message_entry(assistant_default("assistant"), Some(&entry_id(&user)));

    let preparation = prepare_compaction(
        &[branch_summary, custom_message, user, assistant],
        &CompactionSettings {
            enabled: true,
            reserve_tokens: 100,
            keep_recent_tokens: 1,
        },
    )
    .unwrap()
    .expect("preparation");

    let roles: Vec<&str> = preparation
        .messages_to_summarize
        .iter()
        .map(|m| m.get("role").and_then(Value::as_str).unwrap_or(""))
        .collect();
    assert_eq!(roles, vec!["branchSummary", "custom"]);
}

/// pi: "does not prepare compaction when there is nothing valid to compact"
#[test]
fn does_not_prepare_compaction_when_nothing_valid() {
    let compaction = compaction_entry("already compacted", "entry-keep", None);
    assert!(
        prepare_compaction(&[compaction], &DEFAULT_COMPACTION_SETTINGS)
            .unwrap()
            .is_none()
    );
    assert!(prepare_compaction(&[], &DEFAULT_COMPACTION_SETTINGS)
        .unwrap()
        .is_none());
}

/// pi: "serializes conversation with truncated tool results"
#[test]
fn serializes_conversation_with_truncated_tool_results() {
    let long_content = "x".repeat(5000);
    let messages = vec![json!({
        "role": "toolResult",
        "toolCallId": "tc1",
        "toolName": "read",
        "content": [{ "type": "text", "text": long_content }],
        "isError": false,
        "timestamp": 0,
    })];
    let result = atilla_agent::harness::compaction::serialize_conversation(&messages);
    assert!(result.contains("[Tool result]:"));
    assert!(result.contains("[... 3000 more characters truncated]"));
}

/// pi: "passes reasoning through generateSummary only for reasoning models with thinking enabled"
#[test]
fn passes_reasoning_through_generate_summary_only_when_enabled() {
    let messages = vec![create_user_message("Summarize this.")];

    let reasoning = FauxModels::new();
    reasoning.set_responses(vec![FauxModels::text_response("## Goal\nTest summary")]);
    let reasoning_model = create_faux_model(true, 8192);
    generate_summary(
        &messages,
        &reasoning,
        &reasoning_model,
        2000,
        None,
        None,
        None,
        Some("medium"),
    )
    .unwrap();
    assert_eq!(
        reasoning.seen_options.borrow()[0].reasoning.as_deref(),
        Some("medium")
    );

    let off = FauxModels::new();
    off.set_responses(vec![FauxModels::text_response("## Goal\nTest summary")]);
    let off_model = create_faux_model(true, 8192);
    generate_summary(
        &messages,
        &off,
        &off_model,
        2000,
        None,
        None,
        None,
        Some("off"),
    )
    .unwrap();
    assert_eq!(off.seen_options.borrow()[0].reasoning, None);

    let non_reasoning = FauxModels::new();
    non_reasoning.set_responses(vec![FauxModels::text_response("## Goal\nTest summary")]);
    let non_reasoning_model = create_faux_model(false, 8192);
    generate_summary(
        &messages,
        &non_reasoning,
        &non_reasoning_model,
        2000,
        None,
        None,
        None,
        Some("medium"),
    )
    .unwrap();
    assert_eq!(non_reasoning.seen_options.borrow()[0].reasoning, None);
}

/// pi: "includes previous summaries and custom instructions in generateSummary prompts"
#[test]
fn includes_previous_summaries_and_custom_instructions_in_prompts() {
    let messages = vec![create_user_message("Summarize this.")];
    let models = FauxModels::new();
    models.set_responses(vec![FauxModels::text_response("## Goal\nTest summary")]);
    let model = create_faux_model(false, 8192);

    let summary = generate_summary(
        &messages,
        &models,
        &model,
        2000,
        None,
        Some("focus"),
        Some("old summary"),
        None,
    )
    .unwrap();

    assert!(summary.contains("Test summary"));
    // Extract the prompt text the model saw (single user text block).
    let ctx = &models.seen_contexts.borrow()[0];
    let prompt_text = user_text_of(ctx);
    assert!(prompt_text.contains("<previous-summary>\nold summary\n</previous-summary>"));
    assert!(prompt_text.contains("Additional focus: focus"));
}

/// pi: "returns error results for failed or aborted summary generations"
#[test]
fn returns_error_results_for_failed_or_aborted_summaries() {
    let messages = vec![create_user_message("Summarize this.")];

    let error = FauxModels::new();
    error.set_responses(vec![FauxModels::error_response(StopReason::Error, "boom")]);
    let error_model = create_faux_model(false, 8192);
    let err = generate_summary(
        &messages,
        &error,
        &error_model,
        2000,
        None,
        None,
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(err.code, CompactionErrorCode::SummarizationFailed);
    assert_eq!(err.message, "Summarization failed: boom");

    let aborted = FauxModels::new();
    aborted.set_responses(vec![FauxModels::error_response(
        StopReason::Aborted,
        "stopped",
    )]);
    let aborted_model = create_faux_model(false, 8192);
    let err = generate_summary(
        &messages,
        &aborted,
        &aborted_model,
        2000,
        None,
        None,
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(err.code, CompactionErrorCode::Aborted);
    assert_eq!(err.message, "stopped");
}

/// pi: "clamps compaction summary maxTokens to the model output cap"
#[test]
fn clamps_compaction_summary_max_tokens_to_model_output_cap() {
    let messages = vec![create_user_message("Summarize this.")];
    let models = FauxModels::new();
    models.set_responses(vec![
        FauxModels::text_response("## Goal\nTest summary"),
        FauxModels::text_response("## Goal\nTest summary"),
    ]);
    let model = create_faux_model(false, 128000);
    let preparation = CompactionPreparation {
        first_kept_entry_id: "entry-keep".to_string(),
        messages_to_summarize: messages.clone(),
        turn_prefix_messages: messages,
        is_split_turn: true,
        tokens_before: 600000,
        previous_summary: None,
        file_ops: FileOperations::default(),
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 500000,
            keep_recent_tokens: 20000,
        },
    };

    compact(&preparation, &models, &model, None, None, None).unwrap();

    let seen: Vec<i64> = models
        .seen_options
        .borrow()
        .iter()
        .map(|o| o.max_tokens)
        .collect();
    assert_eq!(seen, vec![128000, 128000]);
}

/// pi: "returns compaction error results without throwing"
#[test]
fn returns_compaction_error_results_without_throwing() {
    let messages = vec![create_user_message("Summarize this.")];
    let preparation = CompactionPreparation {
        first_kept_entry_id: "entry-keep".to_string(),
        messages_to_summarize: messages.clone(),
        turn_prefix_messages: vec![],
        is_split_turn: false,
        tokens_before: 100,
        previous_summary: None,
        file_ops: FileOperations::default(),
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 2000,
            keep_recent_tokens: 20,
        },
    };
    let history = FauxModels::new();
    history.set_responses(vec![FauxModels::error_response(
        StopReason::Error,
        "history failed",
    )]);
    let history_model = create_faux_model(false, 8192);
    let err = compact(&preparation, &history, &history_model, None, None, None).unwrap_err();
    assert_eq!(err.code, CompactionErrorCode::SummarizationFailed);
    assert_eq!(err.message, "Summarization failed: history failed");

    let invalid_model = create_faux_model(false, 8192);
    let invalid_models = FauxModels::new();
    let invalid = compact(
        &CompactionPreparation {
            messages_to_summarize: vec![],
            first_kept_entry_id: String::new(),
            ..preparation.clone()
        },
        &invalid_models,
        &invalid_model,
        None,
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(invalid.code, CompactionErrorCode::InvalidSession);
}

/// pi: "passes reasoning through turn-prefix summaries when enabled"
#[test]
fn passes_reasoning_through_turn_prefix_summaries_when_enabled() {
    let messages = vec![create_user_message("Summarize this.")];
    let models = FauxModels::new();
    models.set_responses(vec![FauxModels::text_response(
        "## Original Request\nTest summary",
    )]);
    let model = create_faux_model(true, 8192);
    let preparation = CompactionPreparation {
        first_kept_entry_id: "entry-keep".to_string(),
        messages_to_summarize: vec![],
        turn_prefix_messages: messages,
        is_split_turn: true,
        tokens_before: 100,
        previous_summary: None,
        file_ops: FileOperations::default(),
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 2000,
            keep_recent_tokens: 20,
        },
    };

    compact(&preparation, &models, &model, None, None, Some("high")).unwrap();
    assert_eq!(
        models.seen_options.borrow()[0].reasoning.as_deref(),
        Some("high")
    );
}

/// pi: "returns turn-prefix compaction errors without throwing"
#[test]
fn returns_turn_prefix_compaction_errors_without_throwing() {
    let messages = vec![create_user_message("Summarize this.")];
    let preparation = CompactionPreparation {
        first_kept_entry_id: "entry-keep".to_string(),
        messages_to_summarize: vec![],
        turn_prefix_messages: messages,
        is_split_turn: true,
        tokens_before: 100,
        previous_summary: None,
        file_ops: FileOperations::default(),
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 2000,
            keep_recent_tokens: 20,
        },
    };
    let models = FauxModels::new();
    models.set_responses(vec![FauxModels::error_response(
        StopReason::Error,
        "prefix failed",
    )]);
    let model = create_faux_model(false, 8192);
    let err = compact(&preparation, &models, &model, None, None, None).unwrap_err();
    assert_eq!(err.code, CompactionErrorCode::SummarizationFailed);
    assert_eq!(
        err.message,
        "Turn prefix summarization failed: prefix failed"
    );

    let aborted = FauxModels::new();
    aborted.set_responses(vec![FauxModels::error_response(
        StopReason::Aborted,
        "prefix stopped",
    )]);
    let aborted_model = create_faux_model(false, 8192);
    let err = compact(&preparation, &aborted, &aborted_model, None, None, None).unwrap_err();
    assert_eq!(err.code, CompactionErrorCode::Aborted);
    assert_eq!(err.message, "prefix stopped");
}

/// pi: "returns a compaction result with file details"
#[test]
fn returns_a_compaction_result_with_file_details() {
    let u1 = message_entry(create_user_message("read a file"), None);
    let mut assistant_message =
        create_assistant_message("calling tool", create_mock_usage(1000, 200, 0, 0));
    assistant_message["content"] = json!([{ "type": "toolCall", "id": "tool-1", "name": "read", "arguments": { "path": "src/index.ts" } }]);
    let a1 = message_entry(assistant_message, Some(&entry_id(&u1)));
    let u2 = message_entry(create_user_message("continue"), Some(&entry_id(&a1)));
    let a2 = message_entry(
        create_assistant_message("done", create_mock_usage(4000, 500, 0, 0)),
        Some(&entry_id(&u2)),
    );
    let preparation = prepare_compaction(&[u1, a1, u2, a2], &DEFAULT_COMPACTION_SETTINGS)
        .unwrap()
        .expect("preparation");

    let models = FauxModels::new();
    models.set_responses(vec![FauxModels::text_response("## Goal\nTest summary")]);
    let model = create_faux_model(false, 8192);
    let result = compact(&preparation, &models, &model, None, None, None).unwrap();
    assert!(!result.summary.is_empty());
    assert!(!result.first_kept_entry_id.is_empty());
    assert!(result.details.is_some());
}

// ---------------------------------------------------------------------------
// Branch summarization.
//
// pi ships no branch-summarization test file, so these are atilla-authored
// coverage of the ported deterministic branch logic and its error paths; they
// do not map to a specific pi assertion.
// ---------------------------------------------------------------------------

use atilla_agent::harness::compaction::{
    collect_entries_for_branch_summary, generate_branch_summary, prepare_branch_entries,
    BranchSummaryErrorCode, GenerateBranchSummaryOptions, BRANCH_SUMMARY_PREAMBLE,
};
use atilla_agent::harness::session::{InMemorySessionStorage, Session};
use atilla_ai::seams::AbortSignal;

fn assistant_with_read(path: &str) -> Value {
    let mut msg = assistant_default("reading");
    msg["content"] =
        json!([{ "type": "toolCall", "id": "t1", "name": "read", "arguments": { "path": path } }]);
    msg
}

#[test]
fn prepare_branch_entries_selects_messages_and_extracts_file_ops() {
    let u1 = message_entry(create_user_message("do work"), None);
    let a1 = message_entry(assistant_with_read("src/a.ts"), Some(&entry_id(&u1)));
    let tool_result = message_entry(
        json!({
            "role": "toolResult", "toolCallId": "t1", "toolName": "read",
            "content": [{ "type": "text", "text": "contents" }], "isError": false, "timestamp": 0,
        }),
        Some(&entry_id(&a1)),
    );

    let prep = prepare_branch_entries(&[u1, a1, tool_result], 0);
    // toolResult message entries are dropped by the branch getMessageFromEntry.
    let roles: Vec<&str> = prep
        .messages
        .iter()
        .map(|m| m.get("role").and_then(Value::as_str).unwrap_or(""))
        .collect();
    assert_eq!(roles, vec!["user", "assistant"]);
    assert!(prep.file_ops.read.contains("src/a.ts"));
    assert!(prep.total_tokens > 0);
}

#[test]
fn generate_branch_summary_prepends_preamble_and_reports_files() {
    let u1 = message_entry(create_user_message("explore"), None);
    let a1 = message_entry(assistant_with_read("src/x.ts"), Some(&entry_id(&u1)));
    let models = FauxModels::new();
    models.set_responses(vec![FauxModels::text_response("## Goal\nExplored")]);
    let model = create_faux_model(false, 8192);
    let result = generate_branch_summary(
        &[u1, a1],
        &GenerateBranchSummaryOptions {
            models: &models,
            model: &model,
            signal: AbortSignal::new(),
            custom_instructions: None,
            replace_instructions: false,
            reserve_tokens: None,
        },
    )
    .unwrap();
    assert!(result.summary.starts_with(BRANCH_SUMMARY_PREAMBLE));
    assert!(result.summary.contains("Explored"));
    assert_eq!(result.read_files, vec!["src/x.ts".to_string()]);
    // The branch summary always requests a fixed 2048 max-token cap.
    assert_eq!(models.seen_options.borrow()[0].max_tokens, 2048);
}

#[test]
fn generate_branch_summary_returns_placeholder_for_empty_entries() {
    let models = FauxModels::new();
    let model = create_faux_model(false, 8192);
    let result = generate_branch_summary(
        &[],
        &GenerateBranchSummaryOptions {
            models: &models,
            model: &model,
            signal: AbortSignal::new(),
            custom_instructions: None,
            replace_instructions: false,
            reserve_tokens: None,
        },
    )
    .unwrap();
    assert_eq!(result.summary, "No content to summarize");
    assert!(result.read_files.is_empty());
    assert!(result.modified_files.is_empty());
}

#[test]
fn generate_branch_summary_surfaces_error_stop_reason() {
    let u1 = message_entry(create_user_message("explore"), None);
    let models = FauxModels::new();
    models.set_responses(vec![FauxModels::error_response(StopReason::Error, "boom")]);
    let model = create_faux_model(false, 8192);
    let err = generate_branch_summary(
        &[u1],
        &GenerateBranchSummaryOptions {
            models: &models,
            model: &model,
            signal: AbortSignal::new(),
            custom_instructions: None,
            replace_instructions: false,
            reserve_tokens: None,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, BranchSummaryErrorCode::SummarizationFailed);
    assert_eq!(err.message, "Branch summary failed: boom");
}

#[test]
fn collect_entries_for_branch_summary_walks_to_common_ancestor() {
    let e0 = message_entry(create_user_message("root"), None);
    let e1 = message_entry(assistant_default("a"), Some(&entry_id(&e0)));
    let branch_x = message_entry(create_user_message("x"), Some(&entry_id(&e1)));
    let branch_y = message_entry(create_user_message("y"), Some(&entry_id(&e1)));
    let (e1_id, x_id, y_id) = (entry_id(&e1), entry_id(&branch_x), entry_id(&branch_y));

    let storage =
        InMemorySessionStorage::with_options(Some(vec![e0, e1, branch_x, branch_y]), None);
    let session = Session::new(Rc::new(storage));

    let result = collect_entries_for_branch_summary(&session, Some(&x_id), &y_id).unwrap();
    assert_eq!(result.common_ancestor_id.as_deref(), Some(e1_id.as_str()));
    let ids: Vec<String> = result.entries.iter().map(|e| e.id().to_string()).collect();
    assert_eq!(ids, vec![x_id]);

    // With no previous leaf, nothing is collected.
    let empty = collect_entries_for_branch_summary(&session, None, &y_id).unwrap();
    assert!(empty.entries.is_empty());
    assert_eq!(empty.common_ancestor_id, None);
}

/// Extract the single user text block from a summarization context.
fn user_text_of(ctx: &Context) -> String {
    let value = serde_json::to_value(&ctx.messages[0]).unwrap();
    value
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|b| b.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}
