//! Faithful Rust port of pi's extension event surface.
//!
//! Mirrors the 33 hook-event payload interfaces defined in
//! `packages/coding-agent/src/core/extensions/types.ts` (the union
//! `ExtensionEvent` at `types.ts:1020`), together with their handler-result
//! shapes. The events are grouped into submodules following the recon grouping:
//! [`session`] (session lifecycle + project trust + resource discovery),
//! [`provider`] (provider request/response), [`agent`] (agent lifecycle +
//! context), [`turn`] (turn + message lifecycle), [`tool`] (tool execution +
//! the tool-call/tool-result middleware), and [`selection`] (model/thinking-level
//! selection + user bash + input). Shared opaque payload aliases live in
//! [`common`].
//!
//! # Wire shape
//!
//! These types cross the JavaScript extension boundary as JSON — only
//! [`serde_json::Value`] ever crosses, per `notes/startup/deep-hooks.md` — so
//! every event and result derives serde and matches pi's field casing
//! (camelCase) and its discriminated-union tags. [`ExtensionEvent`] is the
//! internally-tagged union keyed by pi's `type` discriminant, reproducing the
//! exact `{"type": "...", ...}` wire object for each of the 33 events.
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/types.ts`.

// straitjacket-allow-file:duplication

use serde::{Deserialize, Serialize};

use super::hook::HookEvent;

pub mod agent;
pub mod common;
pub mod provider;
pub mod selection;
pub mod session;
pub mod tool;
pub mod turn;

pub use agent::{
    AgentEndEvent, AgentSettledEvent, AgentStartEvent, BeforeAgentStartEvent,
    BeforeAgentStartEventResult, ContextEvent, ContextEventResult,
};
pub use provider::{
    AfterProviderResponseEvent, BeforeProviderHeadersEvent, BeforeProviderRequestEvent,
    BeforeProviderRequestEventResult,
};
pub use selection::{
    InputEvent, InputEventResult, InputSource, ModelSelectEvent, ModelSelectSource,
    StreamingBehavior, ThinkingLevelSelectEvent, UserBashEvent, UserBashEventResult,
};
pub use session::{
    CompactionReason, DiscoveredResourcePath, ForkPosition, ProjectTrustEvent,
    ProjectTrustEventDecision,
    ProjectTrustEventResult, ResourcesDiscoverEvent, ResourcesDiscoverReason,
    ResourcesDiscoverResult, SessionBeforeCompactEvent, SessionBeforeCompactResult,
    SessionBeforeForkEvent, SessionBeforeForkResult, SessionBeforeSwitchEvent,
    SessionBeforeSwitchReason, SessionBeforeSwitchResult, SessionBeforeTreeEvent,
    SessionBeforeTreeResult, SessionBeforeTreeSummary, SessionCompactEvent,
    SessionInfoChangedEvent, SessionShutdownEvent, SessionShutdownReason, SessionStartEvent,
    SessionStartReason, SessionTreeEvent, TreePreparation,
};
pub use tool::{
    ToolCallEvent, ToolCallEventResult, ToolExecutionEndEvent, ToolExecutionStartEvent,
    ToolExecutionUpdateEvent, ToolResultContent, ToolResultEvent, ToolResultEventResult,
};
pub use turn::{
    MessageEndEvent, MessageEndEventResult, MessageStartEvent, MessageUpdateEvent, TurnEndEvent,
    TurnStartEvent,
};

/// The union of all extension event payloads (pi's `ExtensionEvent`,
/// `types.ts:1020`).
///
/// Internally tagged by the `type` discriminant, exactly reproducing pi's wire
/// object. Every variant name lowers to pi's snake_case event name. Note that
/// pi's `ToolCallEvent` and `ToolResultEvent` are themselves unions over
/// `toolName` sharing the `tool_call` / `tool_result` tag; the port collapses
/// each to a single struct variant (see [`tool`] module docs), so this union has
/// exactly one arm per distinct `type` tag — the 33 events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionEvent {
    /// `project_trust`
    ProjectTrust(ProjectTrustEvent),
    /// `resources_discover`
    ResourcesDiscover(ResourcesDiscoverEvent),
    /// `session_start`
    SessionStart(SessionStartEvent),
    /// `session_info_changed`
    SessionInfoChanged(SessionInfoChangedEvent),
    /// `session_before_switch`
    SessionBeforeSwitch(SessionBeforeSwitchEvent),
    /// `session_before_fork`
    SessionBeforeFork(SessionBeforeForkEvent),
    /// `session_before_compact`
    SessionBeforeCompact(SessionBeforeCompactEvent),
    /// `session_compact`
    SessionCompact(SessionCompactEvent),
    /// `session_shutdown`
    SessionShutdown(SessionShutdownEvent),
    /// `session_before_tree`
    SessionBeforeTree(SessionBeforeTreeEvent),
    /// `session_tree`
    SessionTree(SessionTreeEvent),
    /// `context`
    Context(ContextEvent),
    /// `before_provider_request`
    BeforeProviderRequest(BeforeProviderRequestEvent),
    /// `before_provider_headers`
    BeforeProviderHeaders(BeforeProviderHeadersEvent),
    /// `after_provider_response`
    AfterProviderResponse(AfterProviderResponseEvent),
    /// `before_agent_start`
    BeforeAgentStart(BeforeAgentStartEvent),
    /// `agent_start`
    AgentStart(AgentStartEvent),
    /// `agent_end`
    AgentEnd(AgentEndEvent),
    /// `agent_settled`
    AgentSettled(AgentSettledEvent),
    /// `turn_start`
    TurnStart(TurnStartEvent),
    /// `turn_end`
    TurnEnd(TurnEndEvent),
    /// `message_start`
    MessageStart(MessageStartEvent),
    /// `message_update`
    MessageUpdate(MessageUpdateEvent),
    /// `message_end`
    MessageEnd(MessageEndEvent),
    /// `tool_execution_start`
    ToolExecutionStart(ToolExecutionStartEvent),
    /// `tool_execution_update`
    ToolExecutionUpdate(ToolExecutionUpdateEvent),
    /// `tool_execution_end`
    ToolExecutionEnd(ToolExecutionEndEvent),
    /// `model_select`
    ModelSelect(ModelSelectEvent),
    /// `thinking_level_select`
    ThinkingLevelSelect(ThinkingLevelSelectEvent),
    /// `user_bash`
    UserBash(UserBashEvent),
    /// `input`
    Input(InputEvent),
    /// `tool_call`
    ToolCall(ToolCallEvent),
    /// `tool_result`
    ToolResult(ToolResultEvent),
}

impl ExtensionEvent {
    /// The [`HookEvent`] discriminant for this event payload.
    ///
    /// This is the bridge between a concrete event payload and the event-name
    /// enum a [`super::hook::Hook`] subscribes to.
    pub fn hook_event(&self) -> HookEvent {
        match self {
            ExtensionEvent::ProjectTrust(_) => HookEvent::ProjectTrust,
            ExtensionEvent::ResourcesDiscover(_) => HookEvent::ResourcesDiscover,
            ExtensionEvent::SessionStart(_) => HookEvent::SessionStart,
            ExtensionEvent::SessionInfoChanged(_) => HookEvent::SessionInfoChanged,
            ExtensionEvent::SessionBeforeSwitch(_) => HookEvent::SessionBeforeSwitch,
            ExtensionEvent::SessionBeforeFork(_) => HookEvent::SessionBeforeFork,
            ExtensionEvent::SessionBeforeCompact(_) => HookEvent::SessionBeforeCompact,
            ExtensionEvent::SessionCompact(_) => HookEvent::SessionCompact,
            ExtensionEvent::SessionShutdown(_) => HookEvent::SessionShutdown,
            ExtensionEvent::SessionBeforeTree(_) => HookEvent::SessionBeforeTree,
            ExtensionEvent::SessionTree(_) => HookEvent::SessionTree,
            ExtensionEvent::Context(_) => HookEvent::Context,
            ExtensionEvent::BeforeProviderRequest(_) => HookEvent::BeforeProviderRequest,
            ExtensionEvent::BeforeProviderHeaders(_) => HookEvent::BeforeProviderHeaders,
            ExtensionEvent::AfterProviderResponse(_) => HookEvent::AfterProviderResponse,
            ExtensionEvent::BeforeAgentStart(_) => HookEvent::BeforeAgentStart,
            ExtensionEvent::AgentStart(_) => HookEvent::AgentStart,
            ExtensionEvent::AgentEnd(_) => HookEvent::AgentEnd,
            ExtensionEvent::AgentSettled(_) => HookEvent::AgentSettled,
            ExtensionEvent::TurnStart(_) => HookEvent::TurnStart,
            ExtensionEvent::TurnEnd(_) => HookEvent::TurnEnd,
            ExtensionEvent::MessageStart(_) => HookEvent::MessageStart,
            ExtensionEvent::MessageUpdate(_) => HookEvent::MessageUpdate,
            ExtensionEvent::MessageEnd(_) => HookEvent::MessageEnd,
            ExtensionEvent::ToolExecutionStart(_) => HookEvent::ToolExecutionStart,
            ExtensionEvent::ToolExecutionUpdate(_) => HookEvent::ToolExecutionUpdate,
            ExtensionEvent::ToolExecutionEnd(_) => HookEvent::ToolExecutionEnd,
            ExtensionEvent::ModelSelect(_) => HookEvent::ModelSelect,
            ExtensionEvent::ThinkingLevelSelect(_) => HookEvent::ThinkingLevelSelect,
            ExtensionEvent::UserBash(_) => HookEvent::UserBash,
            ExtensionEvent::Input(_) => HookEvent::Input,
            ExtensionEvent::ToolCall(_) => HookEvent::ToolCall,
            ExtensionEvent::ToolResult(_) => HookEvent::ToolResult,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One representative payload per event variant, covering every `type` tag.
    fn sample_events() -> Vec<ExtensionEvent> {
        vec![
            ExtensionEvent::ProjectTrust(ProjectTrustEvent {
                cwd: "/repo".into(),
            }),
            ExtensionEvent::ResourcesDiscover(ResourcesDiscoverEvent {
                cwd: "/repo".into(),
                reason: ResourcesDiscoverReason::Startup,
            }),
            ExtensionEvent::SessionStart(SessionStartEvent {
                reason: SessionStartReason::New,
                previous_session_file: Some("prev.jsonl".into()),
            }),
            ExtensionEvent::SessionInfoChanged(SessionInfoChangedEvent {
                name: Some("work".into()),
            }),
            ExtensionEvent::SessionBeforeSwitch(SessionBeforeSwitchEvent {
                reason: SessionBeforeSwitchReason::Resume,
                target_session_file: None,
            }),
            ExtensionEvent::SessionBeforeFork(SessionBeforeForkEvent {
                entry_id: "e1".into(),
                position: ForkPosition::At,
            }),
            ExtensionEvent::SessionBeforeCompact(SessionBeforeCompactEvent {
                preparation: json!({ "tokens": 100 }),
                branch_entries: vec![json!({ "id": "e1" })],
                custom_instructions: Some("keep the plan".into()),
                reason: CompactionReason::Threshold,
                will_retry: false,
            }),
            ExtensionEvent::SessionCompact(SessionCompactEvent {
                compaction_entry: json!({ "id": "c1" }),
                from_extension: true,
                reason: CompactionReason::Manual,
                will_retry: true,
            }),
            ExtensionEvent::SessionShutdown(SessionShutdownEvent {
                reason: SessionShutdownReason::Quit,
                target_session_file: None,
            }),
            ExtensionEvent::SessionBeforeTree(SessionBeforeTreeEvent {
                preparation: TreePreparation {
                    target_id: "t1".into(),
                    old_leaf_id: None,
                    common_ancestor_id: Some("a1".into()),
                    entries_to_summarize: vec![],
                    user_wants_summary: true,
                    custom_instructions: None,
                    replace_instructions: None,
                    label: None,
                },
            }),
            ExtensionEvent::SessionTree(SessionTreeEvent {
                new_leaf_id: Some("l2".into()),
                old_leaf_id: Some("l1".into()),
                summary_entry: None,
                from_extension: Some(false),
            }),
            ExtensionEvent::Context(ContextEvent {
                messages: vec![json!({ "role": "user" })],
            }),
            ExtensionEvent::BeforeProviderRequest(BeforeProviderRequestEvent {
                payload: json!({ "model": "x" }),
            }),
            ExtensionEvent::BeforeProviderHeaders(BeforeProviderHeadersEvent {
                headers: json!({ "x-trace": "1" }),
            }),
            ExtensionEvent::AfterProviderResponse(AfterProviderResponseEvent {
                status: 200,
                headers: [("content-type".to_string(), "application/json".to_string())]
                    .into_iter()
                    .collect(),
            }),
            ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {
                prompt: "hi".into(),
                images: None,
                system_prompt: "You are Pi".into(),
                system_prompt_options: json!({ "tools": [] }),
            }),
            ExtensionEvent::AgentStart(AgentStartEvent {}),
            ExtensionEvent::AgentEnd(AgentEndEvent {
                messages: vec![json!({ "role": "assistant" })],
            }),
            ExtensionEvent::AgentSettled(AgentSettledEvent {}),
            ExtensionEvent::TurnStart(TurnStartEvent {
                turn_index: 0,
                timestamp: 1_700_000_000,
            }),
            ExtensionEvent::TurnEnd(TurnEndEvent {
                turn_index: 0,
                message: json!({ "role": "assistant" }),
                tool_results: vec![],
            }),
            ExtensionEvent::MessageStart(MessageStartEvent {
                message: json!({ "role": "user" }),
            }),
            ExtensionEvent::MessageUpdate(MessageUpdateEvent {
                message: json!({ "role": "assistant" }),
                assistant_message_event: json!({ "delta": "he" }),
            }),
            ExtensionEvent::MessageEnd(MessageEndEvent {
                message: json!({ "role": "assistant" }),
            }),
            ExtensionEvent::ToolExecutionStart(ToolExecutionStartEvent {
                tool_call_id: "tc1".into(),
                tool_name: "bash".into(),
                args: json!({ "command": "ls" }),
            }),
            ExtensionEvent::ToolExecutionUpdate(ToolExecutionUpdateEvent {
                tool_call_id: "tc1".into(),
                tool_name: "bash".into(),
                args: json!({ "command": "ls" }),
                partial_result: json!("part"),
            }),
            ExtensionEvent::ToolExecutionEnd(ToolExecutionEndEvent {
                tool_call_id: "tc1".into(),
                tool_name: "bash".into(),
                result: json!("done"),
                is_error: false,
            }),
            ExtensionEvent::ModelSelect(ModelSelectEvent {
                model: json!({ "id": "m2" }),
                previous_model: Some(json!({ "id": "m1" })),
                source: ModelSelectSource::Cycle,
            }),
            ExtensionEvent::ThinkingLevelSelect(ThinkingLevelSelectEvent {
                level: json!("high"),
                previous_level: json!("low"),
            }),
            ExtensionEvent::UserBash(UserBashEvent {
                command: "git status".into(),
                exclude_from_context: true,
                cwd: "/repo".into(),
            }),
            ExtensionEvent::Input(InputEvent {
                text: "hello".into(),
                images: None,
                source: InputSource::Interactive,
                streaming_behavior: Some(StreamingBehavior::FollowUp),
            }),
            ExtensionEvent::ToolCall(ToolCallEvent {
                tool_call_id: "tc1".into(),
                tool_name: "bash".into(),
                input: json!({ "command": "ls" }),
            }),
            ExtensionEvent::ToolResult(ToolResultEvent {
                tool_call_id: "tc1".into(),
                tool_name: "bash".into(),
                input: json!({ "command": "ls" }),
                content: vec![json!({ "type": "text", "text": "ok" })],
                is_error: false,
                details: json!({ "exitCode": 0 }),
            }),
        ]
    }

    #[test]
    fn every_event_variant_serde_round_trips() {
        let events = sample_events();
        // One sample per distinct `type` tag: the 33 pi hook events.
        assert_eq!(events.len(), 33);
        for event in events {
            let serialized = serde_json::to_value(&event).unwrap();
            // The wire object carries pi's `type` discriminant.
            assert_eq!(
                serialized.get("type").and_then(|t| t.as_str()),
                Some(event.hook_event().as_str()),
                "event {event:?} serialized without its matching type tag",
            );
            let restored: ExtensionEvent = serde_json::from_value(serialized).unwrap();
            assert_eq!(restored, event, "round-trip changed event {event:?}");
        }
    }

    #[test]
    fn every_hook_event_name_is_covered_once() {
        use std::collections::BTreeSet;
        let tags: BTreeSet<String> = sample_events()
            .iter()
            .map(|e| e.hook_event().as_str().to_string())
            .collect();
        assert_eq!(tags.len(), 33);
        assert_eq!(tags.len(), HookEvent::ALL.len());
        for event in HookEvent::ALL {
            assert!(
                tags.contains(event.as_str()),
                "missing sample for {event:?}"
            );
        }
    }

    #[test]
    fn session_info_changed_cleared_name_serializes_as_null() {
        // pi's `name: string | undefined` is a required property: it must be
        // present (as `null`) even when cleared, not omitted.
        let event = ExtensionEvent::SessionInfoChanged(SessionInfoChangedEvent { name: None });
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({ "type": "session_info_changed", "name": null }),
        );
    }

    #[test]
    fn tool_call_wire_shape_matches_pi() {
        let event = ExtensionEvent::ToolCall(ToolCallEvent {
            tool_call_id: "tc1".into(),
            tool_name: "bash".into(),
            input: json!({ "command": "ls" }),
        });
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "type": "tool_call",
                "toolCallId": "tc1",
                "toolName": "bash",
                "input": { "command": "ls" },
            }),
        );
    }

    #[test]
    fn input_event_result_is_tagged_by_action() {
        let cases = [
            (InputEventResult::Continue, json!({ "action": "continue" })),
            (InputEventResult::Handled, json!({ "action": "handled" })),
            (
                InputEventResult::Transform {
                    text: "new".into(),
                    images: None,
                },
                json!({ "action": "transform", "text": "new" }),
            ),
        ];
        for (result, wire) in cases {
            assert_eq!(serde_json::to_value(&result).unwrap(), wire);
            let restored: InputEventResult = serde_json::from_value(wire).unwrap();
            assert_eq!(restored, result);
        }
    }

    #[test]
    fn result_types_round_trip() {
        let tool_call = ToolCallEventResult {
            block: Some(true),
            reason: Some("denied".into()),
        };
        assert_eq!(
            serde_json::to_value(&tool_call).unwrap(),
            json!({ "block": true, "reason": "denied" }),
        );

        let empty = ToolCallEventResult::default();
        // Omitted optionals produce an empty object, matching an absent result.
        assert_eq!(serde_json::to_value(&empty).unwrap(), json!({}));

        let before_agent = BeforeAgentStartEventResult {
            message: None,
            system_prompt: Some("override".into()),
        };
        assert_eq!(
            serde_json::to_value(&before_agent).unwrap(),
            json!({ "systemPrompt": "override" }),
        );

        let trust = ProjectTrustEventResult {
            trusted: ProjectTrustEventDecision::Yes,
            remember: Some(true),
        };
        assert_eq!(
            serde_json::to_value(&trust).unwrap(),
            json!({ "trusted": "yes", "remember": true }),
        );
    }
}
