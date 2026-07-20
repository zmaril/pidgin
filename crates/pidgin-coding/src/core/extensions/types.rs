//! Extension type surface ported from
//! `packages/coding-agent/src/core/extensions/types.ts`.
//!
//! Only the two types on the exec-tools tool-registry critical path are ported
//! here: [`ToolDefinition`] (pi's `ToolDefinition`, `types.ts:439`) and
//! [`ExtensionContext`] (pi's `ExtensionContext`, `types.ts:304`). The extension
//! engine/loader and the remaining extension types are deferred to a later
//! thread.
//!
//! # Reuse of the agent-loop boundary
//!
//! pi's `ToolDefinition` is a superset of the agent runtime's `AgentTool`: same
//! metadata and the same `execute`/`prepareArguments` behavior, plus UI-render
//! hooks and an [`ExtensionContext`] threaded into `execute`. The port therefore
//! reuses [`pidgin_agent::types`] wholesale — [`AgentToolResult`],
//! [`AgentToolUpdateCallback`], and [`ToolExecutionMode`] — rather than
//! re-porting them, and mirrors [`pidgin_agent::types::AgentTool`]'s field and
//! closure shapes exactly, with the single documented addition of the `ctx`
//! argument on [`execute`](ToolDefinition::execute).
//!
//! # Deferred
//!
//! `renderCall` and `renderResult` (`types.ts:472`/`types.ts:475`) depend on the
//! TUI `Theme`/`Component` layer, which is not ported; they are omitted. pi-ai's
//! base `Tool` is not ported either, so its `name`/`description`/`parameters`
//! fields are inlined here (as pi's interface flattens them).
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/types.ts`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use pidgin_agent::types::{AgentToolResult, AgentToolUpdateCallback, ToolExecutionMode};
use pidgin_ai::seams::AbortSignal;

// ---------------------------------------------------------------------------
// Extension context (`types.ts:304`)
// ---------------------------------------------------------------------------

/// Context passed to extension event handlers and tool `execute` (pi's
/// `ExtensionContext`, `types.ts:304`).
///
/// pi's interface exposes ~17 capability members (`ui`, `mode`, `cwd`,
/// `sessionManager`, `modelRegistry`, `abort()`, `compact()`, …), all of which
/// drag in large subsystems that are not yet ported. The tool-registry wrapper
/// on the exec-tools critical path reads **none** of them: it only needs a value
/// it can construct via a `ctxFactory` and hand to
/// [`ToolDefinition::execute`]. The port therefore models `ExtensionContext` as
/// an opaque marker trait — any host type may implement it — and defers the
/// capability surface to the full extension port.
pub trait ExtensionContext {}

// ---------------------------------------------------------------------------
// Render shell (`types.ts:456`)
// ---------------------------------------------------------------------------

/// Controls whether the tool-execution component renders the standard colored
/// shell or the tool renders its own framing (pi's `renderShell`,
/// `types.ts:456`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderShell {
    /// Render the standard colored shell around the tool result (`"default"`).
    Default,
    /// The tool renders its own framing (`"self"`).
    #[serde(rename = "self")]
    SelfRender,
}

// ---------------------------------------------------------------------------
// Tool definition closures (`types.ts:461`, `types.ts:472`)
// ---------------------------------------------------------------------------

/// Optional compatibility shim that rewrites raw tool-call arguments before
/// schema validation (pi's `ToolDefinition.prepareArguments`, `types.ts:461`).
///
/// pi types this `(args: unknown) => Static<TParams>`; the port keeps both sides
/// opaque as owned [`Value`]s. This is the by-value analog of
/// [`pidgin_agent::types::PrepareArguments`] (which borrows); the extension
/// registry takes ownership of the raw arguments, so the by-value shape matches
/// pi's call site.
pub type PrepareArguments = Arc<dyn Fn(Value) -> Value + Send + Sync>;

/// Executes a tool call (pi's `ToolDefinition.execute`, `types.ts:472`).
///
/// This mirrors [`pidgin_agent::types::AgentToolExecute`] exactly — the
/// tool-call id, the validated arguments (`Static<TParams>` → [`Value`]), the
/// optional abort signal, and the optional update callback — with the **single**
/// addition of a fifth `ctx: &dyn ExtensionContext` argument. The tool-registry
/// wrapper injects `ctx` and drops it when adapting a [`ToolDefinition`] down to
/// an `AgentTool`. pi returns `Promise<AgentToolResult<TDetails>>`; the eager
/// port returns [`AgentToolResult`] directly.
pub type ToolDefinitionExecute = Arc<
    dyn Fn(
            &str,
            &Value,
            Option<&AbortSignal>,
            Option<&AgentToolUpdateCallback>,
            &dyn ExtensionContext,
        ) -> AgentToolResult
        + Send
        + Sync,
>;

// ---------------------------------------------------------------------------
// Tool definition (`types.ts:439`)
// ---------------------------------------------------------------------------

/// Tool definition for `registerTool()` (pi's `ToolDefinition`, `types.ts:439`).
///
/// A superset of [`pidgin_agent::types::AgentTool`]: the same metadata and
/// behavior fields (mirrored one-for-one) plus prompt pass-through hints and an
/// [`ExtensionContext`]-aware [`execute`](Self::execute). The UI-render hooks
/// (`renderCall`/`renderResult`) are deferred with the rest of the TUI layer.
///
/// Runtime-only (carries closures); not serde. Where the metadata fields would
/// cross pi's wire they use pi's camelCase names (`promptSnippet`,
/// `promptGuidelines`, `renderShell`, `executionMode`); [`RenderShell`] and
/// [`ToolExecutionMode`] carry their own serde renaming.
#[derive(Clone)]
pub struct ToolDefinition {
    /// Tool name used in LLM tool calls (inlined pi-ai `Tool.name`).
    pub name: String,
    /// Human-readable label for UI display.
    pub label: String,
    /// Description for the LLM (inlined pi-ai `Tool.description`).
    pub description: String,
    /// TypeBox `TSchema` parameter schema (inlined pi-ai `Tool.parameters`),
    /// kept opaque — never re-derived.
    pub parameters: Value,
    /// Per-tool execution-mode override; falls back to the loop default when
    /// `None` (pi's `executionMode`).
    pub execution_mode: Option<ToolExecutionMode>,
    /// Execute the tool call. See [`ToolDefinitionExecute`].
    pub execute: ToolDefinitionExecute,
    /// Optional shim rewriting raw tool-call arguments before schema validation
    /// (pi's `prepareArguments`).
    pub prepare_arguments: Option<PrepareArguments>,
    /// Optional one-line snippet for the "Available tools" section of the
    /// default system prompt (pi's `promptSnippet`).
    pub prompt_snippet: Option<String>,
    /// Optional guideline bullets appended to the system-prompt Guidelines
    /// section when this tool is active (pi's `promptGuidelines`).
    pub prompt_guidelines: Option<Vec<String>>,
    /// Whether the execution component renders the standard shell or the tool
    /// frames itself (pi's `renderShell`).
    pub render_shell: Option<RenderShell>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pidgin_ai::ContentBlock;
    use serde_json::json;

    /// Trivial [`ExtensionContext`] used to construct a [`ToolDefinition`].
    struct NoopCtx;
    impl ExtensionContext for NoopCtx {}

    #[test]
    fn render_shell_serde_round_trip() {
        assert_eq!(
            serde_json::to_value(RenderShell::Default).unwrap(),
            json!("default")
        );
        assert_eq!(
            serde_json::to_value(RenderShell::SelfRender).unwrap(),
            json!("self")
        );
        assert_eq!(
            serde_json::from_value::<RenderShell>(json!("default")).unwrap(),
            RenderShell::Default
        );
        assert_eq!(
            serde_json::from_value::<RenderShell>(json!("self")).unwrap(),
            RenderShell::SelfRender
        );
    }

    #[test]
    fn tool_definition_constructs_with_trivial_execute() {
        let tool = ToolDefinition {
            name: "read".into(),
            label: "Read".into(),
            description: "Read a file".into(),
            parameters: json!({ "type": "object" }),
            execution_mode: Some(ToolExecutionMode::Parallel),
            execute: Arc::new(|_id, _args, _signal, _on_update, _ctx| AgentToolResult {
                content: vec![ContentBlock::Text {
                    text: "ok".into(),
                    text_signature: None,
                }],
                details: json!(null),
                added_tool_names: None,
                terminate: None,
            }),
            prepare_arguments: Some(Arc::new(|args| args)),
            prompt_snippet: Some("read <path>".into()),
            prompt_guidelines: Some(vec!["Prefer reading before editing.".into()]),
            render_shell: Some(RenderShell::Default),
        };

        // Metadata is plain data.
        assert_eq!(tool.name, "read");
        assert_eq!(tool.execution_mode, Some(ToolExecutionMode::Parallel));

        // The execute closure runs with an ExtensionContext threaded through.
        let ctx = NoopCtx;
        let result = (tool.execute)("call_1", &json!({}), None, None, &ctx);
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({ "content": [{ "type": "text", "text": "ok" }], "details": null })
        );

        // prepareArguments is an identity shim here.
        let prepared = (tool.prepare_arguments.as_ref().unwrap())(json!({ "path": "a" }));
        assert_eq!(prepared, json!({ "path": "a" }));
    }
}
