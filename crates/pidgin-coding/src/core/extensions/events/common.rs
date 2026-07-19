//! Shared opaque payload aliases for the extension event surface.
//!
//! Faithful mirror of the value types referenced by pi's extension event
//! interfaces in `packages/coding-agent/src/core/extensions/types.ts`. Each of
//! these upstream types belongs to a subsystem that is not yet ported (the agent
//! message model, the TUI content model, the model registry, compaction, the
//! session tree). Because the extension event surface crosses the JavaScript
//! boundary as JSON — only [`serde_json::Value`] ever crosses, per
//! `notes/startup/deep-hooks.md` — the port models each not-yet-ported payload
//! as an opaque [`Value`], preserving pi's wire shape without dragging in the
//! upstream subsystem. The alias name records which pi type the field mirrors.
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/types.ts`.

// straitjacket-allow-file:duplication

use serde_json::Value;

/// A single agent conversation message (pi's `AgentMessage`). Opaque on the
/// wire; matches `pidgin_agent::types::AgentMessage`, which is likewise a
/// [`Value`].
pub type AgentMessage = Value;

/// A finalized tool-result message (pi's `ToolResultMessage`). Opaque [`Value`].
pub type ToolResultMessage = Value;

/// A streaming assistant-message delta (pi's `AssistantMessageEvent`). Opaque
/// [`Value`].
pub type AssistantMessageEvent = Value;

/// Image content attached to a prompt or result (pi's `ImageContent`). Opaque
/// [`Value`].
pub type ImageContent = Value;

/// Text content in a tool result (pi's `TextContent`). Opaque [`Value`].
pub type TextContent = Value;

/// A selected model descriptor (pi's `Model<any>`). Opaque [`Value`].
pub type Model = Value;

/// A thinking-effort level (pi's `ThinkingLevel`). Opaque [`Value`].
pub type ThinkingLevel = Value;

/// Provider HTTP headers (pi's `ProviderHeaders`). Opaque [`Value`]; a `null`
/// entry deletes that header upstream.
pub type ProviderHeaders = Value;

/// Structured options used to assemble the system prompt (pi's
/// `BuildSystemPromptOptions`). Opaque [`Value`].
pub type BuildSystemPromptOptions = Value;

/// A persisted session entry (pi's `SessionEntry`). Opaque [`Value`].
pub type SessionEntry = Value;

/// A compaction-boundary entry (pi's `CompactionEntry`). Opaque [`Value`].
pub type CompactionEntry = Value;

/// Preparation data gathered before a compaction (pi's `CompactionPreparation`).
/// Opaque [`Value`].
pub type CompactionPreparation = Value;

/// A completed compaction result an extension can supply (pi's
/// `CompactionResult`). Opaque [`Value`].
pub type CompactionResult = Value;

/// A branch-summary session entry (pi's `BranchSummaryEntry`). Opaque [`Value`].
pub type BranchSummaryEntry = Value;

/// A custom (extension-authored) message (pi's `CustomMessage`). Opaque
/// [`Value`].
pub type CustomMessage = Value;

/// Bash execution operations an extension can override (pi's `BashOperations`).
/// Opaque [`Value`].
pub type BashOperations = Value;

/// A full bash execution result an extension can substitute (pi's `BashResult`).
/// Opaque [`Value`].
pub type BashResult = Value;
