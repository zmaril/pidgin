//! Compaction integration, ported from pi's `AgentSession`
//! (`packages/coding-agent/src/core/agent-session.ts`, the "Compaction" section:
//! `_checkCompaction` L1935, `_runAutoCompaction` L2029, the manual `compact`
//! L1769, and `abortCompaction` L1898).
//!
//! When an assistant response crosses the context-size threshold or reports a
//! context overflow, the session summarizes older history into a compaction
//! boundary and rebuilds the agent transcript from the compacted session:
//!
//! * [`AgentSession::check_compaction`] (pi `_checkCompaction`) decides whether to
//!   compact after a response. It skips aborted / stale (pre-compaction) / cross-
//!   model messages, then handles the **overflow** case (a one-shot compact-and-
//!   retry guarded by `_overflowRecoveryAttempted`) and the **threshold** case
//!   (usage or estimate over `contextWindow - reserveTokens`). Wired into the turn
//!   spine's pre-send check ([`AgentSession::prompt_with`]) and post-run branch
//!   ([`AgentSession::handle_post_agent_run`]).
//! * [`AgentSession::run_auto_compaction`] (pi `_runAutoCompaction`) runs the
//!   compaction: emit `compaction_start`, dispatch `session_before_compact` (which
//!   may cancel or supply a replacement compaction), otherwise summarize via the
//!   [`Models`] seam, append the compaction entry, rebuild agent state from the
//!   compacted history, dispatch `session_compact`, and emit `compaction_end`. On
//!   `will_retry` it strips a trailing error and asks the loop to continue; else it
//!   continues only for queued messages.
//! * [`AgentSession::compact`] (pi manual `compact`) is the `/compact` entry point.
//!
//! ## Divergences from pi
//!
//! pi passes `this.agent.streamFn` (and resolved request auth) to `compact`. The
//! ported `core::compaction::compact` takes a [`Models`] seam instead, so the
//! session threads a [`summarization provider`](AgentSessionConfig::summarization_models):
//! a `Some` seam is the analog of pi's *custom* `streamFn` (summarization runs
//! through it and the configured-auth gate is bypassed), while `None` is the
//! `streamSimple` default (the configured-auth gate applies; the runtime-driven
//! summarization it implies is part of the deferred credential-aware `ModelRuntime`
//! streaming surface). This is the same `has_configured_auth` bridge the `prompt`
//! preflight already uses for the deferred `getAuth`/OAuth paths.
//!
//! pi's `CompactionResult` additionally carries `estimatedTokensAfter`; the ported
//! `CompactionResult` deliberately omits it (documented on the type), so the
//! session does not compute or emit it.
//!
//! pi's manual `compact` also `_disconnectFromAgent()` / `_reconnectToAgent()`
//! around the run. Because the ported summarization goes through the separate
//! [`Models`] seam (not `agent.prompt`), no agent events fire during compaction, so
//! there is nothing to disconnect; the session only rebuilds `agent.state.messages`
//! at the end (a plain `set_messages`, which emits no events).
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/agent-session.ts`.

// straitjacket-allow-file:duplication

use serde_json::Value;

use serde_json::json;

use pidgin_agent::harness::session::messages::parse_iso_millis;
use pidgin_agent::types::AgentMessage;
use pidgin_ai::seams::AbortSignal;
use pidgin_ai::utils::overflow::is_context_overflow;

use crate::core::auth::auth_guidance::{
    format_no_api_key_found_message, format_no_model_selected_message,
};
use crate::core::compaction::{
    calculate_context_tokens, compact, estimate_context_tokens, prepare_compaction, should_compact,
    CompactionError, CompactionErrorCode, CompactionPreparation, CompactionResult,
    CompactionSettings,
};
use crate::core::extensions::events::session::{
    CompactionReason, SessionBeforeCompactEvent, SessionCompactEvent,
};
use crate::core::extensions::runner::{ExtensionDispatchEvent, ExtensionEmitOutcome};
use crate::core::session_manager::{get_latest_compaction_entry, CompactionEntry, SessionEntry};
use crate::core::settings_manager::CompactionResolved;

use super::events::AgentSessionEvent;
use super::retry::as_assistant_message;
use super::session::AgentSession;

/// The message pi emits on `compaction_end` when a second context overflow arrives
/// after the one allowed compact-and-retry (pi L1975).
const OVERFLOW_RECOVERY_EXHAUSTED: &str =
    "Context overflow recovery failed after one compact-and-retry attempt. \
     Try reducing context or switching to a larger-context model.";

/// The plan `check_compaction` derives from an assistant response (pi's
/// `_checkCompaction` decision, split out so the turn spine's side effects and the
/// characterization tests observe the same choice `_runAutoCompaction` was spied on
/// for in pi).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompactionPlan {
    /// No compaction (disabled, aborted, stale, cross-model, or under threshold).
    None,
    /// A second overflow after the one allowed recovery: emit the "recovery
    /// failed" `compaction_end` and do not compact (pi L1974-1985).
    OverflowRecoveryExhausted,
    /// Run auto-compaction with `reason` / `will_retry`. `set_overflow_guard` marks
    /// the overflow-retry first attempt, whose side effects (set the one-shot guard
    /// and strip the trailing error from agent state before the retry) run in
    /// `check_compaction`.
    Run {
        reason: CompactionReason,
        will_retry: bool,
        set_overflow_guard: bool,
    },
}

/// Convert resolved settings ([`CompactionResolved`]) into the compaction module's
/// [`CompactionSettings`] (identical fields; pi's `getCompactionSettings` return
/// value flows straight into `prepareCompaction`/`shouldCompact`).
fn compaction_settings(resolved: CompactionResolved) -> CompactionSettings {
    CompactionSettings {
        enabled: resolved.enabled,
        reserve_tokens: resolved.reserve_tokens,
        keep_recent_tokens: resolved.keep_recent_tokens,
    }
}

/// The saved compaction entry matching `summary`, for the `session_compact` event
/// (pi's `newEntries.find((e) => e.type === "compaction" && e.summary === summary)`).
fn find_saved_compaction<'a>(
    entries: &'a [SessionEntry],
    summary: &str,
) -> Option<&'a CompactionEntry> {
    entries.iter().find_map(|entry| match entry {
        SessionEntry::Compaction(compaction) if compaction.summary == summary => Some(compaction),
        _ => None,
    })
}

/// Project a [`CompactionPreparation`] onto the `Value`-shaped `preparation` the
/// extension `session_before_compact` event carries (the field is a `Value` alias
/// in [`crate::core::extensions::events::common`]; [`CompactionPreparation`] is not
/// `Serialize`). Carries the fields an extension reads — pi's example handlers use
/// `preparation.firstKeptEntryId` / `preparation.tokensBefore` — projected to pi's
/// camelCase keys; the internal `file_ops`/`settings` bookkeeping is omitted.
fn preparation_to_value(preparation: &CompactionPreparation) -> Value {
    json!({
        "firstKeptEntryId": preparation.first_kept_entry_id,
        "messagesToSummarize": preparation.messages_to_summarize,
        "turnPrefixMessages": preparation.turn_prefix_messages,
        "isSplitTurn": preparation.is_split_turn,
        "tokensBefore": preparation.tokens_before,
        "previousSummary": preparation.previous_summary,
    })
}

/// Project the branch entries onto the `Value`-shaped `branch_entries` the
/// extension `session_before_compact` event carries (its field is a `Value` alias
/// in [`crate::core::extensions::events::common`]).
fn branch_entries_to_values(branch: &[SessionEntry]) -> Vec<Value> {
    branch
        .iter()
        .map(|entry| serde_json::to_value(entry).unwrap_or(Value::Null))
        .collect()
}

/// The compaction fields the session persists and emits, unifying the two
/// sources: the strongly-typed [`CompactionResult`] the `compact` seam produces,
/// and the opaque `Value` an extension's `session_before_compact` returns (the
/// extension event/result types are `Value` aliases in
/// [`crate::core::extensions::events::common`], so an extension may supply
/// arbitrary `details` pi keeps as `unknown`).
///
/// `details` is kept as a raw `Value` so extension-authored details persist
/// verbatim through `append_compaction`; the typed [`CompactionOutcome::to_result`]
/// used for the `compaction_end` event / manual return is best-effort (details that
/// do not match [`crate::core::compaction::CompactionDetails`] become `None`, which
/// only the untyped `details` field of the event drops — the tested fields
/// `summary`/`first_kept_entry_id`/`tokens_before` are preserved).
struct CompactionOutcome {
    summary: String,
    first_kept_entry_id: String,
    tokens_before: i64,
    details: Option<Value>,
}

impl CompactionOutcome {
    /// From the `compact` seam's typed result (the internally-generated path).
    fn from_result(result: CompactionResult) -> Self {
        Self {
            summary: result.summary,
            first_kept_entry_id: result.first_kept_entry_id,
            tokens_before: result.tokens_before,
            details: result
                .details
                .and_then(|details| serde_json::to_value(details).ok()),
        }
    }

    /// From an extension-provided compaction `Value` (pi reads `.summary`,
    /// `.firstKeptEntryId`, `.tokensBefore`, `.details` off the returned object).
    fn from_extension_value(value: &Value) -> Self {
        Self {
            summary: value
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            first_kept_entry_id: value
                .get("firstKeptEntryId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            tokens_before: value
                .get("tokensBefore")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            details: value.get("details").cloned(),
        }
    }

    /// The typed [`CompactionResult`] for the `compaction_end` event and the manual
    /// `compact` return value.
    fn to_result(&self) -> CompactionResult {
        CompactionResult {
            summary: self.summary.clone(),
            first_kept_entry_id: self.first_kept_entry_id.clone(),
            tokens_before: self.tokens_before,
            details: self
                .details
                .as_ref()
                .and_then(|details| serde_json::from_value(details.clone()).ok()),
        }
    }
}

/// A compaction failure surfaced by the manual [`AgentSession::compact`] entry
/// point (pi's manual `compact` throws `Error`). The `aborted` flag mirrors pi's
/// `message === "Compaction cancelled" || error.name === "AbortError"` distinction,
/// which decides the `compaction_end` `aborted` flag and whether an `errorMessage`
/// is attached.
#[derive(Debug)]
pub struct CompactionTurnError {
    message: String,
    aborted: bool,
}

impl CompactionTurnError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            aborted: false,
        }
    }

    fn cancelled() -> Self {
        Self {
            message: "Compaction cancelled".to_string(),
            aborted: true,
        }
    }

    fn from_compaction_error(error: &CompactionError) -> Self {
        Self {
            message: error.to_string(),
            aborted: matches!(error.code, CompactionErrorCode::Aborted),
        }
    }
}

impl std::fmt::Display for CompactionTurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CompactionTurnError {}

impl AgentSession {
    // =========================================================================
    // Auto-compaction decision + run (pi `_checkCompaction`/`_runAutoCompaction`)
    // =========================================================================

    /// Decide whether the response `message` triggers auto-compaction, and run it
    /// (pi's `_checkCompaction`, L1935). Returns `true` when the caller should
    /// continue the agent loop (overflow retry, or queued messages waiting behind a
    /// compaction).
    ///
    /// `skip_aborted_check` is `false` for the pre-send check (a fresh prompt
    /// follows regardless) and `true` for the post-run check.
    pub(super) fn check_compaction(
        &self,
        message: &AgentMessage,
        skip_aborted_check: bool,
    ) -> bool {
        match self.compaction_plan(message, skip_aborted_check) {
            CompactionPlan::None => false,
            CompactionPlan::OverflowRecoveryExhausted => {
                self.emit_compaction_end(
                    CompactionReason::Overflow,
                    None,
                    false,
                    false,
                    Some(OVERFLOW_RECOVERY_EXHAUSTED.to_string()),
                );
                false
            }
            CompactionPlan::Run {
                reason,
                will_retry,
                set_overflow_guard,
            } => {
                if set_overflow_guard {
                    // One-shot: mark recovery attempted and drop the trailing error
                    // from agent state (kept in the session for history) so it is not
                    // in context for the retry (pi L1986-1993).
                    *self.overflow_recovery_attempted.lock().unwrap() = true;
                    self.strip_trailing_assistant();
                }
                self.run_auto_compaction(reason, will_retry)
            }
        }
    }

    /// The pure decision behind [`AgentSession::check_compaction`] (pi's
    /// `_checkCompaction`, minus the side effects and the `_runAutoCompaction`
    /// call). `pub(super)` so the characterization tests can assert the choice the
    /// pi tests spied `_runAutoCompaction` for.
    pub(super) fn compaction_plan(
        &self,
        message: &AgentMessage,
        skip_aborted_check: bool,
    ) -> CompactionPlan {
        let resolved = self.settings_manager.get_compaction_settings();
        if !resolved.enabled {
            return CompactionPlan::None;
        }
        let settings = compaction_settings(resolved);

        let Some(assistant) = as_assistant_message(message) else {
            return CompactionPlan::None;
        };

        // Skip user-aborted responses unless the caller opted out (pi L1940).
        if skip_aborted_check && assistant.stop_reason == pidgin_ai::StopReason::Aborted {
            return CompactionPlan::None;
        }

        let model = self.model();
        let context_window = model
            .as_ref()
            .map(|model| model.context_window)
            .unwrap_or(0);

        // Overflow is model-specific: a stale error from a smaller-context model
        // must not compact after switching to a larger one (pi L1948).
        let same_model = model.as_ref().is_some_and(|model| {
            assistant.provider == model.provider && assistant.model == model.id
        });

        // Skip a response older than the latest compaction boundary: its stale usage
        // / error would re-trigger compaction on the first post-compaction prompt
        // (pi L1954).
        let branch = self.session_manager().get_branch(None);
        let compaction_entry_ts =
            get_latest_compaction_entry(&branch).map(|entry| parse_iso_millis(&entry.timestamp));
        if let Some(boundary_ts) = compaction_entry_ts {
            if assistant.timestamp <= boundary_ts {
                return CompactionPlan::None;
            }
        }

        // Case 1: overflow (pi L1964).
        if same_model && is_context_overflow(&assistant, Some(context_window)) {
            let will_retry = assistant.stop_reason != pidgin_ai::StopReason::Stop;
            if !will_retry {
                // A completed answer that overflowed: compact but do not retry —
                // `agent.continue()` cannot continue from an assistant message.
                return CompactionPlan::Run {
                    reason: CompactionReason::Overflow,
                    will_retry: false,
                    set_overflow_guard: false,
                };
            }
            if *self.overflow_recovery_attempted.lock().unwrap() {
                return CompactionPlan::OverflowRecoveryExhausted;
            }
            return CompactionPlan::Run {
                reason: CompactionReason::Overflow,
                will_retry: true,
                set_overflow_guard: true,
            };
        }

        // Case 2: threshold (pi L1997). For error / all-zero-usage responses,
        // estimate from the last valid response instead of the current one.
        let direct_context_tokens = calculate_context_tokens(&assistant.usage);
        let context_tokens = if assistant.stop_reason == pidgin_ai::StopReason::Error
            || direct_context_tokens == 0
        {
            let messages = self.agent.messages();
            let estimate = estimate_context_tokens(&messages);
            let Some(last_usage_index) = estimate.last_usage_index else {
                return CompactionPlan::None;
            };
            // The usage source must be post-compaction; a kept pre-compaction message
            // has stale (larger) usage and would falsely trigger compaction (pi L2005).
            if let Some(boundary_ts) = compaction_entry_ts {
                let usage_msg = &messages[last_usage_index];
                let is_assistant =
                    usage_msg.get("role").and_then(Value::as_str) == Some("assistant");
                let usage_ts = usage_msg
                    .get("timestamp")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                if is_assistant && usage_ts <= boundary_ts {
                    return CompactionPlan::None;
                }
            }
            estimate.tokens
        } else {
            direct_context_tokens
        };

        if should_compact(context_tokens, context_window as i64, &settings) {
            return CompactionPlan::Run {
                reason: CompactionReason::Threshold,
                will_retry: false,
                set_overflow_guard: false,
            };
        }
        CompactionPlan::None
    }

    /// Run auto-compaction with events (pi's `_runAutoCompaction`, L2029). Returns
    /// `true` when the caller should continue the agent loop.
    pub(super) fn run_auto_compaction(&self, reason: CompactionReason, will_retry: bool) -> bool {
        let settings = compaction_settings(self.settings_manager.get_compaction_settings());
        let mut started = false;

        let outcome =
            (|| -> Result<bool, CompactionError> {
                let Some(model) = self.model() else {
                    return Ok(false);
                };

                // Auth gate: `Some` summarization seam is pi's custom-`streamFn` branch
                // (auth bypassed); `None` is `streamSimple` (configured auth required).
                if self.summarization_models.is_none()
                    && !self.model_runtime().has_configured_auth(&model.provider)
                {
                    return Ok(false);
                }

                let branch = self.session_manager().get_branch(None);
                let Some(preparation) = prepare_compaction(&branch, &settings)? else {
                    return Ok(false);
                };

                self.emit(&AgentSessionEvent::CompactionStart { reason });
                let signal = AbortSignal::new();
                *self.auto_compaction_abort_signal.lock().unwrap() = Some(signal.clone());
                started = true;

                // session_before_compact: an extension may cancel or supply a compaction.
                let mut extension_compaction: Option<CompactionOutcome> = None;
                let mut from_extension = false;
                if self
                    .extension_runner()
                    .has_handlers("session_before_compact")
                {
                    let emit_outcome = self.extension_runner().emit(
                        &ExtensionDispatchEvent::SessionBeforeCompact(SessionBeforeCompactEvent {
                            preparation: preparation_to_value(&preparation),
                            branch_entries: branch_entries_to_values(&branch),
                            custom_instructions: None,
                            reason,
                            will_retry,
                        }),
                    );
                    if let ExtensionEmitOutcome::BeforeCompact(result) = emit_outcome {
                        if result.cancel == Some(true) {
                            self.emit_compaction_end(reason, None, true, false, None);
                            return Ok(false);
                        }
                        if let Some(compaction) = result.compaction {
                            extension_compaction =
                                Some(CompactionOutcome::from_extension_value(&compaction));
                            from_extension = true;
                        }
                    }
                }

                let outcome_data = match extension_compaction {
                    Some(outcome) => outcome,
                    None => {
                        let Some(models) = self.summarization_models.as_deref() else {
                            // `streamSimple` runtime summarization is deferred; there is
                            // no provider to summarize with.
                            return Err(CompactionError::new(
                                CompactionErrorCode::InvalidSession,
                                "No summarization provider configured",
                            ));
                        };
                        CompactionOutcome::from_result(compact(
                            &preparation,
                            models,
                            &model,
                            None,
                            Some(&signal),
                            self.thinking_level_str().as_deref(),
                        )?)
                    }
                };

                if signal.is_aborted() {
                    self.emit_compaction_end(reason, None, true, false, None);
                    return Ok(false);
                }

                self.apply_compaction(&outcome_data, from_extension, reason, will_retry);
                self.emit_compaction_end(
                    reason,
                    Some(outcome_data.to_result()),
                    false,
                    will_retry,
                    None,
                );

                if will_retry {
                    self.strip_trailing_assistant_error();
                    return Ok(true);
                }

                // A queued follow-up / steer / custom message waiting behind the
                // compaction needs one continuation to be delivered (pi L2107).
                Ok(self.agent.has_queued_messages())
            })();

        *self.auto_compaction_abort_signal.lock().unwrap() = None;

        match outcome {
            Ok(value) => value,
            Err(error) => {
                if started {
                    let message = match reason {
                        CompactionReason::Overflow => {
                            format!("Context overflow recovery failed: {error}")
                        }
                        _ => format!("Auto-compaction failed: {error}"),
                    };
                    self.emit_compaction_end(reason, None, false, false, Some(message));
                }
                false
            }
        }
    }

    // =========================================================================
    // Manual compaction (pi `compact`/`abortCompaction`)
    // =========================================================================

    /// Manually compact the session context (pi's `compact`, L1769). Aborts any
    /// current run, emits `compaction_start`, summarizes (or takes an extension
    /// override), rebuilds agent state, and returns the [`CompactionResult`].
    pub fn compact(
        &self,
        custom_instructions: Option<&str>,
    ) -> Result<CompactionResult, CompactionTurnError> {
        self.abort();
        let signal = AbortSignal::new();
        *self.compaction_abort_signal.lock().unwrap() = Some(signal.clone());
        self.emit(&AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Manual,
        });

        let result = self.compact_inner(custom_instructions, &signal);

        *self.compaction_abort_signal.lock().unwrap() = None;

        match result {
            Ok(compaction_result) => {
                self.emit_compaction_end(
                    CompactionReason::Manual,
                    Some(compaction_result.clone()),
                    false,
                    false,
                    None,
                );
                Ok(compaction_result)
            }
            Err(error) => {
                let error_message = if error.aborted {
                    None
                } else {
                    Some(format!("Compaction failed: {error}"))
                };
                self.emit_compaction_end(
                    CompactionReason::Manual,
                    None,
                    error.aborted,
                    false,
                    error_message,
                );
                Err(error)
            }
        }
    }

    /// The body of the manual [`AgentSession::compact`] `try` block (pi L1776-1889),
    /// split out so the surrounding `finally`/`catch` (clearing the abort signal and
    /// emitting `compaction_end`) is expressed once.
    fn compact_inner(
        &self,
        custom_instructions: Option<&str>,
        signal: &AbortSignal,
    ) -> Result<CompactionResult, CompactionTurnError> {
        let Some(model) = self.model() else {
            return Err(CompactionTurnError::new(format_no_model_selected_message()));
        };

        if self.summarization_models.is_none()
            && !self.model_runtime().has_configured_auth(&model.provider)
        {
            return Err(CompactionTurnError::new(format_no_api_key_found_message(
                &model.provider,
            )));
        }

        let settings = compaction_settings(self.settings_manager.get_compaction_settings());
        let branch = self.session_manager().get_branch(None);

        let Some(preparation) = prepare_compaction(&branch, &settings)
            .map_err(|error| CompactionTurnError::from_compaction_error(&error))?
        else {
            if matches!(branch.last(), Some(SessionEntry::Compaction(_))) {
                return Err(CompactionTurnError::new("Already compacted"));
            }
            return Err(CompactionTurnError::new(
                "Nothing to compact (session too small)",
            ));
        };

        let mut extension_compaction: Option<CompactionOutcome> = None;
        let mut from_extension = false;
        if self
            .extension_runner()
            .has_handlers("session_before_compact")
        {
            let emit_outcome =
                self.extension_runner()
                    .emit(&ExtensionDispatchEvent::SessionBeforeCompact(
                        SessionBeforeCompactEvent {
                            preparation: preparation_to_value(&preparation),
                            branch_entries: branch_entries_to_values(&branch),
                            custom_instructions: custom_instructions.map(str::to_string),
                            reason: CompactionReason::Manual,
                            will_retry: false,
                        },
                    ));
            if let ExtensionEmitOutcome::BeforeCompact(result) = emit_outcome {
                if result.cancel == Some(true) {
                    return Err(CompactionTurnError::cancelled());
                }
                if let Some(compaction) = result.compaction {
                    extension_compaction =
                        Some(CompactionOutcome::from_extension_value(&compaction));
                    from_extension = true;
                }
            }
        }

        let outcome_data = match extension_compaction {
            Some(outcome) => outcome,
            None => {
                let Some(models) = self.summarization_models.as_deref() else {
                    return Err(CompactionTurnError::new(
                        "No summarization provider configured",
                    ));
                };
                CompactionOutcome::from_result(
                    compact(
                        &preparation,
                        models,
                        &model,
                        custom_instructions,
                        Some(signal),
                        self.thinking_level_str().as_deref(),
                    )
                    .map_err(|error| CompactionTurnError::from_compaction_error(&error))?,
                )
            }
        };

        if signal.is_aborted() {
            return Err(CompactionTurnError::cancelled());
        }

        self.apply_compaction(
            &outcome_data,
            from_extension,
            CompactionReason::Manual,
            false,
        );
        Ok(outcome_data.to_result())
    }

    /// Cancel an in-progress manual or auto compaction (pi's `abortCompaction`,
    /// L1898).
    pub fn abort_compaction(&self) {
        if let Some(signal) = &*self.compaction_abort_signal.lock().unwrap() {
            signal.abort();
        }
        if let Some(signal) = &*self.auto_compaction_abort_signal.lock().unwrap() {
            signal.abort();
        }
    }

    /// Whether auto-compaction is enabled (pi's `get autoCompactionEnabled`, L2148).
    pub fn auto_compaction_enabled(&self) -> bool {
        self.settings_manager.get_compaction_enabled()
    }

    /// Toggle the auto-compaction setting (pi's `setAutoCompactionEnabled`, L2143).
    pub fn set_auto_compaction_enabled(&mut self, enabled: bool) {
        self.settings_manager.set_compaction_enabled(enabled);
    }

    // =========================================================================
    // Shared helpers
    // =========================================================================

    /// Persist the compaction boundary, rebuild agent state from the compacted
    /// history, and dispatch `session_compact` (pi L2083-2103, shared by the manual
    /// and auto paths).
    fn apply_compaction(
        &self,
        outcome: &CompactionOutcome,
        from_extension: bool,
        reason: CompactionReason,
        will_retry: bool,
    ) {
        let new_entries;
        let context_messages;
        {
            let mut manager = self.session_manager();
            manager.append_compaction(
                &outcome.summary,
                &outcome.first_kept_entry_id,
                outcome.tokens_before,
                outcome.details.clone(),
                Some(from_extension),
            );
            new_entries = manager.get_entries();
            context_messages = manager.build_session_context().messages;
        }
        self.agent.set_messages(context_messages);

        if let Some(saved) = find_saved_compaction(&new_entries, &outcome.summary) {
            self.extension_runner()
                .emit(&ExtensionDispatchEvent::SessionCompact(
                    SessionCompactEvent {
                        compaction_entry: serde_json::to_value(saved).unwrap_or(Value::Null),
                        from_extension,
                        reason,
                        will_retry,
                    },
                ));
        }
    }

    /// Drop a trailing `assistant` message from agent state, keeping it in the
    /// session for history (pi's overflow-retry error strip, L1989-1992).
    fn strip_trailing_assistant(&self) {
        let mut messages = self.agent.messages();
        if messages
            .last()
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            == Some("assistant")
        {
            messages.pop();
            self.agent.set_messages(messages);
        }
    }

    /// Drop a trailing `assistant` **error** message from the rebuilt agent state
    /// before an overflow retry (pi L2109-2115).
    fn strip_trailing_assistant_error(&self) {
        let mut messages = self.agent.messages();
        let is_trailing_error = messages.last().is_some_and(|message| {
            message.get("role").and_then(Value::as_str) == Some("assistant")
                && message.get("stopReason").and_then(Value::as_str) == Some("error")
        });
        if is_trailing_error {
            messages.pop();
            self.agent.set_messages(messages);
        }
    }

    /// Emit a `compaction_end` event (pi's `_emit({ type: "compaction_end", ... })`).
    fn emit_compaction_end(
        &self,
        reason: CompactionReason,
        result: Option<CompactionResult>,
        aborted: bool,
        will_retry: bool,
        error_message: Option<String>,
    ) {
        self.emit(&AgentSessionEvent::CompactionEnd {
            reason,
            result,
            aborted,
            will_retry,
            error_message,
        });
    }

    /// The agent's thinking level as a lowercase wire string for the summarization
    /// request options (pi passes `this.thinkingLevel` to `compact`). Inert for
    /// non-reasoning models, which the summary/turn-prefix option builders gate on.
    fn thinking_level_str(&self) -> Option<String> {
        serde_json::to_value(self.agent.thinking_level())
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
    }
}

#[cfg(test)]
mod tests;
