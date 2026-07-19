//! The steering / follow-up queue surface, ported from pi's `AgentSession`
//! (`packages/coding-agent/src/core/agent-session.ts`, the queue section around
//! L1323-1520).
//!
//! pi keeps two string-mirror queues (`_steeringMessages` / `_followUpMessages`)
//! for the UI alongside the agent-core message queues. This module carries the
//! methods that push onto those mirrors and enqueue onto the agent:
//!
//! * [`AgentSession::steer`] / [`AgentSession::follow_up`] — the public queue
//!   entrypoints (pi `steer` L1323 / `followUp` L1343): reject extension
//!   commands, then enqueue.
//! * [`AgentSession::queue_steer`] / [`AgentSession::queue_follow_up`] — the
//!   internal enqueue path (pi `_queueSteer` L1359 / `_queueFollowUp` L1376):
//!   push the mirror, emit `queue_update`, enqueue on the agent.
//! * [`AgentSession::send_user_message`] (pi L1451) and
//!   [`AgentSession::send_custom_message`] (pi `sendCustomMessage` L1418) — the
//!   extension-facing message-delivery methods.
//! * [`AgentSession::clear_queue`] (pi L1498), [`AgentSession::pending_message_count`]
//!   (pi L1509), and the queue getters (pi L1514/L1519).
//!
//! The queue-removal branch of `_handleAgentEvent` (splice on user
//! `message_start`) lives with the agent-event handler in [`super::turn`].
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/agent-session.ts`.

// straitjacket-allow-file:duplication

use serde_json::{json, Value};

use atilla_agent::types::AgentMessage;

use crate::core::extensions::events::common::ImageContent;
use crate::core::extensions::events::selection::{InputSource, StreamingBehavior};

use super::events::AgentSessionEvent;
use super::session::AgentSession;
use super::turn::{now_ms, PromptError, PromptOptions};

/// How a [`AgentSession::send_custom_message`] is delivered (pi's `deliverAs`
/// option, `agent-session.ts:1419`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliverAs {
    /// Interrupt the in-flight turn (agent-core steering queue).
    Steer,
    /// Wait until the agent would otherwise stop (agent-core follow-up queue).
    FollowUp,
    /// Carry the message into the next prompt turn as context.
    NextTurn,
}

/// A custom message to inject (pi's `Pick<CustomMessage, "customType" | "content"
/// | "display" | "details">`, `agent-session.ts:1418`).
#[derive(Debug, Clone)]
pub struct CustomMessageInput {
    /// The extension-defined message subtype (pi `customType`).
    pub custom_type: String,
    /// The message content; `Value::Null` normalizes to `[]` at ingestion.
    pub content: Value,
    /// Whether the message is shown in the transcript (pi `display`).
    pub display: bool,
    /// Optional structured details (pi `details`).
    pub details: Option<Value>,
}

/// User-message content for [`AgentSession::send_user_message`] (pi's `string |
/// (TextContent | ImageContent)[]`, `agent-session.ts:1451`).
pub enum UserMessageContent {
    /// A plain text message.
    Text(String),
    /// A content-part array (text parts joined; image parts collected).
    Parts(Vec<Value>),
}

impl From<&str> for UserMessageContent {
    fn from(text: &str) -> Self {
        UserMessageContent::Text(text.to_string())
    }
}

impl From<String> for UserMessageContent {
    fn from(text: String) -> Self {
        UserMessageContent::Text(text)
    }
}

/// Build a `{ role: "user", content, timestamp }` message from text + images
/// (pi's inline `{ role: "user", content, timestamp: Date.now() }` in
/// `_queueSteer` / `_queueFollowUp`).
fn user_agent_message(text: &str, images: Option<&[ImageContent]>) -> AgentMessage {
    let mut content = vec![json!({ "type": "text", "text": text })];
    if let Some(images) = images {
        content.extend(images.iter().cloned());
    }
    json!({
        "role": "user",
        "content": content,
        "timestamp": now_ms(),
    })
}

impl AgentSession {
    // =========================================================================
    // Public queue entrypoints (pi `steer` / `followUp`)
    // =========================================================================

    /// Queue a steering message while the agent is running (pi's `steer`, L1323).
    ///
    /// Delivered after the current assistant turn finishes its tool calls, before
    /// the next LLM call. Rejects extension commands (they cannot be queued).
    ///
    /// unit5: skill-command and prompt-template expansion (pi L1330) land in PR7;
    /// the text passes through unexpanded.
    pub fn steer(&self, text: &str, images: Option<Vec<ImageContent>>) -> Result<(), PromptError> {
        if text.starts_with('/') {
            self.throw_if_extension_command(text)?;
        }
        self.queue_steer(text, images);
        Ok(())
    }

    /// Queue a follow-up message to run after the agent finishes (pi's `followUp`,
    /// L1343).
    ///
    /// Delivered only when the agent has no more tool calls or steering messages.
    /// Rejects extension commands (they cannot be queued).
    ///
    /// unit5: skill-command and prompt-template expansion (pi L1350) land in PR7;
    /// the text passes through unexpanded.
    pub fn follow_up(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
    ) -> Result<(), PromptError> {
        if text.starts_with('/') {
            self.throw_if_extension_command(text)?;
        }
        self.queue_follow_up(text, images);
        Ok(())
    }

    // =========================================================================
    // Internal enqueue path (pi `_queueSteer` / `_queueFollowUp`)
    // =========================================================================

    /// Enqueue an already-expanded steering message (pi's `_queueSteer`, L1359):
    /// push the mirror, emit `queue_update`, enqueue on the agent.
    pub(super) fn queue_steer(&self, text: &str, images: Option<Vec<ImageContent>>) {
        self.steering_messages
            .lock()
            .unwrap()
            .push(text.to_string());
        self.emit_queue_update();
        self.agent
            .steer(user_agent_message(text, images.as_deref()));
    }

    /// Enqueue an already-expanded follow-up message (pi's `_queueFollowUp`,
    /// L1376): push the mirror, emit `queue_update`, enqueue on the agent.
    pub(super) fn queue_follow_up(&self, text: &str, images: Option<Vec<ImageContent>>) {
        self.follow_up_messages
            .lock()
            .unwrap()
            .push(text.to_string());
        self.emit_queue_update();
        self.agent
            .follow_up(user_agent_message(text, images.as_deref()));
    }

    /// Reject queueing an extension command (pi's `_throwIfExtensionCommand`,
    /// L1393). The command name is the text after the leading `/`, up to the first
    /// space.
    fn throw_if_extension_command(&self, text: &str) -> Result<(), PromptError> {
        let without_slash = &text[1..];
        let command_name = without_slash
            .split_once(' ')
            .map(|(name, _)| name)
            .unwrap_or(without_slash);
        if self.extension_runner().get_command(command_name).is_some() {
            return Err(PromptError::Preflight(format!(
                "Extension command \"/{command_name}\" cannot be queued. Use prompt() or execute \
                 the command when not streaming."
            )));
        }
        Ok(())
    }

    // =========================================================================
    // Message delivery (pi `sendCustomMessage` / `sendUserMessage`)
    // =========================================================================

    /// Send a custom message to the session (pi's `sendCustomMessage`, L1418).
    ///
    /// Delivery depends on `deliver_as` and the streaming state:
    /// * `NextTurn` — queued for the next prompt turn's context.
    /// * streaming — queued on the agent steering / follow-up queue.
    /// * `trigger_turn` (idle) — appended and a new turn is started.
    /// * otherwise (idle) — appended and persisted with no turn.
    pub fn send_custom_message(
        &self,
        message: CustomMessageInput,
        trigger_turn: bool,
        deliver_as: Option<DeliverAs>,
    ) -> Result<(), PromptError> {
        // Untyped extensions can pass null/missing content; normalize at ingestion.
        let normalized_content = if message.content.is_null() {
            json!([])
        } else {
            message.content.clone()
        };
        let app_message: AgentMessage = json!({
            "role": "custom",
            "customType": message.custom_type,
            "content": normalized_content,
            "display": message.display,
            "details": message.details,
            "timestamp": now_ms(),
        });

        if deliver_as == Some(DeliverAs::NextTurn) {
            self.pending_next_turn_messages
                .lock()
                .unwrap()
                .push(app_message);
        } else if self.is_streaming() {
            if deliver_as == Some(DeliverAs::FollowUp) {
                self.agent.follow_up(app_message);
            } else {
                self.agent.steer(app_message);
            }
        } else if trigger_turn {
            self.run_agent_prompt(vec![app_message])?;
        } else {
            self.agent.push_message(app_message.clone());
            self.session_manager().append_custom_message_entry(
                &message.custom_type,
                message.content,
                message.display,
                message.details,
            );
            // (the `MutexGuard` from `session_manager()` is dropped here, before
            // the synchronous listener emits below.)
            self.emit(&AgentSessionEvent::MessageStart {
                message: app_message.clone(),
            });
            self.emit(&AgentSessionEvent::MessageEnd {
                message: app_message,
            });
        }
        Ok(())
    }

    /// Send a user message to the agent, always triggering a turn (pi's
    /// `sendUserMessage`, L1451).
    ///
    /// When the agent is streaming, `deliver_as` selects how to queue the message.
    /// Routed through [`AgentSession::prompt_with`] with template expansion
    /// disabled and source `extension`.
    pub fn send_user_message(
        &self,
        content: impl Into<UserMessageContent>,
        deliver_as: Option<StreamingBehavior>,
    ) -> Result<(), PromptError> {
        let (text, images) = match content.into() {
            UserMessageContent::Text(text) => (text, None),
            UserMessageContent::Parts(parts) => {
                let mut text_parts: Vec<String> = Vec::new();
                let mut images: Vec<Value> = Vec::new();
                for part in parts {
                    if part.get("type").and_then(Value::as_str) == Some("text") {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            text_parts.push(text.to_string());
                        }
                    } else {
                        images.push(part);
                    }
                }
                let images = if images.is_empty() {
                    None
                } else {
                    Some(images)
                };
                (text_parts.join("\n"), images)
            }
        };

        self.prompt_with(
            &text,
            PromptOptions {
                expand_prompt_templates: false,
                images,
                streaming_behavior: deliver_as,
                source: Some(InputSource::Extension),
            },
        )
    }

    // =========================================================================
    // Queue inspection / clearing (pi `clearQueue` / `pendingMessageCount`)
    // =========================================================================

    /// Clear all queued messages and return them (pi's `clearQueue`, L1498).
    ///
    /// Snapshots both mirrors, clears them and the agent-core queues, and emits a
    /// `queue_update`. Useful for restoring queued text to the editor on abort.
    pub fn clear_queue(&self) -> (Vec<String>, Vec<String>) {
        let steering = self.get_steering_messages();
        let follow_up = self.get_follow_up_messages();
        self.steering_messages.lock().unwrap().clear();
        self.follow_up_messages.lock().unwrap().clear();
        self.agent.clear_all_queues();
        self.emit_queue_update();
        (steering, follow_up)
    }

    /// The number of pending messages across both mirror queues (pi's
    /// `pendingMessageCount`, L1509).
    pub fn pending_message_count(&self) -> usize {
        self.steering_messages.lock().unwrap().len() + self.follow_up_messages.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests;
