//! Auto-retry with exponential backoff, ported from pi's `AgentSession`
//! (`packages/coding-agent/src/core/agent-session.ts`, the "Auto-Retry" section
//! ~L2610-2700 plus the retry branches of `_handlePostAgentRun` L1070 and
//! `_handleAgentEvent` L631).
//!
//! When the agent finishes a run whose last assistant message is a *retryable*
//! transient error (overloaded, rate limit, 5xx, transport failures — but **not**
//! context overflow, which compaction handles), the session retries the request
//! with an exponentially growing, abortable backoff:
//!
//! * [`AgentSession::is_retryable_error`] (pi `_isRetryableError`, L2614) excludes
//!   context-overflow errors, then defers to `is_retryable_assistant_error`.
//! * [`AgentSession::prepare_retry`] (pi `_prepareRetry`, L2624) advances the
//!   attempt counter, declines once the budget is spent, emits `auto_retry_start`,
//!   strips the trailing error message from *agent state* (it stays in the session
//!   history), then sleeps `base_delay_ms * 2^(attempt-1)` ms. The sleep is
//!   abortable via [`AgentSession::abort_retry`]; on abort it emits
//!   `auto_retry_end{success:false, final_error:"Retry cancelled"}` and declines.
//! * The success reset (pi `_handleAgentEvent` L631) and the `will_retry`
//!   computation for `agent_end` (pi `_willRetryAfterAgentEnd`, L647) live with the
//!   agent-event handler in [`super::turn`]; this module supplies the shared
//!   [`message_is_retryable`] / [`agent_context_window`] helpers it calls.
//!
//! ## Sync/eager + `!Send` model note
//!
//! pi's backoff is an `await sleep(delayMs, signal)`; the eager Rust agent runs a
//! turn to completion on the drive thread, so the backoff is a **blocking**
//! thread sleep that polls the abort signal so it still bails promptly. Because
//! the drive thread is blocked inside [`AgentSession::prompt`] while it sleeps,
//! the abort must be tripped from another thread holding a clone of the shared
//! [`AbortSignal`] handle (the session-actor ownership contract — see the module
//! docs). The pi test that cancels a retry mid-sleep therefore needs genuine
//! concurrency the single-threaded test harness cannot provide and is `#[ignore]`d
//! with that reason.
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/agent-session.ts`.

// straitjacket-allow-file:duplication

use std::time::{Duration, Instant};

use serde_json::Value;

use atilla_agent::agent::Agent;
use atilla_agent::types::AgentMessage;
use atilla_ai::seams::AbortSignal;
use atilla_ai::utils::overflow::is_context_overflow;
use atilla_ai::{is_retryable_assistant_error, AssistantMessage};

use super::events::AgentSessionEvent;
use super::session::AgentSession;
use super::turn::UNKNOWN_MODEL_SENTINEL;

/// The wall-clock granularity at which the blocking backoff re-checks the abort
/// signal. Small enough that an abort bails promptly, large enough not to spin.
const SLEEP_POLL_STEP: Duration = Duration::from_millis(5);

/// Deserialize an [`AgentMessage`] value into a typed [`AssistantMessage`], or
/// `None` when it is not a well-formed assistant message. The retry predicates
/// operate on the typed shape (pi passes an `AssistantMessage` straight through).
pub(super) fn as_assistant_message(message: &AgentMessage) -> Option<AssistantMessage> {
    serde_json::from_value(message.clone()).ok()
}

/// Whether `message` is a retryable assistant error given `context_window` (pi's
/// `_isRetryableError`, L2614): context-overflow errors are handled by compaction,
/// not retry, so they are excluded first.
pub(super) fn message_is_retryable(message: &AgentMessage, context_window: u64) -> bool {
    let Some(assistant) = as_assistant_message(message) else {
        return false;
    };
    if is_context_overflow(&assistant, Some(context_window)) {
        return false;
    }
    is_retryable_assistant_error(&assistant)
}

/// The context window of `agent`'s current model (pi's `this.model?.contextWindow
/// ?? 0`). The `"unknown"` placeholder model reads as "no model" → `0`.
pub(super) fn agent_context_window(agent: &Agent) -> u64 {
    let model = agent.model();
    if model.provider == UNKNOWN_MODEL_SENTINEL && model.id == UNKNOWN_MODEL_SENTINEL {
        0
    } else {
        model.context_window
    }
}

/// Sleep for `delay_ms`, returning early with `true` if `signal` is tripped (pi's
/// `await sleep(delayMs, signal)` where a rejected sleep signals abort). Blocks the
/// calling thread but re-checks the abort flag every [`SLEEP_POLL_STEP`].
fn abortable_sleep(delay_ms: u64, signal: &AbortSignal) -> bool {
    let deadline = Instant::now() + Duration::from_millis(delay_ms);
    loop {
        if signal.is_aborted() {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep((deadline - now).min(SLEEP_POLL_STEP));
    }
}

impl AgentSession {
    /// The context window of the currently selected model, or `0` when none is
    /// selected (pi's `this.model?.contextWindow ?? 0`).
    fn model_context_window(&self) -> u64 {
        self.model().map(|model| model.context_window).unwrap_or(0)
    }

    /// Whether `message` is a retryable transient error (pi's `_isRetryableError`,
    /// L2614). Context overflow is excluded (compaction handles it).
    pub(super) fn is_retryable_error(&self, message: &AgentMessage) -> bool {
        message_is_retryable(message, self.model_context_window())
    }

    /// Prepare a retryable error for continuation with exponential backoff (pi's
    /// `_prepareRetry`, L2624). Returns `true` when the caller should continue the
    /// agent for another attempt, `false` to give up (retry disabled, budget spent,
    /// or the backoff aborted).
    pub(super) fn prepare_retry(&self, message: &AgentMessage) -> bool {
        let settings = self.settings_manager.get_retry_settings();
        if !settings.enabled {
            return false;
        }

        let attempt = {
            let mut attempt = self.retry_attempt.lock().unwrap();
            *attempt += 1;
            if i64::from(*attempt) > settings.max_retries {
                // Preserve the completed attempt count so post-run handling can emit
                // the final failure (pi L2632-2635).
                *attempt -= 1;
                return false;
            }
            *attempt
        };

        let delay_ms = (settings.base_delay_ms.max(0) as u64) * 2u64.pow(attempt - 1);

        let error_message = as_assistant_message(message)
            .and_then(|assistant| assistant.error_message)
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| "Unknown error".to_string());
        self.emit(&AgentSessionEvent::AutoRetryStart {
            attempt,
            max_attempts: settings.max_retries.max(0) as u32,
            delay_ms,
            error_message,
        });

        // Remove the error message from agent state; it stays in the session for
        // history (pi L2645-2649).
        {
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

        // Wait with exponential backoff (abortable) (pi L2652-2670).
        let signal = AbortSignal::new();
        *self.retry_abort_signal.lock().unwrap() = Some(signal.clone());
        let aborted = abortable_sleep(delay_ms, &signal);
        *self.retry_abort_signal.lock().unwrap() = None;

        if aborted {
            let completed = {
                let mut attempt = self.retry_attempt.lock().unwrap();
                let completed = *attempt;
                *attempt = 0;
                completed
            };
            self.emit(&AgentSessionEvent::AutoRetryEnd {
                success: false,
                attempt: completed,
                final_error: Some("Retry cancelled".to_string()),
            });
            return false;
        }

        true
    }

    /// Cancel an in-progress retry backoff (pi's `abortRetry`, L2679).
    pub fn abort_retry(&self) {
        if let Some(signal) = &*self.retry_abort_signal.lock().unwrap() {
            signal.abort();
        }
    }

    /// Whether an auto-retry backoff is currently in progress (pi's `get
    /// isRetrying`, L2684).
    pub fn is_retrying(&self) -> bool {
        self.retry_abort_signal.lock().unwrap().is_some()
    }

    /// Whether auto-retry is enabled (pi's `get autoRetryEnabled`, L2689).
    pub fn auto_retry_enabled(&self) -> bool {
        self.settings_manager.get_retry_enabled()
    }

    /// Toggle the auto-retry setting (pi's `setAutoRetryEnabled`, L2696). Keeps the
    /// handler's shared settings snapshot in sync so its `will_retry` computation
    /// reads the new value.
    pub fn set_auto_retry_enabled(&mut self, enabled: bool) {
        self.settings_manager.set_retry_enabled(enabled);
        *self.retry_settings.lock().unwrap() = self.settings_manager.get_retry_settings();
    }
}

#[cfg(test)]
mod tests;
