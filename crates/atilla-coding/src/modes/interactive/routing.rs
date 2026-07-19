//! Agent-event -> chat-region routing (Unit 4, PR-4B, offline faux-turn slice).
//!
//! This is the Rust analog of the message-list half of pi's `handleEvent` switch
//! (`modes/interactive/interactive-mode.ts:2816-3110`) — the pure dispatch table
//! that turns each emitted agent event into a small mutation of one render
//! region. It deliberately covers only the **core** [`AgentEvent`] variants the
//! offline faux turn produces (message + tool + turn lifecycle); the compaction /
//! retry / thinking-level / queue branches pi also handles arrive with the typed
//! `AgentSessionEvent` seam (Unit 5) and are out of scope here.
//!
//! The routing runs entirely on the main (render) thread: [`ChatState`] owns the
//! chat entries, the in-flight streaming assistant bubble, the live tool panels,
//! and the (placeholder) status line. Only `Send` data (`AgentEvent`) crosses the
//! thread boundary from the turn worker; the components stay here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use atilla_agent::types::AgentEvent;
use atilla_ai::types::{AssistantMessage as AiAssistantMessage, ContentBlock};
use atilla_tui::renderer::Component;
use serde_json::Value;

use crate::modes::interactive::components::{
    AssistantMessage, ToolExecution, ToolExecutionOptions, ToolExecutionResult, UserMessage,
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

/// A shared line buffer for a placeholder chrome region (status line). Held both
/// by the region's render component and by [`ChatState`] for updates.
pub type StatusHandle = Rc<RefCell<Vec<String>>>;

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
    status: StatusHandle,
    theme: Theme,
    cwd: String,
    /// pi's `streamingComponent`: the assistant bubble currently being streamed.
    streaming: Option<Rc<RefCell<AssistantMessage>>>,
    /// pi's `pendingTools`: live tool panels awaiting a result, by tool-call id.
    pending_tools: HashMap<String, Rc<RefCell<ToolExecution>>>,
}

impl ChatState {
    /// Build the router state over a shared entry list and status handle.
    pub fn new(entries: ChatEntries, status: StatusHandle, theme: Theme, cwd: String) -> Self {
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

    /// Route one core [`AgentEvent`] to the chat region, mirroring the
    /// message-list branches of pi's `handleEvent`.
    pub fn handle_event(&mut self, event: &AgentEvent) {
        match event {
            // Turn lifecycle -> status placeholder (PR-4C chrome).
            AgentEvent::AgentStart | AgentEvent::TurnStart => {
                self.pending_tools.clear();
                self.set_status("Working...");
            }
            AgentEvent::TurnEnd { .. } => {}
            AgentEvent::AgentEnd { .. } => {
                self.streaming = None;
                self.pending_tools.clear();
                self.clear_status();
            }
            // Message list.
            AgentEvent::MessageStart { message } => self.on_message_start(message),
            AgentEvent::MessageUpdate { message, .. } => self.on_message_update(message),
            AgentEvent::MessageEnd { message } => self.on_message_end(message),
            // Tool panels.
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => self.on_tool_start(tool_call_id, tool_name, args),
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
                ..
            } => self.on_tool_update(tool_call_id, partial_result),
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
                ..
            } => self.on_tool_end(tool_call_id, result, *is_error),
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

    fn on_message_end(&mut self, message: &Value) {
        if role_of(message) != Some("assistant") {
            return;
        }
        let Some(assistant) = as_assistant(message) else {
            return;
        };
        if let Some(streaming) = &self.streaming {
            streaming.borrow_mut().update_content(&assistant);
        }
        self.streaming = None;
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

    // --- status placeholder -------------------------------------------------

    fn set_status(&self, text: &str) {
        *self.status.borrow_mut() = vec![text.to_string()];
    }

    fn clear_status(&self) {
        self.status.borrow_mut().clear();
    }
}

/// The `role` field of an [`atilla_agent::types::AgentMessage`] value, if any.
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
