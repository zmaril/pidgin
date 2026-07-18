// straitjacket-allow-file:duplication — faithful port of pi coding-agent's
// compaction test suites; parallel structure to the agent-core compaction test
// (crates/atilla-agent/tests/compaction.rs) is intentional.

//! Ported from `vendor/pi/packages/coding-agent/test/compaction.test.ts` and
//! `vendor/pi/packages/coding-agent/test/compaction-serialization.test.ts`.
//!
//! Each test cites the pi `it(...)` name it maps to. Deterministic assertions
//! (token math, cut points, preparation, serialization) are ported faithfully.
//! Summarization-path tests drive a `FauxModels` fake standing in for pi's
//! `completeSimple` (atilla-ai does not yet wrap `Models`); it records the
//! completion options and context each call receives. pi's `describe.skipIf`
//! LLM tests are ported here by driving the same `FauxModels` fake over the
//! `large-session.jsonl` fixture (no real API key required).

use std::cell::RefCell;
use std::collections::VecDeque;

use serde_json::{json, Value};

use atilla_ai::providers::faux::{
    faux_assistant_message, faux_text, FauxAssistantOptions, FauxModelDefinition, FauxProvider,
    RegisterFauxProviderOptions,
};
use atilla_ai::{AssistantMessage, Context, Model, StopReason, Usage};

use atilla_coding::core::compaction::{
    calculate_context_tokens, compact, estimate_context_tokens, find_cut_point,
    get_last_assistant_usage, prepare_compaction, serialize_conversation, should_compact,
    CompactionErrorCode, CompactionPreparation, CompactionSettings, CompletionOptions,
    CutPointResult, FileOperations, Models, DEFAULT_COMPACTION_SETTINGS,
};
use atilla_coding::core::session_manager::{
    build_session_context, migrate_session_entries, parse_session_entries, AgentMessage,
    CompactionEntry, MessageEntry, SessionEntry,
};

// ---------------------------------------------------------------------------
// Message builders (mirror the `create*Message` helpers atop the pi suite).
// ---------------------------------------------------------------------------

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

/// pi's `createUserMessage`: note the content is a bare string, not a block
/// array (the coding-agent shape).
fn create_user_message(text: &str) -> Value {
    json!({ "role": "user", "content": text, "timestamp": 0 })
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

// ---------------------------------------------------------------------------
// Entry builder (mirrors the counter/lastId chaining in the pi suite).
// ---------------------------------------------------------------------------

struct Builder {
    counter: u64,
    last_id: Option<String>,
}

impl Builder {
    fn new() -> Self {
        Self {
            counter: 0,
            last_id: None,
        }
    }

    fn next_id(&mut self) -> String {
        let id = format!("test-id-{}", self.counter);
        self.counter += 1;
        id
    }

    fn message(&mut self, message: Value) -> SessionEntry {
        let id = self.next_id();
        let parent = self.last_id.clone();
        self.last_id = Some(id.clone());
        SessionEntry::Message(MessageEntry {
            id,
            parent_id: parent,
            timestamp: "2024-01-01T00:00:00.000Z".to_string(),
            message,
        })
    }

    fn compaction(&mut self, summary: &str, first_kept: &str) -> SessionEntry {
        let id = self.next_id();
        let parent = self.last_id.clone();
        self.last_id = Some(id.clone());
        SessionEntry::Compaction(CompactionEntry {
            id,
            parent_id: parent,
            timestamp: "2024-01-01T00:00:00.000Z".to_string(),
            summary: summary.to_string(),
            first_kept_entry_id: first_kept.to_string(),
            tokens_before: 10000,
            details: None,
            from_hook: None,
        })
    }

    fn model_change(&mut self, provider: &str, model_id: &str) -> SessionEntry {
        let id = self.next_id();
        let parent = self.last_id.clone();
        self.last_id = Some(id.clone());
        entry_from_value(json!({
            "type": "model_change",
            "id": id,
            "parentId": parent,
            "timestamp": "2024-01-01T00:00:00.000Z",
            "provider": provider,
            "modelId": model_id,
        }))
    }

    fn thinking(&mut self, level: &str) -> SessionEntry {
        let id = self.next_id();
        let parent = self.last_id.clone();
        self.last_id = Some(id.clone());
        entry_from_value(json!({
            "type": "thinking_level_change",
            "id": id,
            "parentId": parent,
            "timestamp": "2024-01-01T00:00:00.000Z",
            "thinkingLevel": level,
        }))
    }

    fn custom_message(&mut self, content: &str) -> SessionEntry {
        let id = self.next_id();
        let parent = self.last_id.clone();
        self.last_id = Some(id.clone());
        entry_from_value(json!({
            "type": "custom_message",
            "id": id,
            "parentId": parent,
            "timestamp": "2024-01-01T00:00:00.000Z",
            "customType": "test",
            "content": content,
            "display": true,
        }))
    }
}

fn entry_from_value(value: Value) -> SessionEntry {
    serde_json::from_value(value).expect("valid session entry")
}

fn entry_id(entry: &SessionEntry) -> String {
    entry.id().to_string()
}

/// pi's `extractText` helper for the re-summarize assertion.
fn extract_text(messages: &[AgentMessage]) -> String {
    messages
        .iter()
        .map(
            |message| match message.get("role").and_then(Value::as_str) {
                Some("user") | Some("custom") | Some("toolResult") => content_text(message),
                Some("assistant") => content_text(message),
                Some("branchSummary") | Some("compactionSummary") => message
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                Some("bashExecution") => format!(
                    "{}\n{}",
                    message.get("command").and_then(Value::as_str).unwrap_or(""),
                    message.get("output").and_then(Value::as_str).unwrap_or("")
                ),
                _ => String::new(),
            },
        )
        .collect::<Vec<_>>()
        .join("\n")
}

fn content_text(message: &Value) -> String {
    let content = message.get("content");
    if let Some(s) = content.and_then(Value::as_str) {
        return s.to_string();
    }
    let mut texts: Vec<&str> = Vec::new();
    for block in content.and_then(Value::as_array).into_iter().flatten() {
        let is_text = block.get("type").and_then(Value::as_str) == Some("text");
        if let (true, Some(t)) = (is_text, block.get("text").and_then(Value::as_str)) {
            texts.push(t);
        }
    }
    texts.join(" ")
}

// ---------------------------------------------------------------------------
// FauxModels: the test stand-in for pi's completeSimple.
// ---------------------------------------------------------------------------

type ResponseFn = Box<dyn Fn(&Context, &CompletionOptions) -> AssistantMessage>;

struct FauxModels {
    responses: RefCell<VecDeque<ResponseFn>>,
    default: RefCell<Option<ResponseFn>>,
    seen_options: RefCell<Vec<CompletionOptions>>,
    seen_contexts: RefCell<Vec<Context>>,
}

impl FauxModels {
    fn new() -> Self {
        Self {
            responses: RefCell::new(VecDeque::new()),
            default: RefCell::new(None),
            seen_options: RefCell::new(Vec::new()),
            seen_contexts: RefCell::new(Vec::new()),
        }
    }

    fn set_responses(&self, responses: Vec<ResponseFn>) {
        *self.responses.borrow_mut() = responses.into_iter().collect();
    }

    /// Fallback response used when the queue is empty (for the fixture-driven
    /// compaction tests, where the number of model calls depends on the data).
    fn set_default(&self, response: ResponseFn) {
        *self.default.borrow_mut() = Some(response);
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
        if let Some(f) = self.responses.borrow_mut().pop_front() {
            return f(context, options);
        }
        let default = self.default.borrow();
        let f = default.as_ref().expect("no faux response queued");
        f(context, options)
    }
}

fn create_faux_model(reasoning: bool, max_tokens: u64) -> Model {
    let faux = FauxProvider::new(RegisterFauxProviderOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-model".to_string(),
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

// ===========================================================================
// Token calculation
// ===========================================================================

/// pi: "should calculate total context tokens from usage" / "should handle zero"
#[test]
fn calculates_total_context_tokens_from_usage() {
    assert_eq!(calculate_context_tokens(&usage(1000, 500, 200, 100)), 1800);
    assert_eq!(calculate_context_tokens(&usage(0, 0, 0, 0)), 0);
}

// ===========================================================================
// getLastAssistantUsage
// ===========================================================================

/// pi: "should find the last non-aborted assistant message usage"
#[test]
fn finds_last_non_aborted_assistant_usage() {
    let mut b = Builder::new();
    let entries = vec![
        b.message(create_user_message("Hello")),
        b.message(create_assistant_message(
            "Hi",
            create_mock_usage(100, 50, 0, 0),
        )),
        b.message(create_user_message("How are you?")),
        b.message(create_assistant_message(
            "Good",
            create_mock_usage(200, 100, 0, 0),
        )),
    ];
    let u = get_last_assistant_usage(&entries).expect("usage");
    assert_eq!(u.input, 200);
}

/// pi: "should skip aborted messages"
#[test]
fn skips_aborted_messages() {
    let mut b = Builder::new();
    let mut aborted = create_assistant_message("Aborted", create_mock_usage(300, 150, 0, 0));
    aborted["stopReason"] = json!("aborted");
    let entries = vec![
        b.message(create_user_message("Hello")),
        b.message(create_assistant_message(
            "Hi",
            create_mock_usage(100, 50, 0, 0),
        )),
        b.message(create_user_message("How are you?")),
        b.message(aborted),
    ];
    assert_eq!(
        get_last_assistant_usage(&entries).expect("usage").input,
        100
    );
}

/// pi: "should skip all-zero assistant usage"
#[test]
fn skips_all_zero_assistant_usage() {
    let mut b = Builder::new();
    let entries = vec![
        b.message(create_user_message("Hello")),
        b.message(create_assistant_message(
            "Hi",
            create_mock_usage(100, 50, 0, 0),
        )),
        b.message(create_user_message("continue")),
        b.message(create_assistant_message(
            "Partial",
            create_mock_usage(0, 0, 0, 0),
        )),
    ];
    assert_eq!(
        get_last_assistant_usage(&entries).expect("usage").input,
        100
    );
}

/// pi: "should return undefined if no assistant messages"
#[test]
fn returns_none_when_no_assistant_messages() {
    let mut b = Builder::new();
    let entries = vec![b.message(create_user_message("Hello"))];
    assert_eq!(get_last_assistant_usage(&entries), None);
}

// ===========================================================================
// estimateContextTokens
// ===========================================================================

/// pi: "uses the last non-zero assistant usage as the context anchor"
#[test]
fn estimate_context_tokens_anchors_on_last_non_zero_usage() {
    let messages = vec![
        create_user_message("Hello"),
        create_assistant_message("Hi", create_mock_usage(100, 50, 0, 0)),
        create_user_message("continue"),
        create_assistant_message("Partial thinking", create_mock_usage(0, 0, 0, 0)),
    ];
    let estimate = estimate_context_tokens(&messages);
    assert_eq!(estimate.usage_tokens, 150);
    assert_eq!(estimate.last_usage_index, Some(1));
    assert!(estimate.trailing_tokens > 0);
    assert_eq!(estimate.tokens, 150 + estimate.trailing_tokens);
}

// ===========================================================================
// shouldCompact
// ===========================================================================

/// pi: "should return true when context exceeds threshold" / "false when disabled"
#[test]
fn should_compact_threshold_and_disabled() {
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

// ===========================================================================
// findCutPoint
// ===========================================================================

/// pi: "should find cut point based on actual token differences"
#[test]
fn find_cut_point_based_on_token_differences() {
    let mut b = Builder::new();
    let mut entries: Vec<SessionEntry> = Vec::new();
    for i in 0..10u64 {
        entries.push(b.message(create_user_message(&format!("User {i}"))));
        entries.push(b.message(create_assistant_message(
            &format!("Assistant {i}"),
            create_mock_usage(0, 100, (i + 1) * 1000, 0),
        )));
    }
    let result = find_cut_point(&entries, 0, entries.len(), 2500);
    let entry = &entries[result.first_kept_entry_index];
    assert_eq!(entry.type_str(), "message");
    let role = match entry {
        SessionEntry::Message(e) => e.message.get("role").and_then(Value::as_str).unwrap_or(""),
        _ => "",
    };
    assert!(role == "user" || role == "assistant");
}

/// pi: "should return startIndex if no valid cut points in range"
#[test]
fn find_cut_point_returns_start_index_default() {
    let mut b = Builder::new();
    let entries = vec![b.message(assistant_default("a"))];
    let result = find_cut_point(&entries, 0, entries.len(), 1000);
    assert_eq!(result.first_kept_entry_index, 0);
}

/// pi: "should keep everything if all messages fit within budget"
#[test]
fn find_cut_point_keeps_everything_within_budget() {
    let mut b = Builder::new();
    let entries = vec![
        b.message(create_user_message("1")),
        b.message(create_assistant_message(
            "a",
            create_mock_usage(0, 50, 500, 0),
        )),
        b.message(create_user_message("2")),
        b.message(create_assistant_message(
            "b",
            create_mock_usage(0, 50, 1000, 0),
        )),
    ];
    let result = find_cut_point(&entries, 0, entries.len(), 50000);
    assert_eq!(result.first_kept_entry_index, 0);
}

/// pi: "should indicate split turn when cutting at assistant message"
#[test]
fn find_cut_point_indicates_split_turn_at_assistant() {
    let mut b = Builder::new();
    let entries = vec![
        b.message(create_user_message("Turn 1")),
        b.message(create_assistant_message(
            "A1",
            create_mock_usage(0, 100, 1000, 0),
        )),
        b.message(create_user_message("Turn 2")),
        b.message(create_assistant_message(
            "A2-1",
            create_mock_usage(0, 100, 5000, 0),
        )),
        b.message(create_assistant_message(
            "A2-2",
            create_mock_usage(0, 100, 8000, 0),
        )),
        b.message(create_assistant_message(
            "A2-3",
            create_mock_usage(0, 100, 10000, 0),
        )),
    ];
    let result = find_cut_point(&entries, 0, entries.len(), 3000);
    let role = match &entries[result.first_kept_entry_index] {
        SessionEntry::Message(e) => e.message.get("role").and_then(Value::as_str).unwrap_or(""),
        _ => "",
    };
    if role == "assistant" {
        assert!(result.is_split_turn);
        assert_eq!(result.turn_start_index, 2);
    }
}

/// pi: "should budget context-visible custom message entries" — exercises the
/// entry-expansion path.
#[test]
fn find_cut_point_budgets_context_visible_custom_message_entries() {
    let mut b = Builder::new();
    let entries = vec![
        b.message(create_user_message("hi")),
        b.message(assistant_default("hello")),
        b.custom_message(&"x".repeat(4000)),
        b.message(assistant_default("ok")),
    ];

    let tiny_budget = find_cut_point(&entries, 0, entries.len(), 1);
    assert_eq!(
        tiny_budget,
        CutPointResult {
            first_kept_entry_index: 3,
            turn_start_index: 2,
            is_split_turn: true,
        }
    );

    let custom_fits_budget = find_cut_point(&entries, 0, entries.len(), 2);
    assert_eq!(
        custom_fits_budget,
        CutPointResult {
            first_kept_entry_index: 2,
            turn_start_index: -1,
            is_split_turn: false,
        }
    );
}

// ===========================================================================
// buildSessionContext
// ===========================================================================

/// pi: "should load all messages when no compaction"
#[test]
fn build_session_context_loads_all_messages_no_compaction() {
    let mut b = Builder::new();
    let entries = vec![
        b.message(create_user_message("1")),
        b.message(assistant_default("a")),
        b.message(create_user_message("2")),
        b.message(assistant_default("b")),
    ];
    let loaded = build_session_context(&entries, None);
    assert_eq!(loaded.messages.len(), 4);
    assert_eq!(loaded.thinking_level, "off");
    let model = loaded.model.expect("model");
    assert_eq!(model.provider, "anthropic");
    assert_eq!(model.model_id, "claude-sonnet-4-5");
}

/// pi: "should handle single compaction"
#[test]
fn build_session_context_single_compaction() {
    let mut b = Builder::new();
    let u1 = b.message(create_user_message("1"));
    let a1 = b.message(assistant_default("a"));
    let u2 = b.message(create_user_message("2"));
    let a2 = b.message(assistant_default("b"));
    let compaction = b.compaction("Summary of 1,a,2,b", &entry_id(&u2));
    let u3 = b.message(create_user_message("3"));
    let a3 = b.message(assistant_default("c"));
    let entries = vec![u1, a1, u2, a2, compaction, u3, a3];

    let loaded = build_session_context(&entries, None);
    assert_eq!(loaded.messages.len(), 5);
    assert_eq!(
        loaded.messages[0].get("role").and_then(Value::as_str),
        Some("compactionSummary")
    );
    assert!(loaded.messages[0]
        .get("summary")
        .and_then(Value::as_str)
        .unwrap()
        .contains("Summary of 1,a,2,b"));
}

/// pi: "should handle multiple compactions (only latest matters)"
#[test]
fn build_session_context_multiple_compactions_latest_matters() {
    let mut b = Builder::new();
    let u1 = b.message(create_user_message("1"));
    let a1 = b.message(assistant_default("a"));
    let compact1 = b.compaction("First summary", &entry_id(&u1));
    let u2 = b.message(create_user_message("2"));
    let bb = b.message(assistant_default("b"));
    let u3 = b.message(create_user_message("3"));
    let c = b.message(assistant_default("c"));
    let compact2 = b.compaction("Second summary", &entry_id(&u3));
    let u4 = b.message(create_user_message("4"));
    let d = b.message(assistant_default("d"));
    let entries = vec![u1, a1, compact1, u2, bb, u3, c, compact2, u4, d];

    let loaded = build_session_context(&entries, None);
    assert_eq!(loaded.messages.len(), 5);
    assert!(loaded.messages[0]
        .get("summary")
        .and_then(Value::as_str)
        .unwrap()
        .contains("Second summary"));
}

/// pi: "should keep all messages when firstKeptEntryId is first entry"
#[test]
fn build_session_context_keeps_all_when_first_kept_is_first() {
    let mut b = Builder::new();
    let u1 = b.message(create_user_message("1"));
    let a1 = b.message(assistant_default("a"));
    let compact1 = b.compaction("First summary", &entry_id(&u1));
    let u2 = b.message(create_user_message("2"));
    let bb = b.message(assistant_default("b"));
    let entries = vec![u1, a1, compact1, u2, bb];

    let loaded = build_session_context(&entries, None);
    assert_eq!(loaded.messages.len(), 5);
}

/// pi: "should track model and thinking level changes"
#[test]
fn build_session_context_tracks_model_and_thinking() {
    let mut b = Builder::new();
    let entries = vec![
        b.message(create_user_message("1")),
        b.model_change("openai", "gpt-4"),
        b.message(assistant_default("a")),
        b.thinking("high"),
    ];
    let loaded = build_session_context(&entries, None);
    let model = loaded.model.expect("model");
    assert_eq!(model.provider, "anthropic");
    assert_eq!(model.model_id, "claude-sonnet-4-5");
    assert_eq!(loaded.thinking_level, "high");
}

// ===========================================================================
// prepareCompaction with previous compaction
// ===========================================================================

/// pi: "should skip repeated compactions when kept messages still fit"
#[test]
fn prepare_compaction_skips_repeat_when_kept_still_fits() {
    let mut b = Builder::new();
    let u1 = b.message(create_user_message(
        "user msg 1 (summarized by compaction1)",
    ));
    let a1 = b.message(assistant_default("assistant msg 1"));
    let u2 = b.message(create_user_message("user msg 2 - kept by compaction1"));
    let a2 = b.message(assistant_default("assistant msg 2"));
    let u3 = b.message(create_user_message("user msg 3 - kept by compaction1"));
    let a3 = b.message(create_assistant_message(
        "assistant msg 3",
        create_mock_usage(5000, 1000, 0, 0),
    ));
    let compaction1 = b.compaction("First summary", &entry_id(&u2));
    let u4 = b.message(create_user_message("user msg 4 (new after compaction1)"));
    let a4 = b.message(create_assistant_message(
        "assistant msg 4",
        create_mock_usage(8000, 2000, 0, 0),
    ));

    let path = vec![u1, a1, u2, a2, u3, a3, compaction1, u4, a4];
    let preparation = prepare_compaction(&path, &DEFAULT_COMPACTION_SETTINGS).unwrap();
    assert!(preparation.is_none());
}

/// pi: "should re-summarize previously kept messages when the recent window
/// moves past them"
#[test]
fn prepare_compaction_resummarizes_when_window_moves_past() {
    let mut b = Builder::new();
    let u1 = b.message(create_user_message(
        &"user msg 1 (summarized by compaction1)".repeat(4),
    ));
    let a1 = b.message(assistant_default(&"assistant msg 1".repeat(4)));
    let u2 = b.message(create_user_message(
        &"user msg 2 - kept by compaction1 ".repeat(12),
    ));
    let a2 = b.message(assistant_default(&"assistant msg 2 ".repeat(12)));
    let u3 = b.message(create_user_message(
        &"user msg 3 - kept by compaction1 ".repeat(12),
    ));
    let a3 = b.message(create_assistant_message(
        &"assistant msg 3 ".repeat(12),
        create_mock_usage(5000, 1000, 0, 0),
    ));
    let compaction1 = b.compaction("First summary", &entry_id(&u2));
    let u4 = b.message(create_user_message(
        &"user msg 4 (new after compaction1) ".repeat(12),
    ));
    let a4 = b.message(create_assistant_message(
        &"assistant msg 4 ".repeat(12),
        create_mock_usage(8000, 2000, 0, 0),
    ));

    let settings = CompactionSettings {
        keep_recent_tokens: 100,
        ..DEFAULT_COMPACTION_SETTINGS
    };
    let preparation = prepare_compaction(&[u1, a1, u2, a2, u3, a3, compaction1, u4, a4], &settings)
        .unwrap()
        .expect("preparation");

    let summarized_text = extract_text(&preparation.messages_to_summarize);
    assert!(summarized_text.contains("user msg 2 - kept by compaction1"));
    assert!(summarized_text.contains("user msg 3 - kept by compaction1"));
    assert!(!summarized_text.contains("First summary"));
    assert_eq!(
        preparation.previous_summary.as_deref(),
        Some("First summary")
    );
}

// ===========================================================================
// Large session fixture
// ===========================================================================

fn load_large_session_entries() -> Vec<SessionEntry> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/large-session.jsonl"
    );
    let content = std::fs::read_to_string(path).expect("read fixture");
    let mut values = parse_session_entries(&content);
    migrate_session_entries(&mut values); // add id/parentId for v1 fixtures
    values
        .into_iter()
        .filter(|v| v.get("type").and_then(Value::as_str) != Some("session"))
        .map(|v| serde_json::from_value::<SessionEntry>(v).expect("valid entry"))
        .collect()
}

/// pi: "should parse the large session"
#[test]
fn large_session_parses() {
    let entries = load_large_session_entries();
    assert!(entries.len() > 100);
    let message_count = entries
        .iter()
        .filter(|e| matches!(e, SessionEntry::Message(_)))
        .count();
    assert!(message_count > 100);
}

/// pi: "should find cut point in large session"
#[test]
fn large_session_find_cut_point() {
    let entries = load_large_session_entries();
    let result = find_cut_point(
        &entries,
        0,
        entries.len(),
        DEFAULT_COMPACTION_SETTINGS.keep_recent_tokens,
    );
    let entry = &entries[result.first_kept_entry_index];
    assert_eq!(entry.type_str(), "message");
    let role = match entry {
        SessionEntry::Message(e) => e.message.get("role").and_then(Value::as_str).unwrap_or(""),
        _ => "",
    };
    assert!(role == "user" || role == "assistant");
}

/// pi: "should load session correctly"
#[test]
fn large_session_loads() {
    let entries = load_large_session_entries();
    let loaded = build_session_context(&entries, None);
    assert!(loaded.messages.len() > 100);
    assert!(loaded.model.is_some());
}

// ===========================================================================
// LLM summarization (pi runs these only with an API key; ported here by
// driving the FauxModels seam over the fixture).
// ===========================================================================

/// pi: "should generate a compaction result for the large session"
#[test]
fn large_session_generates_compaction_result() {
    let entries = load_large_session_entries();
    let models = FauxModels::new();
    let long = format!("## Goal\n{}", "summarized content ".repeat(20));
    models.set_default(FauxModels::text_response(&long));
    let model = create_faux_model(false, 8192);

    let preparation = prepare_compaction(&entries, &DEFAULT_COMPACTION_SETTINGS)
        .unwrap()
        .expect("preparation");
    let result = compact(&preparation, &models, &model, None, None, None).unwrap();

    assert!(result.summary.len() > 100);
    assert!(!result.first_kept_entry_id.is_empty());
    assert!(result.tokens_before > 0);
}

/// pi: "should produce valid session after compaction"
#[test]
fn large_session_produces_valid_session_after_compaction() {
    let entries = load_large_session_entries();
    let loaded = build_session_context(&entries, None);
    let models = FauxModels::new();
    models.set_default(FauxModels::text_response(
        "## Goal\nCompacted summary of the session.",
    ));
    let model = create_faux_model(false, 8192);

    let preparation = prepare_compaction(&entries, &DEFAULT_COMPACTION_SETTINGS)
        .unwrap()
        .expect("preparation");
    let result = compact(&preparation, &models, &model, None, None, None).unwrap();

    // Simulate appending the compaction entry (as pi's test does).
    let parent_id = entries.last().unwrap().id().to_string();
    let compaction_entry = SessionEntry::Compaction(CompactionEntry {
        id: "compaction-test-id".to_string(),
        parent_id: Some(parent_id),
        timestamp: "2024-01-01T00:00:00.000Z".to_string(),
        summary: result.summary.clone(),
        first_kept_entry_id: result.first_kept_entry_id.clone(),
        tokens_before: result.tokens_before,
        details: result
            .details
            .as_ref()
            .map(|d| json!({ "readFiles": d.read_files, "modifiedFiles": d.modified_files })),
        from_hook: None,
    });
    let mut new_entries = entries.clone();
    new_entries.push(compaction_entry);
    let reloaded = build_session_context(&new_entries, None);

    assert!(reloaded.messages.len() < loaded.messages.len());
    assert_eq!(
        reloaded.messages[0].get("role").and_then(Value::as_str),
        Some("compactionSummary")
    );
    assert!(reloaded.messages[0]
        .get("summary")
        .and_then(Value::as_str)
        .unwrap()
        .contains(&result.summary));
}

// ===========================================================================
// Error paths for the summarization seam (compact()).
// ===========================================================================

/// Faithful mirror of pi's throw-mapped error behavior: a model error stop
/// reason becomes a `SummarizationFailed` compaction error.
#[test]
fn compact_surfaces_summarization_error() {
    // A non-split-turn preparation with history to summarize: the single model
    // call is the history summary, so an error stop reason maps to the
    // "Summarization failed" label.
    let preparation = CompactionPreparation {
        first_kept_entry_id: "entry-keep".to_string(),
        messages_to_summarize: vec![create_user_message("Summarize this.")],
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

    let models = FauxModels::new();
    models.set_responses(vec![FauxModels::error_response(StopReason::Error, "boom")]);
    let model = create_faux_model(false, 8192);
    let err = compact(&preparation, &models, &model, None, None, None).unwrap_err();
    assert_eq!(err.code, CompactionErrorCode::SummarizationFailed);
    assert_eq!(err.message, "Summarization failed: boom");
}

// ===========================================================================
// serializeConversation (compaction-serialization.test.ts)
// ===========================================================================

fn tool_result(text: &str) -> Value {
    json!({
        "role": "toolResult",
        "toolCallId": "tc1",
        "toolName": "read",
        "content": [{ "type": "text", "text": text }],
        "isError": false,
        "timestamp": 0,
    })
}

/// pi: "should truncate long tool results"
#[test]
fn serialize_conversation_truncates_long_tool_results() {
    let long_content = "x".repeat(5000);
    let result = serialize_conversation(&[tool_result(&long_content)]);
    assert!(result.contains("[Tool result]:"));
    assert!(result.contains("[... 3000 more characters truncated]"));
    assert!(!result.contains(&"x".repeat(3000)));
    assert!(result.contains(&"x".repeat(2000)));
}

/// pi: "should not truncate short tool results"
#[test]
fn serialize_conversation_keeps_short_tool_results() {
    let short_content = "x".repeat(1500);
    let result = serialize_conversation(&[tool_result(&short_content)]);
    assert_eq!(result, format!("[Tool result]: {short_content}"));
    assert!(!result.contains("truncated"));
}

/// pi: "should not truncate assistant or user messages"
#[test]
fn serialize_conversation_never_truncates_user_or_assistant() {
    let long_text = "y".repeat(5000);
    let messages = vec![
        json!({ "role": "user", "content": [{ "type": "text", "text": long_text }], "timestamp": 0 }),
        create_assistant_message(&long_text, create_mock_usage(0, 0, 0, 0)),
    ];
    let result = serialize_conversation(&messages);
    assert!(!result.contains("truncated"));
    assert!(result.contains(&long_text));
}

// ===========================================================================
// Branch summarization.
//
// pi ships no branch-summarization test file, so these are atilla-authored
// coverage of the ported deterministic branch logic and its error paths
// (mirroring how the agent-core reference test covers the same module). They do
// not map to a specific pi assertion.
// ===========================================================================

use atilla_ai::seams::AbortSignal;
use atilla_coding::core::compaction::{
    collect_entries_for_branch_summary, generate_branch_summary, prepare_branch_entries,
    BranchSummaryErrorCode, GenerateBranchSummaryOptions, BRANCH_SUMMARY_PREAMBLE,
};
use atilla_coding::core::session_manager::SessionManager;

fn assistant_with_read(path: &str) -> Value {
    let mut msg = assistant_default("reading");
    msg["content"] =
        json!([{ "type": "toolCall", "id": "t1", "name": "read", "arguments": { "path": path } }]);
    msg
}

#[test]
fn prepare_branch_entries_selects_messages_and_extracts_file_ops() {
    let mut b = Builder::new();
    let u1 = b.message(create_user_message("do work"));
    let a1 = b.message(assistant_with_read("src/a.ts"));
    let tool_result = b.message(json!({
        "role": "toolResult", "toolCallId": "t1", "toolName": "read",
        "content": [{ "type": "text", "text": "contents" }], "isError": false, "timestamp": 0,
    }));

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
    let mut b = Builder::new();
    let u1 = b.message(create_user_message("explore"));
    let a1 = b.message(assistant_with_read("src/x.ts"));
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
    let mut b = Builder::new();
    let u1 = b.message(create_user_message("explore"));
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
    // Build a branching tree via the coding SessionManager:
    //   e0(root) -> e1 -> x   and   e1 -> y
    let mut session = SessionManager::in_memory("/tmp/cwd");
    let _e0 = session.append_message(create_user_message("root"));
    let e1_id = session.append_message(assistant_default("a"));
    let x_id = session.append_message(create_user_message("x"));
    session.branch(&e1_id).unwrap();
    let y_id = session.append_message(create_user_message("y"));

    let result = collect_entries_for_branch_summary(&session, Some(&x_id), &y_id);
    assert_eq!(result.common_ancestor_id.as_deref(), Some(e1_id.as_str()));
    let ids: Vec<String> = result.entries.iter().map(|e| e.id().to_string()).collect();
    assert_eq!(ids, vec![x_id]);

    // With no previous leaf, nothing is collected.
    let empty = collect_entries_for_branch_summary(&session, None, &y_id);
    assert!(empty.entries.is_empty());
    assert_eq!(empty.common_ancestor_id, None);
}
