//! Supplementary agent-loop tests — abort branches and follow-up restarts that
//! the TS suite under-exercises. Split out of the parent `tests` module to keep
//! each file under the line-count ceiling; shares the parent module's helpers
//! via `use super::*`.

// straitjacket-allow-file:duplication — like the sibling cases, each `#[test]`
// rebuilds near-identical contexts, mock streams, and config from the shared
// helpers and asserts on the same event/message shapes by design; the clone
// detector reads these parallel ported cases as duplicates.

use super::*;

#[test]
fn supplementary_aborted_turn_ends_the_agent_immediately() {
    // An assistant turn that resolves as `aborted` short-circuits the loop: a
    // `turn_end` with no tool results, then `agent_end`, and no further turns.
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![]),
    };
    let stream_fn = stream_fn_from(vec![assistant_message(vec![], StopReason::Aborted)]);

    let outcome = agent_loop(
        vec![user_message("go")],
        context,
        base_config(),
        None,
        &stream_fn,
    );

    let types: Vec<&str> = outcome.events.iter().map(event_type).collect();
    assert_eq!(types.last(), Some(&"agent_end"));
    // Exactly one turn_end, carrying no tool results.
    let turn_ends: Vec<&AgentEvent> = outcome
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(turn_ends.len(), 1);
    match turn_ends[0] {
        AgentEvent::TurnEnd { tool_results, .. } => assert!(tool_results.is_empty()),
        _ => unreachable!(),
    }
    assert!(!outcome
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolExecutionStart { .. })));
}

#[test]
fn supplementary_error_turn_ends_the_agent_immediately() {
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![]),
    };
    let stream_fn = stream_fn_from(vec![assistant_message(vec![], StopReason::Error)]);

    let outcome = agent_loop(
        vec![user_message("go")],
        context,
        base_config(),
        None,
        &stream_fn,
    );

    let types: Vec<&str> = outcome.events.iter().map(event_type).collect();
    assert_eq!(types.last(), Some(&"agent_end"));
    assert_eq!(
        outcome
            .events
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnEnd { .. }))
            .count(),
        1
    );
}

#[test]
fn supplementary_before_tool_call_abort_blocks_execution() {
    // A `beforeToolCall` hook that trips the signal aborts the call before it runs:
    // the tool never executes and the result is the "Operation aborted" error.
    let executed = Arc::new(Mutex::new(Vec::new()));
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![echo_tool(executed.clone())]),
    };

    let mut config = base_config();
    config.before_tool_call = Some(Arc::new(|_ctx: &BeforeToolCallContext, signal| {
        if let Some(signal) = signal {
            signal.abort();
        }
        None
    }));

    let signal = AbortSignal::new();
    let stream_fn = stream_fn_from(vec![
        assistant_message(
            vec![tool_call_block(
                "echo",
                "tool-1",
                json!({ "value": "hello" }),
            )],
            StopReason::ToolUse,
        ),
        assistant_message(vec![text_block("done")], StopReason::Stop),
    ]);

    let outcome = agent_loop(
        vec![user_message("echo something")],
        context,
        config,
        Some(&signal),
        &stream_fn,
    );

    assert!(executed.lock().unwrap().is_empty());
    let end = outcome
        .events
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolExecutionEnd { .. }));
    match end {
        Some(AgentEvent::ToolExecutionEnd {
            is_error, result, ..
        }) => {
            assert!(is_error);
            let text = result["content"][0]["text"].as_str().unwrap_or_default();
            assert!(text.contains("Operation aborted"), "got: {text}");
        }
        _ => panic!("expected tool_execution_end"),
    }
}

#[test]
fn supplementary_before_tool_call_block_prevents_execution() {
    // A `beforeToolCall` result with `block: true` produces the blocked-error result
    // and the tool never runs.
    let executed = Arc::new(Mutex::new(Vec::new()));
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![echo_tool(executed.clone())]),
    };

    let mut config = base_config();
    config.before_tool_call = Some(Arc::new(|_ctx: &BeforeToolCallContext, _signal| {
        Some(BeforeToolCallResult {
            block: Some(true),
            reason: Some("nope".into()),
        })
    }));

    let stream_fn = stream_fn_from(vec![
        assistant_message(
            vec![tool_call_block(
                "echo",
                "tool-1",
                json!({ "value": "hello" }),
            )],
            StopReason::ToolUse,
        ),
        assistant_message(vec![text_block("done")], StopReason::Stop),
    ]);

    let outcome = agent_loop(
        vec![user_message("echo something")],
        context,
        config,
        None,
        &stream_fn,
    );

    assert!(executed.lock().unwrap().is_empty());
    let end = outcome
        .events
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolExecutionEnd { .. }));
    match end {
        Some(AgentEvent::ToolExecutionEnd {
            is_error, result, ..
        }) => {
            assert!(is_error);
            let text = result["content"][0]["text"].as_str().unwrap_or_default();
            assert_eq!(text, "nope");
        }
        _ => panic!("expected tool_execution_end"),
    }
}

#[test]
fn supplementary_missing_tool_yields_not_found_error() {
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![]),
    };
    let stream_fn = stream_fn_from(vec![
        assistant_message(
            vec![tool_call_block("ghost", "tool-1", json!({}))],
            StopReason::ToolUse,
        ),
        assistant_message(vec![text_block("done")], StopReason::Stop),
    ]);

    let outcome = agent_loop(
        vec![user_message("call ghost")],
        context,
        base_config(),
        None,
        &stream_fn,
    );

    let end = outcome
        .events
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolExecutionEnd { .. }));
    match end {
        Some(AgentEvent::ToolExecutionEnd {
            is_error, result, ..
        }) => {
            assert!(is_error);
            let text = result["content"][0]["text"].as_str().unwrap_or_default();
            assert_eq!(text, "Tool ghost not found");
        }
        _ => panic!("expected tool_execution_end"),
    }
}

#[test]
fn supplementary_follow_up_messages_restart_the_inner_loop() {
    // When the agent would stop, a follow-up message re-enters the inner loop for
    // another turn. A one-shot follow-up produces a second assistant turn.
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![]),
    };

    let delivered = Arc::new(Mutex::new(false));
    let mut config = base_config();
    let d = delivered.clone();
    let follow_up: GetFollowUpMessages = Arc::new(move || {
        if *d.lock().unwrap() {
            vec![]
        } else {
            *d.lock().unwrap() = true;
            vec![user_message("again")]
        }
    });
    config.get_follow_up_messages = Some(follow_up);

    let stream_fn = stream_fn_from(vec![
        assistant_message(vec![text_block("first")], StopReason::Stop),
        assistant_message(vec![text_block("second")], StopReason::Stop),
    ]);

    let outcome = agent_loop(vec![user_message("hi")], context, config, None, &stream_fn);

    let roles: Vec<&str> = outcome.messages.iter().filter_map(role_of).collect();
    assert_eq!(roles, vec!["user", "assistant", "user", "assistant"]);
}
