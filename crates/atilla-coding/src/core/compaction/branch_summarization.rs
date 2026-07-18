// straitjacket-allow-file:duplication faithful mirror of pi coding-agent
// compaction/branch-summarization.ts; parallel structure to the agent-core copy
// (crates/atilla-agent/src/harness/compaction/branch_summarization.rs) is
// intentional.

//! Branch summarization for tree navigation. When navigating to a different
//! point in the session tree, this summarizes the branch being left so context
//! isn't lost. Mirrors
//! `packages/coding-agent/src/core/compaction/branch-summarization.ts`.

use std::collections::HashSet;
use std::fmt;

use serde_json::Value;

use atilla_ai::seams::AbortSignal;
use atilla_ai::{Model, StopReason};

// Extension seam / messages module: pi's coding-agent imports these message
// constructors from `../messages.ts`. atilla-coding has not ported that module
// yet, but the identical constructors are available in the agent-core session
// module; reuse them here until a shared `atilla_coding::core::messages` lands.
use atilla_agent::harness::session::messages::{
    create_branch_summary_message, create_compaction_summary_message, create_custom_message,
};

use super::compaction::{build_summarization_context, estimate_tokens, CompletionOptions, Models};
use super::utils::{
    compute_file_lists, convert_to_llm, create_file_ops, error_message_or,
    extract_file_ops_from_details, extract_file_ops_from_message, format_file_operations,
    response_text, serialize_conversation, FileOperations, SUMMARIZATION_SYSTEM_PROMPT,
};
use crate::core::session_manager::{AgentMessage, ReadonlySessionManager, SessionEntry};

// ---------------------------------------------------------------------------
// Error type (idiomatic Result mapping of pi's `{aborted?, error?}` result).
// ---------------------------------------------------------------------------

/// Stable branch-summary error codes. Mirrors the agent-core
/// `BranchSummaryErrorCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchSummaryErrorCode {
    Aborted,
    SummarizationFailed,
    InvalidSession,
}

impl BranchSummaryErrorCode {
    /// The wire string for this code (`BranchSummaryError.code`).
    pub fn as_str(self) -> &'static str {
        match self {
            BranchSummaryErrorCode::Aborted => "aborted",
            BranchSummaryErrorCode::SummarizationFailed => "summarization_failed",
            BranchSummaryErrorCode::InvalidSession => "invalid_session",
        }
    }
}

/// Error returned by branch summarization helpers. The idiomatic Result mapping
/// of pi's `{ aborted?: true }` / `{ error?: string }` return shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchSummaryError {
    pub code: BranchSummaryErrorCode,
    pub message: String,
}

impl BranchSummaryError {
    pub fn new(code: BranchSummaryErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for BranchSummaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for BranchSummaryError {}

/// The result of a branch summary. Mirrors pi's `BranchSummaryResult` success
/// shape (`{summary, readFiles, modifiedFiles}`); the `aborted`/`error` variants
/// are carried by [`BranchSummaryError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchSummaryResult {
    pub summary: String,
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

// ---------------------------------------------------------------------------
// Preparation structs.
// ---------------------------------------------------------------------------

/// File-operation details stored on generated branch summary entries. Mirrors
/// pi's `BranchSummaryDetails`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchSummaryDetails {
    /// Files read while exploring the summarized branch.
    pub read_files: Vec<String>,
    /// Files modified while exploring the summarized branch.
    pub modified_files: Vec<String>,
}

/// Prepared branch content for summarization. Mirrors pi's `BranchPreparation`.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchPreparation {
    /// Messages selected for the branch summary.
    pub messages: Vec<AgentMessage>,
    /// File operations extracted from the branch.
    pub file_ops: FileOperations,
    /// Estimated token count for selected messages.
    pub total_tokens: i64,
}

/// Entries selected for branch summarization. Mirrors pi's
/// `CollectEntriesResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectEntriesResult {
    /// Entries to summarize in chronological order.
    pub entries: Vec<SessionEntry>,
    /// Deepest common ancestor between the previous leaf and target entry.
    pub common_ancestor_id: Option<String>,
}

/// Options for generating a branch summary. Mirrors pi's
/// `GenerateBranchSummaryOptions`.
pub struct GenerateBranchSummaryOptions<'a> {
    /// Provider collection the summarization request goes through.
    pub models: &'a dyn Models,
    /// Model used for summarization.
    pub model: &'a Model,
    /// Abort signal for the summarization request.
    pub signal: AbortSignal,
    /// Optional instructions appended to or replacing the default prompt.
    pub custom_instructions: Option<String>,
    /// Replace the default prompt with custom instructions instead of appending.
    pub replace_instructions: bool,
    /// Tokens reserved for prompt and model output. Defaults to 16384 when None.
    pub reserve_tokens: Option<i64>,
}

// ---------------------------------------------------------------------------
// Entry collection.
// ---------------------------------------------------------------------------

/// Collect entries that should be summarized before navigating to a different
/// session tree entry. Mirrors pi's `collectEntriesForBranchSummary`.
///
/// Infallible, unlike the agent-core copy: pi's coding-agent
/// `ReadonlySessionManager.getBranch`/`getEntry` never throw, and a missing
/// entry simply breaks the walk (`if (!entry) break;`).
pub fn collect_entries_for_branch_summary(
    session: &dyn ReadonlySessionManager,
    old_leaf_id: Option<&str>,
    target_id: &str,
) -> CollectEntriesResult {
    let Some(old_leaf_id) = old_leaf_id else {
        return CollectEntriesResult {
            entries: Vec::new(),
            common_ancestor_id: None,
        };
    };

    let old_path: HashSet<String> = session
        .get_branch(Some(old_leaf_id))
        .iter()
        .map(|e| e.id().to_string())
        .collect();
    let target_path = session.get_branch(Some(target_id));

    let mut common_ancestor_id: Option<String> = None;
    for entry in target_path.iter().rev() {
        if old_path.contains(entry.id()) {
            common_ancestor_id = Some(entry.id().to_string());
            break;
        }
    }

    let mut entries: Vec<SessionEntry> = Vec::new();
    let mut current: Option<String> = Some(old_leaf_id.to_string());
    while let Some(current_id) = current {
        if common_ancestor_id.as_deref() == Some(current_id.as_str()) {
            break;
        }
        let Some(entry) = session.get_entry(&current_id) else {
            break;
        };
        current = entry.parent_id().map(str::to_string);
        entries.push(entry);
    }
    entries.reverse();

    CollectEntriesResult {
        entries,
        common_ancestor_id,
    }
}

/// Project a session entry to the agent message it contributes for branch
/// summarization. Mirrors pi's branch-summarization `getMessageFromEntry`
/// (drops `toolResult` message entries, handles `compaction` unlike the
/// compaction module's copy, and constructs summary messages directly rather
/// than via entry expansion).
fn get_message_from_entry(entry: &SessionEntry) -> Option<AgentMessage> {
    match entry {
        SessionEntry::Message(e) => {
            if e.message.get("role").and_then(Value::as_str) == Some("toolResult") {
                return None;
            }
            Some(e.message.clone())
        }
        SessionEntry::CustomMessage(e) => Some(create_custom_message(
            &e.custom_type,
            &e.content,
            e.display,
            e.details.as_ref(),
            &e.timestamp,
        )),
        SessionEntry::BranchSummary(e) => Some(create_branch_summary_message(
            &e.summary,
            &e.from_id,
            &e.timestamp,
        )),
        SessionEntry::Compaction(e) => Some(create_compaction_summary_message(
            &e.summary,
            e.tokens_before,
            &e.timestamp,
        )),
        // thinking_level_change, model_change, custom, label, session_info.
        _ => None,
    }
}

/// Prepare branch entries for summarization within an optional token budget.
/// Mirrors pi's `prepareBranchEntries`.
pub fn prepare_branch_entries(entries: &[SessionEntry], token_budget: i64) -> BranchPreparation {
    let mut messages: Vec<AgentMessage> = Vec::new();
    let mut file_ops = create_file_ops();
    let mut total_tokens: i64 = 0;

    // First pass: collect cumulative file ops from nested branch summaries.
    // Extension seam: only pi-generated summaries (`fromHook !== true`) are read;
    // extension-generated ones are skipped.
    for entry in entries {
        if let SessionEntry::BranchSummary(e) = entry {
            if !matches!(e.from_hook, Some(true)) {
                if let Some(details) = &e.details {
                    extract_file_ops_from_details(details, &mut file_ops);
                }
            }
        }
    }

    // Second pass: walk newest to oldest, adding messages until the budget.
    for entry in entries.iter().rev() {
        let Some(message) = get_message_from_entry(entry) else {
            continue;
        };
        extract_file_ops_from_message(&message, &mut file_ops);

        let tokens = estimate_tokens(&message);
        if token_budget > 0 && total_tokens + tokens > token_budget {
            // Summary entries are important context; fit them if there is slack.
            if matches!(
                entry,
                SessionEntry::Compaction(_) | SessionEntry::BranchSummary(_)
            ) && (total_tokens as f64) < token_budget as f64 * 0.9
            {
                messages.insert(0, message);
                total_tokens += tokens;
            }
            break;
        }

        messages.insert(0, message);
        total_tokens += tokens;
    }

    BranchPreparation {
        messages,
        file_ops,
        total_tokens,
    }
}

// ---------------------------------------------------------------------------
// Prompt constants (byte-for-byte from branch-summarization.ts).
// ---------------------------------------------------------------------------

/// pi's `BRANCH_SUMMARY_PREAMBLE`.
pub const BRANCH_SUMMARY_PREAMBLE: &str =
    "The user explored a different conversation branch before returning here.
Summary of that exploration:

";

const BRANCH_SUMMARY_PROMPT: &str =
    "Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// The fixed `maxTokens` cap a branch summary request uses. Mirrors the literal
/// pi passes to `completeSimple`.
pub const BRANCH_SUMMARY_MAX_TOKENS: i64 = 2048;

// ---------------------------------------------------------------------------
// Branch summary generation.
// ---------------------------------------------------------------------------

/// Build the user-message prompt text for a branch summary call. Mirrors the
/// prompt assembly inside pi's `generateBranchSummary`.
pub fn build_branch_summary_prompt(
    messages: &[AgentMessage],
    custom_instructions: Option<&str>,
    replace_instructions: bool,
) -> String {
    let llm_messages = convert_to_llm(messages);
    let conversation_text = serialize_conversation(&llm_messages);
    let instructions = match (replace_instructions, custom_instructions) {
        (true, Some(custom)) => custom.to_string(),
        (_, Some(custom)) => format!("{BRANCH_SUMMARY_PROMPT}\n\nAdditional focus: {custom}"),
        (_, None) => BRANCH_SUMMARY_PROMPT.to_string(),
    };
    format!("<conversation>\n{conversation_text}\n</conversation>\n\n{instructions}")
}

/// Assemble a [`BranchSummaryResult`] from the model's summary text and the
/// prepared file operations. Extracted from [`generate_branch_summary`]'s
/// post-model block so a future napi side can drive summarization itself and
/// reproduce the exact result assembly (preamble prepend, file-op footer).
pub fn assemble_branch_summary_result(
    file_ops: &FileOperations,
    summary_text: &str,
) -> BranchSummaryResult {
    let mut summary = format!("{BRANCH_SUMMARY_PREAMBLE}{summary_text}");
    let (read_files, modified_files) = compute_file_lists(file_ops);
    summary.push_str(&format_file_operations(&read_files, &modified_files));

    BranchSummaryResult {
        summary: if summary.is_empty() {
            "No summary generated".to_string()
        } else {
            summary
        },
        read_files,
        modified_files,
    }
}

/// Generate a summary of abandoned branch entries. Mirrors pi's
/// `generateBranchSummary`.
pub fn generate_branch_summary(
    entries: &[SessionEntry],
    options: &GenerateBranchSummaryOptions,
) -> Result<BranchSummaryResult, BranchSummaryError> {
    let reserve_tokens = options.reserve_tokens.unwrap_or(16384);
    // pi: model.contextWindow || 128000
    let context_window = if options.model.context_window != 0 {
        options.model.context_window as i64
    } else {
        128000
    };
    let token_budget = context_window - reserve_tokens;

    let BranchPreparation {
        messages, file_ops, ..
    } = prepare_branch_entries(entries, token_budget);

    if messages.is_empty() {
        return Ok(BranchSummaryResult {
            summary: "No content to summarize".to_string(),
            read_files: Vec::new(),
            modified_files: Vec::new(),
        });
    }

    let prompt_text = build_branch_summary_prompt(
        &messages,
        options.custom_instructions.as_deref(),
        options.replace_instructions,
    );

    let context = build_summarization_context(SUMMARIZATION_SYSTEM_PROMPT, prompt_text);
    let completion_options = CompletionOptions {
        max_tokens: BRANCH_SUMMARY_MAX_TOKENS,
        signal: Some(options.signal.clone()),
        reasoning: None,
    };

    let response = options
        .models
        .complete_simple(options.model, &context, &completion_options);
    match response.stop_reason {
        StopReason::Aborted => {
            return Err(BranchSummaryError::new(
                BranchSummaryErrorCode::Aborted,
                error_message_or(&response, "Branch summary aborted"),
            ));
        }
        StopReason::Error => {
            return Err(BranchSummaryError::new(
                BranchSummaryErrorCode::SummarizationFailed,
                format!(
                    "Branch summary failed: {}",
                    error_message_or(&response, "Summarization failed")
                ),
            ));
        }
        _ => {}
    }

    Ok(assemble_branch_summary_result(
        &file_ops,
        &response_text(&response),
    ))
}
