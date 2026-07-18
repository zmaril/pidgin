//! Tool-execution extension events.
//!
//! Faithful port of the tool-group event interfaces from
//! `packages/coding-agent/src/core/extensions/types.ts`:
//! `ToolExecutionStartEvent`, `ToolExecutionUpdateEvent`,
//! `ToolExecutionEndEvent`, `ToolCallEvent`, `ToolResultEvent`, plus the
//! `tool_call` and `tool_result` result shapes.
//!
//! # Per-tool narrowing
//!
//! Upstream, `ToolCallEvent` and `ToolResultEvent` are *unions* whose members
//! (`BashToolCallEvent`, `ReadToolCallEvent`, …) share the same discriminant
//! (`type: "tool_call"` / `"tool_result"`) and differ only by the `toolName`
//! literal and the static type of `input` / `details`. This is a TypeScript
//! narrowing convenience — the wire shape is identical across members. The port
//! therefore models each as a single struct carrying `tool_name: String` and an
//! opaque `input` / `details` [`Value`]; the tool-specific input and detail
//! schemas (`BashToolInput`, `ReadToolDetails`, …) are not re-derived. This
//! preserves pi's wire contract exactly while collapsing the redundant union
//! arms — matching how the existing [`super::super::types::ToolDefinition`] port
//! keeps the TypeBox parameter schema opaque.
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/types.ts`.

// straitjacket-allow-file:duplication

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Fired when a tool starts executing (pi's `ToolExecutionStartEvent`,
/// `types.ts:750`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionStartEvent {
    /// The tool-call id.
    pub tool_call_id: String,
    /// The tool name.
    pub tool_name: String,
    /// The validated tool arguments (pi types this `any`).
    pub args: Value,
}

/// Fired during tool execution with partial/streaming output (pi's
/// `ToolExecutionUpdateEvent`, `types.ts:758`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionUpdateEvent {
    /// The tool-call id.
    pub tool_call_id: String,
    /// The tool name.
    pub tool_name: String,
    /// The validated tool arguments (pi types this `any`).
    pub args: Value,
    /// The partial result so far (pi types this `any`).
    pub partial_result: Value,
}

/// Fired when a tool finishes executing (pi's `ToolExecutionEndEvent`,
/// `types.ts:767`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionEndEvent {
    /// The tool-call id.
    pub tool_call_id: String,
    /// The tool name.
    pub tool_name: String,
    /// The tool result (pi types this `any`).
    pub result: Value,
    /// Whether the result is an error.
    pub is_error: bool,
}

/// Fired before a tool executes; can block or mutate `input` (pi's
/// `ToolCallEvent` union, `types.ts:892`).
///
/// `input` is mutable: mutate it in place to patch tool arguments before
/// execution. Later `tool_call` handlers see earlier mutations, and no
/// re-validation is performed after mutation. See the per-tool narrowing note in
/// the module docs for why the union arms collapse to `tool_name` + opaque
/// `input`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallEvent {
    /// The tool-call id.
    pub tool_call_id: String,
    /// The tool name (`"bash"`, `"read"`, … or a custom tool name).
    pub tool_name: String,
    /// The tool-call arguments, mutable in place by handlers.
    pub input: Value,
}

/// Result of a `tool_call` handler (pi's `ToolCallEventResult`, `types.ts:1057`).
///
/// To modify arguments, mutate `event.input` in place instead of returning them
/// here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallEventResult {
    /// When `Some(true)`, block tool execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<bool>,
    /// An optional human-readable block reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Fired after a tool executes; can modify the result (pi's `ToolResultEvent`
/// union, `types.ts:951`).
///
/// See the per-tool narrowing note in the module docs for why the union arms
/// collapse to `tool_name` + opaque `details`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultEvent {
    /// The tool-call id.
    pub tool_call_id: String,
    /// The tool name (`"bash"`, `"read"`, … or a custom tool name).
    pub tool_name: String,
    /// The tool-call arguments.
    pub input: Value,
    /// The result content blocks — each a pi `TextContent | ImageContent`, kept
    /// opaque (see [`ToolResultContent`]).
    pub content: Vec<ToolResultContent>,
    /// Whether the result is an error.
    pub is_error: bool,
    /// Tool-specific structured details (pi's per-tool `details`, opaque here).
    pub details: Value,
}

/// A single tool-result content block (pi's `TextContent | ImageContent`).
///
/// Both union arms are opaque [`Value`] payloads discriminated at runtime by
/// their own `type` field, so the port keeps the array element as an opaque
/// [`Value`] rather than re-deriving the TUI content model. The alias records
/// that the element mirrors pi's `(TextContent | ImageContent)`.
pub type ToolResultContent = Value;

/// Result of a `tool_result` handler (pi's `ToolResultEventResult`,
/// `types.ts:1071`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultEventResult {
    /// Replacement content blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ToolResultContent>>,
    /// Replacement structured details (pi types this `unknown`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Replacement error flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}
