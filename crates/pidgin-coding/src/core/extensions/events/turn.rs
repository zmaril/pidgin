//! Turn- and message-lifecycle extension events.
//!
//! Faithful port of the turn/message-group event interfaces from
//! `packages/coding-agent/src/core/extensions/types.ts`: `TurnStartEvent`,
//! `TurnEndEvent`, `MessageStartEvent`, `MessageUpdateEvent`, `MessageEndEvent`,
//! plus the `message_end` result shape.
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/types.ts`.

use serde::{Deserialize, Serialize};

use super::common::{AgentMessage, AssistantMessageEvent, ToolResultMessage};

/// Fired at the start of each turn (pi's `TurnStartEvent`, `types.ts:716`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartEvent {
    /// The zero-based index of the turn.
    pub turn_index: i64,
    /// A millisecond timestamp for the turn start.
    pub timestamp: i64,
}

/// Fired at the end of each turn (pi's `TurnEndEvent`, `types.ts:723`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnEndEvent {
    /// The zero-based index of the turn.
    pub turn_index: i64,
    /// The assistant message produced this turn.
    pub message: AgentMessage,
    /// Tool-result messages produced this turn.
    pub tool_results: Vec<ToolResultMessage>,
}

/// Fired when a message starts — user, assistant, or tool result (pi's
/// `MessageStartEvent`, `types.ts:731`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageStartEvent {
    /// The message that is starting.
    pub message: AgentMessage,
}

/// Fired during assistant-message streaming with token-by-token updates (pi's
/// `MessageUpdateEvent`, `types.ts:737`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageUpdateEvent {
    /// The message being streamed.
    pub message: AgentMessage,
    /// The streaming delta for this update.
    pub assistant_message_event: AssistantMessageEvent,
}

/// Fired when a message ends (pi's `MessageEndEvent`, `types.ts:744`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageEndEvent {
    /// The finalized message.
    pub message: AgentMessage,
}

/// Result of a `message_end` handler (pi's `MessageEndEventResult`,
/// `types.ts:1077`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MessageEndEventResult {
    /// Replace the finalized message; the replacement must keep the original
    /// role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<AgentMessage>,
}
