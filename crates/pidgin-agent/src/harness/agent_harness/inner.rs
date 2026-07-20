//! [`HarnessInner`](super::HarnessInner) implementation — the interior-mutable
//! runtime for [`AgentHarness`](super::AgentHarness).
//!
//! Split from the parent module to keep each file under the straitjacket
//! file-size ceiling; the type definitions live in the parent and this file
//! carries the bulk of the behavior (emit dispatch, config hooks, provider
//! streaming, agent-event handling, and the compaction/tree drivers).

// straitjacket-allow-file:duplication — the compaction/branch-summary drivers
// and per-event emit wrappers are faithful parallel transcriptions of pi's
// one-method-per-shape source.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use pidgin_ai::seams::{AbortSignal, StreamResult};
use pidgin_ai::{
    AssistantMessage, AssistantRole, ContentBlock, Context, Model, StopReason, Usage, UsageCost,
};

use super::*;
use crate::agent_loop::AgentEventSink;
use crate::harness::compaction::{
    collect_entries_for_branch_summary, compact, generate_branch_summary, prepare_compaction,
    GenerateBranchSummaryOptions, DEFAULT_COMPACTION_SETTINGS,
};
use crate::harness::events::AgentHarnessEventResult;
use crate::harness::events::{
    AfterProviderResponseEvent, AgentHarnessOwnEvent, AgentHarnessPhase, BeforeAgentStartEvent,
    BeforeAgentStartResult, BeforeProviderPayloadEvent, BeforeProviderPayloadResult,
    BeforeProviderRequestEvent, BeforeProviderRequestResult, ContextEvent, ContextResult,
    NavigateTreeResult, QueueUpdateEvent, SavePointEvent, SessionBeforeCompactEvent,
    SessionBeforeCompactResult, SessionBeforeTreeEvent, SessionBeforeTreeResult,
    SessionCompactEvent, SessionTreeEvent, SettledEvent, ToolCallEvent, ToolCallResult,
    ToolResultEvent, ToolResultPatch, TreePreparation,
};
use crate::harness::messages::convert_to_llm;
use crate::harness::options::{
    AgentHarnessError, AgentHarnessErrorCode, PendingActiveToolsChange, PendingCustom,
    PendingCustomMessage, PendingLabel, PendingLeaf, PendingMessage, PendingModelChange,
    PendingSessionInfo, PendingSessionWrite, PendingThinkingLevelChange, ProviderStreamRequest,
    SystemPromptContext, SystemPromptSource,
};
use crate::harness::session::MoveSummary;
use crate::harness::types::{BranchSummaryEntry, SessionTreeEntry};
use crate::types::{
    AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent, AgentLoopConfig,
    AgentLoopTurnUpdate, AgentMessage, BeforeToolCallContext, BeforeToolCallResult,
    PrepareNextTurnContext, QueueMode, StreamFn, ThinkingLevel,
};

// ---------------------------------------------------------------------------
// Interior implementation.
// ---------------------------------------------------------------------------

impl HarnessInner {
    pub(super) fn alloc_id(&self) -> u64 {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        id
    }

    pub(super) fn get_resources(&self) -> AgentHarnessResources {
        let resources = self.resources.borrow();
        AgentHarnessResources {
            skills: resources.skills.clone(),
            prompt_templates: resources.prompt_templates.clone(),
        }
    }

    pub(super) fn queue_snapshot(&self) -> QueueUpdateEvent {
        QueueUpdateEvent {
            steer: self.steer_queue.borrow().clone(),
            follow_up: self.follow_up_queue.borrow().clone(),
            next_turn: self.next_turn_queue.borrow().clone(),
        }
    }

    /// Record the first in-loop hook/session error and trip the run signal so the
    /// loop winds down (see the module docs on error handling).
    pub(super) fn record_run_error(&self, error: AgentHarnessError) {
        {
            let mut slot = self.run_error.borrow_mut();
            if slot.is_none() {
                *slot = Some(error);
            }
        }
        self.suppress.set(true);
        if let Some(signal) = self.run_abort.borrow().as_ref() {
            signal.abort();
        }
    }

    // -- Emit machinery ----------------------------------------------------

    pub(super) fn subscriber_list(&self) -> Vec<Subscriber> {
        self.subscribers
            .borrow()
            .iter()
            .map(|(_, h)| h.clone())
            .collect()
    }

    pub(super) fn on_list(&self, event_type: &str) -> Vec<OwnHandler> {
        self.on_handlers
            .borrow()
            .get(event_type)
            .map(|list| list.iter().map(|(_, h)| h.clone()).collect())
            .unwrap_or_default()
    }

    pub(super) fn emit_own(&self, event: AgentHarnessOwnEvent, signal: Option<&AbortSignal>) {
        let event = AgentHarnessEvent::Own(Box::new(event));
        for listener in self.subscriber_list() {
            listener(&event, signal);
        }
    }

    pub(super) fn emit_any(&self, event: AgentEvent, signal: Option<&AbortSignal>) {
        let event = AgentHarnessEvent::Loop(event);
        for listener in self.subscriber_list() {
            listener(&event, signal);
        }
    }

    pub(super) fn emit_queue_update(&self) -> Result<(), AgentHarnessError> {
        // Subscribers are infallible in the port, so this cannot fail; the
        // `Result` mirrors pi's signature (and lets in-loop drains match pi's
        // unshift-on-error shape structurally).
        self.emit_own(
            AgentHarnessOwnEvent::QueueUpdate(self.queue_snapshot()),
            None,
        );
        Ok(())
    }

    pub(super) fn emit_hook(
        &self,
        event: AgentHarnessOwnEvent,
    ) -> Result<Option<AgentHarnessEventResult>, AgentHarnessError> {
        let handlers = self.on_list(event.type_str());
        if handlers.is_empty() {
            return Ok(None);
        }
        let mut last = None;
        for handler in handlers {
            match handler(&event) {
                Ok(Some(result)) => last = Some(result),
                Ok(None) => {}
                Err(message) => return Err(normalize_hook_error(message)),
            }
        }
        Ok(last)
    }

    pub(super) fn emit_before_agent_start(
        &self,
        prompt: &str,
        images: Option<&[Value]>,
        turn_state: &TurnState,
    ) -> Result<Option<BeforeAgentStartResult>, AgentHarnessError> {
        let event = AgentHarnessOwnEvent::BeforeAgentStart(BeforeAgentStartEvent {
            prompt: prompt.to_string(),
            images: images.map(|i| i.to_vec()),
            system_prompt: turn_state.system_prompt.clone(),
            resources: self.get_resources(),
        });
        match self.emit_hook(event)? {
            Some(AgentHarnessEventResult::BeforeAgentStart(result)) => Ok(result),
            _ => Ok(None),
        }
    }

    pub(super) fn emit_before_provider_request(
        &self,
        model: &Model,
        session_id: &str,
        stream_options: AgentHarnessStreamOptions,
    ) -> Result<AgentHarnessStreamOptions, AgentHarnessError> {
        let handlers = self.on_list("before_provider_request");
        let mut current = clone_stream_options(&stream_options);
        if handlers.is_empty() {
            return Ok(current);
        }
        for handler in handlers {
            let event = AgentHarnessOwnEvent::BeforeProviderRequest(BeforeProviderRequestEvent {
                model: model.clone(),
                session_id: session_id.to_string(),
                stream_options: clone_stream_options(&current),
            });
            match handler(&event) {
                Ok(Some(AgentHarnessEventResult::BeforeProviderRequest(Some(
                    BeforeProviderRequestResult { stream_options },
                )))) => {
                    if let Some(patch) = stream_options {
                        current = apply_stream_options_patch(&current, Some(&patch));
                    }
                }
                Ok(_) => {}
                Err(message) => return Err(normalize_hook_error(message)),
            }
        }
        Ok(current)
    }

    pub(super) fn emit_before_provider_payload(
        &self,
        model: &Model,
        payload: Value,
    ) -> Result<Value, AgentHarnessError> {
        let handlers = self.on_list("before_provider_payload");
        let mut current = payload;
        if handlers.is_empty() {
            return Ok(current);
        }
        for handler in handlers {
            let event = AgentHarnessOwnEvent::BeforeProviderPayload(BeforeProviderPayloadEvent {
                model: model.clone(),
                payload: current.clone(),
            });
            match handler(&event) {
                Ok(Some(AgentHarnessEventResult::BeforeProviderPayload(Some(
                    BeforeProviderPayloadResult { payload },
                )))) => current = payload,
                Ok(_) => {}
                Err(message) => return Err(normalize_hook_error(message)),
            }
        }
        Ok(current)
    }

    // -- Turn-state construction (`agent-harness.ts:314`, `348`).

    pub(super) fn create_turn_state(&self) -> Result<TurnState, AgentHarnessError> {
        let context = self.session.build_context().map_err(session_error)?;
        let resources = self.get_resources();
        let metadata = self.session.get_metadata();
        let model = self.model.borrow().clone();
        let thinking_level = self.thinking_level.get();
        let active_tools = {
            let tools = self.tools.borrow();
            self.active_tool_names
                .borrow()
                .iter()
                .filter_map(|n| tools.iter().find(|t| &t.name == n).cloned())
                .collect::<Vec<_>>()
        };
        let system_prompt = match &self.system_prompt {
            Some(SystemPromptSource::Static(s)) => s.clone(),
            Some(SystemPromptSource::Dynamic(f)) => f(SystemPromptContext {
                env: self.env.as_ref(),
                session: &self.session,
                model: &model,
                thinking_level,
                active_tools: &active_tools,
                resources: &resources,
            }),
            None => "You are a helpful assistant.".to_string(),
        };
        Ok(TurnState {
            messages: context.messages,
            stream_options: clone_stream_options(&self.stream_options.borrow()),
            session_id: metadata.id,
            system_prompt,
            model,
            thinking_level,
            active_tools,
        })
    }

    pub(super) fn create_context(
        &self,
        turn_state: &TurnState,
        system_prompt: Option<&str>,
    ) -> AgentContext {
        AgentContext {
            system_prompt: system_prompt
                .map(str::to_string)
                .unwrap_or_else(|| turn_state.system_prompt.clone()),
            messages: turn_state.messages.clone(),
            tools: Some(turn_state.active_tools.clone()),
        }
    }

    // -- Pending session writes (`agent-harness.ts:462`).

    pub(super) fn flush_pending_session_writes(&self) -> Result<(), AgentHarnessError> {
        loop {
            let write = {
                let mut queue = self.pending_session_writes.borrow_mut();
                if queue.is_empty() {
                    return Ok(());
                }
                queue.remove(0)
            };
            self.apply_pending_write(write).map_err(session_error)?;
        }
    }

    pub(super) fn apply_pending_write(
        &self,
        write: PendingSessionWrite,
    ) -> Result<(), SessionError> {
        match write {
            PendingSessionWrite::Message(PendingMessage { message }) => {
                self.session.append_message(message)?;
            }
            PendingSessionWrite::ModelChange(PendingModelChange { provider, model_id }) => {
                self.session.append_model_change(&provider, &model_id)?;
            }
            PendingSessionWrite::ThinkingLevelChange(PendingThinkingLevelChange {
                thinking_level,
            }) => {
                self.session.append_thinking_level_change(&thinking_level)?;
            }
            PendingSessionWrite::ActiveToolsChange(PendingActiveToolsChange {
                active_tool_names,
            }) => {
                self.session.append_active_tools_change(active_tool_names)?;
            }
            PendingSessionWrite::Custom(PendingCustom { custom_type, data }) => {
                self.session.append_custom_entry(&custom_type, data)?;
            }
            PendingSessionWrite::CustomMessage(PendingCustomMessage {
                custom_type,
                content,
                display,
                details,
            }) => {
                self.session.append_custom_message_entry(
                    &custom_type,
                    content,
                    display,
                    details,
                )?;
            }
            PendingSessionWrite::Label(PendingLabel { target_id, label }) => {
                self.session.append_label(&target_id, label.as_deref())?;
            }
            PendingSessionWrite::SessionInfo(PendingSessionInfo { name }) => {
                self.session
                    .append_session_name(name.as_deref().unwrap_or(""))?;
            }
            PendingSessionWrite::Leaf(PendingLeaf { target_id }) => {
                self.session
                    .get_storage()
                    .set_leaf_id(target_id.as_deref())?;
            }
            // Compaction/branch-summary pending writes are produced only by the
            // compaction/tree drivers, which append directly; they are not
            // enqueued here (pi's `flushPendingSessionWrites` likewise never sees
            // them from the turn path).
            PendingSessionWrite::Compaction(_) | PendingSessionWrite::BranchSummary(_) => {}
        }
        Ok(())
    }

    // -- Agent-event handling (`agent-harness.ts:488`).

    pub(super) fn handle_agent_event(&self, event: AgentEvent, signal: Option<&AbortSignal>) {
        if self.suppress.get() {
            return;
        }
        match &event {
            AgentEvent::MessageEnd { message } => {
                if let Err(error) = self.session.append_message(message.clone()) {
                    self.record_run_error(session_error(error));
                    return;
                }
                self.emit_any(event, signal);
            }
            AgentEvent::TurnEnd { .. } => {
                self.emit_any(event, signal);
                let had_pending = !self.pending_session_writes.borrow().is_empty();
                if let Err(error) = self.flush_pending_session_writes() {
                    self.record_run_error(error);
                    return;
                }
                self.emit_own(
                    AgentHarnessOwnEvent::SavePoint(SavePointEvent {
                        had_pending_mutations: had_pending,
                    }),
                    signal,
                );
            }
            AgentEvent::AgentEnd { .. } => {
                if let Err(error) = self.flush_pending_session_writes() {
                    self.record_run_error(error);
                    return;
                }
                self.phase.set(AgentHarnessPhase::Idle);
                self.emit_any(event, signal);
                let next_turn_count = self.next_turn_queue.borrow().len() as i64;
                self.emit_own(
                    AgentHarnessOwnEvent::Settled(SettledEvent { next_turn_count }),
                    signal,
                );
            }
            _ => self.emit_any(event, signal),
        }
    }

    /// Synthesize and emit a failure run (pi's `emitRunFailure`,
    /// `agent-harness.ts:517`).
    pub(super) fn drive_run_failure(&self, failure: AgentMessage, signal: &AbortSignal) {
        let signal = Some(signal);
        self.handle_agent_event(
            AgentEvent::MessageStart {
                message: failure.clone(),
            },
            signal,
        );
        self.handle_agent_event(
            AgentEvent::MessageEnd {
                message: failure.clone(),
            },
            signal,
        );
        self.handle_agent_event(
            AgentEvent::TurnEnd {
                message: failure.clone(),
                tool_results: Vec::new(),
            },
            signal,
        );
        self.handle_agent_event(
            AgentEvent::AgentEnd {
                messages: vec![failure],
            },
            signal,
        );
    }

    // -- Loop wiring (`agent-harness.ts:359`, `399`, `sink`).

    pub(super) fn make_sink(self: &Rc<Self>, signal: &AbortSignal) -> AgentEventSink {
        let bridge = SendSync(self.clone());
        let signal = SendSync(signal.clone());
        Arc::new(move |event: AgentEvent| {
            bridge.get().handle_agent_event(event, Some(signal.get()));
        })
    }

    pub(super) fn make_stream_fn(self: &Rc<Self>) -> StreamFn {
        let bridge = SendSync(self.clone());
        Arc::new(move |model, context, _options, signal| {
            bridge.get().do_stream(model, context, signal)
        })
    }

    pub(super) fn make_loop_config(self: &Rc<Self>) -> AgentLoopConfig {
        let turn_state = self
            .active_turn_state
            .borrow()
            .clone()
            .expect("turn state set");
        let model = turn_state.model.clone();
        let reasoning = if turn_state.thinking_level == ThinkingLevel::Off {
            None
        } else {
            Some(turn_state.thinking_level)
        };

        let transform_bridge = SendSync(self.clone());
        let before_bridge = SendSync(self.clone());
        let after_bridge = SendSync(self.clone());
        let prepare_bridge = SendSync(self.clone());
        let steer_bridge = SendSync(self.clone());
        let follow_bridge = SendSync(self.clone());

        AgentLoopConfig {
            stream_options: pidgin_ai::StreamOptions::default(),
            reasoning,
            model,
            convert_to_llm: Arc::new(|messages: &[AgentMessage]| convert_to_llm(messages)),
            transform_context: Some(Arc::new(move |messages: &[AgentMessage], _signal| {
                transform_bridge.get().hook_transform_context(messages)
            })),
            get_api_key: None,
            should_stop_after_turn: None,
            prepare_next_turn: Some(Arc::new(move |ctx: &PrepareNextTurnContext| {
                prepare_bridge.get().hook_prepare_next_turn(ctx)
            })),
            get_steering_messages: Some(Arc::new(move || {
                steer_bridge.get().drain_queued(QueueKind::Steer)
            })),
            get_follow_up_messages: Some(Arc::new(move || {
                follow_bridge.get().drain_queued(QueueKind::FollowUp)
            })),
            tool_execution: None,
            before_tool_call: Some(Arc::new(move |ctx: &mut BeforeToolCallContext, _signal| {
                before_bridge.get().hook_before_tool_call(ctx)
            })),
            after_tool_call: Some(Arc::new(move |ctx: &AfterToolCallContext, _signal| {
                after_bridge.get().hook_after_tool_call(ctx)
            })),
        }
    }

    // -- Config hook bodies ------------------------------------------------

    pub(super) fn hook_transform_context(&self, messages: &[AgentMessage]) -> Vec<AgentMessage> {
        let event = AgentHarnessOwnEvent::Context(ContextEvent {
            messages: messages.to_vec(),
        });
        match self.emit_hook(event) {
            Ok(Some(AgentHarnessEventResult::Context(Some(ContextResult { messages })))) => {
                messages
            }
            Ok(_) => messages.to_vec(),
            Err(error) => {
                self.record_run_error(error);
                messages.to_vec()
            }
        }
    }

    pub(super) fn hook_before_tool_call(
        &self,
        ctx: &BeforeToolCallContext,
    ) -> Option<BeforeToolCallResult> {
        let event = AgentHarnessOwnEvent::ToolCall(ToolCallEvent {
            tool_call_id: ctx.tool_call.id.clone(),
            tool_name: ctx.tool_call.name.clone(),
            input: value_to_map(&ctx.args),
        });
        match self.emit_hook(event) {
            Ok(Some(AgentHarnessEventResult::ToolCall(Some(ToolCallResult { block, reason })))) => {
                Some(BeforeToolCallResult { block, reason })
            }
            Ok(_) => None,
            Err(error) => {
                self.record_run_error(error);
                None
            }
        }
    }

    pub(super) fn hook_after_tool_call(
        &self,
        ctx: &AfterToolCallContext,
    ) -> Option<AfterToolCallResult> {
        let content: Vec<Value> = ctx
            .result
            .content
            .iter()
            .map(|block| serde_json::to_value(block).unwrap_or(Value::Null))
            .collect();
        let event = AgentHarnessOwnEvent::ToolResult(ToolResultEvent {
            tool_call_id: ctx.tool_call.id.clone(),
            tool_name: ctx.tool_call.name.clone(),
            input: value_to_map(&ctx.args),
            content,
            details: ctx.result.details.clone(),
            is_error: ctx.is_error,
        });
        match self.emit_hook(event) {
            Ok(Some(AgentHarnessEventResult::ToolResult(Some(patch)))) => {
                Some(patch_to_after_tool_call(patch))
            }
            Ok(_) => None,
            Err(error) => {
                self.record_run_error(error);
                None
            }
        }
    }

    pub(super) fn hook_prepare_next_turn(
        &self,
        _ctx: &PrepareNextTurnContext,
    ) -> Option<AgentLoopTurnUpdate> {
        if let Err(error) = self.flush_pending_session_writes() {
            self.record_run_error(error);
            return None;
        }
        match self.create_turn_state() {
            Ok(turn_state) => {
                let context = self.create_context(&turn_state, None);
                let model = turn_state.model.clone();
                let thinking_level = turn_state.thinking_level;
                *self.active_turn_state.borrow_mut() = Some(turn_state);
                Some(AgentLoopTurnUpdate {
                    context: Some(context),
                    model: Some(model),
                    thinking_level: Some(thinking_level),
                })
            }
            Err(error) => {
                self.record_run_error(error);
                None
            }
        }
    }

    pub(super) fn drain_queued(&self, kind: QueueKind) -> Vec<AgentMessage> {
        let (queue, mode) = match kind {
            QueueKind::Steer => (&self.steer_queue, self.steering_mode.get()),
            QueueKind::FollowUp => (&self.follow_up_queue, self.follow_up_mode.get()),
        };
        let drained: Vec<AgentMessage> = {
            let mut queue = queue.borrow_mut();
            match mode {
                QueueMode::All => queue.drain(..).collect(),
                QueueMode::OneAtATime => {
                    if queue.is_empty() {
                        Vec::new()
                    } else {
                        vec![queue.remove(0)]
                    }
                }
            }
        };
        if drained.is_empty() {
            return drained;
        }
        if let Err(error) = self.emit_queue_update() {
            queue.borrow_mut().splice(0..0, drained);
            self.record_run_error(error);
            return Vec::new();
        }
        drained
    }

    // -- Provider streaming (`agent-harness.ts:359`).

    pub(super) fn do_stream(
        &self,
        model: &Model,
        context: &Context,
        signal: Option<&AbortSignal>,
    ) -> StreamResult {
        // A hook/session error recorded before streaming means pi never reaches
        // `models.streamSimple`; short-circuit with a terminal aborted message so
        // the loop winds down and no provider response is consumed.
        if self.suppress.get() {
            return StreamResult {
                events: Vec::new(),
                message: aborted_assistant_message(model),
            };
        }
        let (snapshot, session_id, reasoning) = {
            let turn_state = self.active_turn_state.borrow();
            let turn_state = turn_state.as_ref().expect("turn state set");
            let reasoning = if turn_state.thinking_level == ThinkingLevel::Off {
                None
            } else {
                Some(turn_state.thinking_level)
            };
            (
                clone_stream_options(&turn_state.stream_options),
                turn_state.session_id.clone(),
                reasoning,
            )
        };
        let request_options =
            match self.emit_before_provider_request(model, &session_id, snapshot.clone()) {
                Ok(options) => options,
                Err(error) => {
                    self.record_run_error(error);
                    snapshot
                }
            };

        let on_payload = |payload: Value| -> Value {
            match self.emit_before_provider_payload(model, payload.clone()) {
                Ok(next) => next,
                Err(error) => {
                    self.record_run_error(error);
                    payload
                }
            }
        };
        let on_response = |status: i64, headers: BTreeMap<String, String>| {
            self.emit_own(
                AgentHarnessOwnEvent::AfterProviderResponse(AfterProviderResponseEvent {
                    status,
                    headers,
                }),
                None,
            );
        };

        let stream = self.stream.clone();
        stream(ProviderStreamRequest {
            model,
            context,
            session_id: &session_id,
            reasoning,
            options: &request_options,
            signal,
            on_payload: &on_payload,
            on_response: &on_response,
        })
    }

    // -- Compaction driver (`agent-harness.ts:686`).

    pub(super) fn do_compact(
        &self,
        custom_instructions: Option<&str>,
    ) -> Result<crate::harness::events::CompactResult, AgentHarnessError> {
        let model = self.model.borrow().clone();
        let branch_entries = self.session.get_branch(None).map_err(session_error)?;
        let preparation = prepare_compaction(&branch_entries, &DEFAULT_COMPACTION_SETTINGS)
            .map_err(|e| AgentHarnessError::new(AgentHarnessErrorCode::Compaction, e.message))?
            .ok_or_else(|| {
                AgentHarnessError::new(AgentHarnessErrorCode::Compaction, "Nothing to compact")
            })?;

        let hook = {
            let event = AgentHarnessOwnEvent::SessionBeforeCompact(SessionBeforeCompactEvent {
                preparation: preparation.clone(),
                branch_entries: branch_entries.clone(),
                custom_instructions: custom_instructions.map(str::to_string),
                signal: AbortSignal::new(),
            });
            match self.emit_hook(event)? {
                Some(AgentHarnessEventResult::SessionBeforeCompact(result)) => result,
                _ => None,
            }
        };
        if let Some(SessionBeforeCompactResult {
            cancel: Some(true), ..
        }) = &hook
        {
            return Err(AgentHarnessError::new(
                AgentHarnessErrorCode::Compaction,
                "Compaction cancelled",
            ));
        }
        let provided = hook.and_then(|h| h.compaction);

        let result = match &provided {
            Some(result) => result.clone(),
            None => {
                let compaction = compact(
                    &preparation,
                    self.models.as_ref(),
                    &model,
                    custom_instructions,
                    None,
                    Some(thinking_level_str(self.thinking_level.get())),
                )
                .map_err(|e| {
                    AgentHarnessError::new(AgentHarnessErrorCode::Compaction, e.message)
                })?;
                crate::harness::events::CompactResult {
                    summary: compaction.summary,
                    first_kept_entry_id: compaction.first_kept_entry_id,
                    tokens_before: compaction.tokens_before,
                    details: compaction.details.map(
                        |d| json!({ "readFiles": d.read_files, "modifiedFiles": d.modified_files }),
                    ),
                }
            }
        };

        let entry_id = self
            .session
            .append_compaction(
                &result.summary,
                &result.first_kept_entry_id,
                result.tokens_before,
                result.details.clone(),
                Some(provided.is_some()),
            )
            .map_err(session_error)?;
        if let Some(SessionTreeEntry::Compaction(entry)) = self.session.get_entry(&entry_id) {
            self.emit_own(
                AgentHarnessOwnEvent::SessionCompact(SessionCompactEvent {
                    compaction_entry: entry,
                    from_hook: provided.is_some(),
                }),
                None,
            );
        }
        Ok(result)
    }

    // -- Tree-navigation driver (`agent-harness.ts:732`).

    pub(super) fn do_navigate_tree(
        &self,
        target_id: &str,
        options: NavigateTreeOptions,
    ) -> Result<NavigateTreeResult, AgentHarnessError> {
        let old_leaf_id = self.session.get_leaf_id().map_err(session_error)?;
        if old_leaf_id.as_deref() == Some(target_id) {
            return Ok(NavigateTreeResult {
                cancelled: false,
                editor_text: None,
                summary_entry: None,
            });
        }
        let target_entry = self.session.get_entry(target_id).ok_or_else(|| {
            AgentHarnessError::new(
                AgentHarnessErrorCode::InvalidArgument,
                format!("Entry {target_id} not found"),
            )
        })?;
        let collected =
            collect_entries_for_branch_summary(&self.session, old_leaf_id.as_deref(), target_id)
                .map_err(session_error)?;
        let entries = collected.entries;
        let common_ancestor_id = collected.common_ancestor_id;

        let preparation = TreePreparation {
            target_id: target_id.to_string(),
            old_leaf_id: old_leaf_id.clone(),
            common_ancestor_id,
            entries_to_summarize: entries.clone(),
            user_wants_summary: options.summarize,
            custom_instructions: options.custom_instructions.clone(),
            replace_instructions: options.replace_instructions,
            label: options.label.clone(),
        };
        let hook = {
            let event = AgentHarnessOwnEvent::SessionBeforeTree(SessionBeforeTreeEvent {
                preparation,
                signal: AbortSignal::new(),
            });
            match self.emit_hook(event)? {
                Some(AgentHarnessEventResult::SessionBeforeTree(result)) => result,
                _ => None,
            }
        };
        if let Some(SessionBeforeTreeResult {
            cancel: Some(true), ..
        }) = &hook
        {
            return Ok(NavigateTreeResult {
                cancelled: true,
                editor_text: None,
                summary_entry: None,
            });
        }

        let mut summary_text: Option<String> = hook
            .as_ref()
            .and_then(|h| h.summary.as_ref())
            .map(|s| s.summary.clone());
        let mut summary_details: Option<Value> = hook
            .as_ref()
            .and_then(|h| h.summary.as_ref())
            .and_then(|s| s.details.clone());
        let summary_from_hook = hook.as_ref().and_then(|h| h.summary.as_ref()).is_some();

        if summary_text.is_none() && options.summarize && !entries.is_empty() {
            let model = self.model.borrow().clone();
            let custom = hook
                .as_ref()
                .and_then(|h| h.custom_instructions.clone())
                .or_else(|| options.custom_instructions.clone());
            let replace = hook
                .as_ref()
                .and_then(|h| h.replace_instructions)
                .or(options.replace_instructions)
                .unwrap_or(false);
            let branch = generate_branch_summary(
                &entries,
                &GenerateBranchSummaryOptions {
                    models: self.models.as_ref(),
                    model: &model,
                    signal: AbortSignal::new(),
                    custom_instructions: custom,
                    replace_instructions: replace,
                    reserve_tokens: None,
                },
            );
            match branch {
                Ok(summary) => {
                    summary_text = Some(summary.summary);
                    summary_details = Some(json!({
                        "readFiles": summary.read_files,
                        "modifiedFiles": summary.modified_files,
                    }));
                }
                Err(error) => {
                    if error.code == crate::harness::compaction::BranchSummaryErrorCode::Aborted {
                        return Ok(NavigateTreeResult {
                            cancelled: true,
                            editor_text: None,
                            summary_entry: None,
                        });
                    }
                    return Err(AgentHarnessError::new(
                        AgentHarnessErrorCode::BranchSummary,
                        error.message,
                    ));
                }
            }
        }

        let (new_leaf_id, editor_text) = self.navigate_target(&target_entry, target_id);

        let summary = summary_text.map(|summary| MoveSummary {
            summary,
            details: summary_details,
            from_hook: Some(summary_from_hook),
        });
        let summary_id = self
            .session
            .move_to(new_leaf_id.as_deref(), summary)
            .map_err(session_error)?;
        let mut summary_entry: Option<BranchSummaryEntry> = None;
        if let Some(id) = summary_id {
            if let Some(SessionTreeEntry::BranchSummary(entry)) = self.session.get_entry(&id) {
                summary_entry = Some(entry);
            }
        }
        let new_leaf = self.session.get_leaf_id().map_err(session_error)?;
        self.emit_own(
            AgentHarnessOwnEvent::SessionTree(SessionTreeEvent {
                new_leaf_id: new_leaf,
                old_leaf_id,
                summary_entry: summary_entry.clone(),
                from_hook: Some(summary_from_hook),
            }),
            None,
        );
        Ok(NavigateTreeResult {
            cancelled: false,
            editor_text,
            summary_entry,
        })
    }

    /// Resolve the new leaf and editor text for a navigation target
    /// (pi's `navigateTree` branch on `targetEntry.type`).
    pub(super) fn navigate_target(
        &self,
        target_entry: &SessionTreeEntry,
        target_id: &str,
    ) -> (Option<String>, Option<String>) {
        match target_entry {
            SessionTreeEntry::Message(entry)
                if entry.message.get("role").and_then(Value::as_str) == Some("user") =>
            {
                let text = entry
                    .message
                    .get("content")
                    .map(content_text)
                    .unwrap_or_default();
                (entry.parent_id.clone(), Some(text))
            }
            SessionTreeEntry::CustomMessage(entry) => {
                let text = content_text(&entry.content);
                (entry.parent_id.clone(), Some(text))
            }
            _ => (Some(target_id.to_string()), None),
        }
    }
}

/// Which queue [`HarnessInner::drain_queued`] drains.
pub(super) enum QueueKind {
    Steer,
    FollowUp,
}

/// Convert an arguments [`Value`] object to the `Map` the tool events carry.
pub(super) fn value_to_map(args: &Value) -> Map<String, Value> {
    args.as_object().cloned().unwrap_or_default()
}

/// A minimal terminal `aborted` assistant message, used by [`HarnessInner::do_stream`]
/// to wind down the loop after a pre-stream hook error without touching the
/// provider.
pub(super) fn aborted_assistant_message(model: &Model) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: UsageCost::default(),
        },
        stop_reason: StopReason::Aborted,
        error_message: None,
        timestamp: 0,
    }
}

/// Convert a harness `tool_result` patch to the loop's [`AfterToolCallResult`]
/// (pi's `afterToolCall` return mapping).
pub(super) fn patch_to_after_tool_call(patch: ToolResultPatch) -> AfterToolCallResult {
    AfterToolCallResult {
        content: patch.content.map(|blocks| {
            blocks
                .into_iter()
                .filter_map(|v| serde_json::from_value::<ContentBlock>(v).ok())
                .collect()
        }),
        details: patch.details,
        is_error: patch.is_error,
        terminate: patch.terminate,
    }
}
