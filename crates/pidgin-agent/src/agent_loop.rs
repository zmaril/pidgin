//! The agent loop, ported from `packages/agent/src/agent-loop.ts`.
//!
//! pi's agent loop works with `AgentMessage[]` throughout and transforms to
//! `Message[]` only at the LLM-call boundary. It streams an assistant response,
//! collects the tool calls from that message, orchestrates their execution
//! (sequential or parallel), injects steering/follow-up messages, and emits a
//! stream of [`AgentEvent`]s describing the run.
//!
//! # Streaming adaptation (eager / synchronous)
//!
//! Per the crate convention (see [`crate::types`]), pidgin is synchronous and
//! eager: there is no `tokio`, no async-iterable event stream, and no
//! `Promise`. Every pi `await` becomes a synchronous call. Concretely:
//!
//! - pi's `agentLoop`/`agentLoopContinue` return an `EventStream<AgentEvent,
//!   AgentMessage[]>` that a caller iterates for events and then `await`s for the
//!   final messages. The eager analogs [`agent_loop`]/[`agent_loop_continue`] run
//!   the whole loop to completion and return an [`AgentLoopOutcome`] bundling the
//!   collected `events` and the final `messages` — the same two things a caller
//!   pulls out of pi's `EventStream`.
//! - pi's `runAgentLoop`/`runAgentLoopContinue` take an `emit` sink and return the
//!   messages. The eager [`run_agent_loop`]/[`run_agent_loop_continue`] keep that
//!   shape: an [`AgentEventSink`] plus a `Vec<AgentMessage>` return.
//! - [`StreamFn`] returns an eager [`StreamResult`] (`{ events, message }`); the
//!   loop iterates `events` where pi does `for await (const event of response)`
//!   and reads `message` where pi does `await response.result()`.
//! - Every hook (`convertToLlm`, `beforeToolCall`, …) is a synchronous closure.
//! - Tool calls execute inline on the loop's thread. pi's `Promise.all` over the
//!   parallel-batch closures becomes an in-order synchronous drain; because the
//!   eager model has no real concurrency, the "parallel" path differs from the
//!   "sequential" path only in **event ordering** (it emits all
//!   `tool_execution_start`s, then all `tool_execution_end`s, then all result
//!   messages — see [`execute_tool_calls_parallel`]), not in wall-clock
//!   interleaving. The load-bearing golden — that tool-result messages are
//!   appended in **source order** regardless of completion order — is preserved
//!   exactly, matching pi's `Promise.all` (which resolves in array order).
//!
//! # Where pi throws
//!
//! `agentLoopContinue`/`runAgentLoopContinue` throw synchronously for an empty or
//! assistant-tailed context; the port returns [`AgentLoopError`] instead. pi's
//! `try/catch` around `validateToolArguments`, `tool.execute`, and `afterToolCall`
//! guards against thrown JS errors; pidgin's tool/hook closures return values
//! (not `Result`), so those catch arms are structurally preserved but cannot be
//! reached from a well-typed Rust closure (noted at each site).
//!
//! Source of truth: `vendor/pi/packages/agent/src/agent-loop.ts`.

// straitjacket-allow-file:duplication — this module is a faithful, line-by-line
// transcription of pi's `agent-loop.ts`; its sequential and parallel executors
// share the prepare/execute/finalize/emit shape by design (pi factors them the
// same way), and the `#[cfg(test)]` scenarios mirror pi's ~20 parametric cases,
// which repeat near-identical tool/stream scaffolding per case. The clone
// detector reads these deliberate mirrors as duplication.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use pidgin_ai::seams::clock::{Clock, SystemClock};
use pidgin_ai::seams::provider::AbortSignal;
use pidgin_ai::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, Message, StopReason,
    ToolResultMessage, ToolResultRole,
};

use crate::types::{
    AfterToolCallContext, AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentTool,
    AgentToolCall, AgentToolResult, AgentToolUpdateCallback, BeforeToolCallContext,
    IncrementalStreamFn, PrepareNextTurnContext, ShouldStopAfterTurnContext, StreamFn,
    ThinkingLevel, ToolCallType, ToolExecutionMode,
};

/// Consumer of [`AgentEvent`]s emitted by the loop — the eager analog of pi's
/// `AgentEventSink` (`agent-loop.ts:25`).
///
/// pi types it as `(event: AgentEvent) => Promise<void> | void`; the eager port
/// drops the `Promise` and keeps a plain synchronous sink. It is a thread-safe
/// `Arc` so the tool-update callback (which may be invoked from a tool's own
/// thread) can clone and emit through it.
pub type AgentEventSink = Arc<dyn Fn(AgentEvent) + Send + Sync>;

/// The result of a completed loop run — the eager analog of what a caller pulls
/// out of pi's `EventStream<AgentEvent, AgentMessage[]>`.
///
/// `events` is the ordered event sequence a caller would get by iterating the
/// stream; `messages` is what `stream.result()` resolves to (the `agent_end`
/// event's `messages`).
#[derive(Debug, Clone, PartialEq)]
pub struct AgentLoopOutcome {
    /// The ordered `agent_start … agent_end` event sequence.
    pub events: Vec<AgentEvent>,
    /// The messages produced by this run (prompts, assistant turns, tool results).
    pub messages: Vec<AgentMessage>,
}

/// The synchronous errors pi's continue-variants throw (`agent-loop.ts:71`,
/// `agent-loop.ts:75`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLoopError {
    /// `throw new Error("Cannot continue: no messages in context")`.
    NoMessages,
    /// `throw new Error("Cannot continue from message role: assistant")`.
    ContinueFromAssistant,
}

impl std::fmt::Display for AgentLoopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentLoopError::NoMessages => f.write_str("Cannot continue: no messages in context"),
            AgentLoopError::ContinueFromAssistant => {
                f.write_str("Cannot continue from message role: assistant")
            }
        }
    }
}

impl std::error::Error for AgentLoopError {}

// ---------------------------------------------------------------------------
// Public entry points (`agent-loop.ts:31-143`)
// ---------------------------------------------------------------------------

/// Start an agent loop with new prompt messages (pi's `agentLoop`,
/// `agent-loop.ts:31`).
///
/// The prompts are added to the context and events are emitted for them. Runs the
/// loop to completion, collecting every event and the final messages into an
/// [`AgentLoopOutcome`].
pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<&AbortSignal>,
    stream_fn: &StreamFn,
) -> AgentLoopOutcome {
    let collected: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(&collected);
    let messages = run_agent_loop(prompts, context, config, &sink, signal, stream_fn);
    let events = Arc::try_unwrap(collected)
        .map(|m| m.into_inner().unwrap())
        .unwrap_or_else(|arc| arc.lock().unwrap().clone());
    AgentLoopOutcome { events, messages }
}

/// Continue an agent loop from the current context without adding a new message
/// (pi's `agentLoopContinue`, `agent-loop.ts:64`).
///
/// Used for retries — the context already ends with a `user` or `toolResult`
/// message. Returns [`AgentLoopError`] where pi throws synchronously.
pub fn agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<&AbortSignal>,
    stream_fn: &StreamFn,
) -> Result<AgentLoopOutcome, AgentLoopError> {
    validate_continue(&context)?;
    let collected: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(&collected);
    let messages = run_agent_loop_continue(context, config, &sink, signal, stream_fn)?;
    let events = Arc::try_unwrap(collected)
        .map(|m| m.into_inner().unwrap())
        .unwrap_or_else(|arc| arc.lock().unwrap().clone());
    Ok(AgentLoopOutcome { events, messages })
}

/// Run an agent loop with new prompts, emitting through `emit` (pi's
/// `runAgentLoop`, `agent-loop.ts:95`).
pub fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    emit: &AgentEventSink,
    signal: Option<&AbortSignal>,
    stream_fn: &StreamFn,
) -> Vec<AgentMessage> {
    run_agent_loop_impl(prompts, context, config, emit, signal, stream_fn, None)
}

/// Incremental variant of [`run_agent_loop`]: identical to it, but when
/// `incremental_stream_fn` is `Some`, each turn DRIVES the provider one event at
/// a time through that closure (real inter-event timing) instead of iterating an
/// already-materialized [`StreamResult`]. Passing `None` is byte-identical to
/// [`run_agent_loop`]. Additive — existing callers keep using [`run_agent_loop`].
pub fn run_agent_loop_incremental(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    emit: &AgentEventSink,
    signal: Option<&AbortSignal>,
    stream_fn: &StreamFn,
    incremental_stream_fn: Option<&IncrementalStreamFn>,
) -> Vec<AgentMessage> {
    run_agent_loop_impl(
        prompts,
        context,
        config,
        emit,
        signal,
        stream_fn,
        incremental_stream_fn,
    )
}

/// Shared body of [`run_agent_loop`] and [`run_agent_loop_incremental`]. The
/// only difference is the optional incremental stream fn threaded into
/// [`run_loop`].
fn run_agent_loop_impl(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    emit: &AgentEventSink,
    signal: Option<&AbortSignal>,
    stream_fn: &StreamFn,
    incremental_stream_fn: Option<&IncrementalStreamFn>,
) -> Vec<AgentMessage> {
    // const newMessages = [...prompts];
    let mut new_messages: Vec<AgentMessage> = prompts.clone();
    // const currentContext = { ...context, messages: [...context.messages, ...prompts] };
    let mut current_context = context;
    current_context.messages.extend(prompts.iter().cloned());

    dispatch(emit, AgentEvent::AgentStart);
    dispatch(emit, AgentEvent::TurnStart);
    for prompt in &prompts {
        dispatch(
            emit,
            AgentEvent::MessageStart {
                message: prompt.clone(),
            },
        );
        dispatch(
            emit,
            AgentEvent::MessageEnd {
                message: prompt.clone(),
            },
        );
    }

    run_loop(
        current_context,
        &mut new_messages,
        config,
        signal,
        emit,
        stream_fn,
        incremental_stream_fn,
    );
    new_messages
}

/// Continue an agent loop from the current context, emitting through `emit`
/// (pi's `runAgentLoopContinue`, `agent-loop.ts:120`).
pub fn run_agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    emit: &AgentEventSink,
    signal: Option<&AbortSignal>,
    stream_fn: &StreamFn,
) -> Result<Vec<AgentMessage>, AgentLoopError> {
    validate_continue(&context)?;

    // const newMessages: AgentMessage[] = [];
    let mut new_messages: Vec<AgentMessage> = Vec::new();
    // const currentContext = { ...context };
    let current_context = context;

    dispatch(emit, AgentEvent::AgentStart);
    dispatch(emit, AgentEvent::TurnStart);

    run_loop(
        current_context,
        &mut new_messages,
        config,
        signal,
        emit,
        stream_fn,
        None,
    );
    Ok(new_messages)
}

/// The two guards pi's continue-variants share (`agent-loop.ts:70-76`,
/// `agent-loop.ts:127-133`).
fn validate_continue(context: &AgentContext) -> Result<(), AgentLoopError> {
    if context.messages.is_empty() {
        return Err(AgentLoopError::NoMessages);
    }
    if message_role(context.messages.last().unwrap()) == Some("assistant") {
        return Err(AgentLoopError::ContinueFromAssistant);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Main loop (`agent-loop.ts:155-275`)
// ---------------------------------------------------------------------------

/// Main loop logic shared by [`run_agent_loop`] and [`run_agent_loop_continue`]
/// (pi's `runLoop`, `agent-loop.ts:155`).
fn run_loop(
    initial_context: AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    initial_config: AgentLoopConfig,
    signal: Option<&AbortSignal>,
    emit: &AgentEventSink,
    stream_fn: &StreamFn,
    incremental_stream_fn: Option<&IncrementalStreamFn>,
) {
    let mut current_context = initial_context;
    let mut config = initial_config;
    let mut first_turn = true;
    // Check for steering messages at start (user may have typed while waiting).
    let mut pending_messages: Vec<AgentMessage> = get_steering(&config);

    // Outer loop: continues when queued follow-up messages arrive after the agent
    // would stop.
    loop {
        let mut has_more_tool_calls = true;

        // Inner loop: process tool calls and steering messages.
        while has_more_tool_calls || !pending_messages.is_empty() {
            if !first_turn {
                dispatch(emit, AgentEvent::TurnStart);
            } else {
                first_turn = false;
            }

            // Process pending messages (inject before next assistant response).
            if !pending_messages.is_empty() {
                for message in std::mem::take(&mut pending_messages) {
                    dispatch(
                        emit,
                        AgentEvent::MessageStart {
                            message: message.clone(),
                        },
                    );
                    dispatch(
                        emit,
                        AgentEvent::MessageEnd {
                            message: message.clone(),
                        },
                    );
                    current_context.messages.push(message.clone());
                    new_messages.push(message);
                }
            }

            // Stream assistant response.
            let message = stream_assistant_response(
                &mut current_context,
                &config,
                signal,
                emit,
                stream_fn,
                incremental_stream_fn,
            );
            new_messages.push(to_agent_message(&message));

            if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
                dispatch(
                    emit,
                    AgentEvent::TurnEnd {
                        message: to_agent_message(&message),
                        tool_results: Vec::new(),
                    },
                );
                dispatch(
                    emit,
                    AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    },
                );
                return;
            }

            // Check for tool calls.
            let tool_calls = collect_tool_calls(&message);

            let mut tool_results: Vec<ToolResultMessage> = Vec::new();
            has_more_tool_calls = false;
            if !tool_calls.is_empty() {
                // A "length" stop means the output was cut off by the token limit,
                // so every tool call in the message may carry truncated arguments.
                // Fail them all instead of executing potentially borked calls.
                let batch = if message.stop_reason == StopReason::Length {
                    fail_tool_calls_from_truncated_message(&tool_calls, emit)
                } else {
                    execute_tool_calls(&current_context, &message, &config, signal, emit)
                };
                has_more_tool_calls = !batch.terminate;
                for result in batch.messages {
                    current_context.messages.push(to_agent_message_tr(&result));
                    new_messages.push(to_agent_message_tr(&result));
                    tool_results.push(result);
                }
            }

            dispatch(
                emit,
                AgentEvent::TurnEnd {
                    message: to_agent_message(&message),
                    tool_results: tool_results.clone(),
                },
            );

            // prepareNextTurn sees currentContext BEFORE any snapshot is applied.
            if let Some(prepare_next_turn) = &config.prepare_next_turn {
                let next_turn_context = PrepareNextTurnContext {
                    message: message.clone(),
                    tool_results: tool_results.clone(),
                    context: current_context.clone(),
                    new_messages: new_messages.clone(),
                };
                if let Some(snapshot) = prepare_next_turn(&next_turn_context) {
                    if let Some(ctx) = snapshot.context {
                        current_context = ctx;
                    }
                    if let Some(model) = snapshot.model {
                        config.model = model;
                    }
                    // reasoning:
                    //   thinkingLevel === undefined ? config.reasoning
                    //   : thinkingLevel === "off"    ? undefined
                    //   : thinkingLevel
                    config.reasoning = match snapshot.thinking_level {
                        None => config.reasoning,
                        Some(ThinkingLevel::Off) => None,
                        Some(level) => Some(level),
                    };
                }
            }

            // shouldStopAfterTurn sees currentContext AFTER the snapshot.
            if let Some(should_stop) = &config.should_stop_after_turn {
                let stop_context = ShouldStopAfterTurnContext {
                    message: message.clone(),
                    tool_results: tool_results.clone(),
                    context: current_context.clone(),
                    new_messages: new_messages.clone(),
                };
                if should_stop(&stop_context) {
                    dispatch(
                        emit,
                        AgentEvent::AgentEnd {
                            messages: new_messages.clone(),
                        },
                    );
                    return;
                }
            }

            pending_messages = get_steering(&config);
        }

        // Agent would stop here. Check for follow-up messages.
        let follow_up_messages = get_follow_up(&config);
        if !follow_up_messages.is_empty() {
            // Set as pending so the inner loop processes them.
            pending_messages = follow_up_messages;
            continue;
        }

        // No more messages, exit.
        break;
    }

    dispatch(
        emit,
        AgentEvent::AgentEnd {
            messages: new_messages.clone(),
        },
    );
}

// ---------------------------------------------------------------------------
// Streaming a turn (`agent-loop.ts:281-374`)
// ---------------------------------------------------------------------------

/// Stream an assistant response from the LLM (pi's `streamAssistantResponse`,
/// `agent-loop.ts:281`). This is where `AgentMessage[]` gets transformed to
/// `Message[]` for the LLM.
fn stream_assistant_response(
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    signal: Option<&AbortSignal>,
    emit: &AgentEventSink,
    stream_fn: &StreamFn,
    incremental_stream_fn: Option<&IncrementalStreamFn>,
) -> AssistantMessage {
    // Apply context transform if configured (AgentMessage[] → AgentMessage[]).
    let messages: Vec<AgentMessage> = if let Some(transform) = &config.transform_context {
        transform(&context.messages, signal)
    } else {
        context.messages.clone()
    };

    // Convert to LLM-compatible messages (AgentMessage[] → Message[]).
    let llm_messages: Vec<Message> = (config.convert_to_llm)(&messages);

    // Build LLM context.
    let llm_context = Context {
        system_prompt: Some(context.system_prompt.clone()),
        messages: llm_messages,
        tools: context_tools(&context.tools),
    };

    // Resolve API key (important for expiring tokens). pidgin-ai's StreamOptions
    // carries no apiKey field — provider auth lives provider-side — so the
    // resolved value is only used to preserve pi's per-call resolver invocation.
    if let Some(get_api_key) = &config.get_api_key {
        let _resolved_api_key = get_api_key(&config.model.provider);
    }

    let mut partial_message: Option<AssistantMessage> = None;
    let mut added_partial = false;

    // Incremental path: DRIVE the provider one event at a time. Each event is
    // pushed through `sink` as it is pulled, so downstream subscribers observe
    // real inter-event timing. The per-event dispatch bodies are the SAME shared
    // `handle_stream_event`/`emit_terminal_fallthrough` the buffered path runs;
    // the only difference is where the events come from.
    if let Some(incremental) = incremental_stream_fn {
        let mut terminal: Option<AssistantMessage> = None;
        let result = {
            let mut sink = |event: &AssistantMessageEvent| {
                if terminal.is_some() {
                    return;
                }
                if let Some(final_message) = handle_stream_event(
                    context,
                    emit,
                    event,
                    event_terminal_message(event),
                    &mut partial_message,
                    &mut added_partial,
                ) {
                    terminal = Some(final_message);
                }
            };
            incremental(
                &config.model,
                &llm_context,
                Some(&config.stream_options),
                signal,
                &mut sink,
            )
        };
        if let Some(final_message) = terminal {
            return final_message;
        }
        // Stream ended without a terminal event (pi's post-loop fallthrough); the
        // terminal message is the driver's returned `StreamResult.message`.
        let final_message = result.message.clone();
        emit_terminal_fallthrough(context, emit, &final_message, added_partial);
        return final_message;
    }

    // Buffered path (unchanged behavior): the provider hands back a fully
    // materialized `StreamResult` and the loop iterates its events.
    let response = stream_fn(
        &config.model,
        &llm_context,
        Some(&config.stream_options),
        signal,
    );

    for event in &response.events {
        if let Some(final_message) = handle_stream_event(
            context,
            emit,
            event,
            &response.message,
            &mut partial_message,
            &mut added_partial,
        ) {
            return final_message;
        }
    }

    // Stream ended without a terminal event (pi's post-loop fallthrough).
    let final_message = response.message.clone();
    emit_terminal_fallthrough(context, emit, &final_message, added_partial);
    final_message
}

/// The per-event dispatch body shared by the buffered and incremental streaming
/// paths in [`stream_assistant_response`] (pi's `agent-loop.ts:301-370` match).
///
/// Mutates `context.messages`, `partial_message`, and `added_partial` in place
/// and emits the matching UI [`AgentEvent`]. `terminal_message` is only consumed
/// on a terminal (`done`/`error`) event; the buffered path passes the
/// `StreamResult.message`, and the incremental path passes the terminal event's
/// own carried message (which is byte-identical to it, both produced by the same
/// decoder `finish`). Returns `Some(final_message)` exactly when a terminal event
/// is handled, signalling the caller to return it.
fn handle_stream_event(
    context: &mut AgentContext,
    emit: &AgentEventSink,
    event: &AssistantMessageEvent,
    terminal_message: &AssistantMessage,
    partial_message: &mut Option<AssistantMessage>,
    added_partial: &mut bool,
) -> Option<AssistantMessage> {
    match event {
        pidgin_ai::AssistantMessageEvent::Start { partial } => {
            *partial_message = Some(partial.clone());
            context.messages.push(to_agent_message(partial));
            *added_partial = true;
            dispatch(
                emit,
                AgentEvent::MessageStart {
                    message: to_agent_message(partial),
                },
            );
            None
        }
        pidgin_ai::AssistantMessageEvent::Done { .. }
        | pidgin_ai::AssistantMessageEvent::Error { .. } => {
            let final_message = terminal_message.clone();
            if *added_partial {
                let last = context.messages.len() - 1;
                context.messages[last] = to_agent_message(&final_message);
            } else {
                context.messages.push(to_agent_message(&final_message));
            }
            if !*added_partial {
                dispatch(
                    emit,
                    AgentEvent::MessageStart {
                        message: to_agent_message(&final_message),
                    },
                );
            }
            dispatch(
                emit,
                AgentEvent::MessageEnd {
                    message: to_agent_message(&final_message),
                },
            );
            Some(final_message)
        }
        // Non-terminal delta events: text/thinking/toolcall start/delta/end.
        _ => {
            if partial_message.is_some() {
                if let Some(partial) = event_partial(event) {
                    *partial_message = Some(partial.clone());
                    let last = context.messages.len() - 1;
                    context.messages[last] = to_agent_message(partial);
                    dispatch(
                        emit,
                        AgentEvent::MessageUpdate {
                            assistant_message_event: Box::new(event.clone()),
                            message: to_agent_message(partial),
                        },
                    );
                }
            }
            None
        }
    }
}

/// The post-loop fallthrough shared by both streaming paths: reached when the
/// stream ended without a terminal event (pi's `agent-loop.ts:372-...`). Records
/// the final message into `context.messages` and emits `MessageStart` (only when
/// no partial was ever added) plus `MessageEnd`.
fn emit_terminal_fallthrough(
    context: &mut AgentContext,
    emit: &AgentEventSink,
    final_message: &AssistantMessage,
    added_partial: bool,
) {
    if added_partial {
        let last = context.messages.len() - 1;
        context.messages[last] = to_agent_message(final_message);
    } else {
        context.messages.push(to_agent_message(final_message));
        dispatch(
            emit,
            AgentEvent::MessageStart {
                message: to_agent_message(final_message),
            },
        );
    }
    dispatch(
        emit,
        AgentEvent::MessageEnd {
            message: to_agent_message(final_message),
        },
    );
}

// ---------------------------------------------------------------------------
// Tool-call orchestration (`agent-loop.ts:383-556`)
// ---------------------------------------------------------------------------

/// The outcome of executing a batch of tool calls (pi's `ExecutedToolCallBatch`,
/// `agent-loop.ts:430`).
struct ExecutedToolCallBatch {
    messages: Vec<ToolResultMessage>,
    terminate: bool,
}

/// A finalized tool-call outcome (pi's `FinalizedToolCallOutcome`,
/// `agent-loop.ts:576`).
struct FinalizedToolCallOutcome {
    tool_call: AgentToolCall,
    result: AgentToolResult,
    is_error: bool,
}

/// A prepared tool call ready for execution (pi's `PreparedToolCall`,
/// `agent-loop.ts:558`).
struct PreparedToolCall {
    tool_call: AgentToolCall,
    tool: AgentTool,
    args: Value,
}

/// The outcome of `prepareToolCall`: either ready to run, or an immediate
/// (error/blocked) result (pi's `PreparedToolCall | ImmediateToolCallOutcome`).
enum Preparation {
    Prepared(Box<PreparedToolCall>),
    Immediate {
        result: AgentToolResult,
        is_error: bool,
    },
}

/// The outcome of executing a prepared tool call (pi's `ExecutedToolCallOutcome`,
/// `agent-loop.ts:571`).
struct ExecutedToolCallOutcome {
    result: AgentToolResult,
    is_error: bool,
}

/// Fail all tool calls from a length-truncated assistant message (pi's
/// `failToolCallsFromTruncatedMessage`, `agent-loop.ts:383`).
fn fail_tool_calls_from_truncated_message(
    tool_calls: &[AgentToolCall],
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut messages: Vec<ToolResultMessage> = Vec::new();
    for tool_call in tool_calls {
        dispatch(
            emit,
            AgentEvent::ToolExecutionStart {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                args: tool_call.arguments.clone(),
            },
        );
        let finalized = FinalizedToolCallOutcome {
            tool_call: tool_call.clone(),
            result: create_error_tool_result(&format!(
                "Tool call \"{}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
                tool_call.name
            )),
            is_error: true,
        };
        emit_tool_execution_end(&finalized, emit);
        let tool_result_message = create_tool_result_message(&finalized);
        emit_tool_result_message(&tool_result_message, emit);
        messages.push(tool_result_message);
    }
    ExecutedToolCallBatch {
        messages,
        terminate: false,
    }
}

/// Execute tool calls from an assistant message (pi's `executeToolCalls`,
/// `agent-loop.ts:413`). Picks the sequential or parallel executor.
fn execute_tool_calls(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    config: &AgentLoopConfig,
    signal: Option<&AbortSignal>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let tool_calls = collect_tool_calls(assistant_message);
    // hasSequentialToolCall: any called tool declares executionMode "sequential".
    let has_sequential_tool_call = tool_calls.iter().any(|tc| {
        find_tool(current_context, &tc.name)
            .map(|t| t.execution_mode == Some(ToolExecutionMode::Sequential))
            .unwrap_or(false)
    });
    if config.tool_execution == Some(ToolExecutionMode::Sequential) || has_sequential_tool_call {
        execute_tool_calls_sequential(
            current_context,
            assistant_message,
            &tool_calls,
            config,
            signal,
            emit,
        )
    } else {
        execute_tool_calls_parallel(
            current_context,
            assistant_message,
            &tool_calls,
            config,
            signal,
            emit,
        )
    }
}

/// Sequential executor (pi's `executeToolCallsSequential`, `agent-loop.ts:435`).
///
/// Each tool is prepared, executed, finalized, and its result message emitted
/// before the next tool starts — so the events interleave per tool.
fn execute_tool_calls_sequential(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[AgentToolCall],
    config: &AgentLoopConfig,
    signal: Option<&AbortSignal>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut finalized_calls: Vec<FinalizedToolCallOutcome> = Vec::new();
    let mut messages: Vec<ToolResultMessage> = Vec::new();

    for tool_call in tool_calls {
        dispatch(
            emit,
            AgentEvent::ToolExecutionStart {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                args: tool_call.arguments.clone(),
            },
        );

        let preparation = prepare_tool_call(
            current_context,
            assistant_message,
            tool_call,
            config,
            signal,
        );
        let finalized = match preparation {
            Preparation::Immediate { result, is_error } => FinalizedToolCallOutcome {
                tool_call: tool_call.clone(),
                result,
                is_error,
            },
            Preparation::Prepared(prepared) => {
                let executed = execute_prepared_tool_call(&prepared, signal, emit);
                finalize_executed_tool_call(
                    current_context,
                    assistant_message,
                    &prepared,
                    executed,
                    config,
                    signal,
                )
            }
        };

        emit_tool_execution_end(&finalized, emit);
        let tool_result_message = create_tool_result_message(&finalized);
        emit_tool_result_message(&tool_result_message, emit);
        finalized_calls.push(finalized);
        messages.push(tool_result_message);

        if aborted(signal) {
            break;
        }
    }

    let terminate = should_terminate_tool_batch(&finalized_calls);
    ExecutedToolCallBatch {
        messages,
        terminate,
    }
}

/// Parallel executor (pi's `executeToolCallsParallel`, `agent-loop.ts:491`).
///
/// Phase 1 prepares every tool in source order, emitting `tool_execution_start`s
/// and, for immediate (error/blocked) outcomes, the `tool_execution_end` right
/// away. Phase 2 drains the deferred (prepared) entries — executing, finalizing,
/// and emitting their `tool_execution_end` — in source order (pi's `Promise.all`
/// preserves array order; the eager port has no real concurrency, so this
/// straightforwardly runs in order). Phase 3 builds every result message in
/// source order. The observable golden pi's test locks in is that result
/// messages persist in **source order** even when a `tool_execution_end` fires
/// out of source order (which happens here whenever an immediate outcome, ended
/// in phase 1, precedes a prepared outcome, ended in phase 2).
fn execute_tool_calls_parallel(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[AgentToolCall],
    config: &AgentLoopConfig,
    signal: Option<&AbortSignal>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    // pi's `FinalizedToolCallEntry`: either an already-finalized immediate
    // outcome, or a deferred computation (here, the prepared call to run later).
    enum Entry {
        Immediate(Box<FinalizedToolCallOutcome>),
        Deferred(Box<PreparedToolCall>),
    }

    let mut entries: Vec<Entry> = Vec::new();

    for tool_call in tool_calls {
        dispatch(
            emit,
            AgentEvent::ToolExecutionStart {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                args: tool_call.arguments.clone(),
            },
        );

        let preparation = prepare_tool_call(
            current_context,
            assistant_message,
            tool_call,
            config,
            signal,
        );
        match preparation {
            Preparation::Immediate { result, is_error } => {
                let finalized = FinalizedToolCallOutcome {
                    tool_call: tool_call.clone(),
                    result,
                    is_error,
                };
                emit_tool_execution_end(&finalized, emit);
                entries.push(Entry::Immediate(Box::new(finalized)));
                if aborted(signal) {
                    break;
                }
            }
            Preparation::Prepared(prepared) => {
                entries.push(Entry::Deferred(prepared));
                if aborted(signal) {
                    break;
                }
            }
        }
    }

    // Promise.all in array (source) order.
    let mut ordered_finalized_calls: Vec<FinalizedToolCallOutcome> = Vec::new();
    for entry in entries {
        let finalized = match entry {
            Entry::Immediate(finalized) => *finalized,
            Entry::Deferred(prepared) => {
                let executed = execute_prepared_tool_call(&prepared, signal, emit);
                let finalized = finalize_executed_tool_call(
                    current_context,
                    assistant_message,
                    &prepared,
                    executed,
                    config,
                    signal,
                );
                emit_tool_execution_end(&finalized, emit);
                finalized
            }
        };
        ordered_finalized_calls.push(finalized);
    }

    let mut messages: Vec<ToolResultMessage> = Vec::new();
    for finalized in &ordered_finalized_calls {
        let tool_result_message = create_tool_result_message(finalized);
        emit_tool_result_message(&tool_result_message, emit);
        messages.push(tool_result_message);
    }

    let terminate = should_terminate_tool_batch(&ordered_finalized_calls);
    ExecutedToolCallBatch {
        messages,
        terminate,
    }
}

/// Terminate the batch only when it is non-empty and every result sets
/// `terminate === true` (pi's `shouldTerminateToolBatch`, `agent-loop.ts:584`).
fn should_terminate_tool_batch(finalized_calls: &[FinalizedToolCallOutcome]) -> bool {
    !finalized_calls.is_empty()
        && finalized_calls
            .iter()
            .all(|finalized| finalized.result.terminate == Some(true))
}

/// Apply a tool's `prepareArguments` shim (pi's `prepareToolCallArguments`,
/// `agent-loop.ts:588`).
///
/// pi returns the original `toolCall` when the shim returns the *same reference*
/// as the input arguments; the eager port compares by value instead (a shim that
/// produces a fresh, equal object is indistinguishable from the identity shim in
/// both worlds, so the observable result is unchanged).
fn prepare_tool_call_arguments(tool: &AgentTool, tool_call: &AgentToolCall) -> AgentToolCall {
    let Some(prepare_arguments) = &tool.prepare_arguments else {
        return tool_call.clone();
    };
    let prepared_arguments = prepare_arguments(&tool_call.arguments);
    if prepared_arguments == tool_call.arguments {
        return tool_call.clone();
    }
    AgentToolCall {
        arguments: prepared_arguments,
        ..tool_call.clone()
    }
}

/// Validate tool-call arguments against the tool schema (pi's
/// `validateToolArguments`, `ai/utils/validation.ts:278`).
///
/// pi runs TypeBox `Value.Convert` + `Compile().Check` and throws a formatted
/// error on failure. pidgin-ai does not yet port TypeBox, and [`AgentTool`]
/// keeps `parameters` opaque, so this is an identity pass-through: it returns the
/// (prepared) arguments unchanged and never errors. The `Err` arm of
/// [`prepare_tool_call`] that maps a validation throw to an immediate error
/// result is therefore structurally preserved but unreachable via this function.
fn validate_tool_arguments(_tool: &AgentTool, tool_call: &AgentToolCall) -> Result<Value, String> {
    Ok(tool_call.arguments.clone())
}

/// Prepare a single tool call: resolve the tool, prepare + validate its
/// arguments, and run the `beforeToolCall` hook (pi's `prepareToolCall`,
/// `agent-loop.ts:602`).
fn prepare_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &AgentToolCall,
    config: &AgentLoopConfig,
    signal: Option<&AbortSignal>,
) -> Preparation {
    let Some(tool) = find_tool(current_context, &tool_call.name).cloned() else {
        return Preparation::Immediate {
            result: create_error_tool_result(&format!("Tool {} not found", tool_call.name)),
            is_error: true,
        };
    };

    // pi wraps prepare/validate/beforeToolCall in try/catch; the Rust closures do
    // not throw, so the catch arm (mapping a thrown error to an immediate error
    // result) cannot be reached here.
    let prepared_tool_call = prepare_tool_call_arguments(&tool, tool_call);
    let mut validated_args = match validate_tool_arguments(&tool, &prepared_tool_call) {
        Ok(args) => args,
        Err(error) => {
            return Preparation::Immediate {
                result: create_error_tool_result(&error),
                is_error: true,
            }
        }
    };

    if let Some(before_tool_call) = &config.before_tool_call {
        let mut before_context = BeforeToolCallContext {
            assistant_message: assistant_message.clone(),
            tool_call: tool_call.clone(),
            args: validated_args,
            context: current_context.clone(),
        };
        let before_result = before_tool_call(&mut before_context, signal);
        // Faithful to pi (`agent-loop.ts:657`): pi's hook mutates the validated
        // `args` object in place and the loop reuses that same reference for
        // `execute`. pidgin mirrors that by passing `&mut before_context` and
        // adopting the (possibly hook-mutated) args for execution — validation is
        // not re-run afterward.
        validated_args = before_context.args;
        if aborted(signal) {
            return Preparation::Immediate {
                result: create_error_tool_result("Operation aborted"),
                is_error: true,
            };
        }
        if let Some(before_result) = before_result {
            if before_result.block == Some(true) {
                return Preparation::Immediate {
                    result: create_error_tool_result(
                        before_result
                            .reason
                            .as_deref()
                            .unwrap_or("Tool execution was blocked"),
                    ),
                    is_error: true,
                };
            }
        }
    }
    if aborted(signal) {
        return Preparation::Immediate {
            result: create_error_tool_result("Operation aborted"),
            is_error: true,
        };
    }
    Preparation::Prepared(Box::new(PreparedToolCall {
        tool_call: tool_call.clone(),
        tool,
        args: validated_args,
    }))
}

/// Execute a prepared tool call, wiring up the streaming-update callback (pi's
/// `executePreparedToolCall`, `agent-loop.ts:668`).
///
/// pi's `try/catch` maps a thrown execute error to an error result; pidgin's
/// [`crate::types::AgentToolExecute`] returns an [`AgentToolResult`] directly and
/// cannot throw, so execution here always yields `is_error: false` and the catch
/// arm is unreachable. The `acceptingUpdates` gate is preserved: partial-result
/// callbacks fired after `execute` returns are ignored.
fn execute_prepared_tool_call(
    prepared: &PreparedToolCall,
    signal: Option<&AbortSignal>,
    emit: &AgentEventSink,
) -> ExecutedToolCallOutcome {
    let accepting_updates = Arc::new(AtomicBool::new(true));

    let cb_accepting = accepting_updates.clone();
    let cb_emit = emit.clone();
    let cb_tool_call_id = prepared.tool_call.id.clone();
    let cb_tool_name = prepared.tool_call.name.clone();
    let cb_args = prepared.tool_call.arguments.clone();
    let update_callback: AgentToolUpdateCallback =
        Arc::new(move |partial_result: &AgentToolResult| {
            if !cb_accepting.load(Ordering::SeqCst) {
                return;
            }
            dispatch(
                &cb_emit,
                AgentEvent::ToolExecutionUpdate {
                    tool_call_id: cb_tool_call_id.clone(),
                    tool_name: cb_tool_name.clone(),
                    args: cb_args.clone(),
                    partial_result: serde_json::to_value(partial_result).unwrap_or(Value::Null),
                },
            );
        });

    let result = (prepared.tool.execute)(
        &prepared.tool_call.id,
        &prepared.args,
        signal,
        Some(&update_callback),
    );
    accepting_updates.store(false, Ordering::SeqCst);

    ExecutedToolCallOutcome {
        result,
        is_error: false,
    }
}

/// Apply the `afterToolCall` hook to an executed tool call (pi's
/// `finalizeExecutedToolCall`, `agent-loop.ts:711`).
///
/// pi's `try/catch` maps a thrown `afterToolCall` error to an error result;
/// pidgin's hook returns `Option<AfterToolCallResult>` and cannot throw, so the
/// catch arm is unreachable. The field-by-field override (`?? existing`) is
/// preserved.
fn finalize_executed_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    prepared: &PreparedToolCall,
    executed: ExecutedToolCallOutcome,
    config: &AgentLoopConfig,
    signal: Option<&AbortSignal>,
) -> FinalizedToolCallOutcome {
    let mut result = executed.result;
    let mut is_error = executed.is_error;

    if let Some(after_tool_call) = &config.after_tool_call {
        let after_context = AfterToolCallContext {
            assistant_message: assistant_message.clone(),
            tool_call: prepared.tool_call.clone(),
            args: prepared.args.clone(),
            result: result.clone(),
            is_error,
            context: current_context.clone(),
        };
        if let Some(after_result) = after_tool_call(&after_context, signal) {
            result = AgentToolResult {
                content: after_result.content.unwrap_or(result.content),
                details: after_result.details.unwrap_or(result.details),
                terminate: after_result.terminate.or(result.terminate),
                added_tool_names: result.added_tool_names,
            };
            is_error = after_result.is_error.unwrap_or(is_error);
        }
    }

    FinalizedToolCallOutcome {
        tool_call: prepared.tool_call.clone(),
        result,
        is_error,
    }
}

/// Build a plain error tool result (pi's `createErrorToolResult`,
/// `agent-loop.ts:757`).
fn create_error_tool_result(message: &str) -> AgentToolResult {
    AgentToolResult {
        content: vec![ContentBlock::Text {
            text: message.to_string(),
            text_signature: None,
        }],
        details: json!({}),
        added_tool_names: None,
        terminate: None,
    }
}

/// Emit `tool_execution_end` for a finalized call (pi's `emitToolExecutionEnd`,
/// `agent-loop.ts:764`).
fn emit_tool_execution_end(finalized: &FinalizedToolCallOutcome, emit: &AgentEventSink) {
    dispatch(
        emit,
        AgentEvent::ToolExecutionEnd {
            tool_call_id: finalized.tool_call.id.clone(),
            tool_name: finalized.tool_call.name.clone(),
            result: serde_json::to_value(&finalized.result).unwrap_or(Value::Null),
            is_error: finalized.is_error,
        },
    );
}

/// Build the `toolResult` message for a finalized call (pi's
/// `createToolResultMessage`, `agent-loop.ts:774`).
fn create_tool_result_message(finalized: &FinalizedToolCallOutcome) -> ToolResultMessage {
    // pi: addedToolNames is only included when non-empty.
    let added_tool_names = finalized
        .result
        .added_tool_names
        .as_ref()
        .filter(|names| !names.is_empty())
        .cloned();
    ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        // Untyped tools can return results without content; normalize so the null
        // never enters session history or provider payloads.
        content: finalized.result.content.clone(),
        details: Some(finalized.result.details.clone()),
        added_tool_names,
        is_error: finalized.is_error,
        timestamp: now_ms(),
    }
}

/// Emit `message_start`/`message_end` for a tool-result message (pi's
/// `emitToolResultMessage`, `agent-loop.ts:789`).
fn emit_tool_result_message(tool_result_message: &ToolResultMessage, emit: &AgentEventSink) {
    dispatch(
        emit,
        AgentEvent::MessageStart {
            message: to_agent_message_tr(tool_result_message),
        },
    );
    dispatch(
        emit,
        AgentEvent::MessageEnd {
            message: to_agent_message_tr(tool_result_message),
        },
    );
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Emit an event through the sink. Named `dispatch` so it never shadows a local
/// `emit` binding.
fn dispatch(sink: &AgentEventSink, event: AgentEvent) {
    let f: &(dyn Fn(AgentEvent) + Send + Sync) = sink.as_ref();
    f(event);
}

/// A collecting sink that pushes every event into a shared vec (the eager analog
/// of iterating pi's `EventStream`).
fn collecting_sink(collected: &Arc<Mutex<Vec<AgentEvent>>>) -> AgentEventSink {
    let collected = collected.clone();
    Arc::new(move |event: AgentEvent| {
        collected.lock().unwrap().push(event);
    })
}

/// Whether the abort signal is tripped (pi's `signal?.aborted`).
fn aborted(signal: Option<&AbortSignal>) -> bool {
    signal.is_some_and(AbortSignal::is_aborted)
}

/// `config.getSteeringMessages?.() || []`.
fn get_steering(config: &AgentLoopConfig) -> Vec<AgentMessage> {
    config
        .get_steering_messages
        .as_ref()
        .map(|f| f())
        .unwrap_or_default()
}

/// `config.getFollowUpMessages?.() || []`.
fn get_follow_up(config: &AgentLoopConfig) -> Vec<AgentMessage> {
    config
        .get_follow_up_messages
        .as_ref()
        .map(|f| f())
        .unwrap_or_default()
}

/// Find a tool by name in the context (pi's `context.tools?.find(...)`).
fn find_tool<'a>(context: &'a AgentContext, name: &str) -> Option<&'a AgentTool> {
    context
        .tools
        .as_ref()
        .and_then(|tools| tools.iter().find(|t| t.name == name))
}

/// Collect the `toolCall` content blocks of an assistant message as
/// [`AgentToolCall`]s (pi's `message.content.filter(c => c.type === "toolCall")`).
fn collect_tool_calls(message: &AssistantMessage) -> Vec<AgentToolCall> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
                thought_signature,
            } => Some(AgentToolCall {
                kind: ToolCallType::ToolCall,
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
                thought_signature: thought_signature.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Convert [`AgentContext`] tools into the LLM [`Context`] tool schemas. pi
/// passes `context.tools` (a `Tool[]` of `{name, description, parameters}`)
/// straight through to the LLM context; the port projects each runtime
/// [`AgentTool`] down to that schema shape.
fn context_tools(tools: &Option<Vec<AgentTool>>) -> Option<Vec<Value>> {
    tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect()
    })
}

/// The `role` of an [`AgentMessage`] value, if present.
fn message_role(message: &AgentMessage) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

/// Serialize an [`AssistantMessage`] into an [`AgentMessage`] value.
fn to_agent_message(message: &AssistantMessage) -> AgentMessage {
    serde_json::to_value(message).expect("AssistantMessage serializes")
}

/// Serialize a [`ToolResultMessage`] into an [`AgentMessage`] value.
fn to_agent_message_tr(message: &ToolResultMessage) -> AgentMessage {
    serde_json::to_value(message).expect("ToolResultMessage serializes")
}

/// The `partial` accumulator carried by a non-terminal stream event.
fn event_partial(event: &pidgin_ai::AssistantMessageEvent) -> Option<&AssistantMessage> {
    use pidgin_ai::AssistantMessageEvent as E;
    match event {
        E::Start { partial }
        | E::TextStart { partial, .. }
        | E::TextDelta { partial, .. }
        | E::TextEnd { partial, .. }
        | E::ThinkingStart { partial, .. }
        | E::ThinkingDelta { partial, .. }
        | E::ThinkingEnd { partial, .. }
        | E::ToolcallStart { partial, .. }
        | E::ToolcallDelta { partial, .. }
        | E::ToolcallEnd { partial, .. } => Some(partial),
        E::Done { .. } | E::Error { .. } => None,
    }
}

/// The [`AssistantMessage`] carried by any event: the terminal message for
/// `done`/`error`, the running partial otherwise.
///
/// Used by [`stream_assistant_response`]'s incremental sink to feed
/// [`handle_stream_event`] a `terminal_message` for every event. On a terminal
/// event this is the same message the buffered path reads from
/// `StreamResult.message` (both come from the decoder's `finish`); on a
/// non-terminal event the value is passed through but never consumed.
fn event_terminal_message(event: &pidgin_ai::AssistantMessageEvent) -> &AssistantMessage {
    use pidgin_ai::AssistantMessageEvent as E;
    match event {
        E::Done { message, .. } => message,
        E::Error { error, .. } => error,
        E::Start { partial }
        | E::TextStart { partial, .. }
        | E::TextDelta { partial, .. }
        | E::TextEnd { partial, .. }
        | E::ThinkingStart { partial, .. }
        | E::ThinkingDelta { partial, .. }
        | E::ThinkingEnd { partial, .. }
        | E::ToolcallStart { partial, .. }
        | E::ToolcallDelta { partial, .. }
        | E::ToolcallEnd { partial, .. } => partial,
    }
}

/// `Date.now()` for tool-result timestamps. pi calls `Date.now()` inline; the
/// port reads the production [`SystemClock`]. No test asserts on these values.
fn now_ms() -> i64 {
    SystemClock::new().now_ms()
}

#[cfg(test)]
mod tests;
