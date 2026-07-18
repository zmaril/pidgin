//! Steering agent-loop tests — `shouldStop` / batch-termination / `afterToolCall`
//! control-flow cases. Split out of the parent `tests` module to keep each file
//! under the line-count ceiling; shares the parent module's helpers via
//! `use super::*`.

// straitjacket-allow-file:duplication — like the sibling cases, each `#[test]`
// rebuilds near-identical contexts, mock streams, and config from the shared
// helpers and asserts on the same event/message shapes by design; the clone
// detector reads these parallel ported cases as duplicates.

use super::*;

// ---------------------------------------------------------------------------
// Direct ports — steering hooks (shouldStop / afterToolCall)
// ---------------------------------------------------------------------------

#[test]
fn should_stop_after_the_current_turn_when_should_stop_returns_true() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![echo_tool(executed.clone())]),
    };

    let steering_polls = Arc::new(AtomicUsize::new(0));
    let follow_up_polls = Arc::new(AtomicUsize::new(0));
    let callback_tool_result_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let callback_context_roles: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let mut config = base_config();
    let sp = steering_polls.clone();
    config.get_steering_messages = Some(Arc::new(move || {
        sp.fetch_add(1, Ordering::SeqCst);
        vec![]
    }));
    let fp = follow_up_polls.clone();
    config.get_follow_up_messages = Some(Arc::new(move || {
        fp.fetch_add(1, Ordering::SeqCst);
        vec![user_message("follow up should stay queued")]
    }));
    let ids = callback_tool_result_ids.clone();
    let roles = callback_context_roles.clone();
    config.should_stop_after_turn = Some(Arc::new(move |ctx: &ShouldStopAfterTurnContext| {
        assert_eq!(ctx.message.role, atilla_ai::AssistantRole::Assistant);
        *ids.lock().unwrap() = ctx
            .tool_results
            .iter()
            .map(|t| t.tool_call_id.clone())
            .collect();
        *roles.lock().unwrap() = ctx
            .context
            .messages
            .iter()
            .filter_map(|m| role_of(m).map(str::to_string))
            .collect();
        true
    }));

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let lc = llm_calls.clone();
    let stream_fn: StreamFn = Arc::new(move |_model, _ctx, _opts, _signal| {
        let n = lc.fetch_add(1, Ordering::SeqCst) + 1;
        let message = if n == 1 {
            assistant_message(
                vec![tool_call_block(
                    "echo",
                    "tool-1",
                    json!({ "value": "hello" }),
                )],
                StopReason::ToolUse,
            )
        } else {
            assistant_message(vec![text_block("should not run")], StopReason::Stop)
        };
        mock_stream(message)
    });

    let outcome = agent_loop(
        vec![user_message("echo something")],
        context,
        config,
        None,
        &stream_fn,
    );

    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(*executed.lock().unwrap(), vec!["hello".to_string()]);
    assert_eq!(steering_polls.load(Ordering::SeqCst), 1);
    assert_eq!(follow_up_polls.load(Ordering::SeqCst), 0);
    assert_eq!(
        *callback_tool_result_ids.lock().unwrap(),
        vec!["tool-1".to_string()]
    );
    assert_eq!(
        *callback_context_roles.lock().unwrap(),
        vec!["user", "assistant", "toolResult"]
    );
    let roles: Vec<&str> = outcome.messages.iter().filter_map(role_of).collect();
    assert_eq!(roles, vec!["user", "assistant", "toolResult"]);

    let types: Vec<&str> = outcome.events.iter().map(event_type).collect();
    assert_eq!(
        types,
        vec![
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "tool_execution_start",
            "tool_execution_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
}

#[test]
fn should_stop_after_batch_when_every_result_terminates() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let tool = echo_tool_with(executed, None, Some(Arc::new(|_v| true)));
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![tool]),
    };

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let lc = llm_calls.clone();
    let stream_fn: StreamFn = Arc::new(move |_model, _ctx, _opts, _signal| {
        lc.fetch_add(1, Ordering::SeqCst);
        mock_stream(assistant_message(
            vec![tool_call_block(
                "echo",
                "tool-1",
                json!({ "value": "hello" }),
            )],
            StopReason::ToolUse,
        ))
    });

    let outcome = agent_loop(
        vec![user_message("echo something")],
        context,
        base_config(),
        None,
        &stream_fn,
    );

    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    let roles: Vec<&str> = outcome.messages.iter().filter_map(role_of).collect();
    assert_eq!(roles, vec!["user", "assistant", "toolResult"]);
    let turn_ends = outcome
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnEnd { .. }))
        .count();
    assert_eq!(turn_ends, 1);
}

#[test]
fn should_continue_after_parallel_when_not_all_results_terminate() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let tool = echo_tool_with(executed, None, Some(Arc::new(|v| v == "first")));
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![tool]),
    };

    let mut config = base_config();
    config.tool_execution = Some(ToolExecutionMode::Parallel);

    let call_index = Arc::new(AtomicUsize::new(0));
    let ci = call_index.clone();
    let stream_fn: StreamFn = Arc::new(move |_model, _ctx, _opts, _signal| {
        let i = ci.fetch_add(1, Ordering::SeqCst);
        let message = if i == 0 {
            assistant_message(
                vec![
                    tool_call_block("echo", "tool-1", json!({ "value": "first" })),
                    tool_call_block("echo", "tool-2", json!({ "value": "second" })),
                ],
                StopReason::ToolUse,
            )
        } else {
            assistant_message(vec![text_block("done")], StopReason::Stop)
        };
        mock_stream(message)
    });

    let outcome = agent_loop(
        vec![user_message("echo both")],
        context,
        config,
        None,
        &stream_fn,
    );

    assert_eq!(call_index.load(Ordering::SeqCst), 2);
    let roles: Vec<&str> = outcome.messages.iter().filter_map(role_of).collect();
    assert_eq!(
        roles,
        vec!["user", "assistant", "toolResult", "toolResult", "assistant"]
    );
}

#[test]
fn should_allow_after_tool_call_to_mark_batch_terminating() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let context = AgentContext {
        system_prompt: "".into(),
        messages: vec![],
        tools: Some(vec![echo_tool(executed)]),
    };

    let mut config = base_config();
    config.after_tool_call = Some(Arc::new(|_ctx: &AfterToolCallContext, _signal| {
        Some(AfterToolCallResult {
            terminate: Some(true),
            ..Default::default()
        })
    }));

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let lc = llm_calls.clone();
    let stream_fn: StreamFn = Arc::new(move |_model, _ctx, _opts, _signal| {
        lc.fetch_add(1, Ordering::SeqCst);
        mock_stream(assistant_message(
            vec![tool_call_block(
                "echo",
                "tool-1",
                json!({ "value": "hello" }),
            )],
            StopReason::ToolUse,
        ))
    });

    agent_loop(
        vec![user_message("echo something")],
        context,
        config,
        None,
        &stream_fn,
    );

    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
}
