//! The turn-runner core, ported from pi's `AgentSession`
//! (`packages/coding-agent/src/core/agent-session.ts`).
//!
//! This module carries the code that runs one prompt turn end-to-end on top of
//! the [`AgentSession`] scaffold in [`super::session`]:
//!
//! * [`AgentEventHandler`] / [`build_agent_listener`] — the real `agent.subscribe`
//!   handler (pi `_handleAgentEvent`, L574): map each core
//!   [`AgentEvent`](atilla_agent::types::AgentEvent) to its extension event, fan
//!   the corresponding [`AgentSessionEvent`] out to listeners, and persist
//!   finalized messages to the session manager.
//! * [`AgentSession::prompt`] — the prompt spine (pi `prompt`, L1102): preflight
//!   (model + auth), build the user message, `before_agent_start`, then run.
//! * [`AgentSession::run_agent_prompt`] / [`AgentSession::handle_post_agent_run`]
//!   — the drive loop (pi `_runAgentPrompt` L1049 / `_handlePostAgentRun` L1063).
//! * the `emit_*` helpers (pi `_emit`/`_emitQueueUpdate`/`_emitAgentSettled`).
//!
//! The streaming-guard queue routing and pending-next-turn draining land here
//! (the steering / follow-up queue methods themselves live in [`super::queue`]).
//! Branches that belong to later PRs of the AgentSession port are stubbed to
//! their minimal safe default with a plain `// unit5:` note pointing at the PR
//! that lands them: the `/`-command shortcut and skill/prompt-template expansion
//! (PR7) and the pre-send compaction check (PR6). Auto-retry (the retryable-error
//! branch of `handle_post_agent_run`, the `will_retry` computation, and the
//! success reset) lands in the sibling [`super::retry`] module.
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/agent-session.ts`.

// straitjacket-allow-file:duplication

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use atilla_agent::agent::{AgentError, Listener};
use atilla_agent::types::{AgentEvent, AgentMessage};
use atilla_ai::seams::AbortSignal;
use atilla_ai::Model;

use crate::core::auth::auth_guidance::{
    format_no_api_key_found_message, format_no_model_selected_message,
};
use crate::core::extensions::events::agent::{AgentEndEvent, AgentSettledEvent, AgentStartEvent};
use crate::core::extensions::events::common::ImageContent;
use crate::core::extensions::events::selection::{
    InputEventResult, InputSource, StreamingBehavior,
};
use crate::core::extensions::events::tool::{
    ToolExecutionEndEvent, ToolExecutionStartEvent, ToolExecutionUpdateEvent,
};
use crate::core::extensions::events::turn::{
    MessageEndEvent, MessageStartEvent, MessageUpdateEvent, TurnEndEvent, TurnStartEvent,
};
use crate::core::extensions::runner::{ExtensionDispatchEvent, ExtensionRunner};
use crate::core::session_manager::SessionManager;

use super::events::{AgentSessionEvent, AgentSessionEventListener};
use super::session::AgentSession;

/// The `"unknown"` placeholder [`Model`] id/provider `atilla_agent`'s
/// [`AgentState`](atilla_agent::types::AgentState) uses where pi represents an
/// unselected model as `agent.state.model === undefined`. `atilla_agent`'s field
/// is non-optional and defaults to this placeholder (see `agent.rs` `DEFAULT_MODEL`
/// and [`crate::core::auth::auth_guidance`]'s `UNKNOWN_PROVIDER`), so the session
/// treats the placeholder as "no model selected".
pub(super) const UNKNOWN_MODEL_SENTINEL: &str = "unknown";

/// The error pi's `prompt` throws — a preflight failure (no model / no auth /
/// streaming guard) or an error surfaced by the wrapped [`Agent`](atilla_agent::agent::Agent).
///
/// pi throws `Error` with a user-facing message; the port keeps that message
/// verbatim in [`PromptError::Preflight`] and wraps agent-level failures in
/// [`PromptError::Agent`].
#[derive(Debug)]
pub enum PromptError {
    /// A preflight failure whose [`Display`](std::fmt::Display) string matches
    /// pi's thrown message.
    Preflight(String),
    /// A failure surfaced by the wrapped agent's `prompt`/`continue`.
    Agent(AgentError),
}

impl std::fmt::Display for PromptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PromptError::Preflight(message) => f.write_str(message),
            PromptError::Agent(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PromptError {}

/// Options for [`AgentSession::prompt_with`] (pi's `PromptOptions`,
/// `agent-session.ts:219`).
///
/// The `preflightResult` RPC hook (pi's internal preflight-acceptance callback)
/// is not modeled here; it lands with the RPC turn-command wiring.
#[derive(Default)]
pub struct PromptOptions {
    /// Whether to expand file-based prompt templates and dispatch `/`-commands
    /// (pi default `true`).
    pub expand_prompt_templates: bool,
    /// Image attachments (pi `images`).
    pub images: Option<Vec<ImageContent>>,
    /// When streaming, how to queue the message — [`StreamingBehavior::Steer`]
    /// (interrupt) or [`StreamingBehavior::FollowUp`] (wait). Required if
    /// streaming (pi `streamingBehavior`).
    pub streaming_behavior: Option<StreamingBehavior>,
    /// Source of input for extension input-event handlers; defaults to
    /// [`InputSource::Interactive`] (pi `source`).
    pub source: Option<InputSource>,
}

impl PromptOptions {
    /// The pi default: `expandPromptTemplates: true`, everything else unset.
    pub fn defaults() -> Self {
        Self {
            expand_prompt_templates: true,
            images: None,
            streaming_behavior: None,
            source: None,
        }
    }
}

/// A millisecond wall-clock timestamp (pi's `Date.now()`).
pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The `role` discriminant of an [`AgentMessage`] value, if present.
fn message_role(message: &AgentMessage) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

/// The concatenated text content of a `user` message (pi's `_getUserMessageText`,
/// L663). Non-user messages and non-text content yield the empty string.
fn user_message_text(message: &AgentMessage) -> String {
    if message_role(message) != Some("user") {
        return String::new();
    }
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Fan `event` out to every listener synchronously in registration order (pi's
/// `_emit`, L527). Shared by [`AgentSession::emit`] and [`AgentEventHandler`].
///
/// The registry snapshot is taken before the lock is released so a listener that
/// re-enters `subscribe`/unsubscribe cannot deadlock or observe a half-mutated
/// registry.
pub(super) fn emit_to_listeners(
    listeners: &Mutex<Vec<(u64, AgentSessionEventListener)>>,
    event: &AgentSessionEvent,
) {
    let snapshot: Vec<AgentSessionEventListener> = {
        let guard = listeners.lock().unwrap();
        guard
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect()
    };
    for listener in &snapshot {
        listener(event);
    }
}

/// The internal `agent.subscribe` handler's shared state (pi's `_handleAgentEvent`
/// captures `this`, L574). Every field is an [`Arc`] clone of the matching
/// [`AgentSession`] field so the handler — a `'static` [`Listener`] closure — can
/// reach the session's mutable turn state.
pub(super) struct AgentEventHandler {
    /// Session-tree persistence (pi `sessionManager`).
    pub session_manager: Arc<Mutex<SessionManager>>,
    /// The extension runner emit target (pi `_extensionRunner`).
    pub extension_runner: Arc<dyn ExtensionRunner>,
    /// TUI-facing listeners (pi `_eventListeners`).
    pub listeners: Arc<Mutex<Vec<(u64, AgentSessionEventListener)>>>,
    /// Pending steering messages for UI display (pi `_steeringMessages`).
    pub steering_messages: Arc<Mutex<Vec<String>>>,
    /// Pending follow-up messages for UI display (pi `_followUpMessages`).
    pub follow_up_messages: Arc<Mutex<Vec<String>>>,
    /// The last assistant message, read by `handle_post_agent_run`
    /// (pi `_lastAssistantMessage`).
    pub last_assistant_message: Arc<Mutex<Option<AgentMessage>>>,
    /// The extension-facing turn index (pi `_turnIndex`).
    pub turn_index: Arc<Mutex<i64>>,
    /// A cheap handle to the same shared agent, for the live model the
    /// `will_retry` computation reads (pi `this.model`).
    pub agent: atilla_agent::agent::Agent,
    /// The shared auto-retry attempt count (pi `_retryAttempt`). Reset on a
    /// successful assistant response; read for `will_retry`.
    pub retry_attempt: Arc<Mutex<u32>>,
    /// The shared snapshot of resolved retry settings (pi's `getRetrySettings`),
    /// read for `will_retry`.
    pub retry_settings: Arc<Mutex<crate::core::settings_manager::RetryResolved>>,
}

impl AgentEventHandler {
    /// Port of pi's `_handleAgentEvent` (L574): queue removal, extension bridging,
    /// listener fan-out, then session persistence.
    fn handle(&self, event: &AgentEvent) {
        // 1. On a user message_start, remove it from whichever queue mirrored it
        //    so the UI sees the updated queue state before the event is emitted.
        if let AgentEvent::MessageStart { message } = event {
            if message_role(message) == Some("user") {
                // unit5: _overflowRecoveryAttempted reset lands with compaction (PR6).
                let text = user_message_text(message);
                if !text.is_empty() {
                    self.remove_from_queues(&text);
                }
            }
        }

        // 2. Emit to extensions first.
        self.emit_extension_event(event);

        // 3. Notify all listeners. `will_retry` is folded into agent_end (pi
        //    `_willRetryAfterAgentEnd`, L647); every other event ignores it.
        let will_retry = match event {
            AgentEvent::AgentEnd { messages } => self.will_retry_after_agent_end(messages),
            _ => false,
        };
        let session_event = AgentSessionEvent::from_agent_event(event.clone(), will_retry);
        emit_to_listeners(&self.listeners, &session_event);

        // 4. Session persistence on message_end (pi L604).
        if let AgentEvent::MessageEnd { message } = event {
            self.persist_message_end(message);
        }
    }

    /// Splice a mirrored queue entry out on user `message_start` and emit a queue
    /// update (pi L582-593). Steering is checked before follow-up.
    fn remove_from_queues(&self, text: &str) {
        {
            let mut steering = self.steering_messages.lock().unwrap();
            if let Some(index) = steering.iter().position(|entry| entry == text) {
                steering.remove(index);
                drop(steering);
                self.emit_queue_update();
                return;
            }
        }
        let mut follow_up = self.follow_up_messages.lock().unwrap();
        if let Some(index) = follow_up.iter().position(|entry| entry == text) {
            follow_up.remove(index);
            drop(follow_up);
            self.emit_queue_update();
        }
    }

    /// Emit the current queue state to listeners (pi's `_emitQueueUpdate`, L533).
    fn emit_queue_update(&self) {
        let steering = self.steering_messages.lock().unwrap().clone();
        let follow_up = self.follow_up_messages.lock().unwrap().clone();
        emit_to_listeners(
            &self.listeners,
            &AgentSessionEvent::QueueUpdate {
                steering,
                follow_up,
            },
        );
    }

    /// Map a core [`AgentEvent`] to its extension event and emit it (pi's
    /// `_emitExtensionEvent`, L700). The strongly-typed core payloads are
    /// projected onto the `Value`-shaped extension event fields via serde.
    fn emit_extension_event(&self, event: &AgentEvent) {
        match event {
            AgentEvent::AgentStart => {
                *self.turn_index.lock().unwrap() = 0;
                self.extension_runner
                    .emit(&ExtensionDispatchEvent::AgentStart(AgentStartEvent {}));
            }
            AgentEvent::AgentEnd { messages } => {
                self.extension_runner
                    .emit(&ExtensionDispatchEvent::AgentEnd(AgentEndEvent {
                        messages: messages.clone(),
                    }));
            }
            AgentEvent::TurnStart => {
                let turn_index = *self.turn_index.lock().unwrap();
                self.extension_runner
                    .emit(&ExtensionDispatchEvent::TurnStart(TurnStartEvent {
                        turn_index,
                        timestamp: now_ms(),
                    }));
            }
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => {
                let turn_index = *self.turn_index.lock().unwrap();
                let tool_results = tool_results
                    .iter()
                    .map(|result| serde_json::to_value(result).unwrap_or(Value::Null))
                    .collect();
                self.extension_runner
                    .emit(&ExtensionDispatchEvent::TurnEnd(TurnEndEvent {
                        turn_index,
                        message: message.clone(),
                        tool_results,
                    }));
                *self.turn_index.lock().unwrap() += 1;
            }
            AgentEvent::MessageStart { message } => {
                self.extension_runner
                    .emit(&ExtensionDispatchEvent::MessageStart(MessageStartEvent {
                        message: message.clone(),
                    }));
            }
            AgentEvent::MessageUpdate {
                message,
                assistant_message_event,
            } => {
                let assistant_message_event =
                    serde_json::to_value(&**assistant_message_event).unwrap_or(Value::Null);
                self.extension_runner
                    .emit(&ExtensionDispatchEvent::MessageUpdate(MessageUpdateEvent {
                        message: message.clone(),
                        assistant_message_event,
                    }));
            }
            AgentEvent::MessageEnd { message } => {
                let _replacement = self.extension_runner.emit_message_end(&MessageEndEvent {
                    message: message.clone(),
                });
                // unit5: applying an extension replacement back into agent state
                // (pi `_replaceMessageInPlace`, L683) lands in PR7; the
                // StubExtensionRunner returns None, so nothing is replaced here.
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                self.extension_runner
                    .emit(&ExtensionDispatchEvent::ToolExecutionStart(
                        ToolExecutionStartEvent {
                            tool_call_id: tool_call_id.clone(),
                            tool_name: tool_name.clone(),
                            args: args.clone(),
                        },
                    ));
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
            } => {
                self.extension_runner
                    .emit(&ExtensionDispatchEvent::ToolExecutionUpdate(
                        ToolExecutionUpdateEvent {
                            tool_call_id: tool_call_id.clone(),
                            tool_name: tool_name.clone(),
                            args: args.clone(),
                            partial_result: partial_result.clone(),
                        },
                    ));
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            } => {
                self.extension_runner
                    .emit(&ExtensionDispatchEvent::ToolExecutionEnd(
                        ToolExecutionEndEvent {
                            tool_call_id: tool_call_id.clone(),
                            tool_name: tool_name.clone(),
                            result: result.clone(),
                            is_error: *is_error,
                        },
                    ));
            }
        }
    }

    /// Persist a finalized message and track the last assistant message (pi
    /// L604-643). Custom messages become custom-message entries; user / assistant
    /// / tool-result messages become session-message entries.
    fn persist_message_end(&self, message: &AgentMessage) {
        match message_role(message) {
            Some("custom") => {
                let custom_type = message
                    .get("customType")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                // Untyped extensions can pass null/missing content; normalize.
                let content = message.get("content").cloned().unwrap_or_else(|| json!([]));
                let display = message
                    .get("display")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let details = message.get("details").cloned();
                self.session_manager
                    .lock()
                    .unwrap()
                    .append_custom_message_entry(&custom_type, content, display, details);
            }
            Some("user") | Some("assistant") | Some("toolResult") => {
                self.session_manager
                    .lock()
                    .unwrap()
                    .append_message(message.clone());
            }
            // bashExecution / compactionSummary / branchSummary are persisted
            // elsewhere; any other role is ignored.
            _ => {}
        }

        if message_role(message) == Some("assistant") {
            *self.last_assistant_message.lock().unwrap() = Some(message.clone());
            // unit5: the stopReason-gated _overflowRecoveryAttempted reset (PR6)
            // lands with compaction.

            // Reset the retry counter immediately on a successful assistant
            // response so it does not accumulate across LLM calls within a turn
            // (pi L631-642). Emitting the terminal `auto_retry_end{success:true}`
            // here — during the successful message_end — is what lets the next
            // `agent_end`'s `will_retry` read a cleared counter.
            let stop_reason = message.get("stopReason").and_then(Value::as_str);
            let mut attempt = self.retry_attempt.lock().unwrap();
            if stop_reason != Some("error") && *attempt > 0 {
                let completed = *attempt;
                *attempt = 0;
                drop(attempt);
                emit_to_listeners(
                    &self.listeners,
                    &AgentSessionEvent::AutoRetryEnd {
                        success: true,
                        attempt: completed,
                        final_error: None,
                    },
                );
            }
        }
    }

    /// Whether a retryable error at `agent_end` will trigger another attempt (pi's
    /// `_willRetryAfterAgentEnd`, L647): retry must be enabled and under budget, and
    /// the last assistant message in the run must be retryable.
    fn will_retry_after_agent_end(&self, messages: &[AgentMessage]) -> bool {
        let settings = *self.retry_settings.lock().unwrap();
        let attempt = *self.retry_attempt.lock().unwrap();
        if !settings.enabled || i64::from(attempt) >= settings.max_retries {
            return false;
        }
        let context_window = super::retry::agent_context_window(&self.agent);
        for message in messages.iter().rev() {
            if message_role(message) == Some("assistant") {
                return super::retry::message_is_retryable(message, context_window);
            }
        }
        false
    }
}

/// Build the `'static` [`Listener`] that drives `handler` on each agent event
/// (pi installs `_handleAgentEvent` via `agent.subscribe`, L818). Called from the
/// [`AgentSession`] constructor.
pub(super) fn build_agent_listener(handler: AgentEventHandler) -> Listener {
    Arc::new(move |event: &AgentEvent, _signal: &AbortSignal| handler.handle(event))
}

impl AgentSession {
    // =========================================================================
    // Read-only turn state
    // =========================================================================

    /// The current model, or `None` when none is selected (pi's `get model`,
    /// L854, `agent.state.model`).
    ///
    /// `atilla_agent`'s `AgentState.model` is non-optional and defaults to the
    /// `"unknown"` placeholder where pi has `undefined`; the placeholder reads as
    /// "no model selected" (see [`UNKNOWN_MODEL_SENTINEL`]).
    pub fn model(&self) -> Option<Model> {
        let model = self.agent.model();
        if model.provider == UNKNOWN_MODEL_SENTINEL && model.id == UNKNOWN_MODEL_SENTINEL {
            None
        } else {
            Some(model)
        }
    }

    /// All messages including custom types (pi's `get messages`, L941,
    /// `agent.state.messages`).
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.agent.messages()
    }

    /// Whether the session is currently processing an agent run or post-run
    /// continuation (pi's `get isStreaming`, L864).
    pub fn is_streaming(&self) -> bool {
        !self.is_idle()
    }

    // =========================================================================
    // Emit helpers (pi `_emit`/`_emitQueueUpdate`/`_emitAgentSettled`)
    // =========================================================================

    /// Emit the current queue state to listeners (pi's `_emitQueueUpdate`, L533).
    /// Called by the steering / follow-up queue mutators in [`super::queue`]; the
    /// agent-event handler uses its own queue-update path on splice.
    pub(super) fn emit_queue_update(&self) {
        self.emit(&AgentSessionEvent::QueueUpdate {
            steering: self.get_steering_messages(),
            follow_up: self.get_follow_up_messages(),
        });
    }

    /// Settle the run: clear the active flag and emit `agent_settled` to the
    /// extension runner and then to listeners (pi's `_emitAgentSettled`, L560).
    pub(super) fn emit_agent_settled(&self) {
        self.set_agent_run_active(false);
        self.extension_runner()
            .emit(&ExtensionDispatchEvent::AgentSettled(AgentSettledEvent {}));
        self.emit(&AgentSessionEvent::AgentSettled);
        // unit5: resolving the idle-wait promise (pi `_resolveIdleWaitIfIdle`,
        // L550) lands with the idle-wait PR.
    }

    // =========================================================================
    // Prompting (pi `prompt`/`_runAgentPrompt`/`_handlePostAgentRun`)
    // =========================================================================

    /// Send a prompt to the agent and run the turn to completion (a convenience
    /// wrapper over [`AgentSession::prompt_with`] with pi's default options).
    ///
    /// `source` defaults to [`InputSource::Interactive`]. Template/command
    /// expansion is enabled (pi `expandPromptTemplates: true`).
    pub fn prompt(
        &self,
        text: &str,
        images: Option<Vec<AgentMessage>>,
        source: Option<InputSource>,
    ) -> Result<(), PromptError> {
        self.prompt_with(
            text,
            PromptOptions {
                images,
                source,
                ..PromptOptions::defaults()
            },
        )
    }

    /// Send a prompt to the agent and run the turn to completion (pi's `prompt`,
    /// L1102).
    ///
    /// Preflight validates the model and configured auth (when not streaming),
    /// builds the user message (with any images and any pending next-turn custom
    /// messages), fires `before_agent_start`, and drives the run. When the session
    /// is already streaming, the prompt is routed to the steering / follow-up
    /// queue per `options.streaming_behavior` instead of starting a new turn
    /// (pi L1147).
    pub fn prompt_with(&self, text: &str, options: PromptOptions) -> Result<(), PromptError> {
        // unit5: the `/`-command extension shortcut (pi L1110,
        // `_tryExecuteExtensionCommand`, gated on `expand_prompt_templates`) lands
        // in PR7; non-command text is unaffected.

        // Emit the input event for extension interception (pi L1122). With the
        // StubExtensionRunner `has_handlers` is false, so this is skipped. When
        // streaming, the delivery behavior is reported to handlers; when idle it
        // is `None` (pi `this.isStreaming ? options.streamingBehavior : undefined`).
        let mut current_text = text.to_string();
        let mut current_images = options.images;
        if self.extension_runner().has_handlers("input") {
            let reported_behavior = if self.is_streaming() {
                options.streaming_behavior
            } else {
                None
            };
            let result = self.extension_runner().emit_input(
                &current_text,
                current_images.as_deref(),
                options.source.unwrap_or(InputSource::Interactive),
                reported_behavior,
            );
            match result {
                InputEventResult::Handled => return Ok(()),
                InputEventResult::Transform { text, images } => {
                    current_text = text;
                    if let Some(images) = images {
                        current_images = Some(images);
                    }
                }
                InputEventResult::Continue => {}
            }
        }

        // unit5: skill-command and prompt-template expansion (pi L1140, gated on
        // `expand_prompt_templates`) land in PR7; the text passes through
        // unexpanded.
        let expanded_text = current_text;

        // If streaming, route to the steering / follow-up queue rather than
        // starting a new turn (pi L1147). A steering behavior must be supplied.
        if self.is_streaming() {
            let Some(behavior) = options.streaming_behavior else {
                return Err(PromptError::Preflight(
                    "Agent is already processing. Specify streamingBehavior ('steer' or \
                     'followUp') to queue the message."
                        .to_string(),
                ));
            };
            match behavior {
                StreamingBehavior::FollowUp => self.queue_follow_up(&expanded_text, current_images),
                StreamingBehavior::Steer => self.queue_steer(&expanded_text, current_images),
            }
            return Ok(());
        }

        // Flush any pending bash messages before the new prompt (pi L1163).
        self.flush_pending_bash_messages();

        // Validate model (pi L1166).
        let model = self
            .model()
            .ok_or_else(|| PromptError::Preflight(format_no_model_selected_message()))?;

        // Validate configured auth (pi L1170).
        // unit5: the async env-based `checkAuth` fallback and the OAuth
        // re-login branch (pi L1172-1182) land with the credential-aware runtime
        // work; the synchronous `has_configured_auth` check is faithful for the
        // configured / not-configured cases exercised here.
        if !self.model_runtime().has_configured_auth(&model.provider) {
            return Err(PromptError::Preflight(format_no_api_key_found_message(
                &model.provider,
            )));
        }

        // unit5: the pre-send compaction check (pi L1187, `_checkCompaction`) lands
        // in PR6; the safe default is to skip compaction.

        // Build the user message (pi L1193).
        let mut content = vec![json!({ "type": "text", "text": expanded_text })];
        if let Some(images) = &current_images {
            content.extend(images.iter().cloned());
        }
        let user_message = json!({
            "role": "user",
            "content": content,
            "timestamp": now_ms(),
        });
        let mut messages = vec![user_message];
        // Inject any pending "nextTurn" custom messages alongside the user message,
        // then clear them (pi L1207).
        {
            let mut pending = self.pending_next_turn_messages.lock().unwrap();
            messages.extend(pending.drain(..));
        }

        // Emit before_agent_start (pi L1213). With the StubExtensionRunner this
        // returns None.
        let before_start = self.extension_runner().emit_before_agent_start(
            &expanded_text,
            current_images.as_deref(),
            &self.agent.system_prompt(),
            &Value::Null,
        );
        if before_start.is_some() {
            // unit5: injecting extension custom messages and applying a
            // system-prompt override (pi L1220-1241) land in PR7.
        }

        self.run_agent_prompt(messages)
    }

    /// Drive the agent to completion, looping post-run continuations (pi's
    /// `_runAgentPrompt`, L1049). `pub(super)` so `send_custom_message(triggerTurn)`
    /// in [`super::queue`] can start a turn from a single custom message.
    pub(super) fn run_agent_prompt(&self, messages: Vec<AgentMessage>) -> Result<(), PromptError> {
        self.set_agent_run_active(true);
        let outcome = (|| -> Result<(), AgentError> {
            self.agent.prompt_messages(messages)?;
            while self.handle_post_agent_run() {
                self.agent.continue_()?;
            }
            Ok(())
        })();

        // finally (pi L1056):
        // unit5: resetting `_systemPromptOverride` (pi L1057) lands in PR7.
        self.flush_pending_bash_messages();
        self.emit_agent_settled();

        outcome.map_err(PromptError::Agent)
    }

    /// Decide whether to loop `agent.continue()` again (pi's `_handlePostAgentRun`,
    /// L1063).
    ///
    /// For PR3 this handles the terminal/normal path and queued-message
    /// continuations. The retry (PR5) and compaction (PR6) branches default to no
    /// continuation.
    fn handle_post_agent_run(&self) -> bool {
        let Some(message) = self.last_assistant_message.lock().unwrap().take() else {
            return false;
        };

        // Retryable error: back off and continue the loop for another attempt (pi
        // L1070). `prepare_retry` may decline (retry disabled, budget exhausted, or
        // the backoff aborted), in which case fall through.
        if self.is_retryable_error(&message) && self.prepare_retry(&message) {
            return true;
        }

        // A terminal error after at least one attempt: emit the final failure and
        // reset the counter (pi L1074).
        let stop_reason = message.get("stopReason").and_then(Value::as_str);
        let attempt = *self.retry_attempt.lock().unwrap();
        if stop_reason == Some("error") && attempt > 0 {
            let final_error = message
                .get("errorMessage")
                .and_then(Value::as_str)
                .map(str::to_string);
            self.emit(&AgentSessionEvent::AutoRetryEnd {
                success: false,
                attempt,
                final_error,
            });
            *self.retry_attempt.lock().unwrap() = 0;
        }

        // unit5: the auto-compaction branch (pi L1084, PR6) lands with compaction;
        // its safe default is no continuation.

        // The agent loop drains both queues before emitting agent_end; anything
        // queued during agent_end handlers needs a continuation (pi L1090).
        self.agent.has_queued_messages()
    }

    /// Flush pending bash messages before/after a run (pi's
    /// `_flushPendingBashMessages`, L2732).
    fn flush_pending_bash_messages(&self) {
        // unit5: bash execution and its pending-message plumbing land in PR8; no
        // pending bash messages exist yet, so this is a no-op.
    }
}

#[cfg(test)]
mod tests;
