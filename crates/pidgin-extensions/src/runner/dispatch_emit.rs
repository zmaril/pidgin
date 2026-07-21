//! The dispatch-time emitters the [`ExtensionRunner`] seam needs beyond the six
//! acceptance emitters in [`emit`](super::emit): the enum-dispatch generic
//! `emit`, plus `emit_tool_call` / `emit_message_end` / `emit_resources_discover`
//! / `emit_session_shutdown`.
//!
//! Each is a faithful port of its `runner.ts` counterpart: build the JSON event,
//! invoke every registered handler in order over the [`JsPlaneHandle`] rendezvous
//! (`super::ExtensionRunner::plane`), isolate a throw into an `onError` record and
//! continue, and fold the result per pi's per-hook semantics. Errors never
//! propagate — pi's emitters return a bare value, side-channelling failures to
//! `onError`.
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/runner.ts`.

use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};

use pidgin_coding::core::extensions::events::common::AgentMessage;
use pidgin_coding::core::extensions::events::{
    DiscoveredResourcePath, MessageEndEvent, MessageEndEventResult, ResourcesDiscoverReason,
    ResourcesDiscoverResult, SessionBeforeCompactResult, SessionBeforeForkResult,
    SessionBeforeSwitchResult, SessionBeforeTreeResult, SessionShutdownEvent, ToolCallEvent,
    ToolCallEventResult,
};
use pidgin_coding::core::extensions::runner::{ExtensionDispatchEvent, ExtensionEmitOutcome};
use serde::Deserialize;

use super::ExtensionRunner;

/// Deserialize a handler's JSON return value into a typed result, treating a
/// `null` (the JS `undefined`) or any malformed value as "no result".
fn parse_result<T: DeserializeOwned>(value: &Value) -> Option<T> {
    if value.is_null() {
        return None;
    }
    serde_json::from_value(value.clone()).ok()
}

/// Tag a serialized event object with its `type` discriminant, wrapping a
/// non-object payload (pi's opaque `model_select` / `entry_appended` values)
/// under a `value` key so the JS handler still receives an object.
fn tagged(type_name: &str, payload: Value) -> Value {
    let mut map = match payload {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        other => {
            let mut map = Map::new();
            map.insert("value".into(), other);
            map
        }
    };
    map.insert("type".into(), Value::String(type_name.into()));
    Value::Object(map)
}

/// The paths-only per-handler shape of a `resources_discover` result (pi's
/// `ResourcesDiscoverResult` interface, `types.ts:539`). Private to the impl: the
/// runner folds it into the widened `{ path, extensionPath }` aggregate.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ResourcesDiscoverHandlerResult {
    skill_paths: Option<Vec<String>>,
    prompt_paths: Option<Vec<String>>,
    theme_paths: Option<Vec<String>>,
}

/// Map a dispatch variant to `(event_name, event_json)`. The event JSON is the
/// serialized ported event struct tagged with its snake_case `type`.
fn dispatch_event_json(event: &ExtensionDispatchEvent) -> (&'static str, Value) {
    use ExtensionDispatchEvent::*;
    fn to_json<T: serde::Serialize>(value: &T) -> Value {
        serde_json::to_value(value).unwrap_or(Value::Null)
    }
    match event {
        AgentStart(e) => ("agent_start", tagged("agent_start", to_json(e))),
        AgentEnd(e) => ("agent_end", tagged("agent_end", to_json(e))),
        AgentSettled(e) => ("agent_settled", tagged("agent_settled", to_json(e))),
        TurnStart(e) => ("turn_start", tagged("turn_start", to_json(e))),
        TurnEnd(e) => ("turn_end", tagged("turn_end", to_json(e))),
        MessageStart(e) => ("message_start", tagged("message_start", to_json(e))),
        MessageUpdate(e) => ("message_update", tagged("message_update", to_json(e))),
        ToolExecutionStart(e) => (
            "tool_execution_start",
            tagged("tool_execution_start", to_json(e)),
        ),
        ToolExecutionUpdate(e) => (
            "tool_execution_update",
            tagged("tool_execution_update", to_json(e)),
        ),
        ToolExecutionEnd(e) => (
            "tool_execution_end",
            tagged("tool_execution_end", to_json(e)),
        ),
        ModelSelect(v) => ("model_select", tagged("model_select", v.clone())),
        // No `HookEvent` member yet; pi fires "thinking_level_changed".
        ThinkingLevelChanged(v) => (
            "thinking_level_changed",
            tagged("thinking_level_changed", v.clone()),
        ),
        SessionStart(e) => ("session_start", tagged("session_start", to_json(e))),
        SessionCompact(e) => ("session_compact", tagged("session_compact", to_json(e))),
        SessionBeforeCompact(e) => (
            "session_before_compact",
            tagged("session_before_compact", to_json(e)),
        ),
        SessionTree(e) => ("session_tree", tagged("session_tree", to_json(e))),
        SessionBeforeTree(e) => (
            "session_before_tree",
            tagged("session_before_tree", to_json(e)),
        ),
        SessionBeforeSwitch(e) => (
            "session_before_switch",
            tagged("session_before_switch", to_json(e)),
        ),
        SessionBeforeFork(e) => (
            "session_before_fork",
            tagged("session_before_fork", to_json(e)),
        ),
        EntryAppended(v) => ("entry_appended", tagged("entry_appended", v.clone())),
    }
}

impl ExtensionRunner {
    /// The enum-dispatch generic `emit` (pi's `emit<TEvent>`, `runner.ts:784`).
    ///
    /// Every registered handler for the event runs in order (isolating throws to
    /// `onError`). Only the two `session_before_*` events produce a non-`None`
    /// outcome: the last non-`undefined` handler result wins, and a `cancel`
    /// result short-circuits the remaining handlers.
    pub async fn emit_dispatch(&self, event: &ExtensionDispatchEvent) -> ExtensionEmitOutcome {
        let (name, event_json) = dispatch_event_json(event);
        let ctx = self.context_config().to_json();
        let is_before_compact = matches!(event, ExtensionDispatchEvent::SessionBeforeCompact(_));
        let is_before_tree = matches!(event, ExtensionDispatchEvent::SessionBeforeTree(_));
        let is_before_switch = matches!(event, ExtensionDispatchEvent::SessionBeforeSwitch(_));
        let is_before_fork = matches!(event, ExtensionDispatchEvent::SessionBeforeFork(_));

        let sites: Vec<String> = self
            .sites_by_name(name)
            .into_iter()
            .map(str::to_string)
            .collect();
        let mut outcome = ExtensionEmitOutcome::None;

        for (index, extension_path) in sites.into_iter().enumerate() {
            let invocation = match self
                .plane()
                .invoke_hook(name, index, &event_json, &ctx)
                .await
            {
                Ok(invocation) => invocation,
                Err(_) => continue,
            };
            if !invocation.ok {
                self.record_error(name, &extension_path, invocation);
                continue;
            }
            if is_before_compact {
                if let Some(result) = parse_result::<SessionBeforeCompactResult>(&invocation.result)
                {
                    let cancel = result.cancel == Some(true);
                    outcome = ExtensionEmitOutcome::BeforeCompact(result);
                    if cancel {
                        return outcome;
                    }
                }
            } else if is_before_tree {
                if let Some(result) = parse_result::<SessionBeforeTreeResult>(&invocation.result) {
                    let cancel = result.cancel == Some(true);
                    outcome = ExtensionEmitOutcome::BeforeTree(result);
                    if cancel {
                        return outcome;
                    }
                }
            } else if is_before_switch {
                if let Some(result) = parse_result::<SessionBeforeSwitchResult>(&invocation.result)
                {
                    let cancel = result.cancel == Some(true);
                    outcome = ExtensionEmitOutcome::BeforeSwitch(result);
                    if cancel {
                        return outcome;
                    }
                }
            } else if is_before_fork {
                if let Some(result) = parse_result::<SessionBeforeForkResult>(&invocation.result) {
                    let cancel = result.cancel == Some(true);
                    outcome = ExtensionEmitOutcome::BeforeFork(result);
                    if cancel {
                        return outcome;
                    }
                }
            }
        }

        outcome
    }

    /// `emitToolCall` (`runner.ts:910`): the first `block` result short-circuits;
    /// otherwise the last non-`undefined` result wins.
    pub async fn emit_tool_call(&self, event: &ToolCallEvent) -> Option<ToolCallEventResult> {
        let sites: Vec<String> = self
            .sites_by_name("tool_call")
            .into_iter()
            .map(str::to_string)
            .collect();
        let ctx = self.context_config().to_json();
        let event_json = json!({
            "type": "tool_call",
            "toolCallId": event.tool_call_id,
            "toolName": event.tool_name,
            "input": event.input,
        });
        let mut result = None;

        for (index, extension_path) in sites.into_iter().enumerate() {
            let invocation = match self
                .plane()
                .invoke_hook("tool_call", index, &event_json, &ctx)
                .await
            {
                Ok(invocation) => invocation,
                Err(_) => continue,
            };
            if !invocation.ok {
                self.record_error("tool_call", &extension_path, invocation);
                continue;
            }
            if let Some(parsed) = parse_result::<ToolCallEventResult>(&invocation.result) {
                let block = parsed.block == Some(true);
                result = Some(parsed);
                if block {
                    return result;
                }
            }
        }

        result
    }

    /// `emitMessageEnd` (`runner.ts:818`): each handler may replace the message,
    /// but the replacement must keep the same role; a role change is isolated as
    /// an error and skipped.
    pub async fn emit_message_end(&self, event: &MessageEndEvent) -> Option<AgentMessage> {
        let sites: Vec<String> = self
            .sites_by_name("message_end")
            .into_iter()
            .map(str::to_string)
            .collect();
        let ctx = self.context_config().to_json();
        let mut current = event.message.clone();
        let mut modified = false;

        for (index, extension_path) in sites.into_iter().enumerate() {
            let event_json = json!({ "type": "message_end", "message": current });
            let invocation = match self
                .plane()
                .invoke_hook("message_end", index, &event_json, &ctx)
                .await
            {
                Ok(invocation) => invocation,
                Err(_) => continue,
            };
            if !invocation.ok {
                self.record_error("message_end", &extension_path, invocation);
                continue;
            }
            let Some(result) = parse_result::<MessageEndEventResult>(&invocation.result) else {
                continue;
            };
            let Some(message) = result.message else {
                continue;
            };
            if message.get("role") != current.get("role") {
                self.record_synthetic_error(
                    "message_end",
                    &extension_path,
                    "message_end handlers must return a message with the same role",
                );
                continue;
            }
            current = message;
            modified = true;
        }

        if modified {
            Some(current)
        } else {
            None
        }
    }

    /// `emitResourcesDiscover` (`runner.ts:1125`): collect every handler's
    /// paths-only result into the widened `{ path, extensionPath }` aggregate,
    /// stamping each path with its contributing extension.
    pub async fn emit_resources_discover(
        &self,
        cwd: &str,
        reason: ResourcesDiscoverReason,
    ) -> ResourcesDiscoverResult {
        let sites: Vec<String> = self
            .sites_by_name("resources_discover")
            .into_iter()
            .map(str::to_string)
            .collect();
        let ctx = self.context_config().to_json();
        let reason_str = match reason {
            ResourcesDiscoverReason::Startup => "startup",
            ResourcesDiscoverReason::Reload => "reload",
        };
        let mut result = ResourcesDiscoverResult::default();

        for (index, extension_path) in sites.into_iter().enumerate() {
            let event_json = json!({
                "type": "resources_discover",
                "cwd": cwd,
                "reason": reason_str,
            });
            let invocation = match self
                .plane()
                .invoke_hook("resources_discover", index, &event_json, &ctx)
                .await
            {
                Ok(invocation) => invocation,
                Err(_) => continue,
            };
            if !invocation.ok {
                self.record_error("resources_discover", &extension_path, invocation);
                continue;
            }
            let Some(handler_result) =
                parse_result::<ResourcesDiscoverHandlerResult>(&invocation.result)
            else {
                continue;
            };
            push_discovered(
                &mut result.skill_paths,
                handler_result.skill_paths,
                &extension_path,
            );
            push_discovered(
                &mut result.prompt_paths,
                handler_result.prompt_paths,
                &extension_path,
            );
            push_discovered(
                &mut result.theme_paths,
                handler_result.theme_paths,
                &extension_path,
            );
        }

        result
    }

    /// `emitSessionShutdown` (pi free fn `emitSessionShutdownEvent`,
    /// `agent-session` L2583): fire every `session_shutdown` handler; no result
    /// is collected.
    pub async fn emit_session_shutdown(&self, event: &SessionShutdownEvent) {
        let sites: Vec<String> = self
            .sites_by_name("session_shutdown")
            .into_iter()
            .map(str::to_string)
            .collect();
        let ctx = self.context_config().to_json();
        let event_json = tagged(
            "session_shutdown",
            serde_json::to_value(event).unwrap_or(Value::Null),
        );

        for (index, extension_path) in sites.into_iter().enumerate() {
            let invocation = match self
                .plane()
                .invoke_hook("session_shutdown", index, &event_json, &ctx)
                .await
            {
                Ok(invocation) => invocation,
                Err(_) => continue,
            };
            if !invocation.ok {
                self.record_error("session_shutdown", &extension_path, invocation);
            }
        }
    }
}

/// Append each path in `paths` to `into` as a `{ path, extensionPath }` pair.
fn push_discovered(
    into: &mut Vec<DiscoveredResourcePath>,
    paths: Option<Vec<String>>,
    extension_path: &str,
) {
    for path in paths.into_iter().flatten() {
        into.push(DiscoveredResourcePath {
            path,
            extension_path: extension_path.to_string(),
        });
    }
}
