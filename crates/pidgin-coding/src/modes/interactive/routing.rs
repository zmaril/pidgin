//! Session-event -> chat-region routing (Unit 5, offline-echo slice).
//!
//! This is the Rust analog of the message-list half of pi's `handleEvent` switch
//! (`modes/interactive/interactive-mode.ts:2816-3110`) — the pure dispatch table
//! that turns each emitted [`AgentSessionEvent`] into a small mutation of one
//! render region. It covers the nine **core** variants the offline-echo turn
//! produces (message + tool + turn lifecycle) plus `AgentSettled` (which keys the
//! idle status); the other session-specific variants (compaction / retry /
//! thinking-level / queue / entry / info) are not surfaced by this slice and fall
//! through to a no-op.
//!
//! The routing runs entirely on the main (render) thread: [`ChatState`] owns the
//! chat entries, the in-flight streaming assistant bubble, the live tool panels,
//! and the (placeholder) status line. Only `Send` data ([`AgentSessionEvent`],
//! cloned) crosses the thread boundary from the turn worker; the components stay
//! here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use pidgin_ai::types::{AssistantMessage as AiAssistantMessage, ContentBlock};
use pidgin_tui::renderer::Component;
use serde_json::Value;

use crate::core::agent_session::AgentSessionEvent;
use crate::modes::interactive::components::{
    AssistantMessage, IdleStatus, ToolExecution, ToolExecutionOptions, ToolExecutionResult,
    UserMessage, WorkingStatusIndicator,
};
use crate::modes::interactive::theme::Theme;

/// A chat entry as a shared, render-only [`Component`]. Concrete message
/// components are held here as `Rc<RefCell<dyn Component>>` so the render tree
/// (via [`ChatRegion`]) and the mutator (via a retained typed `Rc`) share one
/// allocation — mutating the typed handle is visible on the next render.
pub type SharedComponent = Rc<RefCell<dyn Component>>;

/// The ordered list of chat entries, shared between [`ChatRegion`] (render) and
/// [`ChatState`] (mutation). Mirrors pi's `chatContainer` children vector.
pub type ChatEntries = Rc<RefCell<Vec<SharedComponent>>>;

/// The status region's current view: an idle placeholder (two blank lines) or a
/// live working spinner. Mirrors pi's `activeStatusIndicator` slot, restricted to
/// the two indicators the offline shell mounts (PR-4C); retry / compaction /
/// branch-summary indicators arrive with the `AgentSessionEvent` seam.
pub enum StatusView {
    /// No turn running: two full-width blank lines.
    Idle(IdleStatus),
    /// A turn is running: the accent-spinner working indicator.
    Working(WorkingStatusIndicator),
}

/// A shared status view, flipped by [`ChatState`] and rendered by [`StatusRegion`].
pub type StatusSlot = Rc<RefCell<StatusView>>;

/// The status-region component: renders whichever [`StatusView`] is currently
/// mounted. pi's interactive shell swaps `activeStatusIndicator` in the same slot.
pub struct StatusRegion {
    slot: StatusSlot,
}

impl StatusRegion {
    /// Wrap a shared status view as a render component.
    pub fn new(slot: StatusSlot) -> Self {
        Self { slot }
    }
}

impl Component for StatusRegion {
    fn render(&self, width: usize) -> Vec<String> {
        match &*self.slot.borrow() {
            StatusView::Idle(idle) => idle.render(width),
            StatusView::Working(working) => working.render(width),
        }
    }
}

/// The chat message-list region component: renders each entry in insertion
/// order, concatenating their lines. This is pi's `chatContainer` (a `Container`
/// of message components) reduced to the render contract the run loop needs.
pub struct ChatRegion {
    entries: ChatEntries,
}

impl ChatRegion {
    /// Wrap a shared entry list as a render component.
    pub fn new(entries: ChatEntries) -> Self {
        Self { entries }
    }
}

impl Component for ChatRegion {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for entry in self.entries.borrow().iter() {
            lines.extend(entry.borrow().render(width));
        }
        lines
    }
}

/// The mutable chat-region state driven by the event router. Holds the entry
/// list, the in-flight streaming assistant component, the live tool panels keyed
/// by tool-call id, and the placeholder status line — the Rust counterpart to
/// pi's `streamingComponent` / `pendingTools` / `activeStatusIndicator` runtime
/// fields.
pub struct ChatState {
    entries: ChatEntries,
    status: StatusSlot,
    theme: Theme,
    cwd: String,
    /// pi's `streamingComponent`: the assistant bubble currently being streamed.
    streaming: Option<Rc<RefCell<AssistantMessage>>>,
    /// pi's `pendingTools`: live tool panels awaiting a result, by tool-call id.
    pending_tools: HashMap<String, Rc<RefCell<ToolExecution>>>,
}

impl ChatState {
    /// Build the router state over a shared entry list and status slot.
    pub fn new(entries: ChatEntries, status: StatusSlot, theme: Theme, cwd: String) -> Self {
        Self {
            entries,
            status,
            theme,
            cwd,
            streaming: None,
            pending_tools: HashMap::new(),
        }
    }

    /// Append a user prompt bubble to the chat list and finalize any in-flight
    /// stream. Called from the editor submit handler for immediate feedback
    /// (before the turn worker echoes the same prompt back as a `user`
    /// `message_start`, which the router deliberately ignores — see
    /// [`ChatState::handle_event`]).
    pub fn push_user_message(&mut self, text: &str) {
        let user = UserMessage::new(text, self.theme.clone(), 1);
        self.entries
            .borrow_mut()
            .push(Rc::new(RefCell::new(user)) as SharedComponent);
    }

    /// Append a plain notice line to the chat list (the shell's notification
    /// surface). Used by the render-thread `/llama` intercept to surface the
    /// `run_llama_command` notification sink and any `UiError::Failed` message,
    /// since the offline shell has no dedicated notification region wired yet
    /// (header/status are placeholder chrome — see [`super::app`]).
    pub fn push_notice(&mut self, text: &str) {
        let notice = pidgin_tui::widgets::Text::new(text, 0, 0, None);
        self.entries
            .borrow_mut()
            .push(Rc::new(RefCell::new(notice)) as SharedComponent);
    }

    /// Route one [`AgentSessionEvent`] to the chat region, mirroring the
    /// message-list branches of pi's `handleEvent`.
    pub fn handle_event(&mut self, event: &AgentSessionEvent) {
        match event {
            // Turn lifecycle -> status region (PR-4C chrome): a working spinner
            // while a turn runs, restored to the idle placeholder once the run
            // fully settles.
            AgentSessionEvent::AgentStart | AgentSessionEvent::TurnStart => {
                self.pending_tools.clear();
                self.set_working();
            }
            AgentSessionEvent::TurnEnd { .. } => {}
            // `agent_end` may be followed by a retry / queued continuation, so it
            // only tears down the in-flight turn state; the idle status is keyed
            // off `AgentSettled` (the true-settle signal) instead, which is correct
            // for live auto-retries later. `will_retry` is ignored here.
            AgentSessionEvent::AgentEnd { .. } => {
                self.streaming = None;
                self.pending_tools.clear();
            }
            AgentSessionEvent::AgentSettled => self.set_idle(),
            // Message list.
            AgentSessionEvent::MessageStart { message } => self.on_message_start(message),
            AgentSessionEvent::MessageUpdate { message, .. } => self.on_message_update(message),
            AgentSessionEvent::MessageEnd { message } => self.on_message_end(message),
            // Tool panels.
            AgentSessionEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => self.on_tool_start(tool_call_id, tool_name, args),
            AgentSessionEvent::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
                ..
            } => self.on_tool_update(tool_call_id, partial_result),
            AgentSessionEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
                ..
            } => self.on_tool_end(tool_call_id, result, *is_error),
            // Session-specific variants (queue / compaction / entry / info /
            // thinking-level / auto-retry) are not surfaced by this slice's
            // message-list router.
            _ => {}
        }
    }

    /// Whether a stream is currently in flight (test/introspection helper).
    pub fn is_streaming(&self) -> bool {
        self.streaming.is_some()
    }

    /// The number of live (unresolved) tool panels (test/introspection helper).
    pub fn pending_tool_count(&self) -> usize {
        self.pending_tools.len()
    }

    // --- message list -------------------------------------------------------

    fn on_message_start(&mut self, message: &Value) {
        // Only assistant messages open a streaming bubble; user/toolResult roles
        // are already shown (submit handler) or not surfaced in the chat list.
        if role_of(message) != Some("assistant") {
            return;
        }
        let Some(assistant) = as_assistant(message) else {
            return;
        };
        let component = AssistantMessage::new(
            Some(&assistant),
            self.theme.clone(),
            false,
            "Thinking...",
            1,
        );
        let shared = Rc::new(RefCell::new(component));
        self.streaming = Some(Rc::clone(&shared));
        self.entries.borrow_mut().push(shared as SharedComponent);
    }

    fn on_message_update(&mut self, message: &Value) {
        self.apply_streaming_content(message);
    }

    fn on_message_end(&mut self, message: &Value) {
        self.apply_streaming_content(message);
        self.streaming = None;
    }

    /// Push an assistant message value into the in-flight streaming bubble, if
    /// any. Shared by `message_update` (keep streaming) and `message_end`
    /// (finalize, then the caller clears `streaming`).
    fn apply_streaming_content(&self, message: &Value) {
        if role_of(message) != Some("assistant") {
            return;
        }
        let Some(assistant) = as_assistant(message) else {
            return;
        };
        if let Some(streaming) = &self.streaming {
            streaming.borrow_mut().update_content(&assistant);
        }
    }

    // --- tool panels --------------------------------------------------------

    fn on_tool_start(&mut self, tool_call_id: &str, tool_name: &str, args: &Value) {
        let panel = self
            .pending_tools
            .entry(tool_call_id.to_string())
            .or_insert_with(|| {
                let component = ToolExecution::new(
                    tool_name,
                    tool_call_id,
                    args.clone(),
                    ToolExecutionOptions::default(),
                    None,
                    &self.cwd,
                    self.theme.clone(),
                );
                let shared = Rc::new(RefCell::new(component));
                self.entries
                    .borrow_mut()
                    .push(Rc::clone(&shared) as SharedComponent);
                shared
            });
        panel.borrow_mut().mark_execution_started();
    }

    fn on_tool_update(&mut self, tool_call_id: &str, partial_result: &Value) {
        if let Some(panel) = self.pending_tools.get(tool_call_id) {
            panel
                .borrow_mut()
                .update_result(tool_result_from(partial_result, false), true);
        }
    }

    fn on_tool_end(&mut self, tool_call_id: &str, result: &Value, is_error: bool) {
        if let Some(panel) = self.pending_tools.remove(tool_call_id) {
            panel
                .borrow_mut()
                .update_result(tool_result_from(result, is_error), false);
        }
    }

    // --- status region ------------------------------------------------------

    /// Mount the working spinner (pi's default working message). The spinner is
    /// rendered at frame 0 statically — the offline run loop has no timer tick to
    /// animate it. PR-4C follow-up: drive [`WorkingStatusIndicator::tick`] from a
    /// render-loop timer to animate the spinner, matching pi's `setInterval`.
    fn set_working(&self) {
        *self.status.borrow_mut() =
            StatusView::Working(WorkingStatusIndicator::new(&self.theme, "Working...", None));
    }

    /// Restore the idle placeholder (two blank lines).
    fn set_idle(&self) {
        *self.status.borrow_mut() = StatusView::Idle(IdleStatus);
    }
}

/// The `role` field of an [`pidgin_agent::types::AgentMessage`] value, if any.
fn role_of(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

/// Deserialize an agent message value into an [`AiAssistantMessage`]. Returns
/// `None` if the value is not an assistant-shaped message.
fn as_assistant(message: &Value) -> Option<AiAssistantMessage> {
    serde_json::from_value(message.clone()).ok()
}

/// Map a tool result/partial-result [`Value`] (the serialized `AgentToolResult`
/// the agent loop emits) into a [`ToolExecutionResult`] the panel renders.
fn tool_result_from(result: &Value, is_error: bool) -> ToolExecutionResult {
    let content = result
        .get("content")
        .and_then(|c| serde_json::from_value::<Vec<ContentBlock>>(c.clone()).ok())
        .unwrap_or_default();
    let details = result.get("details").cloned().unwrap_or(Value::Null);
    ToolExecutionResult {
        content,
        is_error,
        details,
    }
}
