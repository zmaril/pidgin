//! Deferred port of pi's `core/tools/tool-definition-wrapper.ts`
//! (`vendor/pi/packages/coding-agent/src/core/tools/tool-definition-wrapper.ts`).
//!
//! `wrapToolDefinition` / `wrapToolDefinitions` adapt a coding-agent
//! `ToolDefinition` into the `AgentTool` shape the agent runtime consumes
//! (copying `name`/`label`/`description`/`parameters`/`prepareArguments`/
//! `executionMode` and wrapping `execute` to inject an `ExtensionContext` via a
//! `ctxFactory`), and `createToolDefinitionFromAgentTool` synthesizes a minimal
//! `ToolDefinition` back from a plain `AgentTool`. It is pure glue over the
//! extension/registry types.
//!
//! Not yet ported: it is blocked on agent-core / extension types that are owned
//! by sibling threads and do not exist in atilla yet.
//!
//! Blocking dependencies (must land before this module can be ported):
//!
//! * `AgentTool` (plus `AgentToolResult`, `AgentToolUpdateCallback`,
//!   `ToolExecutionMode`) — the agent-core tool interface. In pi this is
//!   `@earendil-works/pi-agent-core`,
//!   `vendor/pi/packages/agent/src/types.ts` (`AgentTool` at line 373). In
//!   atilla this maps to the `atilla-agent` crate, whose
//!   `crates/atilla-agent/src/types.rs` is currently a placeholder — none of
//!   these types are ported or exported. Owned by the agent-core (`atilla-agent`)
//!   thread.
//! * `ToolDefinition` and `ExtensionContext` — from coding-agent's extensions
//!   module, `vendor/pi/packages/coding-agent/src/core/extensions/types.ts`
//!   (`ExtensionContext` at line 304, `ToolDefinition` at line 439). The
//!   `core/extensions/` module is not ported in `atilla-coding` at all (no
//!   `crates/atilla-coding/src/core/extensions/` directory). Owned by the
//!   coding-agent extensions thread.
//!
//! Wiring to do once those types land:
//!
//! 1. Depend on `atilla-agent` for `AgentTool`/`AgentToolResult`/
//!    `AgentToolUpdateCallback`/`ToolExecutionMode` (add the crate dep once the
//!    types are exported, provided it introduces no dependency cycle).
//! 2. Reference the local `core::extensions` `ToolDefinition`/`ExtensionContext`
//!    once that module exists.
//! 3. Port `wrap_tool_definition`, `wrap_tool_definitions`, and
//!    `create_tool_definition_from_agent_tool` as faithful mirrors, preserving
//!    the `execute` closure that threads the optional `ctxFactory`-produced
//!    `ExtensionContext` into the definition's `execute`.
