//! Incremental streaming agent-loop tests.
//!
//! These cover the additive incremental path
//! ([`run_agent_loop_incremental`](crate::agent_loop::run_agent_loop_incremental)
//! with an [`IncrementalStreamFn`](crate::types::IncrementalStreamFn)): the loop
//! DRIVES the provider one event at a time through a sink instead of iterating an
//! already-materialized [`StreamResult`]. Two properties are asserted:
//!
//! - **Live timing**: when the incremental fn sleeps between sink calls (the
//!   sse.rs sleeping-chunk pattern, one sleep per pulled event), the loop emits
//!   the matching UI events with a real inter-event spread of about
//!   `(n - 1) * delay`; the SAME turn driven through the buffered
//!   [`StreamFn`](crate::types::StreamFn) has ~0 spread because every event is
//!   already in the `Vec`.
//! - **Equivalence**: a turn driven incrementally yields the SAME events and
//!   terminal messages as the buffered path — for a plain text turn AND for a
//!   `tool_use` turn that loops back through tool execution.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::*;
use crate::agent_loop::{run_agent_loop_incremental, AgentEventSink};
use crate::types::{IncrementalStreamFn, StreamFn};

// ---------------------------------------------------------------------------
// Incremental fixtures
// ---------------------------------------------------------------------------

/// One provider response: the events to stream plus its terminal message.
type Response = (Vec<AssistantMessageEvent>, AssistantMessage);

/// Build a streamed text response: `Start`, one `TextDelta` per word, then the
/// terminal `Done` carrying the fully-rendered message.
fn streamed_text_response(words: &[&str]) -> Response {
    let full: String = words.concat();
    let final_message = assistant_message(vec![text_block(&full)], StopReason::Stop);

    let mut events = Vec::new();
    events.push(AssistantMessageEvent::Start {
        partial: assistant_message(vec![], StopReason::Stop),
    });
    let mut acc = String::new();
    for word in words {
        acc.push_str(word);
        events.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: (*word).to_string(),
            partial: assistant_message(vec![text_block(&acc)], StopReason::Stop),
        });
    }
    events.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message: final_message.clone(),
    });
    (events, final_message)
}

/// A terminal-only response (the eager `MockAssistantStream` shape): a single
/// `Done` carrying `message`. Used for the `tool_use` turn.
fn terminal_response(message: AssistantMessage) -> Response {
    let event = AssistantMessageEvent::Done {
        reason: message.stop_reason,
        message: message.clone(),
    };
    (vec![event], message)
}

/// A buffered [`StreamFn`] that replays `responses` in order, one per call, with
/// every event already materialized in the returned [`StreamResult`].
fn buffered_stream_fn(responses: Vec<Response>) -> StreamFn {
    let responses = Arc::new(responses);
    let index = Arc::new(AtomicUsize::new(0));
    Arc::new(move |_model, _ctx, _opts, _signal| {
        let i = index.fetch_add(1, Ordering::SeqCst);
        let (events, message) = responses.get(i).cloned().expect("a queued response");
        StreamResult { events, message }
    })
}

/// An [`IncrementalStreamFn`] that replays `responses` in order, sleeping `delay`
/// before pushing each event to the sink (the sse.rs sleeping-chunk timing), and
/// returns the terminal [`StreamResult`] with an empty `events` `Vec` (the events
/// were already delivered through the sink).
fn sleeping_incremental_fn(responses: Vec<Response>, delay: Duration) -> IncrementalStreamFn {
    let responses = Arc::new(responses);
    let index = Arc::new(AtomicUsize::new(0));
    Arc::new(move |_model, _ctx, _opts, _signal, sink| {
        let i = index.fetch_add(1, Ordering::SeqCst);
        let (events, message) = responses.get(i).cloned().expect("a queued response");
        for event in &events {
            std::thread::sleep(delay);
            sink(event);
        }
        StreamResult {
            events: Vec::new(),
            message,
        }
    })
}

/// A shared buffer of `(elapsed, event_type)` samples recorded by a sink.
type StampBuffer = Arc<Mutex<Vec<(Duration, &'static str)>>>;

/// An [`AgentEventSink`] that records `(elapsed, event_type)` for each emitted
/// event, alongside the shared timestamp buffer it writes into.
fn stamping_sink() -> (AgentEventSink, StampBuffer) {
    let stamps: StampBuffer = Arc::new(Mutex::new(Vec::new()));
    let start = Instant::now();
    let recorder = stamps.clone();
    let sink: AgentEventSink = Arc::new(move |event: AgentEvent| {
        recorder
            .lock()
            .unwrap()
            .push((start.elapsed(), event_type(&event)));
    });
    (sink, stamps)
}

/// The spread between the first and last `message_update` timestamps.
fn message_update_spread(stamps: &[(Duration, &'static str)]) -> Duration {
    let updates: Vec<Duration> = stamps
        .iter()
        .filter(|(_, ty)| *ty == "message_update")
        .map(|(t, _)| *t)
        .collect();
    assert!(!updates.is_empty(), "expected at least one message_update");
    *updates.last().unwrap() - *updates.first().unwrap()
}

/// Collect the events + messages of an incremental run (the incremental analog of
/// [`agent_loop`](crate::agent_loop::agent_loop)'s outcome collection).
fn run_incremental_collecting(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    stream_fn: &StreamFn,
    incremental: &IncrementalStreamFn,
) -> AgentLoopOutcome {
    let collected: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(&collected);
    let messages = run_agent_loop_incremental(
        prompts,
        context,
        config,
        &sink,
        None,
        stream_fn,
        Some(incremental),
    );
    let events = collected.lock().unwrap().clone();
    AgentLoopOutcome { events, messages }
}

fn user_context() -> AgentContext {
    AgentContext {
        system_prompt: "You are helpful.".into(),
        messages: vec![],
        tools: Some(vec![]),
    }
}

/// Recursively zero every `timestamp` field. Tool-result messages stamp
/// `Date.now()` (pi's `now_ms`) at execution time, so two sequential runs differ
/// only by wall-clock noise there; normalizing lets the equivalence checks assert
/// on the load-bearing content.
fn zero_timestamps(mut value: Value) -> Value {
    match &mut value {
        Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if key == "timestamp" {
                    *entry = json!(0);
                } else {
                    *entry = zero_timestamps(entry.take());
                }
            }
        }
        Value::Array(items) => {
            for entry in items.iter_mut() {
                *entry = zero_timestamps(entry.take());
            }
        }
        _ => {}
    }
    value
}

/// Serialize the loop's events with timestamps normalized, for cross-path
/// comparison.
fn normalized_events(events: &[AgentEvent]) -> Vec<Value> {
    events
        .iter()
        .map(|event| zero_timestamps(serde_json::to_value(event).expect("event serializes")))
        .collect()
}

/// The loop's produced messages with timestamps normalized.
fn normalized_messages(messages: &[AgentMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| zero_timestamps(message.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// Live timing
// ---------------------------------------------------------------------------

#[test]
fn incremental_path_emits_events_with_inter_event_timing() {
    let n = 5usize;
    let delay = Duration::from_millis(15);
    let words: Vec<String> = (0..n).map(|i| format!("word{i}")).collect();
    let word_refs: Vec<&str> = words.iter().map(String::as_str).collect();
    let response = streamed_text_response(&word_refs);

    let incremental = sleeping_incremental_fn(vec![response.clone()], delay);
    // The buffered fn shares the SAME events; it is present as the loop's
    // fallback but the incremental path drives the run.
    let buffered = buffered_stream_fn(vec![response]);

    let (sink, stamps) = stamping_sink();
    run_agent_loop_incremental(
        vec![user_message("stream please")],
        user_context(),
        base_config(),
        &sink,
        None,
        &buffered,
        Some(&incremental),
    );

    // n text deltas arrive one sleeping chunk apart, so their emitted
    // message_update events span at least (n-1) delays. Use the same lenient
    // half-delay lower bound as the sse.rs pull-timing test.
    let spread = message_update_spread(&stamps.lock().unwrap());
    let lower_bound = delay.mul_f64((n as f64 - 1.0) * 0.5);
    assert!(
        spread >= lower_bound,
        "expected incremental message_update spread >= {lower_bound:?}, got {spread:?}",
    );
}

#[test]
fn buffered_path_emits_events_without_inter_event_timing() {
    let n = 5usize;
    let words: Vec<String> = (0..n).map(|i| format!("word{i}")).collect();
    let word_refs: Vec<&str> = words.iter().map(String::as_str).collect();
    let (events, message) = streamed_text_response(&word_refs);
    let buffered = buffered_stream_fn(vec![(events, message)]);

    let (sink, stamps) = stamping_sink();
    // No incremental fn: the loop uses the buffered path, iterating an
    // already-materialized event Vec with ~0 inter-event spread.
    run_agent_loop_incremental(
        vec![user_message("stream please")],
        user_context(),
        base_config(),
        &sink,
        None,
        &buffered,
        None,
    );

    let spread = message_update_spread(&stamps.lock().unwrap());
    assert!(
        spread < Duration::from_millis(5),
        "expected buffered message_update spread ~0, got {spread:?}",
    );
}

// ---------------------------------------------------------------------------
// Equivalence
// ---------------------------------------------------------------------------

#[test]
fn incremental_text_turn_matches_buffered() {
    let response = streamed_text_response(&["Hello", ", ", "world", "!"]);
    let buffered = buffered_stream_fn(vec![response.clone()]);
    let incremental = sleeping_incremental_fn(vec![response], Duration::ZERO);

    let buffered_outcome = agent_loop(
        vec![user_message("hi")],
        user_context(),
        base_config(),
        None,
        &buffered,
    );
    let incremental_outcome = run_incremental_collecting(
        vec![user_message("hi")],
        user_context(),
        base_config(),
        &buffered,
        &incremental,
    );

    assert_eq!(
        normalized_events(&buffered_outcome.events),
        normalized_events(&incremental_outcome.events)
    );
    assert_eq!(
        normalized_messages(&buffered_outcome.messages),
        normalized_messages(&incremental_outcome.messages)
    );
}

#[test]
fn incremental_tool_use_turn_matches_buffered() {
    // Turn 1: a `tool_use` response calling echo; turn 2: a streamed text reply
    // after the tool result feeds back. The tool loop must work through the
    // incremental path, so both fixtures replay the SAME two responses.
    let tool_use = assistant_message(
        vec![tool_call_block(
            "echo",
            "call-1",
            json!({ "value": "ping" }),
        )],
        StopReason::ToolUse,
    );
    let responses = vec![
        terminal_response(tool_use),
        streamed_text_response(&["done", "!"]),
    ];

    let buffered = buffered_stream_fn(responses.clone());
    let incremental = sleeping_incremental_fn(responses, Duration::ZERO);

    let executed_buffered = Arc::new(Mutex::new(Vec::new()));
    let buffered_context = AgentContext {
        system_prompt: "You are helpful.".into(),
        messages: vec![],
        tools: Some(vec![echo_tool(executed_buffered.clone())]),
    };
    let buffered_outcome = agent_loop(
        vec![user_message("use the tool")],
        buffered_context,
        base_config(),
        None,
        &buffered,
    );

    let executed_incremental = Arc::new(Mutex::new(Vec::new()));
    let incremental_context = AgentContext {
        system_prompt: "You are helpful.".into(),
        messages: vec![],
        tools: Some(vec![echo_tool(executed_incremental.clone())]),
    };
    let incremental_outcome = run_incremental_collecting(
        vec![user_message("use the tool")],
        incremental_context,
        base_config(),
        &buffered,
        &incremental,
    );

    // The tool executed once on each path.
    assert_eq!(*executed_buffered.lock().unwrap(), vec!["ping".to_string()]);
    assert_eq!(
        *executed_incremental.lock().unwrap(),
        vec!["ping".to_string()]
    );
    // Identical events and messages across the two paths.
    assert_eq!(
        normalized_events(&buffered_outcome.events),
        normalized_events(&incremental_outcome.events)
    );
    assert_eq!(
        normalized_messages(&buffered_outcome.messages),
        normalized_messages(&incremental_outcome.messages)
    );
}
