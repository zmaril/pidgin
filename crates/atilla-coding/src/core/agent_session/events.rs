//! The TUI-facing session event union, ported from pi's `AgentSessionEvent`
//! (`packages/coding-agent/src/core/agent-session.ts:136-165`).

// straitjacket-allow-file:duplication

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use atilla_agent::types::{AgentEvent, AgentMessage, ThinkingLevel};
use atilla_ai::{AssistantMessageEvent, ToolResultMessage};

use crate::core::compaction::CompactionResult;
use crate::core::extensions::events::session::CompactionReason;
use crate::core::session_manager::SessionEntry;

/// The TUI-facing session event union (pi's `AgentSessionEvent`,
/// `agent-session.ts:136`).
///
/// = `Exclude<AgentEvent, {type:"agent_end"}>` (the nine core variants, copied
/// verbatim so the wire tag/field bytes match [`atilla_agent::AgentEvent`]) plus
/// the ten session-specific variants, including the session's own `agent_end`
/// that overrides the core one by adding `willRetry`.
///
/// Internally tagged exactly like [`atilla_agent::AgentEvent`]: snake_case `type`
/// discriminants, camelCase fields. [`Clone`] is required by the TUI consumer,
/// which clones each event onto its render channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentSessionEvent {
    // ---- core AgentEvent variants (agent-core), copied minus `agent_end` -----
    /// A run started.
    AgentStart,
    /// A turn started.
    TurnStart,
    /// A turn ended.
    TurnEnd {
        message: AgentMessage,
        tool_results: Vec<ToolResultMessage>,
    },
    /// A message started.
    MessageStart { message: AgentMessage },
    /// An assistant message received a streamed update.
    MessageUpdate {
        message: AgentMessage,
        assistant_message_event: Box<AssistantMessageEvent>,
    },
    /// A message finished.
    MessageEnd { message: AgentMessage },
    /// A tool execution started.
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: Value,
    },
    /// A tool execution produced a partial update.
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        partial_result: Value,
    },
    /// A tool execution ended.
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: Value,
        is_error: bool,
    },

    // ---- session-specific variants (agent-session.ts) -----------------------
    /// A run ended. OVERRIDES core `agent_end`, adding `will_retry`.
    AgentEnd {
        messages: Vec<AgentMessage>,
        will_retry: bool,
    },
    /// The run fully settled — no retry/compaction/queued continuation follows.
    AgentSettled,
    /// The steering / follow-up queues changed.
    QueueUpdate {
        steering: Vec<String>,
        follow_up: Vec<String>,
    },
    /// Compaction began.
    CompactionStart { reason: CompactionReason },
    /// A session entry was appended.
    EntryAppended { entry: SessionEntry },
    /// Session metadata changed. `name` is present-but-`undefined` in pi, so it
    /// ALWAYS serializes (as `null` when cleared) — no `skip_serializing_if`.
    SessionInfoChanged { name: Option<String> },
    /// The thinking level changed.
    ThinkingLevelChanged { level: ThinkingLevel },
    /// Compaction ended. `result` is present-but-`undefined` (always emitted);
    /// `error_message` is truly optional (dropped when absent).
    CompactionEnd {
        reason: CompactionReason,
        result: Option<CompactionResult>,
        aborted: bool,
        will_retry: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
    },
    /// An automatic retry started.
    AutoRetryStart {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    },
    /// An automatic retry finished. `final_error` is truly optional.
    AutoRetryEnd {
        success: bool,
        attempt: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_error: Option<String>,
    },
}

impl AgentSessionEvent {
    /// Lift a core agent event into the session union, folding `will_retry` into
    /// `agent_end`. Called from `_handle_agent_event` (pi's session forwards each
    /// core `AgentEvent` to its subscribers, replacing `agent_end`'s payload).
    pub fn from_agent_event(event: AgentEvent, will_retry: bool) -> Self {
        use AgentEvent as A;
        match event {
            A::AgentStart => Self::AgentStart,
            A::AgentEnd { messages } => Self::AgentEnd {
                messages,
                will_retry,
            },
            A::TurnStart => Self::TurnStart,
            A::TurnEnd {
                message,
                tool_results,
            } => Self::TurnEnd {
                message,
                tool_results,
            },
            A::MessageStart { message } => Self::MessageStart { message },
            A::MessageUpdate {
                message,
                assistant_message_event,
            } => Self::MessageUpdate {
                message,
                assistant_message_event,
            },
            A::MessageEnd { message } => Self::MessageEnd { message },
            A::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => Self::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            },
            A::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
            } => Self::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
            },
            A::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            } => Self::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            },
        }
    }
}

/// The confirmed TUI-facing listener type (pi's `AgentSessionEventListener`,
/// `agent-session.ts:165`).
///
/// pi's listener is a synchronous `(event) => void`. atilla's TUI drives the
/// turn on a worker thread while the render loop runs on the main thread, so the
/// listener must cross threads: it is `Send + Sync` and typically forwards the
/// (cloned) event over an `mpsc` channel to the render loop. `Arc` (over `Box`)
/// makes it cheaply clonable and lets unsubscribe compare by pointer identity.
///
/// The `subscribe`/`_emit` machinery that stores and fans out to these listeners
/// lives on the `AgentSession` struct and lands in a later PR; PR1 defines only
/// the alias.
pub type AgentSessionEventListener = Arc<dyn Fn(&AgentSessionEvent) + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Round-trip a value through JSON and assert it is preserved.
    fn round_trip(event: &AgentSessionEvent) -> serde_json::Value {
        let wire = serde_json::to_value(event).unwrap();
        let back: AgentSessionEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(&back, event);
        wire
    }

    #[test]
    fn agent_end_carries_will_retry() {
        let event = AgentSessionEvent::AgentEnd {
            messages: vec![json!({"role": "assistant"})],
            will_retry: true,
        };
        let wire = round_trip(&event);
        assert_eq!(
            wire,
            json!({
                "type": "agent_end",
                "messages": [{"role": "assistant"}],
                "willRetry": true,
            })
        );
    }

    #[test]
    fn session_info_changed_emits_null_name() {
        let event = AgentSessionEvent::SessionInfoChanged { name: None };
        let wire = round_trip(&event);
        // `name` is present-but-undefined in pi: the key is ALWAYS emitted, as
        // `null` when cleared.
        let obj = wire.as_object().unwrap();
        assert!(obj.contains_key("name"), "name key must be present");
        assert_eq!(obj.get("name"), Some(&Value::Null));
        assert_eq!(wire, json!({"type": "session_info_changed", "name": null}));
    }

    #[test]
    fn session_info_changed_emits_some_name() {
        let event = AgentSessionEvent::SessionInfoChanged {
            name: Some("my session".to_string()),
        };
        let wire = round_trip(&event);
        assert_eq!(
            wire,
            json!({"type": "session_info_changed", "name": "my session"})
        );
    }

    #[test]
    fn compaction_end_result_null_and_error_message_absent() {
        let event = AgentSessionEvent::CompactionEnd {
            reason: CompactionReason::Threshold,
            result: None,
            aborted: false,
            will_retry: false,
            error_message: None,
        };
        let wire = round_trip(&event);
        let obj = wire.as_object().unwrap();
        // `result` is present-but-undefined: key present, null when None.
        assert!(obj.contains_key("result"), "result key must be present");
        assert_eq!(obj.get("result"), Some(&Value::Null));
        // `errorMessage` is truly optional: key omitted when None.
        assert!(
            !obj.contains_key("errorMessage"),
            "errorMessage key must be omitted when None"
        );
        assert_eq!(
            wire,
            json!({
                "type": "compaction_end",
                "reason": "threshold",
                "result": null,
                "aborted": false,
                "willRetry": false,
            })
        );
    }

    #[test]
    fn compaction_end_with_result_and_error_message() {
        let event = AgentSessionEvent::CompactionEnd {
            reason: CompactionReason::Manual,
            result: Some(CompactionResult {
                summary: "did stuff".to_string(),
                first_kept_entry_id: "entry-7".to_string(),
                tokens_before: 12345,
                details: None,
            }),
            aborted: false,
            will_retry: true,
            error_message: Some("boom".to_string()),
        };
        let wire = round_trip(&event);
        assert_eq!(
            wire,
            json!({
                "type": "compaction_end",
                "reason": "manual",
                "result": {
                    "summary": "did stuff",
                    "firstKeptEntryId": "entry-7",
                    "tokensBefore": 12345,
                },
                "aborted": false,
                "willRetry": true,
                "errorMessage": "boom",
            })
        );
        // `details` is optional on CompactionResult: omitted when None.
        let result_obj = wire.get("result").unwrap().as_object().unwrap();
        assert!(!result_obj.contains_key("details"));
    }

    #[test]
    fn auto_retry_end_omits_final_error_when_absent() {
        let event = AgentSessionEvent::AutoRetryEnd {
            success: true,
            attempt: 2,
            final_error: None,
        };
        let wire = round_trip(&event);
        let obj = wire.as_object().unwrap();
        assert!(
            !obj.contains_key("finalError"),
            "finalError key must be omitted when None"
        );
        assert_eq!(
            wire,
            json!({"type": "auto_retry_end", "success": true, "attempt": 2})
        );
    }

    #[test]
    fn auto_retry_end_includes_final_error_when_present() {
        let event = AgentSessionEvent::AutoRetryEnd {
            success: false,
            attempt: 3,
            final_error: Some("gave up".to_string()),
        };
        let wire = round_trip(&event);
        assert_eq!(
            wire,
            json!({
                "type": "auto_retry_end",
                "success": false,
                "attempt": 3,
                "finalError": "gave up",
            })
        );
    }

    #[test]
    fn auto_retry_start_wire_shape() {
        let event = AgentSessionEvent::AutoRetryStart {
            attempt: 1,
            max_attempts: 5,
            delay_ms: 250,
            error_message: "transient".to_string(),
        };
        let wire = round_trip(&event);
        assert_eq!(
            wire,
            json!({
                "type": "auto_retry_start",
                "attempt": 1,
                "maxAttempts": 5,
                "delayMs": 250,
                "errorMessage": "transient",
            })
        );
    }

    #[test]
    fn tool_execution_end_wire_shape() {
        let event = AgentSessionEvent::ToolExecutionEnd {
            tool_call_id: "call-1".to_string(),
            tool_name: "bash".to_string(),
            result: json!({"stdout": "ok"}),
            is_error: false,
        };
        let wire = round_trip(&event);
        assert_eq!(
            wire,
            json!({
                "type": "tool_execution_end",
                "toolCallId": "call-1",
                "toolName": "bash",
                "result": {"stdout": "ok"},
                "isError": false,
            })
        );
    }

    #[test]
    fn queue_update_wire_shape() {
        let event = AgentSessionEvent::QueueUpdate {
            steering: vec!["steer-a".to_string()],
            follow_up: vec!["follow-a".to_string(), "follow-b".to_string()],
        };
        let wire = round_trip(&event);
        assert_eq!(
            wire,
            json!({
                "type": "queue_update",
                "steering": ["steer-a"],
                "followUp": ["follow-a", "follow-b"],
            })
        );
    }

    #[test]
    fn agent_settled_is_bare_tag() {
        let event = AgentSessionEvent::AgentSettled;
        let wire = round_trip(&event);
        assert_eq!(wire, json!({"type": "agent_settled"}));
    }

    #[test]
    fn thinking_level_changed_wire_shape() {
        let event = AgentSessionEvent::ThinkingLevelChanged {
            level: ThinkingLevel::Medium,
        };
        let wire = round_trip(&event);
        assert_eq!(
            wire,
            json!({"type": "thinking_level_changed", "level": "medium"})
        );
    }

    #[test]
    fn compaction_start_wire_shape() {
        let event = AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Overflow,
        };
        let wire = round_trip(&event);
        assert_eq!(
            wire,
            json!({"type": "compaction_start", "reason": "overflow"})
        );
    }

    // ---- from_agent_event bridge --------------------------------------------

    #[test]
    fn from_agent_event_folds_will_retry_into_agent_end() {
        let core = AgentEvent::AgentEnd {
            messages: vec![json!({"role": "assistant"})],
        };
        let lifted = AgentSessionEvent::from_agent_event(core, true);
        assert_eq!(
            lifted,
            AgentSessionEvent::AgentEnd {
                messages: vec![json!({"role": "assistant"})],
                will_retry: true,
            }
        );
    }

    #[test]
    fn from_agent_event_maps_each_core_variant() {
        let cases: Vec<(AgentEvent, AgentSessionEvent)> = vec![
            (AgentEvent::AgentStart, AgentSessionEvent::AgentStart),
            (AgentEvent::TurnStart, AgentSessionEvent::TurnStart),
            (
                AgentEvent::TurnEnd {
                    message: json!({"role": "assistant"}),
                    tool_results: vec![],
                },
                AgentSessionEvent::TurnEnd {
                    message: json!({"role": "assistant"}),
                    tool_results: vec![],
                },
            ),
            (
                AgentEvent::MessageStart {
                    message: json!({"role": "user"}),
                },
                AgentSessionEvent::MessageStart {
                    message: json!({"role": "user"}),
                },
            ),
            (
                AgentEvent::MessageEnd {
                    message: json!({"role": "assistant"}),
                },
                AgentSessionEvent::MessageEnd {
                    message: json!({"role": "assistant"}),
                },
            ),
            (
                AgentEvent::ToolExecutionStart {
                    tool_call_id: "c1".to_string(),
                    tool_name: "bash".to_string(),
                    args: json!({"cmd": "ls"}),
                },
                AgentSessionEvent::ToolExecutionStart {
                    tool_call_id: "c1".to_string(),
                    tool_name: "bash".to_string(),
                    args: json!({"cmd": "ls"}),
                },
            ),
            (
                AgentEvent::ToolExecutionUpdate {
                    tool_call_id: "c1".to_string(),
                    tool_name: "bash".to_string(),
                    args: json!({"cmd": "ls"}),
                    partial_result: json!({"stdout": "partial"}),
                },
                AgentSessionEvent::ToolExecutionUpdate {
                    tool_call_id: "c1".to_string(),
                    tool_name: "bash".to_string(),
                    args: json!({"cmd": "ls"}),
                    partial_result: json!({"stdout": "partial"}),
                },
            ),
            (
                AgentEvent::ToolExecutionEnd {
                    tool_call_id: "c1".to_string(),
                    tool_name: "bash".to_string(),
                    result: json!({"stdout": "done"}),
                    is_error: false,
                },
                AgentSessionEvent::ToolExecutionEnd {
                    tool_call_id: "c1".to_string(),
                    tool_name: "bash".to_string(),
                    result: json!({"stdout": "done"}),
                    is_error: false,
                },
            ),
        ];
        for (core, expected) in cases {
            // `will_retry` is irrelevant for non-agent_end variants.
            assert_eq!(AgentSessionEvent::from_agent_event(core, false), expected);
        }
    }

    #[test]
    fn from_agent_event_maps_message_update() {
        // MessageUpdate carries a boxed AssistantMessageEvent; build one via
        // serde from a representative streaming event to avoid hand-constructing
        // the full AssistantMessage.
        let ame: AssistantMessageEvent = serde_json::from_value(json!({
            "type": "text_delta",
            "contentIndex": 0,
            "delta": "hi",
            "partial": {
                "role": "assistant",
                "content": [],
                "api": "test-api",
                "provider": "test-provider",
                "model": "test-model",
                "stopReason": "stop",
                "timestamp": 0,
                "usage": {
                    "input": 0,
                    "output": 0,
                    "cacheRead": 0,
                    "cacheWrite": 0,
                    "totalTokens": 0,
                    "cost": {
                        "input": 0.0,
                        "output": 0.0,
                        "cacheRead": 0.0,
                        "cacheWrite": 0.0,
                        "total": 0.0
                    }
                }
            }
        }))
        .expect("AssistantMessageEvent should deserialize");
        let core = AgentEvent::MessageUpdate {
            message: json!({"role": "assistant"}),
            assistant_message_event: Box::new(ame.clone()),
        };
        assert_eq!(
            AgentSessionEvent::from_agent_event(core, false),
            AgentSessionEvent::MessageUpdate {
                message: json!({"role": "assistant"}),
                assistant_message_event: Box::new(ame),
            }
        );
    }
}
