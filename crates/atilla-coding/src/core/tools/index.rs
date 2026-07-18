//! Port of pi's `core/tools/index.ts`
//! (`vendor/pi/packages/coding-agent/src/core/tools/index.ts`).
//!
//! The tools barrel plus the registry factory layer: the [`ToolName`] union,
//! [`all_tool_names`], the [`ToolsOptions`] per-tool option bag, and the factory
//! functions that assemble the default tool registry consumed by the agent loop.
//!
//! The per-tool `create_<tool>_tool_definition` factories live in
//! [`super::definitions`] (see that module's docs for why they are centralized
//! rather than per-file); this module re-exports them and composes them into
//! pi's exact groupings:
//!
//! * [`create_coding_tool_definitions`] — read, bash, edit, write.
//! * [`create_read_only_tool_definitions`] — read, grep, find, ls.
//! * [`create_all_tool_definitions`] — the full [`ToolName`]-keyed map.
//!
//! The `create_*_tools` variants wrap each definition through
//! [`wrap_tool_definition`] into the runtime [`AgentTool`] shape.

use indexmap::IndexMap;

use atilla_agent::types::AgentTool;

use crate::core::extensions::types::ToolDefinition;

use super::bash::BashToolOptions;
use super::definitions::{
    create_bash_tool_definition, create_edit_tool_definition, create_find_tool_definition,
    create_grep_tool_definition, create_ls_tool_definition, create_read_tool_definition,
    create_write_tool_definition,
};
use super::tool_definition_wrapper::wrap_tool_definition;

pub use super::definitions::{
    EditToolOptions, FindToolOptions, GrepToolOptions, LsToolOptions, ReadToolOptions,
    WriteToolOptions,
};

/// The set of built-in tool names (pi's `ToolName`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolName {
    /// The `read` tool.
    Read,
    /// The `bash` tool.
    Bash,
    /// The `edit` tool.
    Edit,
    /// The `write` tool.
    Write,
    /// The `grep` tool.
    Grep,
    /// The `find` tool.
    Find,
    /// The `ls` tool.
    Ls,
}

impl ToolName {
    /// The tool name as it appears in LLM tool calls (pi's string union value).
    pub fn as_str(self) -> &'static str {
        match self {
            ToolName::Read => "read",
            ToolName::Bash => "bash",
            ToolName::Edit => "edit",
            ToolName::Write => "write",
            ToolName::Grep => "grep",
            ToolName::Find => "find",
            ToolName::Ls => "ls",
        }
    }
}

/// Every built-in tool name (pi's `allToolNames`), in pi's declaration order:
/// `read, bash, edit, write, grep, find, ls`.
pub fn all_tool_names() -> [ToolName; 7] {
    [
        ToolName::Read,
        ToolName::Bash,
        ToolName::Edit,
        ToolName::Write,
        ToolName::Grep,
        ToolName::Find,
        ToolName::Ls,
    ]
}

/// Per-tool option bag (pi's `ToolsOptions`). Each field is consumed by the
/// matching factory; a `None` field selects that tool's defaults.
#[derive(Default)]
pub struct ToolsOptions {
    /// Options for the `read` tool.
    pub read: Option<ReadToolOptions>,
    /// Options for the `bash` tool.
    pub bash: Option<BashToolOptions>,
    /// Options for the `write` tool.
    pub write: Option<WriteToolOptions>,
    /// Options for the `edit` tool.
    pub edit: Option<EditToolOptions>,
    /// Options for the `grep` tool.
    pub grep: Option<GrepToolOptions>,
    /// Options for the `find` tool.
    pub find: Option<FindToolOptions>,
    /// Options for the `ls` tool.
    pub ls: Option<LsToolOptions>,
}

/// Build a single tool's [`ToolDefinition`] by name (pi's
/// `createToolDefinition`). Consumes `options`, using only the matching field.
pub fn create_tool_definition(
    tool_name: ToolName,
    cwd: &str,
    options: ToolsOptions,
) -> ToolDefinition {
    match tool_name {
        ToolName::Read => create_read_tool_definition(cwd, options.read),
        ToolName::Bash => create_bash_tool_definition(cwd, options.bash),
        ToolName::Edit => create_edit_tool_definition(cwd, options.edit),
        ToolName::Write => create_write_tool_definition(cwd, options.write),
        ToolName::Grep => create_grep_tool_definition(cwd, options.grep),
        ToolName::Find => create_find_tool_definition(cwd, options.find),
        ToolName::Ls => create_ls_tool_definition(cwd, options.ls),
    }
}

/// Build a single wrapped [`AgentTool`] by name (pi's `createTool`).
pub fn create_tool(tool_name: ToolName, cwd: &str, options: ToolsOptions) -> AgentTool {
    wrap_tool_definition(create_tool_definition(tool_name, cwd, options), None)
}

/// The default coding tool set (pi's `createCodingToolDefinitions`): read, bash,
/// edit, write.
pub fn create_coding_tool_definitions(cwd: &str, options: ToolsOptions) -> Vec<ToolDefinition> {
    vec![
        create_read_tool_definition(cwd, options.read),
        create_bash_tool_definition(cwd, options.bash),
        create_edit_tool_definition(cwd, options.edit),
        create_write_tool_definition(cwd, options.write),
    ]
}

/// The read-only tool set (pi's `createReadOnlyToolDefinitions`): read, grep,
/// find, ls.
pub fn create_read_only_tool_definitions(cwd: &str, options: ToolsOptions) -> Vec<ToolDefinition> {
    vec![
        create_read_tool_definition(cwd, options.read),
        create_grep_tool_definition(cwd, options.grep),
        create_find_tool_definition(cwd, options.find),
        create_ls_tool_definition(cwd, options.ls),
    ]
}

/// The full tool set keyed by [`ToolName`] (pi's `createAllToolDefinitions`).
/// Ordering follows pi's object literal: read, bash, edit, write, grep, find, ls.
pub fn create_all_tool_definitions(
    cwd: &str,
    options: ToolsOptions,
) -> IndexMap<ToolName, ToolDefinition> {
    let mut map = IndexMap::new();
    map.insert(
        ToolName::Read,
        create_read_tool_definition(cwd, options.read),
    );
    map.insert(
        ToolName::Bash,
        create_bash_tool_definition(cwd, options.bash),
    );
    map.insert(
        ToolName::Edit,
        create_edit_tool_definition(cwd, options.edit),
    );
    map.insert(
        ToolName::Write,
        create_write_tool_definition(cwd, options.write),
    );
    map.insert(
        ToolName::Grep,
        create_grep_tool_definition(cwd, options.grep),
    );
    map.insert(
        ToolName::Find,
        create_find_tool_definition(cwd, options.find),
    );
    map.insert(ToolName::Ls, create_ls_tool_definition(cwd, options.ls));
    map
}

/// The default coding tool set, wrapped as [`AgentTool`]s (pi's
/// `createCodingTools`).
pub fn create_coding_tools(cwd: &str, options: ToolsOptions) -> Vec<AgentTool> {
    create_coding_tool_definitions(cwd, options)
        .into_iter()
        .map(|def| wrap_tool_definition(def, None))
        .collect()
}

/// The read-only tool set, wrapped as [`AgentTool`]s (pi's
/// `createReadOnlyTools`).
pub fn create_read_only_tools(cwd: &str, options: ToolsOptions) -> Vec<AgentTool> {
    create_read_only_tool_definitions(cwd, options)
        .into_iter()
        .map(|def| wrap_tool_definition(def, None))
        .collect()
}

/// The full tool set keyed by [`ToolName`], wrapped as [`AgentTool`]s (pi's
/// `createAllTools`).
pub fn create_all_tools(cwd: &str, options: ToolsOptions) -> IndexMap<ToolName, AgentTool> {
    create_all_tool_definitions(cwd, options)
        .into_iter()
        .map(|(name, def)| (name, wrap_tool_definition(def, None)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(defs: &[ToolDefinition]) -> Vec<String> {
        defs.iter().map(|d| d.name.clone()).collect()
    }

    #[test]
    fn all_tool_names_matches_pi_order() {
        let names: Vec<&str> = all_tool_names().iter().map(|n| n.as_str()).collect();
        assert_eq!(
            names,
            ["read", "bash", "edit", "write", "grep", "find", "ls"]
        );
    }

    #[test]
    fn coding_tool_definitions_grouping() {
        let defs = create_coding_tool_definitions(".", ToolsOptions::default());
        assert_eq!(names(&defs), ["read", "bash", "edit", "write"]);
    }

    #[test]
    fn read_only_tool_definitions_grouping() {
        let defs = create_read_only_tool_definitions(".", ToolsOptions::default());
        assert_eq!(names(&defs), ["read", "grep", "find", "ls"]);
    }

    #[test]
    fn all_tool_definitions_map() {
        let map = create_all_tool_definitions(".", ToolsOptions::default());
        let keys: Vec<ToolName> = map.keys().copied().collect();
        assert_eq!(keys, all_tool_names().to_vec());
        for name in all_tool_names() {
            assert_eq!(map[&name].name, name.as_str());
        }
    }

    #[test]
    fn wrapped_variants_map_through() {
        let coding = create_coding_tools(".", ToolsOptions::default());
        assert_eq!(
            coding.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
            ["read", "bash", "edit", "write"]
        );
        let all = create_all_tools(".", ToolsOptions::default());
        assert_eq!(all.len(), 7);
        assert_eq!(all[&ToolName::Ls].name, "ls");
    }

    #[test]
    fn create_single_tool_definition_by_name() {
        let def = create_tool_definition(ToolName::Grep, ".", ToolsOptions::default());
        assert_eq!(def.name, "grep");
        let tool = create_tool(ToolName::Bash, ".", ToolsOptions::default());
        assert_eq!(tool.name, "bash");
    }
}
