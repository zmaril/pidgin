// straitjacket-allow-file:duplication faithful mirror of pi coding-agent
// compaction/compaction.ts; parallel structure to the agent-core copy
// (crates/atilla-agent/src/harness/compaction/compaction.rs) is intentional.

//! Compaction: token estimation, cut-point detection, and summary generation,
//! mirroring `packages/coding-agent/src/core/compaction/compaction.ts`.
//!
//! This is the coding-agent copy. Unlike the agent-core copy, its cut-point and
//! token logic never reads `entry.message` 1:1 — it expands each entry through
//! [`session_entry_to_context_messages`] and operates on the resulting
//! [`AgentMessage`] array. This is what lets a context-visible custom-message
//! entry participate in the recent-token budget and turn-start detection.
//!
//! # Model abstraction
//!
//! pi drives summarization through `completeSimple` from `@earendil-works/pi-ai`.
//! atilla-ai ports the `Provider`/faux surface but not yet a `Models`/
//! `completeSimple` wrapper, so this module defines a minimal [`Models`] trait
//! mirroring pi's `completeSimple(model, context, options)` signature. The
//! deterministic surface (token math, cut points, preparation, serialization) is
//! independent of it; only [`generate_summary`] and [`compact`] call through it.
//! A test-only fake lives in `tests/compaction.rs`.

use std::fmt;

use serde_json::{json, Value};

use atilla_ai::seams::AbortSignal;
use atilla_ai::{AssistantMessage, Context, Message, Model, StopReason, Usage};

use super::utils::{
    compute_file_lists, convert_to_llm, create_file_ops, error_message_or,
    extract_file_ops_from_details, extract_file_ops_from_message, format_file_operations, js_len,
    json_stringify, response_text, serialize_conversation, FileOperations,
    SUMMARIZATION_SYSTEM_PROMPT,
};
use crate::core::session_manager::{
    build_session_context, session_entry_to_context_messages, AgentMessage, SessionEntry,
};

// ---------------------------------------------------------------------------
// Error type (mirrors the agent-core `CompactionError`; pi's coding-agent copy
// is throw-based, and this is the idiomatic Result mapping of those throws).
// ---------------------------------------------------------------------------

/// Stable compaction error codes returned by compaction helpers. Mirrors the
/// agent-core `CompactionErrorCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionErrorCode {
    Aborted,
    SummarizationFailed,
    InvalidSession,
    Unknown,
}

impl CompactionErrorCode {
    /// The wire string for this code (`CompactionError.code`).
    pub fn as_str(self) -> &'static str {
        match self {
            CompactionErrorCode::Aborted => "aborted",
            CompactionErrorCode::SummarizationFailed => "summarization_failed",
            CompactionErrorCode::InvalidSession => "invalid_session",
            CompactionErrorCode::Unknown => "unknown",
        }
    }
}

/// Error returned by compaction helpers. The idiomatic Result mapping of the
/// `throw new Error(...)` sites in pi's coding-agent compaction.
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
/// `SimpleStreamOptions` compaction sets: `maxTokens`, `signal`, and an optional
/// `reasoning` (thinking) level.
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
/// `completeSimple` call compaction uses.
pub trait Models {
    /// Complete `context` with `model`, returning the final assistant message.
    /// Mirrors `completeSimple`, which resolves the streamed result.
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
/// Mirrors pi's `CompactionResult`.
///
/// Extension seam: pi's `CompactionResult<T>` carries a generic `details?: T`
/// "extension-specific data (e.g. ArtifactIndex)". atilla-coding has no
/// extension engine yet, so [`compact`] always produces the concrete
/// [`CompactionDetails`] (file lists). A future extension engine would surface
/// its own structured payload through this field.
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
// File operation extraction (`extractFileOperations`).
// ---------------------------------------------------------------------------

fn extract_file_operations(
    messages: &[AgentMessage],
    entries: &[SessionEntry],
    prev_compaction_index: i64,
) -> FileOperations {
    let mut file_ops = create_file_ops();
    if prev_compaction_index >= 0 {
        if let SessionEntry::Compaction(prev) = &entries[prev_compaction_index as usize] {
            // Extension seam: pi guards this on `!prevCompaction.fromHook` so
            // extension-authored (hook) compactions are not re-counted. atilla
            // has no extension engine yet; the `from_hook` field is honored here
            // for session-file compatibility.
            if !matches!(prev.from_hook, Some(true)) {
                if let Some(details) = &prev.details {
                    extract_file_ops_from_details(details, &mut file_ops);
                }
            }
        }
    }
    for msg in messages {
        extract_file_ops_from_message(msg, &mut file_ops);
    }
    file_ops
}

// ---------------------------------------------------------------------------
// Message extraction (entry expansion — divergence from the agent-core copy).
// ---------------------------------------------------------------------------

/// Extract the [`AgentMessage`] an entry contributes for compaction, dropping
/// `compaction` entries. Mirrors pi's `getMessageFromEntryForCompaction`, which
/// returns `sessionEntryToContextMessages(entry)[0]` — the first expanded
/// message — rather than reading `entry.message` directly.
fn get_message_from_entry_for_compaction(entry: &SessionEntry) -> Option<AgentMessage> {
    if matches!(entry, SessionEntry::Compaction(_)) {
        return None;
    }
    session_entry_to_context_messages(entry).into_iter().next()
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
    if matches!(stop_reason, Some("aborted" | "error")) {
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
pub fn get_last_assistant_usage(entries: &[SessionEntry]) -> Option<Usage> {
    for entry in entries.iter().rev() {
        if let SessionEntry::Message(e) = entry {
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
        let estimated: i64 = messages.iter().map(estimate_tokens).sum();
        return ContextUsageEstimate {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        };
    };

    let usage_tokens = calculate_context_tokens(&usage);
    let trailing_tokens: i64 = messages[index + 1..].iter().map(estimate_tokens).sum();

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
                // straitjacket-allow:duplication
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
                            // Divergence: pi's coding-agent uses bare
                            // `JSON.stringify(block.arguments)` here.
                            let args = block.get("arguments").cloned().unwrap_or(Value::Null);
                            chars += js_len(name) + js_len(&json_stringify(&args));
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

/// Whether an expanded message can serve as a cut point. Mirrors pi's
/// `isCutPointMessage` (never cut at a `toolResult`).
fn is_cut_point_message(message: &AgentMessage) -> bool {
    matches!(
        message.get("role").and_then(Value::as_str),
        Some("user")
            | Some("assistant")
            | Some("bashExecution")
            | Some("custom")
            | Some("branchSummary")
            | Some("compactionSummary")
    )
}

/// Whether an expanded message begins a turn. Mirrors pi's `isTurnStartMessage`
/// (assistant and toolResult messages are mid-turn).
fn is_turn_start_message(message: &AgentMessage) -> bool {
    matches!(
        message.get("role").and_then(Value::as_str),
        Some("user")
            | Some("bashExecution")
            | Some("custom")
            | Some("branchSummary")
            | Some("compactionSummary")
    )
}

/// Whether an entry begins a turn, via entry expansion. Mirrors pi's
/// `isTurnStartEntry`: `compaction` entries are never turn starts, and every
/// other entry is checked through its expanded context messages.
fn is_turn_start_entry(entry: &SessionEntry) -> bool {
    if matches!(entry, SessionEntry::Compaction(_)) {
        return false;
    }
    session_entry_to_context_messages(entry)
        .iter()
        .any(is_turn_start_message)
}

/// Find valid cut points via entry expansion. Mirrors pi's `findValidCutPoints`.
fn find_valid_cut_points(
    entries: &[SessionEntry],
    start_index: usize,
    end_index: usize,
) -> Vec<usize> {
    let mut cut_points: Vec<usize> = Vec::new();
    for (i, entry) in entries.iter().enumerate().take(end_index).skip(start_index) {
        if matches!(entry, SessionEntry::Compaction(_)) {
            continue;
        }
        if session_entry_to_context_messages(entry)
            .iter()
            .any(is_cut_point_message)
        {
            cut_points.push(i);
        }
    }
    cut_points
}

/// Find the context-visible entry that starts the turn containing `entry_index`.
/// Mirrors pi's `findTurnStartIndex`. Returns -1 when no turn start is found.
pub fn find_turn_start_index(
    entries: &[SessionEntry],
    entry_index: usize,
    start_index: usize,
) -> i64 {
    let mut i = entry_index as i64;
    while i >= start_index as i64 {
        if is_turn_start_entry(&entries[i as usize]) {
            return i;
        }
        i -= 1;
    }
    -1
}

/// Find the compaction cut point that keeps approximately the requested
/// recent-token budget. Mirrors pi's `findCutPoint`.
///
/// Divergence from the agent-core copy: token accumulation is summed over ALL
/// expanded messages per entry (`sessionEntryToContextMessages(entry)`), and an
/// entry contributing zero tokens is skipped, so context-visible non-`message`
/// entries (custom messages, branch/compaction summaries) participate in the
/// budget.
pub fn find_cut_point(
    entries: &[SessionEntry],
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
        let message_tokens: i64 = session_entry_to_context_messages(entry)
            .iter()
            .map(estimate_tokens)
            .sum();
        if message_tokens == 0 {
            i -= 1;
            continue;
        }
        accumulated_tokens += message_tokens;

        if accumulated_tokens >= keep_recent_tokens {
            for &c in &cut_points {
                if c >= i as usize {
                    cut_index = c;
                    break;
                }
            }
            break;
        }
        i -= 1;
    }

    // Scan backwards to include adjacent metadata entries that do not affect
    // context. Stop at compaction boundaries or context-visible entries.
    while cut_index > start_index {
        let prev_entry = &entries[cut_index - 1];
        if matches!(prev_entry, SessionEntry::Compaction(_))
            || !session_entry_to_context_messages(prev_entry).is_empty()
        {
            break;
        }
        cut_index -= 1;
    }

    let cut_entry = &entries[cut_index];
    let starts_turn = is_turn_start_entry(cut_entry);
    let turn_start_index = if starts_turn {
        -1
    } else {
        find_turn_start_index(entries, cut_index, start_index)
    };

    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index != -1,
    }
}

// ---------------------------------------------------------------------------
// Prompt constants (byte-for-byte from compaction.ts).
// ---------------------------------------------------------------------------

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
///
/// Exposed so a future napi side can reproduce the exact [`Context`] each
/// summarization call uses (system prompt + user message).
pub fn build_summarization_context(system_prompt: &str, prompt_text: String) -> Context {
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

/// The numeric completion controls a summarization call applies. Mirrors the
/// subset of [`CompletionOptions`] that depends only on the model, reserve
/// budget, and thinking level. Exposed so a future napi side can reproduce the
/// exact `max_tokens` and `reasoning` each summarization request uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummarizationRequestOptions {
    /// The `maxTokens` cap for the request.
    pub max_tokens: i64,
    /// The `reasoning` (thinking) level, or `None` when reasoning is off.
    pub reasoning: Option<String>,
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

/// The `reasoning` level for a summarization request, present only for reasoning
/// models with a non-`off` thinking level.
fn summarization_reasoning(model: &Model, thinking_level: Option<&str>) -> Option<String> {
    match thinking_level {
        Some(level) if model.reasoning && level != "off" => Some(level.to_string()),
        _ => None,
    }
}

/// The request options for a history summarization call (`generateSummary`'s
/// `0.8 * reserveTokens` max-tokens factor).
pub fn build_summary_options(
    reserve_tokens: i64,
    model: &Model,
    thinking_level: Option<&str>,
) -> SummarizationRequestOptions {
    SummarizationRequestOptions {
        max_tokens: summarization_max_tokens(0.8, reserve_tokens, model),
        reasoning: summarization_reasoning(model, thinking_level),
    }
}

/// The request options for a turn-prefix summarization call
/// (`generateTurnPrefixSummary`'s `0.5 * reserveTokens` max-tokens factor).
pub fn build_turn_prefix_options(
    reserve_tokens: i64,
    model: &Model,
    thinking_level: Option<&str>,
) -> SummarizationRequestOptions {
    SummarizationRequestOptions {
        max_tokens: summarization_max_tokens(0.5, reserve_tokens, model),
        reasoning: summarization_reasoning(model, thinking_level),
    }
}

/// Build the user-message prompt text for a history summarization call. Mirrors
/// the prompt assembly inside pi's `generateSummary`.
pub fn build_summary_prompt(
    current_messages: &[AgentMessage],
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
) -> String {
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
    prompt_text
}

/// Build the user-message prompt text for a turn-prefix summarization call.
/// Mirrors the prompt assembly inside pi's `generateTurnPrefixSummary`.
pub fn build_turn_prefix_prompt(messages: &[AgentMessage]) -> String {
    let llm_messages = convert_to_llm(messages);
    let conversation_text = serialize_conversation(&llm_messages);
    format!(
        "<conversation>\n{conversation_text}\n</conversation>\n\n{TURN_PREFIX_SUMMARIZATION_PROMPT}"
    )
}

/// Run a summarization completion and map its stop reason to a
/// [`CompactionError`], returning the joined response text on success. Shared by
/// [`generate_summary`] and [`generate_turn_prefix_summary`].
fn run_summarization(
    models: &dyn Models,
    model: &Model,
    context: Context,
    request_options: &SummarizationRequestOptions,
    signal: Option<&AbortSignal>,
    aborted_message: &str,
    failed_label: &str,
) -> Result<String, CompactionError> {
    let options = CompletionOptions {
        max_tokens: request_options.max_tokens,
        signal: signal.cloned(),
        reasoning: request_options.reasoning.clone(),
    };

    let response = models.complete_simple(model, &context, &options);
    match response.stop_reason {
        StopReason::Aborted => {
            return Err(CompactionError::new(
                CompactionErrorCode::Aborted,
                error_message_or(&response, aborted_message),
            ));
        }
        StopReason::Error => {
            return Err(CompactionError::new(
                CompactionErrorCode::SummarizationFailed,
                format!(
                    "{failed_label}: {}",
                    error_message_or(&response, "Unknown error")
                ),
            ));
        }
        _ => {}
    }

    Ok(response_text(&response))
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
    let request_options = build_summary_options(reserve_tokens, model, thinking_level);
    let prompt_text = build_summary_prompt(current_messages, custom_instructions, previous_summary);
    let context = build_summarization_context(SUMMARIZATION_SYSTEM_PROMPT, prompt_text);

    run_summarization(
        models,
        model,
        context,
        &request_options,
        signal,
        "Summarization aborted",
        "Summarization failed",
    )
}

/// Prepare session entries for compaction, or return `None` when compaction is
/// not applicable. Mirrors pi's `prepareCompaction`.
///
/// The signature returns `Result` to match the agent-core seam, but pi's
/// coding-agent `prepareCompaction` never throws — every non-applicable path
/// returns `undefined` (mapped to `Ok(None)` here), including the missing-id
/// "session needs migration" case, which the agent-core copy instead reports as
/// an error.
pub fn prepare_compaction(
    path_entries: &[SessionEntry],
    settings: &CompactionSettings,
) -> Result<Option<CompactionPreparation>, CompactionError> {
    if !path_entries.is_empty() && matches!(path_entries.last(), Some(SessionEntry::Compaction(_)))
    {
        return Ok(None);
    }

    let mut prev_compaction_index: i64 = -1;
    for (i, entry) in path_entries.iter().enumerate().rev() {
        if matches!(entry, SessionEntry::Compaction(_)) {
            prev_compaction_index = i as i64;
            break;
        }
    }

    let mut previous_summary: Option<String> = None;
    let mut boundary_start = 0usize;
    if prev_compaction_index >= 0 {
        if let SessionEntry::Compaction(prev) = &path_entries[prev_compaction_index as usize] {
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

    // Divergence: coding-agent expands entries via buildSessionContext (no
    // leafId), so context-visible custom-message entries count toward the
    // before-compaction token estimate.
    let context_messages = build_session_context(path_entries, None).messages;
    let tokens_before = estimate_context_tokens(&context_messages).tokens;

    let cut_point = find_cut_point(
        path_entries,
        boundary_start,
        boundary_end,
        settings.keep_recent_tokens,
    );

    // pi: `if (!firstKeptEntry?.id) return undefined;` — an out-of-range index
    // (empty session) or a missing id both mean the session needs migration.
    let Some(first_kept_entry) = path_entries.get(cut_point.first_kept_entry_index) else {
        return Ok(None);
    };
    if first_kept_entry.id().is_empty() {
        return Ok(None);
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

    if messages_to_summarize.is_empty() && turn_prefix_messages.is_empty() {
        return Ok(None);
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
    let summaries: Vec<String> =
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
            vec![history, turn_prefix]
        } else {
            vec![generate_summary(
                &preparation.messages_to_summarize,
                models,
                model,
                preparation.settings.reserve_tokens,
                signal,
                custom_instructions,
                preparation.previous_summary.as_deref(),
                thinking_level,
            )?]
        };

    // pi throws here if firstKeptEntryId is empty; mapped to an error result.
    if preparation.first_kept_entry_id.is_empty() {
        return Err(CompactionError::new(
            CompactionErrorCode::InvalidSession,
            "First kept entry has no UUID - session may need migration",
        ));
    }

    Ok(assemble_compaction_result(preparation, summaries))
}

/// Assemble a [`CompactionResult`] from the generated summary text and the
/// prepared file operations. Extracted from [`compact`]'s post-model block so a
/// future napi side can drive summarization itself and then reproduce the exact
/// result assembly.
///
/// `summaries` carries the model output: a single element is the normal path;
/// two elements are a split turn `[history, turn_prefix]`, concatenated here
/// byte-for-byte the same way [`compact`] does.
pub fn assemble_compaction_result(
    preparation: &CompactionPreparation,
    summaries: Vec<String>,
) -> CompactionResult {
    let mut summary = if summaries.len() >= 2 {
        let history = &summaries[0];
        let turn_prefix = &summaries[1];
        format!("{history}\n\n---\n\n**Turn Context (split turn):**\n\n{turn_prefix}")
    } else {
        summaries.into_iter().next().unwrap_or_default()
    };

    let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
    summary.push_str(&format_file_operations(&read_files, &modified_files));

    CompactionResult {
        summary,
        first_kept_entry_id: preparation.first_kept_entry_id.clone(),
        tokens_before: preparation.tokens_before,
        details: Some(CompactionDetails {
            read_files,
            modified_files,
        }),
    }
}

fn generate_turn_prefix_summary(
    messages: &[AgentMessage],
    models: &dyn Models,
    model: &Model,
    reserve_tokens: i64,
    signal: Option<&AbortSignal>,
    thinking_level: Option<&str>,
) -> Result<String, CompactionError> {
    let request_options = build_turn_prefix_options(reserve_tokens, model, thinking_level);
    let prompt_text = build_turn_prefix_prompt(messages);
    let context = build_summarization_context(SUMMARIZATION_SYSTEM_PROMPT, prompt_text);

    run_summarization(
        models,
        model,
        context,
        &request_options,
        signal,
        "Turn prefix summarization aborted",
        "Turn prefix summarization failed",
    )
}
