//! Agent-lifecycle and context extension events.
//!
//! Faithful port of the agent-group event interfaces from
//! `packages/coding-agent/src/core/extensions/types.ts`: `ContextEvent`,
//! `BeforeAgentStartEvent`, `AgentStartEvent`, `AgentEndEvent`,
//! `AgentSettledEvent`, plus the `context` and `before_agent_start` result
//! shapes.
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/types.ts`.

// straitjacket-allow-file:duplication

use serde::{Deserialize, Serialize};

use super::common::{AgentMessage, BuildSystemPromptOptions, CustomMessage, ImageContent};

/// Fired before each LLM call; can rewrite the message array (pi's
/// `ContextEvent`, `types.ts:658`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextEvent {
    /// The messages that will be sent to the model.
    pub messages: Vec<AgentMessage>,
}

/// Result of a `context` handler (pi's `ContextEventResult`, `types.ts:1051`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextEventResult {
    /// Replacement messages; `None` leaves the array unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<AgentMessage>>,
}

/// Fired after the user submits a prompt but before the agent loop (pi's
/// `BeforeAgentStartEvent`, `types.ts:687`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeAgentStartEvent {
    /// The raw user prompt text (after expansion).
    pub prompt: String,
    /// Images attached to the user prompt, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageContent>>,
    /// The fully assembled system prompt string.
    pub system_prompt: String,
    /// Structured options used to build the system prompt.
    pub system_prompt_options: BuildSystemPromptOptions,
}

/// Result of a `before_agent_start` handler (pi's `BeforeAgentStartEventResult`,
/// `types.ts:1082`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeAgentStartEventResult {
    /// A custom message to inject (a `Pick` of pi's `CustomMessage`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<CustomMessage>,
    /// Replace the system prompt for this turn; chained across extensions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

/// Fired when an agent loop starts (pi's `AgentStartEvent`, `types.ts:700`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStartEvent {}

/// Fired when an agent loop ends (pi's `AgentEndEvent`, `types.ts:705`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEndEvent {
    /// The final message array for the run.
    pub messages: Vec<AgentMessage>,
}

/// Fired after an agent run has fully settled — no retry, compaction, or queued
/// continuation will run (pi's `AgentSettledEvent`, `types.ts:711`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSettledEvent {}
