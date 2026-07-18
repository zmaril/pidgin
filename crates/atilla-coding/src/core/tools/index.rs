//! Deferred port of pi's `core/tools/index.ts`
//! (`vendor/pi/packages/coding-agent/src/core/tools/index.ts`).
//!
//! This module is the tools barrel plus the registry factory layer: it defines
//! the `ToolName` union (`"read" | "bash" | "edit" | "write" | "grep" |
//! "find" | "ls"`), `allToolNames`, the `ToolsOptions` bag of per-tool option
//! structs, and the factory functions that assemble the default tool registry
//! consumed by the agent loop:
//!
//! * `createToolDefinition` / `createTool` — dispatch a single `ToolName` to its
//!   factory.
//! * `createCodingToolDefinitions` / `createCodingTools` — read, bash, edit,
//!   write.
//! * `createReadOnlyToolDefinitions` / `createReadOnlyTools` — read, grep, find,
//!   ls.
//! * `createAllToolDefinitions` / `createAllTools` — the full `ToolName`-keyed
//!   map.
//!
//! It carries no algorithm of its own; it is registry wiring over the
//! agent-loop and tool-definition surfaces.
//!
//! Not yet ported: it is blocked on both the agent-core tool interface and the
//! per-tool `ToolDefinition` factories, which do not exist in atilla yet.
//!
//! Blocking dependencies (must land before this module can be ported):
//!
//! * `ToolDefinition<TParams, TDetails>` and the wrapped `AgentTool` type —
//!   same blockers as `tool_definition_wrapper`: `AgentTool` from the
//!   `atilla-agent` crate (pi `@earendil-works/pi-agent-core`,
//!   `vendor/pi/packages/agent/src/types.ts:373`, currently a placeholder in
//!   `crates/atilla-agent/src/types.rs`) and `ToolDefinition` from the
//!   unported coding-agent extensions module
//!   (`vendor/pi/packages/coding-agent/src/core/extensions/types.ts:439`).
//! * The per-tool `create<Tool>ToolDefinition` / `create<Tool>Tool` factories.
//!   The ported tools in this directory currently expose plain execution
//!   surfaces (e.g. `bash.rs`: `create_local_bash_operations`, `BashExecResult`,
//!   `BashToolResult`, `BashToolDetails`; `read.rs`: `ReadTextOutput`,
//!   `format_text_read`), NOT `ToolDefinition` objects. There is no
//!   `ToolDefinition`/`AgentTool` abstraction in atilla for these factories to
//!   return yet, so the barrel has nothing to re-export or assemble. Each tool's
//!   `create*ToolDefinition` factory must be ported (which itself depends on the
//!   `ToolDefinition` type above) before this registry can be wired.
//!
//! Wiring to do once those types land:
//!
//! 1. Re-export the per-tool factories and option/detail types from `bash`,
//!    `edit`, `find`, `grep`, `ls`, `read`, `write`, plus `truncate` helpers and
//!    `file_mutation_queue::with_file_mutation_queue`, matching pi's barrel.
//! 2. Define `ToolName`, `all_tool_names`, and `ToolsOptions` exactly as pi does
//!    (same seven names, same option fields).
//! 3. Port the factory functions with pi's exact groupings: coding =
//!    read/bash/edit/write, read-only = read/grep/find/ls, all = the seven-entry
//!    map keyed by `ToolName`.
//! 4. Add tests asserting `all_tool_names` membership and each factory grouping
//!    matches pi.
