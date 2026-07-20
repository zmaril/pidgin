//! Session-stats / context-usage tests, ported from pi's
//! `test/agent-session-stats.test.ts` (`AgentSession.getSessionStats`, 4 cases).
//!
//! pi builds a real `AgentSession` over an in-memory session manager, appends
//! messages with controlled token usages directly to the manager, syncs the
//! agent's live messages to the rebuilt session context, and asserts the reported
//! stats / context usage. The Rust port rebuilds the same fixtures over the
//! in-memory harness in [`super::super::test_support`]: the harness's `faux-1`
//! model stands in for pi's `claude-sonnet-4-5` (only its context window matters),
//! and the token-usage assertions are preserved verbatim.

// The three post-compaction cases share the same fixture-building preamble (append
// the same first/second/response sequence, then diverge on the tail), so they read
// as clones of one another. Keeping each case self-contained mirrors pi's suite,
// where every `it(...)` rebuilds the session from scratch.
// straitjacket-allow-file:duplication

use serde_json::{json, Value};

use super::super::test_support::{
    assistant_text, create_harness, FauxResponse, Harness, HarnessOptions,
};

/// pi's `createUsage(totalTokens)`: a usage where `input == totalTokens` and every
/// other component (and the cost) is zero.
fn create_usage(total_tokens: u64) -> Value {
    json!({
        "input": total_tokens,
        "output": 0,
        "cacheRead": 0,
        "cacheWrite": 0,
        "totalTokens": total_tokens,
        "cost": {
            "input": 0,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "total": 0,
        },
    })
}

/// pi's `createAssistantMessage(text, totalTokens, timestamp)`.
fn create_assistant_message(
    harness: &Harness,
    text: &str,
    total_tokens: u64,
    timestamp: i64,
) -> Value {
    let model = harness.default_model();
    json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": text }],
        "api": model.api,
        "provider": model.provider,
        "model": model.id,
        "usage": create_usage(total_tokens),
        "stopReason": "stop",
        "timestamp": timestamp,
    })
}

/// pi's `createUserMessage(text, timestamp)`.
fn create_user_message(text: &str, timestamp: i64) -> Value {
    json!({
        "role": "user",
        "content": text,
        "timestamp": timestamp,
    })
}

/// pi's `syncAgentMessages`: mirror the agent's live messages onto the session
/// context rebuilt from the manager's branch.
fn sync_agent_messages(harness: &Harness) {
    let messages = harness
        .session
        .session_manager()
        .build_session_context()
        .messages;
    harness.session.agent.set_messages(messages);
}

/// The harness context window (pi's `model.contextWindow`).
fn context_window(harness: &Harness) -> i64 {
    harness.default_model().context_window as i64
}

#[test]
fn exposes_current_context_usage_alongside_token_totals() {
    let harness = create_harness(HarnessOptions::default());

    harness
        .session
        .session_manager()
        .append_message(create_user_message("hello", 1));
    harness
        .session
        .session_manager()
        .append_message(create_assistant_message(&harness, "hi", 200, 2));
    sync_agent_messages(&harness);

    let stats = harness.session.get_session_stats();
    assert_eq!(stats.context_usage, harness.session.get_context_usage());
    let usage = stats.context_usage.expect("context usage is defined");
    assert_eq!(usage.tokens, Some(200));
    assert_eq!(usage.context_window, context_window(&harness));
    let expected_percent = (200.0_f64 / context_window(&harness) as f64) * 100.0;
    assert_eq!(usage.percent, Some(expected_percent));
}

#[test]
fn reports_unknown_current_context_usage_immediately_after_compaction() {
    let harness = create_harness(HarnessOptions::default());

    harness
        .session
        .session_manager()
        .append_message(create_user_message("first", 1));
    harness
        .session
        .session_manager()
        .append_message(create_assistant_message(&harness, "response1", 180_000, 2));
    let kept_user_id = harness
        .session
        .session_manager()
        .append_message(create_user_message("second", 3));
    harness
        .session
        .session_manager()
        .append_message(create_assistant_message(&harness, "response2", 195_000, 4));
    harness.session.session_manager().append_compaction(
        "summary",
        &kept_user_id,
        195_000,
        None,
        None,
    );
    harness
        .session
        .session_manager()
        .append_message(create_user_message("third", 5));
    sync_agent_messages(&harness);

    let stats = harness.session.get_session_stats();
    // Totals cover ALL entries, including history compacted away (180k + 195k).
    assert_eq!(stats.tokens.input, 375_000);
    let usage = stats.context_usage.expect("context usage is defined");
    assert_eq!(usage.tokens, None);
    assert_eq!(usage.percent, None);
}

#[test]
fn uses_post_compaction_usage_for_current_context_instead_of_stale_kept_usage() {
    let harness = create_harness(HarnessOptions::default());

    harness
        .session
        .session_manager()
        .append_message(create_user_message("first", 1));
    harness
        .session
        .session_manager()
        .append_message(create_assistant_message(&harness, "response1", 180_000, 2));
    let kept_user_id = harness
        .session
        .session_manager()
        .append_message(create_user_message("second", 3));
    harness
        .session
        .session_manager()
        .append_message(create_assistant_message(&harness, "response2", 195_000, 4));
    harness.session.session_manager().append_compaction(
        "summary",
        &kept_user_id,
        195_000,
        None,
        None,
    );
    harness
        .session
        .session_manager()
        .append_message(create_user_message("third", 5));
    harness
        .session
        .session_manager()
        .append_message(create_assistant_message(&harness, "response3", 25_000, 6));
    sync_agent_messages(&harness);

    let stats = harness.session.get_session_stats();
    // Totals cover ALL entries, including history compacted away (180k + 195k + 25k).
    assert_eq!(stats.tokens.input, 400_000);
    let usage = stats.context_usage.expect("context usage is defined");
    assert_eq!(usage.tokens, Some(25_000));
    let expected_percent = (25_000.0_f64 / context_window(&harness) as f64) * 100.0;
    assert_eq!(usage.percent, Some(expected_percent));
}

#[test]
fn ignores_zero_usage_messages_when_checking_for_post_compaction_context_usage() {
    let harness = create_harness(HarnessOptions::default());

    harness
        .session
        .session_manager()
        .append_message(create_user_message("first", 1));
    harness
        .session
        .session_manager()
        .append_message(create_assistant_message(&harness, "response1", 180_000, 2));
    let kept_user_id = harness
        .session
        .session_manager()
        .append_message(create_user_message("second", 3));
    harness
        .session
        .session_manager()
        .append_message(create_assistant_message(&harness, "response2", 195_000, 4));
    harness.session.session_manager().append_compaction(
        "summary",
        &kept_user_id,
        195_000,
        None,
        None,
    );
    harness
        .session
        .session_manager()
        .append_message(create_user_message("third", 5));
    harness
        .session
        .session_manager()
        .append_message(create_assistant_message(&harness, "response3", 25_000, 6));
    harness
        .session
        .session_manager()
        .append_message(create_user_message("continue", 7));
    harness
        .session
        .session_manager()
        .append_message(create_assistant_message(&harness, "partial", 0, 8));
    sync_agent_messages(&harness);

    let stats = harness.session.get_session_stats();
    let usage = stats.context_usage.expect("context usage is defined");
    assert!(usage.tokens.is_some());
    assert!(usage.tokens.unwrap_or(0) > 25_000);
}

/// Ported from `test/rpc.test.ts` ("should get session stats"): after a driven
/// turn the reported session id is present and the user/assistant message counts
/// are each at least one. (The RPC test also asserts `sessionFile` is defined; the
/// in-memory harness has no session file, so that field is exercised by the
/// export suite's persisted-session fixtures instead.)
#[test]
fn driven_turn_counts_user_and_assistant_messages() {
    let harness = create_harness(HarnessOptions::default());
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_text("Hi")))]);

    harness
        .session
        .prompt("Hello", None, None)
        .expect("turn runs");

    let stats = harness.session.get_session_stats();
    assert!(!stats.session_id.is_empty());
    assert!(stats.user_messages >= 1);
    assert!(stats.assistant_messages >= 1);
}
