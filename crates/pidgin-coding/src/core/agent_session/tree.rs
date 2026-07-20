//! Session-tree navigation and branch summarization, ported from pi's
//! `AgentSession.navigateTree` / `getUserMessagesForForking` /
//! `abortBranchSummary` / `isCompacting` (`agent-session.ts:2836`, `:3027`,
//! `:1920`, `:931`).
//!
//! [`AgentSession::navigate_tree`] moves the session leaf to a different node in
//! the entry tree without minting a new session file (that is fork, a
//! `AgentSessionRuntime` concern in a separate slice). When navigating to a user
//! or custom message it moves the leaf to that entry's parent and returns the
//! message text so the caller can drop it back into the editor; when navigating
//! to any other entry the leaf becomes the selected entry. With `summarize` set it
//! first summarizes the abandoned branch (through the merged compaction seam's
//! [`generate_branch_summary`](crate::core::compaction::generate_branch_summary))
//! and attaches the result at the navigation target position.
//!
//! Two extension events bracket the leaf move: `session_before_tree` (dispatched
//! through the runner; may cancel the navigation, supply the branch summary, or
//! override the summarization instructions / label) and `session_tree` (fired
//! after the leaf move with the old/new leaf ids and the written summary entry).
//! With the always-compiled [`StubExtensionRunner`](crate::core::extensions::runner::StubExtensionRunner)
//! neither has handlers, so navigation follows the non-extension path.
//!
//! ## Sync/eager + `!Send` model note
//!
//! pi's `navigateTree` sets up an `AbortController` so a concurrent
//! `abortBranchSummary` (or an extension `session_before_tree` handler that awaits
//! the signal) can cancel a summarization in flight while
//! [`AgentSession::is_compacting`] reports `true`. Under the sync/eager agent the
//! whole `navigate_tree` call runs to completion on the calling thread, so there
//! is no window in which another `&self` method observes a mid-summarization
//! state. The abort *plumbing* is still wired faithfully — the branch-summary
//! signal is installed for the duration of the call, [`AgentSession::abort_branch_summary`]
//! trips it, and a summarizer that reports `aborted` yields the `cancelled` +
//! `aborted` result with the tree left unchanged — but the pi case that asserts
//! `isCompacting === true` *while* awaiting the abort is structurally N/A here and
//! is `#[ignore]`d with that reason.

// straitjacket-allow-file:duplication

use serde_json::{json, Value};

use pidgin_ai::seams::AbortSignal;

use crate::core::compaction::{
    collect_entries_for_branch_summary, generate_branch_summary, BranchSummaryErrorCode,
    GenerateBranchSummaryOptions,
};
use crate::core::extensions::events::session::{
    SessionBeforeTreeEvent, SessionBeforeTreeSummary, SessionTreeEvent, TreePreparation,
};
use crate::core::extensions::runner::{ExtensionDispatchEvent, ExtensionEmitOutcome};
use crate::core::session_manager::{BranchSummaryEntry, SessionEntry};

use super::session::AgentSession;

/// Options for [`AgentSession::navigate_tree`] (pi's `navigateTree` `options`
/// argument, `agent-session.ts:2838`).
#[derive(Debug, Clone, Default)]
pub struct NavigateTreeOptions {
    /// Whether the user wants to summarize the abandoned branch.
    pub summarize: bool,
    /// Custom instructions for the summarizer.
    pub custom_instructions: Option<String>,
    /// When `Some(true)`, `custom_instructions` replaces (rather than appends to)
    /// the default summarization prompt.
    pub replace_instructions: Option<bool>,
    /// Label to attach to the branch summary entry.
    pub label: Option<String>,
}

/// The outcome of a tree navigation (pi's `navigateTree` return value,
/// `agent-session.ts:2839`).
#[derive(Debug, Clone, Default)]
pub struct NavigateTreeResult {
    /// The user/custom message text to drop back into the editor, if the target
    /// was a user or custom message (pi's `editorText`).
    pub editor_text: Option<String>,
    /// Whether the navigation was cancelled (by an extension or an abort).
    pub cancelled: bool,
    /// Whether the cancellation was caused by aborting the summarization.
    pub aborted: bool,
    /// The branch summary entry written at the navigation target, if any.
    pub summary_entry: Option<BranchSummaryEntry>,
}

/// A user message discovered for the fork selector (pi's
/// `getUserMessagesForForking` element, `agent-session.ts:3027`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkableUserMessage {
    /// The entry id of the user message.
    pub entry_id: String,
    /// The extracted text of the user message.
    pub text: String,
}

/// An error raised while navigating the session tree (pi's `navigateTree`
/// `throw`s: no model for summarization, unknown target, or a summarizer error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigateTreeError {
    /// The human-readable message (matches pi's `Error` text).
    pub message: String,
}

impl NavigateTreeError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for NavigateTreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for NavigateTreeError {}

/// The text an entry contributes to the editor when navigated to (pi's
/// `_extractUserMessageText`, `agent-session.ts:3044`): a plain string, or the
/// joined `text` blocks of an array content.
fn extract_message_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// The `role` of a message value (`message.role`).
fn message_role(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

impl AgentSession {
    // =========================================================================
    // Tree Navigation (pi `navigateTree`, agent-session.ts:2836)
    // =========================================================================

    /// Navigate to a different node in the session tree (pi's `navigateTree`,
    /// `agent-session.ts:2836`).
    ///
    /// Unlike fork (a runtime concern) this stays in the same session file. A
    /// no-op when `target_id` is already the leaf. When `options.summarize` is set
    /// the abandoned branch (from the old leaf up to the common ancestor with the
    /// target) is summarized and the summary is attached at the navigation target;
    /// otherwise the leaf is simply moved. The `session_before_tree` extension
    /// event may cancel, supply the summary, or override instructions/label, and
    /// `session_tree` fires after the move.
    pub fn navigate_tree(
        &self,
        target_id: &str,
        options: NavigateTreeOptions,
    ) -> Result<NavigateTreeResult, NavigateTreeError> {
        let old_leaf_id = self.session_manager().get_leaf_id().map(str::to_string);

        // No-op if already at target.
        if old_leaf_id.as_deref() == Some(target_id) {
            return Ok(NavigateTreeResult::default());
        }

        // Model required for summarization.
        if options.summarize && self.model().is_none() {
            return Err(NavigateTreeError::new(
                "No model available for summarization",
            ));
        }

        let target_entry = match self.session_manager().get_entry(target_id) {
            Some(entry) => entry,
            None => {
                return Err(NavigateTreeError::new(format!(
                    "Entry {target_id} not found"
                )))
            }
        };

        // Collect entries to summarize (from old leaf to common ancestor).
        let collected = {
            let manager = self.session_manager();
            collect_entries_for_branch_summary(&*manager, old_leaf_id.as_deref(), target_id)
        };

        // Install the branch-summary abort signal for the duration of the call (pi
        // `this._branchSummaryAbortController = new AbortController()`), and clear
        // it in the `finally` regardless of outcome.
        let signal = AbortSignal::new();
        *self.branch_summary_abort_signal.lock().unwrap() = Some(signal.clone());

        let result = self.navigate_tree_inner(
            target_id,
            &target_entry,
            old_leaf_id.as_deref(),
            collected.common_ancestor_id.as_deref(),
            &collected.entries,
            &options,
            &signal,
        );

        *self.branch_summary_abort_signal.lock().unwrap() = None;
        result
    }

    /// The body of [`navigate_tree`](Self::navigate_tree)'s `try` block (pi
    /// L2883-3018), split out so the surrounding abort-signal clear (`finally`) is
    /// expressed once.
    #[allow(clippy::too_many_arguments)]
    fn navigate_tree_inner(
        &self,
        target_id: &str,
        target_entry: &SessionEntry,
        old_leaf_id: Option<&str>,
        common_ancestor_id: Option<&str>,
        entries_to_summarize: &[SessionEntry],
        options: &NavigateTreeOptions,
        signal: &AbortSignal,
    ) -> Result<NavigateTreeResult, NavigateTreeError> {
        let mut custom_instructions = options.custom_instructions.clone();
        let mut replace_instructions = options.replace_instructions;
        let mut label = options.label.clone();

        let mut extension_summary: Option<SessionBeforeTreeSummary> = None;
        let mut from_extension = false;

        // session_before_tree: an extension may cancel, supply the summary, or
        // override the instructions/label (pi L2887-2914).
        if self.extension_runner().has_handlers("session_before_tree") {
            let preparation = TreePreparation {
                target_id: target_id.to_string(),
                old_leaf_id: old_leaf_id.map(str::to_string),
                common_ancestor_id: common_ancestor_id.map(str::to_string),
                entries_to_summarize: entries_to_values(entries_to_summarize),
                user_wants_summary: options.summarize,
                custom_instructions: custom_instructions.clone(),
                replace_instructions,
                label: label.clone(),
            };
            let outcome = self
                .extension_runner()
                .emit(&ExtensionDispatchEvent::SessionBeforeTree(
                    SessionBeforeTreeEvent { preparation },
                ));
            if let ExtensionEmitOutcome::BeforeTree(result) = outcome {
                if result.cancel == Some(true) {
                    return Ok(NavigateTreeResult {
                        cancelled: true,
                        ..NavigateTreeResult::default()
                    });
                }
                if options.summarize {
                    if let Some(summary) = result.summary {
                        extension_summary = Some(summary);
                        from_extension = true;
                    }
                }
                if result.custom_instructions.is_some() {
                    custom_instructions = result.custom_instructions;
                }
                if result.replace_instructions.is_some() {
                    replace_instructions = result.replace_instructions;
                }
                if result.label.is_some() {
                    label = result.label;
                }
            }
        }

        // Run the default summarizer if needed (pi L2916-2948).
        let mut summary_text: Option<String> = None;
        let mut summary_details: Option<Value> = None;
        if options.summarize && !entries_to_summarize.is_empty() && extension_summary.is_none() {
            // The model presence was checked in `navigate_tree`.
            let model = self
                .model()
                .ok_or_else(|| NavigateTreeError::new("No model available for summarization"))?;
            // The `summarization_models` seam is pi's custom-`streamFn` analog (the
            // path the offline suites take). `None` is the `streamSimple` analog
            // whose runtime summarization is part of the deferred credential-aware
            // streaming surface — there is no provider to summarize with here.
            let Some(models) = self.summarization_models.as_deref() else {
                return Err(NavigateTreeError::new(
                    "No summarization provider configured",
                ));
            };
            let reserve_tokens = self
                .settings_manager
                .get_branch_summary_settings()
                .reserve_tokens;
            let generated = generate_branch_summary(
                entries_to_summarize,
                &GenerateBranchSummaryOptions {
                    models,
                    model: &model,
                    signal: signal.clone(),
                    custom_instructions: custom_instructions.clone(),
                    replace_instructions: replace_instructions.unwrap_or(false),
                    reserve_tokens: Some(reserve_tokens),
                },
            );
            match generated {
                Ok(result) => {
                    summary_text = Some(result.summary);
                    summary_details = Some(json!({
                        "readFiles": result.read_files,
                        "modifiedFiles": result.modified_files,
                    }));
                }
                Err(error) if error.code == BranchSummaryErrorCode::Aborted => {
                    return Ok(NavigateTreeResult {
                        cancelled: true,
                        aborted: true,
                        ..NavigateTreeResult::default()
                    });
                }
                Err(error) => return Err(NavigateTreeError::new(error.message)),
            }
        } else if let Some(extension) = extension_summary {
            summary_text = Some(extension.summary);
            summary_details = extension.details;
        }

        // Determine the new leaf position based on target type (pi L2950-2971). A
        // `None` new leaf means "navigate to root" (pi's `newLeafId === null`).
        let (new_leaf_id, editor_text): (Option<String>, Option<String>) = match target_entry {
            SessionEntry::Message(entry) if message_role(&entry.message) == Some("user") => {
                let content = entry.message.get("content").unwrap_or(&Value::Null);
                (entry.parent_id.clone(), Some(extract_message_text(content)))
            }
            SessionEntry::CustomMessage(entry) => (
                entry.parent_id.clone(),
                Some(extract_message_text(&entry.content)),
            ),
            _ => (Some(target_id.to_string()), None),
        };

        // Switch the leaf (with or without summary). The summary is attached at the
        // navigation target position, not the abandoned branch (pi L2973-3001).
        let mut summary_entry: Option<BranchSummaryEntry> = None;
        if let Some(text) = &summary_text {
            let summary_id = self
                .session_manager()
                .branch_with_summary(
                    new_leaf_id.as_deref(),
                    text,
                    summary_details.clone(),
                    Some(from_extension),
                )
                .map_err(|error| NavigateTreeError::new(error.to_string()))?;
            summary_entry = match self.session_manager().get_entry(&summary_id) {
                Some(SessionEntry::BranchSummary(entry)) => Some(entry),
                _ => None,
            };
            // Attach the label to the summary entry.
            if let Some(label) = &label {
                self.session_manager()
                    .append_label_change(&summary_id, Some(label))
                    .map_err(|error| NavigateTreeError::new(error.to_string()))?;
            }
        } else if new_leaf_id.is_none() {
            // No summary, navigating to root - reset the leaf.
            self.session_manager().reset_leaf();
        } else {
            // No summary, navigating to a non-root entry.
            self.session_manager()
                .branch(new_leaf_id.as_deref().unwrap())
                .map_err(|error| NavigateTreeError::new(error.to_string()))?;
        }

        // Attach the label to the target entry when not summarizing (no summary
        // entry to label) (pi L2998-3001).
        if label.is_some() && summary_text.is_none() {
            self.session_manager()
                .append_label_change(target_id, label.as_deref())
                .map_err(|error| NavigateTreeError::new(error.to_string()))?;
        }

        // Rebuild agent state from the new branch context (pi L3003-3005).
        let context_messages = self.session_manager().build_session_context().messages;
        self.agent.set_messages(context_messages);

        // session_tree: fire after the move (pi L3007-3014).
        let new_leaf = self.session_manager().get_leaf_id().map(str::to_string);
        self.extension_runner()
            .emit(&ExtensionDispatchEvent::SessionTree(SessionTreeEvent {
                new_leaf_id: new_leaf,
                old_leaf_id: old_leaf_id.map(str::to_string),
                summary_entry: summary_entry
                    .as_ref()
                    .map(|entry| serde_json::to_value(entry).unwrap_or(Value::Null)),
                from_extension: summary_text.as_ref().map(|_| from_extension),
            }));

        Ok(NavigateTreeResult {
            editor_text,
            cancelled: false,
            aborted: false,
            summary_entry,
        })
    }

    /// Cancel an in-progress branch summarization (pi's `abortBranchSummary`,
    /// `agent-session.ts:1920`).
    pub fn abort_branch_summary(&self) {
        if let Some(signal) = &*self.branch_summary_abort_signal.lock().unwrap() {
            signal.abort();
        }
    }

    /// Whether compaction or branch summarization is currently running (pi's `get
    /// isCompacting`, `agent-session.ts:931`).
    pub fn is_compacting(&self) -> bool {
        self.auto_compaction_abort_signal.lock().unwrap().is_some()
            || self.compaction_abort_signal.lock().unwrap().is_some()
            || self.branch_summary_abort_signal.lock().unwrap().is_some()
    }

    /// All user messages in the session, for the fork selector (pi's
    /// `getUserMessagesForForking`, `agent-session.ts:3027`).
    ///
    /// Walks every entry (not just the current branch) and returns the ones that
    /// are user messages with non-empty text.
    pub fn get_user_messages_for_forking(&self) -> Vec<ForkableUserMessage> {
        let entries = self.session_manager().get_entries();
        let mut result = Vec::new();
        for entry in &entries {
            let SessionEntry::Message(message_entry) = entry else {
                continue;
            };
            if message_role(&message_entry.message) != Some("user") {
                continue;
            }
            let content = message_entry.message.get("content").unwrap_or(&Value::Null);
            let text = extract_message_text(content);
            if !text.is_empty() {
                result.push(ForkableUserMessage {
                    entry_id: message_entry.id.clone(),
                    text,
                });
            }
        }
        result
    }
}

/// Serialize a session-manager entry list to the opaque `Value` shape the
/// `TreePreparation` extension payload carries (pi passes the entries directly;
/// the ported event type is `Value`-opaque).
fn entries_to_values(entries: &[SessionEntry]) -> Vec<Value> {
    entries
        .iter()
        .map(|entry| serde_json::to_value(entry).unwrap_or(Value::Null))
        .collect()
}

#[cfg(test)]
mod tests;
