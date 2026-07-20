//! Session statistics and context-window usage, ported from pi's `AgentSession`
//! (`packages/coding-agent/src/core/agent-session.ts:3055-3159`).
//!
//! This slice ports [`AgentSession::get_session_stats`] (pi's `getSessionStats`)
//! and [`AgentSession::get_context_usage`] (pi's `getContextUsage`), plus the
//! [`SessionStats`] / [`SessionTokenTotals`] / [`ContextUsage`] result types.
//!
//! `getSessionStats` aggregates over **all** session entries (including history
//! that was compacted away), so token/cost totals reflect what was actually billed
//! across the session. `getContextUsage` computes the current context-window
//! occupancy from the live agent messages, returning `tokens == None` right after a
//! compaction (before the next LLM response) because the last assistant usage then
//! reflects the pre-compaction context size.
//!
//! The context-usage computation is shared as [`compute_context_usage`] so the
//! extension host bridge's `getContextUsage` callback ([`super::host`]) can answer
//! with the same value pi's `getContextUsage: () => this.getContextUsage()` seam
//! (`agent-session.ts:2402`) returns.

use serde::Serialize;
use serde_json::Value;

use pidgin_ai::{Model, Usage};

use crate::core::compaction::{calculate_context_tokens, estimate_context_tokens};
use crate::core::session_manager::{AgentMessage, SessionEntry};

use super::session::AgentSession;
use super::turn::UNKNOWN_MODEL_SENTINEL;

/// Token totals across a session (pi's `SessionStats.tokens`,
/// `agent-session.ts:249`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTokenTotals {
    /// Total input tokens billed.
    pub input: u64,
    /// Total output tokens billed.
    pub output: u64,
    /// Total cache-read tokens billed.
    pub cache_read: u64,
    /// Total cache-write tokens billed.
    pub cache_write: u64,
    /// `input + output + cacheRead + cacheWrite`.
    pub total: u64,
}

/// Aggregate session statistics (pi's `SessionStats`, `agent-session.ts:241`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    /// The current session file, or `None` for an in-memory session.
    pub session_file: Option<String>,
    /// The session id.
    pub session_id: String,
    /// Number of user-role message entries.
    pub user_messages: u64,
    /// Number of assistant-role message entries.
    pub assistant_messages: u64,
    /// Number of tool calls across all assistant messages.
    pub tool_calls: u64,
    /// Number of tool-result message entries.
    pub tool_results: u64,
    /// Total number of message entries.
    pub total_messages: u64,
    /// Token totals across all entries.
    pub tokens: SessionTokenTotals,
    /// Total cost across all entries, in dollars.
    pub cost: f64,
    /// Current context-window usage, or `None` when no model is selected / the
    /// model has no context window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<ContextUsage>,
}

/// Current context-window occupancy (pi's `ContextUsage`,
/// `core/extensions/types.ts:285`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    /// Estimated context tokens, or `None` if unknown (e.g. right after
    /// compaction, before the next LLM response).
    pub tokens: Option<i64>,
    /// The model's context window in tokens.
    pub context_window: i64,
    /// Context usage as a percentage of the context window, or `None` when
    /// `tokens` is unknown.
    pub percent: Option<f64>,
}

impl AgentSession {
    /// Aggregate session statistics (pi's `getSessionStats`,
    /// `agent-session.ts:3060`).
    ///
    /// Aggregates over **all** session entries (including history that was
    /// compacted away), so token/cost totals reflect what was actually billed
    /// across the session. Tool calls are counted from each assistant message's
    /// `toolCall` content blocks.
    pub fn get_session_stats(&self) -> SessionStats {
        let mut user_messages = 0u64;
        let mut assistant_messages = 0u64;
        let mut tool_results = 0u64;
        let mut total_messages = 0u64;
        let mut tool_calls = 0u64;
        let mut total_input = 0u64;
        let mut total_output = 0u64;
        let mut total_cache_read = 0u64;
        let mut total_cache_write = 0u64;
        let mut total_cost = 0.0f64;

        for entry in self.session_manager().get_entries() {
            let SessionEntry::Message(message_entry) = entry else {
                continue;
            };
            total_messages += 1;
            let message = &message_entry.message;
            match message.get("role").and_then(Value::as_str) {
                Some("user") => user_messages += 1,
                Some("toolResult") => tool_results += 1,
                Some("assistant") => {
                    assistant_messages += 1;
                    if let Some(content) = message.get("content").and_then(Value::as_array) {
                        tool_calls += content
                            .iter()
                            .filter(|block| {
                                block.get("type").and_then(Value::as_str) == Some("toolCall")
                            })
                            .count() as u64;
                    }
                    if let Some(usage) = message.get("usage") {
                        total_input += usage.get("input").and_then(Value::as_u64).unwrap_or(0);
                        total_output += usage.get("output").and_then(Value::as_u64).unwrap_or(0);
                        total_cache_read +=
                            usage.get("cacheRead").and_then(Value::as_u64).unwrap_or(0);
                        total_cache_write +=
                            usage.get("cacheWrite").and_then(Value::as_u64).unwrap_or(0);
                        total_cost += usage
                            .get("cost")
                            .and_then(|cost| cost.get("total"))
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0);
                    }
                }
                _ => {}
            }
        }

        SessionStats {
            session_file: self.session_file(),
            session_id: self.session_id(),
            user_messages,
            assistant_messages,
            tool_calls,
            tool_results,
            total_messages,
            tokens: SessionTokenTotals {
                input: total_input,
                output: total_output,
                cache_read: total_cache_read,
                cache_write: total_cache_write,
                total: total_input + total_output + total_cache_read + total_cache_write,
            },
            cost: total_cost,
            context_usage: self.get_context_usage(),
        }
    }

    /// Current context-window usage (pi's `getContextUsage`,
    /// `agent-session.ts:3115`).
    ///
    /// Returns `None` when no model is selected or the model has no context
    /// window. After a compaction, returns `ContextUsage { tokens: None, .. }`
    /// until an assistant responds again, because the last assistant usage then
    /// reflects the pre-compaction context size.
    pub fn get_context_usage(&self) -> Option<ContextUsage> {
        let model = self.model();
        let branch = self.session_manager().get_branch(None);
        let messages = self.messages();
        compute_context_usage(model.as_ref(), &messages, &branch)
    }
}

/// Compute the context-window usage from a model, the live agent messages, and the
/// current session branch (pi's `getContextUsage` body, `agent-session.ts:3115`).
///
/// Shared by [`AgentSession::get_context_usage`] and the extension host bridge in
/// [`super::host`], which both feed the same three inputs.
pub(super) fn compute_context_usage(
    model: Option<&Model>,
    messages: &[AgentMessage],
    branch: &[SessionEntry],
) -> Option<ContextUsage> {
    let model = model?;
    let context_window = model.context_window;
    if context_window == 0 {
        return None;
    }

    // After compaction, the last assistant usage reflects pre-compaction context
    // size. Trust usage only from an assistant that responded after the latest
    // compaction; if none exists, the context token count is unknown until the
    // next LLM response.
    if let Some(compaction_index) = branch
        .iter()
        .rposition(|entry| matches!(entry, SessionEntry::Compaction(_)))
    {
        let mut has_post_compaction_usage = false;
        for entry in branch[compaction_index + 1..].iter().rev() {
            if let SessionEntry::Message(message_entry) = entry {
                if assistant_context_tokens(&message_entry.message) > 0 {
                    has_post_compaction_usage = true;
                    break;
                }
            }
        }

        if !has_post_compaction_usage {
            return Some(ContextUsage {
                tokens: None,
                context_window: context_window as i64,
                percent: None,
            });
        }
    }

    let estimate = estimate_context_tokens(messages);
    let percent = (estimate.tokens as f64 / context_window as f64) * 100.0;

    Some(ContextUsage {
        tokens: Some(estimate.tokens),
        context_window: context_window as i64,
        percent: Some(percent),
    })
}

/// The context-token count of a message when it is a non-aborted, non-error
/// assistant message with a positive usage, else `0` (pi's inline
/// `assistant.stopReason` + `calculateContextTokens(assistant.usage)` guard in
/// `getContextUsage`, `agent-session.ts:3133`).
fn assistant_context_tokens(message: &AgentMessage) -> i64 {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return 0;
    }
    if matches!(
        message.get("stopReason").and_then(Value::as_str),
        Some("aborted") | Some("error")
    ) {
        return 0;
    }
    let Some(usage) = message.get("usage") else {
        return 0;
    };
    let Ok(usage) = serde_json::from_value::<Usage>(usage.clone()) else {
        return 0;
    };
    calculate_context_tokens(&usage)
}

/// Map an agent model to `None` when it is the `"unknown"` placeholder pi treats
/// as "no model selected" (mirrors [`AgentSession::model`]). Used by the host
/// bridge, which holds the raw agent handle.
pub(super) fn model_or_none(model: Model) -> Option<Model> {
    if model.provider == UNKNOWN_MODEL_SENTINEL && model.id == UNKNOWN_MODEL_SENTINEL {
        None
    } else {
        Some(model)
    }
}

#[cfg(test)]
mod tests;
