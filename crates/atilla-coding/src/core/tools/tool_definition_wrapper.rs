//! Deferred port of pi's `core/tools/tool-definition-wrapper.ts`.
//!
//! `wrapToolDefinition` adapts a pi `ToolDefinition` into the `AgentTool`
//! shape expected by `pi-agent-core`, bridging render hooks and execution
//! callbacks. It is pure glue over the extension/registry types; Not yet
//! ported: it needs the agent-core tool interface it adapts to.
