// straitjacket-allow-file:duplication — faithful-mirror pair with crates/pidgin-agent/src/harness/compaction (agent-core copy); parallel structure is intentional
//! Context compaction subsystem, mirroring
//! `packages/coding-agent/src/core/compaction/`.
//!
//! This is the coding-agent copy. Its cut-point detection and token accounting
//! expand each [`SessionEntry`](crate::core::session_manager::SessionEntry)
//! through
//! [`session_entry_to_context_messages`](crate::core::session_manager::session_entry_to_context_messages)
//! and operate on the returned messages, rather than reading `entry.message`
//! 1:1 the way the agent-core copy
//! (`crates/pidgin-agent/src/harness/compaction/`) does. That entry-expansion is
//! what lets context-visible custom-message entries participate in the recent
//! token budget.
//!
//! The module split mirrors pi: [`utils`] (file ops + serialization),
//! [`compaction`] (token math, cut points, summary generation), and
//! [`branch_summarization`] (abandoned-branch summaries). The
//! [`Models`]/[`CompletionOptions`] seam stands in for pi-ai's `completeSimple`,
//! which pidgin-ai does not yet wrap; see [`compaction`] for the rationale.

pub mod branch_summarization;
#[allow(clippy::module_inception)]
pub mod compaction;
pub mod utils;

// Public compaction surface (mirrors pi's `compaction/index.ts` barrel plus the
// symbols the coding-agent test suite imports).
pub use compaction::{
    assemble_compaction_result, build_summarization_context, build_summary_options,
    build_summary_prompt, build_turn_prefix_options, build_turn_prefix_prompt,
    calculate_context_tokens, compact, estimate_context_tokens, estimate_tokens, find_cut_point,
    find_turn_start_index, generate_summary, get_last_assistant_usage, prepare_compaction,
    should_compact, CompactionDetails, CompactionError, CompactionErrorCode, CompactionPreparation,
    CompactionResult, CompactionSettings, CompletionOptions, ContextUsageEstimate, CutPointResult,
    Models, SummarizationRequestOptions, DEFAULT_COMPACTION_SETTINGS, ESTIMATED_IMAGE_CHARS,
};

// Branch-summarization surface.
pub use branch_summarization::{
    assemble_branch_summary_result, build_branch_summary_prompt,
    collect_entries_for_branch_summary, generate_branch_summary, prepare_branch_entries,
    BranchPreparation, BranchSummaryDetails, BranchSummaryError, BranchSummaryErrorCode,
    BranchSummaryResult, CollectEntriesResult, GenerateBranchSummaryOptions,
    BRANCH_SUMMARY_MAX_TOKENS, BRANCH_SUMMARY_PREAMBLE,
};

// Shared utilities (`serializeConversation` is part of the public compaction
// surface; the file-op helpers back both submodules). `SUMMARIZATION_SYSTEM_PROMPT`
// lives in pi's coding-agent `compaction/utils.ts`, not `compaction.ts`.
pub use utils::{
    compute_file_lists, create_file_ops, extract_file_ops_from_message, format_file_operations,
    response_text, serialize_conversation, FileOperations, SUMMARIZATION_SYSTEM_PROMPT,
};
