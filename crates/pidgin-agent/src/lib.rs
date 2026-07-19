//! Rust mirror of `@earendil-works/pi-agent-core` (`packages/agent`).
//!
//! pi's agent package splits into two entry points: the portable `.` export
//! (`index.ts`, aggregating `agent`, `agent-loop`, `harness`, `proxy`, and
//! `types`) and a platform-specific `./node` export (`node.ts`). The modules
//! below mirror that split: everything is portable except [`node`], which
//! carries the Node-only surface. Port order runs `types` first, then the
//! `agent`/`agent_loop`/`harness` core, then `proxy`, then `node`. Every
//! module here is an empty stub except [`harness`], whose `session` subtree
//! ports pi's version-3 JSONL session-tree format (types, uuidv7, storage,
//! session, and repo).

pub mod agent;
pub mod agent_loop;
pub mod harness;
pub mod node;
pub mod proxy;
pub mod types;

// Crate-root re-exports of the tool-facing boundary types, so downstream crates
// (e.g. pidgin-coding's tool wrappers) can reach them by the shorter path.
pub use types::{
    AgentTool, AgentToolCall, AgentToolExecute, AgentToolResult, AgentToolUpdateCallback,
    PrepareArguments, ToolExecutionMode,
};

// Compaction subsystem public surface, mirroring the re-exports in
// `packages/agent/src/index.ts` (lines ~6-27).
pub use harness::compaction::{
    assemble_branch_summary_result, assemble_compaction_result, build_branch_summary_prompt,
    build_summarization_context, build_summary_options, build_summary_prompt,
    build_turn_prefix_options, build_turn_prefix_prompt, calculate_context_tokens,
    collect_entries_for_branch_summary, compact, estimate_context_tokens, estimate_tokens,
    find_cut_point, find_turn_start_index, generate_branch_summary, generate_summary,
    get_last_assistant_usage, prepare_branch_entries, prepare_compaction, response_text,
    serialize_conversation, should_compact, BranchPreparation, BranchSummaryDetails,
    BranchSummaryError, BranchSummaryErrorCode, BranchSummaryResult, CollectEntriesResult,
    CompactionDetails, CompactionError, CompactionErrorCode, CompactionPreparation,
    CompactionResult, CompactionSettings, CompletionOptions, ContextUsageEstimate, CutPointResult,
    FileOperations, GenerateBranchSummaryOptions, Models, SummarizationRequestOptions,
    BRANCH_SUMMARY_MAX_TOKENS, DEFAULT_COMPACTION_SETTINGS,
};

/// Name of the pi package this crate mirrors.
pub const PI_PACKAGE: &str = "@earendil-works/pi-agent-core";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_pi_agent_core() {
        assert_eq!(PI_PACKAGE, "@earendil-works/pi-agent-core");
    }
}
