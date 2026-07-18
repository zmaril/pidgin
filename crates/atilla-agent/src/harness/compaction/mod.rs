//! Compaction subsystem, mirroring
//! `packages/agent/src/harness/compaction/`.
//!
//! This is the agent-core copy (the `.` entrypoint), whose cut-point detection
//! and file-operation extraction operate directly on
//! [`SessionTreeEntry`](crate::harness::types::SessionTreeEntry) `message`
//! payloads. The coding-agent copy
//! (`packages/coding-agent/src/core/compaction/`) is a separate, unported
//! variant that depends on the not-yet-ported session-manager `SessionEntry`
//! expansion.
//!
//! The module split mirrors pi: [`utils`] (file ops + serialization),
//! [`compaction`] (token math, cut points, summary generation), and
//! [`branch_summarization`] (abandoned-branch summaries). The
//! [`Models`]/[`CompletionOptions`] seam stands in for pi-ai's
//! `Models.completeSimple`, which atilla-ai does not yet wrap; see
//! [`compaction`] for the rationale.

pub mod branch_summarization;
#[allow(clippy::module_inception)]
pub mod compaction;
pub mod utils;

// Public compaction surface, mirroring the re-exports in
// `packages/agent/src/index.ts`.
pub use compaction::{
    calculate_context_tokens, compact, estimate_context_tokens, estimate_tokens, find_cut_point,
    find_turn_start_index, generate_summary, get_last_assistant_usage, prepare_compaction,
    should_compact, CompactionDetails, CompactionError, CompactionErrorCode, CompactionPreparation,
    CompactionResult, CompactionSettings, CompletionOptions, ContextUsageEstimate, CutPointResult,
    Models, DEFAULT_COMPACTION_SETTINGS, ESTIMATED_IMAGE_CHARS, SUMMARIZATION_SYSTEM_PROMPT,
};

// Branch-summarization surface, mirroring `agent/src/index.ts`.
pub use branch_summarization::{
    collect_entries_for_branch_summary, generate_branch_summary, prepare_branch_entries,
    BranchPreparation, BranchSummaryDetails, BranchSummaryError, BranchSummaryErrorCode,
    BranchSummaryResult, CollectEntriesResult, GenerateBranchSummaryOptions,
    BRANCH_SUMMARY_PREAMBLE,
};

// Shared utilities (`serializeConversation` is part of the public compaction
// surface; the file-op helpers back both submodules).
pub use utils::{
    compute_file_lists, create_file_ops, extract_file_ops_from_message, format_file_operations,
    serialize_conversation, FileOperations,
};
