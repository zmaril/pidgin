//! Compaction: token estimation, cut-point detection, and summary generation,
//! mirroring `packages/agent/src/harness/compaction/compaction.ts`.
//!
//! This is the agent-core copy. Its cut-point detection operates on
//! [`SessionTreeEntry`] `message` payloads 1:1 (there is no entry-expansion
//! step, unlike the coding-agent copy), and token estimation reads the opaque
//! [`AgentMessage`](crate::harness::types::AgentMessage) JSON shape directly.
//!
//! # Model abstraction
//!
//! pi drives summarization through `Models.completeSimple` from
//! `@earendil-works/pi-ai`. atilla-ai ports the `Provider`/faux surface but not
//! yet a `Models`/`completeSimple` wrapper, so this module defines a minimal
//! [`Models`] trait mirroring pi's `completeSimple(model, context, options)`
//! signature. The deterministic surface (token math, cut points, preparation,
//! serialization) is independent of it; only [`generate_summary`] and
//! [`compact`] call through it. A test-only fake lives in `tests/compaction.rs`.

use std::fmt;

use serde_json::{json, Value};

use atilla_ai::seams::AbortSignal;
use atilla_ai::{AssistantMessage, ContentBlock, Context, Message, Model, StopReason, Usage};

use super::utils::{
    compute_file_lists, convert_to_llm, create_file_ops, extract_file_ops_from_message,
    format_file_operations, js_len, safe_json_stringify, serialize_conversation, FileOperations,
};
use crate::harness::session::{
    build_session_context, create_branch_summary_message, create_compaction_summary_message,
    create_custom_message, SessionContextBuildOptions,
};
use crate::harness::types::{AgentMessage, SessionTreeEntry};

// ---------------------------------------------------------------------------
// Error type (`packages/agent/src/harness/types.ts` `CompactionError`).
// ---------------------------------------------------------------------------

/// Stable compaction error codes returned by compaction helpers. Mirrors pi's
/// `CompactionErrorCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionErrorCode {
    Aborted,
    SummarizationFailed,
    InvalidSession,
    Unknown,
}

impl CompactionErrorCode {
    /// The wire string pi uses for this code (`CompactionError.code`).
    pub fn as_str(self) -> &'static str {
        match self {
            CompactionErrorCode::Aborted => "aborted",
            CompactionErrorCode::SummarizationFailed => "summarization_failed",
            CompactionErrorCode::InvalidSession => "invalid_session",
            CompactionErrorCode::Unknown => "unknown",
        }
    }
}

/// Error returned by compaction helpers. Mirrors pi's `CompactionError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionError {
    pub code: CompactionErrorCode,
    pub message: String,
}

impl CompactionError {
    pub fn new(code: CompactionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for CompactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CompactionError {}

// ---------------------------------------------------------------------------
// Model abstraction seam.
// ---------------------------------------------------------------------------

/// Per-request completion controls, mirroring the subset of pi's
/// `ModelsSimpleStreamOptions` compaction sets: `maxTokens`, `signal`, and an
/// optional `reasoning` (thinking) level.
#[derive(Clone, Default)]
pub struct CompletionOptions {
    pub max_tokens: i64,
    pub signal: Option<AbortSignal>,
    pub reasoning: Option<String>,
}

impl fmt::Debug for CompletionOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompletionOptions")
            .field("max_tokens", &self.max_tokens)
            .field("has_signal", &self.signal.is_some())
            .field("reasoning", &self.reasoning)
            .finish()
    }
}

/// The provider collection compaction summarizes through. Mirrors the single
/// `completeSimple` method compaction uses from pi's `Models` interface.
pub trait Models {
    /// Complete `context` with `model`, returning the final assistant message.
    /// Mirrors `Models.completeSimple`, which resolves the streamed result.
    fn complete_simple(
        &self,
        model: &Model,
        context: &Context,
        options: &CompletionOptions,
    ) -> AssistantMessage;
}

// ---------------------------------------------------------------------------
// Settings and results.
// ---------------------------------------------------------------------------

/// File-operation details stored on generated compaction entries. Mirrors pi's
/// `CompactionDetails`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionDetails {
    /// Files read in the compacted history.
    pub read_files: Vec<String>,
    /// Files modified in the compacted history.
    pub modified_files: Vec<String>,
}

/// Compaction thresholds and retention settings. Mirrors pi's
/// `CompactionSettings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionSettings {
    /// Enable automatic compaction decisions.
    pub enabled: bool,
    /// Tokens reserved for summary prompt and output.
    pub reserve_tokens: i64,
    /// Approximate recent-context tokens to keep after compaction.
    pub keep_recent_tokens: i64,
}

/// Default compaction settings used by the harness. Mirrors pi's
/// `DEFAULT_COMPACTION_SETTINGS`.
pub const DEFAULT_COMPACTION_SETTINGS: CompactionSettings = CompactionSettings {
    enabled: true,
    reserve_tokens: 16384,
    keep_recent_tokens: 20000,
};

/// Generated compaction data ready to be persisted as a compaction entry.
/// Mirrors pi's `CompactionResult` (with `details` concretely typed as
/// [`CompactionDetails`], which is what [`compact`] always produces).
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionResult {
    /// Summary text that replaces compacted history in future context.
    pub summary: String,
    /// Entry id where retained history starts.
    pub first_kept_entry_id: String,
    /// Estimated context tokens before compaction.
    pub tokens_before: i64,
    /// Implementation-specific details stored with the compaction entry.
    pub details: Option<CompactionDetails>,
}

/// Estimated context-token usage for a message list. Mirrors pi's
/// `ContextUsageEstimate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    /// Estimated total context tokens.
    pub tokens: i64,
    /// Tokens reported by the most recent assistant usage block.
    pub usage_tokens: i64,
    /// Estimated tokens after the most recent assistant usage block.
    pub trailing_tokens: i64,
    /// Index of the message that provided usage, or `None` when none exists.
    pub last_usage_index: Option<usize>,
}

/// Cut point selected for compaction. Mirrors pi's `CutPointResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutPointResult {
    /// Index of the first entry retained after compaction.
    pub first_kept_entry_index: usize,
    /// Index of the turn-start entry when the cut splits a turn, otherwise -1.
    pub turn_start_index: i64,
    /// Whether the selected cut point splits an in-progress turn.
    pub is_split_turn: bool,
}

/// Prepared inputs for a compaction run. Mirrors pi's `CompactionPreparation`.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionPreparation {
    /// Entry id where retained history starts.
    pub first_kept_entry_id: String,
    /// Messages summarized into the history summary.
    pub messages_to_summarize: Vec<AgentMessage>,
    /// Prefix messages summarized separately when compaction splits a turn.
    pub turn_prefix_messages: Vec<AgentMessage>,
    /// Whether compaction splits a turn.
    pub is_split_turn: bool,
    /// Estimated context tokens before compaction.
    pub tokens_before: i64,
    /// Previous compaction summary used for iterative updates.
    pub previous_summary: Option<String>,
    /// File operations extracted from summarized history.
    pub file_ops: FileOperations,
    /// Settings used to prepare compaction.
    pub settings: CompactionSettings,
}

// ---------------------------------------------------------------------------
// safeJsonStringify (`compaction.ts`) — re-exposed via utils.
// ---------------------------------------------------------------------------

fn extract_file_operations(
    messages: &[AgentMessage],
    entries: &[SessionTreeEntry],
    prev_compaction_index: i64,
) -> FileOperations {
    let mut file_ops = create_file_ops();
    if prev_compaction_index >= 0 {
        if let SessionTreeEntry::Compaction(prev) = &entries[prev_compaction_index as usize] {
            // pi: !prevCompaction.fromHook && prevCompaction.details
            if !matches!(prev.from_hook, Some(true)) {
                if let Some(details) = &prev.details {
                    if let Some(read_files) = details.get("readFiles").and_then(Value::as_array) {
                        for f in read_files.iter().filter_map(Value::as_str) {
                            file_ops.read.insert(f.to_string());
                        }
                    }
                    if let Some(modified) = details.get("modifiedFiles").and_then(Value::as_array) {
                        for f in modified.iter().filter_map(Value::as_str) {
                            file_ops.edited.insert(f.to_string());
                        }
                    }
                }
            }
        }
    }
    for msg in messages {
        extract_file_ops_from_message(msg, &mut file_ops);
    }
    file_ops
}

/// Project a session entry to the agent message it contributes. Mirrors pi's
/// `getMessageFromEntry` (the compaction-module copy).
fn get_message_from_entry(entry: &SessionTreeEntry) -> Option<AgentMessage> {
    match entry {
        SessionTreeEntry::Message(e) => Some(e.message.clone()),
        SessionTreeEntry::CustomMessage(e) => Some(create_custom_message(
            &e.custom_type,
            &e.content,
            e.display,
            e.details.as_ref(),
            &e.timestamp,
        )),
        SessionTreeEntry::BranchSummary(e) => Some(create_branch_summary_message(
            &e.summary,
            &e.from_id,
            &e.timestamp,
        )),
        SessionTreeEntry::Compaction(e) => Some(create_compaction_summary_message(
            &e.summary,
            e.tokens_before,
            &e.timestamp,
        )),
        _ => None,
    }
}

/// As [`get_message_from_entry`], but drops `compaction` entries. Mirrors pi's
/// `getMessageFromEntryForCompaction`.
fn get_message_from_entry_for_compaction(entry: &SessionTreeEntry) -> Option<AgentMessage> {
    if matches!(entry, SessionTreeEntry::Compaction(_)) {
        return None;
    }
    get_message_from_entry(entry)
}

/// Calculate total context tokens from provider usage. Mirrors pi's
/// `calculateContextTokens`.
pub fn calculate_context_tokens(usage: &Usage) -> i64 {
    if usage.total_tokens != 0 {
        usage.total_tokens as i64
    } else {
        (usage.input + usage.output + usage.cache_read + usage.cache_write) as i64
    }
}

/// Extract usage from an assistant `AgentMessage`, honoring pi's guards
/// (non-`aborted`/`error` stop reason, positive token count). Mirrors pi's
/// `getAssistantUsage`.
fn get_assistant_usage(msg: &AgentMessage) -> Option<Usage> {
    if msg.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let stop_reason = msg.get("stopReason").and_then(Value::as_str);
    if stop_reason == Some("aborted") || stop_reason == Some("error") {
        return None;
    }
    let usage_value = msg.get("usage")?;
    let usage: Usage = serde_json::from_value(usage_value.clone()).ok()?;
    if calculate_context_tokens(&usage) > 0 {
        Some(usage)
    } else {
        None
    }
}

/// Return usage from the last valid assistant message in session entries.
/// Mirrors pi's `getLastAssistantUsage`.
pub fn get_last_assistant_usage(entries: &[SessionTreeEntry]) -> Option<Usage> {
    for entry in entries.iter().rev() {
        if let SessionTreeEntry::Message(e) = entry {
            if let Some(usage) = get_assistant_usage(&e.message) {
                return Some(usage);
            }
        }
    }
    None
}

fn get_last_assistant_usage_info(messages: &[AgentMessage]) -> Option<(Usage, usize)> {
    for (i, msg) in messages.iter().enumerate().rev() {
        if let Some(usage) = get_assistant_usage(msg) {
            return Some((usage, i));
        }
    }
    None
}

/// Estimate context tokens for messages using provider usage when available.
/// Mirrors pi's `estimateContextTokens`.
pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    let Some((usage, index)) = get_last_assistant_usage_info(messages) else {
        let mut estimated = 0;
        for message in messages {
            estimated += estimate_tokens(message);
        }
        return ContextUsageEstimate {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        };
    };

    let usage_tokens = calculate_context_tokens(&usage);
    let mut trailing_tokens = 0;
    for message in &messages[index + 1..] {
        trailing_tokens += estimate_tokens(message);
    }

    ContextUsageEstimate {
        tokens: usage_tokens + trailing_tokens,
        usage_tokens,
        trailing_tokens,
        last_usage_index: Some(index),
    }
}

/// Return whether context usage exceeds the configured compaction threshold.
/// Mirrors pi's `shouldCompact`.
pub fn should_compact(
    context_tokens: i64,
    context_window: i64,
    settings: &CompactionSettings,
) -> bool {
    if !settings.enabled {
        return false;
    }
    context_tokens > context_window - settings.reserve_tokens
}

/// pi's `ESTIMATED_IMAGE_CHARS` (`compaction.ts`): the char budget an image
/// block contributes to token estimation.
pub const ESTIMATED_IMAGE_CHARS: usize = 4800;

fn estimate_text_and_image_content_chars(content: &Value) -> usize {
    if let Some(s) = content.as_str() {
        return js_len(s);
    }
    let Some(blocks) = content.as_array() else {
        return 0;
    };
    let mut chars = 0;
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    chars += js_len(text);
                }
            }
            Some("image") => chars += ESTIMATED_IMAGE_CHARS,
            _ => {}
        }
    }
    chars
}

/// Estimate token count for one message using a conservative character
/// heuristic (`ceil(chars / 4)`). Mirrors pi's `estimateTokens`.
pub fn estimate_tokens(message: &AgentMessage) -> i64 {
    let chars: usize = match message.get("role").and_then(Value::as_str) {
        Some("user") => message
            .get("content")
            .map(estimate_text_and_image_content_chars)
            .unwrap_or(0),
        Some("assistant") => {
            let mut chars = 0;
            if let Some(blocks) = message.get("content").and_then(Value::as_array) {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = block.get("text").and_then(Value::as_str) {
                                chars += js_len(t);
                            }
                        }
                        Some("thinking") => {
                            if let Some(t) = block.get("thinking").and_then(Value::as_str) {
                                chars += js_len(t);
                            }
                        }
                        Some("toolCall") => {
                            let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                            let args = block.get("arguments").cloned().unwrap_or(Value::Null);
                            chars += js_len(name) + js_len(&safe_json_stringify(&args));
                        }
                        _ => {}
                    }
                }
            }
            chars
        }
        Some("custom") | Some("toolResult") => message
            .get("content")
            .map(estimate_text_and_image_content_chars)
            .unwrap_or(0),
        Some("bashExecution") => {
            let command = message.get("command").and_then(Value::as_str).unwrap_or("");
            let output = message.get("output").and_then(Value::as_str).unwrap_or("");
            js_len(command) + js_len(output)
        }
        Some("branchSummary") | Some("compactionSummary") => message
            .get("summary")
            .and_then(Value::as_str)
            .map(js_len)
            .unwrap_or(0),
        _ => return 0,
    };
    chars.div_ceil(4) as i64
}

fn find_valid_cut_points(
    entries: &[SessionTreeEntry],
    start_index: usize,
    end_index: usize,
) -> Vec<usize> {
    let mut cut_points: Vec<usize> = Vec::new();
    for (i, entry) in entries.iter().enumerate().take(end_index).skip(start_index) {
        match entry {
            SessionTreeEntry::Message(e) => {
                match e.message.get("role").and_then(Value::as_str) {
                    Some("bashExecution")
                    | Some("custom")
                    | Some("branchSummary")
                    | Some("compactionSummary")
                    | Some("user")
                    | Some("assistant") => {
                        cut_points.push(i);
                    }
                    // "toolResult" and everything else: not a cut point.
                    _ => {}
                }
            }
            SessionTreeEntry::BranchSummary(_) | SessionTreeEntry::CustomMessage(_) => {
                cut_points.push(i);
            }
            _ => {}
        }
    }
    cut_points
}

/// Find the user-visible message that starts the turn containing an entry.
/// Mirrors pi's `findTurnStartIndex`. Returns -1 when no turn start is found.
pub fn find_turn_start_index(
    entries: &[SessionTreeEntry],
    entry_index: usize,
    start_index: usize,
) -> i64 {
    let mut i = entry_index as i64;
    while i >= start_index as i64 {
        let entry = &entries[i as usize];
        match entry {
            SessionTreeEntry::BranchSummary(_) | SessionTreeEntry::CustomMessage(_) => {
                return i;
            }
            SessionTreeEntry::Message(e) => match e.message.get("role").and_then(Value::as_str) {
                Some("user") | Some("bashExecution") => return i,
                _ => {}
            },
            _ => {}
        }
        i -= 1;
    }
    -1
}

/// Find the compaction cut point that keeps approximately the requested
/// recent-token budget. Mirrors pi's `findCutPoint`.
pub fn find_cut_point(
    entries: &[SessionTreeEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: i64,
) -> CutPointResult {
    let cut_points = find_valid_cut_points(entries, start_index, end_index);

    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: -1,
            is_split_turn: false,
        };
    }

    let mut accumulated_tokens = 0;
    let mut cut_index = cut_points[0];

    let mut i = end_index as i64 - 1;
    while i >= start_index as i64 {
        let entry = &entries[i as usize];
        if let SessionTreeEntry::Message(e) = entry {
            accumulated_tokens += estimate_tokens(&e.message);
            if accumulated_tokens >= keep_recent_tokens {
                for &c in &cut_points {
                    if c >= i as usize {
                        cut_index = c;
                        break;
                    }
                }
                break;
            }
        }
        i -= 1;
    }

    while cut_index > start_index {
        let prev_entry = &entries[cut_index - 1];
        if matches!(prev_entry, SessionTreeEntry::Compaction(_)) {
            break;
        }
        if matches!(prev_entry, SessionTreeEntry::Message(_)) {
            break;
        }
        cut_index -= 1;
    }

    let cut_entry = &entries[cut_index];
    let is_user_message = matches!(
        cut_entry,
        SessionTreeEntry::Message(e) if e.message.get("role").and_then(Value::as_str) == Some("user")
    );
    let turn_start_index = if is_user_message {
        -1
    } else {
        find_turn_start_index(entries, cut_index, start_index)
    };

    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !is_user_message && turn_start_index != -1,
    }
}

// ---------------------------------------------------------------------------
// Prompt constants (byte-for-byte from compaction.ts).
// ---------------------------------------------------------------------------

/// pi's `SUMMARIZATION_SYSTEM_PROMPT`.
pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or \"(none)\" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed
- UPDATE \"Next Steps\" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

const TURN_PREFIX_SUMMARIZATION_PROMPT: &str =
    "This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.

Summarize the prefix to provide context for the retained suffix:

## Original Request
[What did the user ask for in this turn?]

## Early Progress
- [Key decisions and work done in the prefix]

## Context for Suffix
- [Information needed to understand the retained recent work]

Be concise. Focus on what's needed to understand the kept suffix.";

// ---------------------------------------------------------------------------
// Summary generation.
// ---------------------------------------------------------------------------

/// Build the single-user-message context summarization sends to the model.
fn build_summarization_context(system_prompt: &str, prompt_text: String) -> Context {
    let message = json!({
        "role": "user",
        "content": [{ "type": "text", "text": prompt_text }],
        "timestamp": 0,
    });
    // Round-trips through the pi-ai Message shape; the user text message is a
    // strict subset so deserialization always succeeds.
    let message: Message =
        serde_json::from_value(message).expect("summarization user message is valid");
    Context {
        system_prompt: Some(system_prompt.to_string()),
        messages: vec![message],
        tools: None,
    }
}

/// The model's max-tokens cap for a summarization request: `min(factor *
/// reserveTokens, model.maxTokens)` when the model reports a positive cap.
fn summarization_max_tokens(factor: f64, reserve_tokens: i64, model: &Model) -> i64 {
    let cap = (factor * reserve_tokens as f64).floor() as i64;
    if model.max_tokens > 0 {
        cap.min(model.max_tokens as i64)
    } else {
        cap
    }
}

/// The completion options for a summarization request, attaching `reasoning`
/// only for reasoning models with a non-`off` thinking level.
fn summarization_options(
    max_tokens: i64,
    model: &Model,
    signal: Option<&AbortSignal>,
    thinking_level: Option<&str>,
) -> CompletionOptions {
    let reasoning = match thinking_level {
        Some(level) if model.reasoning && level != "off" => Some(level.to_string()),
        _ => None,
    };
    CompletionOptions {
        max_tokens,
        signal: signal.cloned(),
        reasoning,
    }
}

/// Join the text blocks of an assistant response with newlines.
fn response_text(response: &AssistantMessage) -> String {
    response
        .content
        .iter()
        .filter_map(|c| match c {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// pi's `errorMessage || fallback`: an absent or empty message uses `fallback`.
fn error_message_or(response: &AssistantMessage, fallback: &str) -> String {
    match &response.error_message {
        Some(m) if !m.is_empty() => m.clone(),
        _ => fallback.to_string(),
    }
}

/// Generate or update a conversation summary for compaction. Mirrors pi's
/// `generateSummary`.
#[allow(clippy::too_many_arguments)]
pub fn generate_summary(
    current_messages: &[AgentMessage],
    models: &dyn Models,
    model: &Model,
    reserve_tokens: i64,
    signal: Option<&AbortSignal>,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    thinking_level: Option<&str>,
) -> Result<String, CompactionError> {
    let max_tokens = summarization_max_tokens(0.8, reserve_tokens, model);
    let mut base_prompt = if previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT.to_string()
    } else {
        SUMMARIZATION_PROMPT.to_string()
    };
    if let Some(instructions) = custom_instructions {
        base_prompt = format!("{base_prompt}\n\nAdditional focus: {instructions}");
    }
    let llm_messages = convert_to_llm(current_messages);
    let conversation_text = serialize_conversation(&llm_messages);
    let mut prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n");
    if let Some(previous) = previous_summary {
        prompt_text.push_str(&format!(
            "<previous-summary>\n{previous}\n</previous-summary>\n\n"
        ));
    }
    prompt_text.push_str(&base_prompt);

    let context = build_summarization_context(SUMMARIZATION_SYSTEM_PROMPT, prompt_text);
    let options = summarization_options(max_tokens, model, signal, thinking_level);

    let response = models.complete_simple(model, &context, &options);
    match response.stop_reason {
        StopReason::Aborted => {
            return Err(CompactionError::new(
                CompactionErrorCode::Aborted,
                error_message_or(&response, "Summarization aborted"),
            ));
        }
        StopReason::Error => {
            return Err(CompactionError::new(
                CompactionErrorCode::SummarizationFailed,
                format!(
                    "Summarization failed: {}",
                    error_message_or(&response, "Unknown error")
                ),
            ));
        }
        _ => {}
    }

    Ok(response_text(&response))
}

/// Prepare session entries for compaction, or return `None` when compaction is
/// not applicable. Mirrors pi's `prepareCompaction`.
pub fn prepare_compaction(
    path_entries: &[SessionTreeEntry],
    settings: &CompactionSettings,
) -> Result<Option<CompactionPreparation>, CompactionError> {
    if path_entries.is_empty()
        || matches!(path_entries.last(), Some(SessionTreeEntry::Compaction(_)))
    {
        return Ok(None);
    }

    let mut prev_compaction_index: i64 = -1;
    for (i, entry) in path_entries.iter().enumerate().rev() {
        if matches!(entry, SessionTreeEntry::Compaction(_)) {
            prev_compaction_index = i as i64;
            break;
        }
    }

    let mut previous_summary: Option<String> = None;
    let mut boundary_start = 0usize;
    if prev_compaction_index >= 0 {
        if let SessionTreeEntry::Compaction(prev) = &path_entries[prev_compaction_index as usize] {
            previous_summary = Some(prev.summary.clone());
            let first_kept = path_entries
                .iter()
                .position(|entry| entry.id() == prev.first_kept_entry_id);
            boundary_start = match first_kept {
                Some(idx) => idx,
                None => (prev_compaction_index + 1) as usize,
            };
        }
    }
    let boundary_end = path_entries.len();

    let context_messages =
        build_session_context(path_entries, &SessionContextBuildOptions::default()).messages;
    let tokens_before = estimate_context_tokens(&context_messages).tokens;

    let cut_point = find_cut_point(
        path_entries,
        boundary_start,
        boundary_end,
        settings.keep_recent_tokens,
    );
    let first_kept_entry = &path_entries[cut_point.first_kept_entry_index];
    if first_kept_entry.id().is_empty() {
        return Err(CompactionError::new(
            CompactionErrorCode::InvalidSession,
            "First kept entry has no UUID - session may need migration",
        ));
    }
    let first_kept_entry_id = first_kept_entry.id().to_string();

    let history_end = if cut_point.is_split_turn {
        cut_point.turn_start_index as usize
    } else {
        cut_point.first_kept_entry_index
    };
    let mut messages_to_summarize: Vec<AgentMessage> = Vec::new();
    for entry in &path_entries[boundary_start..history_end] {
        if let Some(msg) = get_message_from_entry_for_compaction(entry) {
            messages_to_summarize.push(msg);
        }
    }
    let mut turn_prefix_messages: Vec<AgentMessage> = Vec::new();
    if cut_point.is_split_turn {
        for entry in
            &path_entries[cut_point.turn_start_index as usize..cut_point.first_kept_entry_index]
        {
            if let Some(msg) = get_message_from_entry_for_compaction(entry) {
                turn_prefix_messages.push(msg);
            }
        }
    }
    let mut file_ops =
        extract_file_operations(&messages_to_summarize, path_entries, prev_compaction_index);
    if cut_point.is_split_turn {
        for msg in &turn_prefix_messages {
            extract_file_ops_from_message(msg, &mut file_ops);
        }
    }

    Ok(Some(CompactionPreparation {
        first_kept_entry_id,
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings: settings.clone(),
    }))
}

/// Generate compaction summary data from prepared session history. Mirrors pi's
/// `compact`.
pub fn compact(
    preparation: &CompactionPreparation,
    models: &dyn Models,
    model: &Model,
    custom_instructions: Option<&str>,
    signal: Option<&AbortSignal>,
    thinking_level: Option<&str>,
) -> Result<CompactionResult, CompactionError> {
    if preparation.first_kept_entry_id.is_empty() {
        return Err(CompactionError::new(
            CompactionErrorCode::InvalidSession,
            "First kept entry has no UUID - session may need migration",
        ));
    }

    let mut summary: String;

    if preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty() {
        let history = if !preparation.messages_to_summarize.is_empty() {
            generate_summary(
                &preparation.messages_to_summarize,
                models,
                model,
                preparation.settings.reserve_tokens,
                signal,
                custom_instructions,
                preparation.previous_summary.as_deref(),
                thinking_level,
            )?
        } else {
            "No prior history.".to_string()
        };
        let turn_prefix = generate_turn_prefix_summary(
            &preparation.turn_prefix_messages,
            models,
            model,
            preparation.settings.reserve_tokens,
            signal,
            thinking_level,
        )?;
        summary = format!("{history}\n\n---\n\n**Turn Context (split turn):**\n\n{turn_prefix}");
    } else {
        summary = generate_summary(
            &preparation.messages_to_summarize,
            models,
            model,
            preparation.settings.reserve_tokens,
            signal,
            custom_instructions,
            preparation.previous_summary.as_deref(),
            thinking_level,
        )?;
    }

    let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
    summary.push_str(&format_file_operations(&read_files, &modified_files));

    Ok(CompactionResult {
        summary,
        first_kept_entry_id: preparation.first_kept_entry_id.clone(),
        tokens_before: preparation.tokens_before,
        details: Some(CompactionDetails {
            read_files,
            modified_files,
        }),
    })
}

fn generate_turn_prefix_summary(
    messages: &[AgentMessage],
    models: &dyn Models,
    model: &Model,
    reserve_tokens: i64,
    signal: Option<&AbortSignal>,
    thinking_level: Option<&str>,
) -> Result<String, CompactionError> {
    let max_tokens = summarization_max_tokens(0.5, reserve_tokens, model);
    let llm_messages = convert_to_llm(messages);
    let conversation_text = serialize_conversation(&llm_messages);
    let prompt_text = format!(
        "<conversation>\n{conversation_text}\n</conversation>\n\n{TURN_PREFIX_SUMMARIZATION_PROMPT}"
    );

    let context = build_summarization_context(SUMMARIZATION_SYSTEM_PROMPT, prompt_text);
    let options = summarization_options(max_tokens, model, signal, thinking_level);

    let response = models.complete_simple(model, &context, &options);
    match response.stop_reason {
        StopReason::Aborted => {
            return Err(CompactionError::new(
                CompactionErrorCode::Aborted,
                error_message_or(&response, "Turn prefix summarization aborted"),
            ));
        }
        StopReason::Error => {
            return Err(CompactionError::new(
                CompactionErrorCode::SummarizationFailed,
                format!(
                    "Turn prefix summarization failed: {}",
                    error_message_or(&response, "Unknown error")
                ),
            ));
        }
        _ => {}
    }

    Ok(response_text(&response))
}
