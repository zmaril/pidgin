//! Agent tool-call / tool-result and next-turn hooks, ported from pi's
//! `AgentSession._installAgentToolHooks` and `_installAgentNextTurnRefresh`
//! (`packages/coding-agent/src/core/agent-session.ts:449-518`).
//!
//! pi installs these in the `AgentSession` constructor, right after it subscribes
//! its internal agent-event handler. They bridge the low-level agent loop's
//! `beforeToolCall` / `afterToolCall` / `prepareNextTurnWithContext` hooks to the
//! [`ExtensionRunner`] so extensions can block a tool call, replace a tool
//! result, and so each next turn re-reads the live system prompt / tools / model /
//! thinking level.
//!
//! The closures are built as `Send + Sync + 'static` so they satisfy the agent's
//! hook alias types. They capture only `Arc` handles (the runner and the two
//! system-prompt cells) — never the `!Send` [`AgentSession`] itself.

use std::sync::{Arc, Mutex};

use serde_json::Value;

use pidgin_agent::agent::Agent;
use pidgin_agent::types::{
    AfterToolCallContext, AfterToolCallResult, AgentLoopTurnUpdate, BeforeToolCallContext,
    BeforeToolCallResult, PrepareNextTurnContext,
};
use pidgin_ai::seams::AbortSignal;
use pidgin_ai::ContentBlock;

use crate::core::extensions::events::tool::{ToolCallEvent, ToolResultEvent};
use crate::core::extensions::runner::ExtensionRunner;

/// Install the tool-call / tool-result bridges on `agent` (pi's
/// `_installAgentToolHooks`, `agent-session.ts:449`).
///
/// `beforeToolCall` builds a [`ToolCallEvent`] from the validated call and asks
/// the runner whether to block it; `afterToolCall` builds a [`ToolResultEvent`]
/// from the executed result and applies any replacement the runner returns. Both
/// are gated on `has_handlers` so a runner with no `tool_call` / `tool_result`
/// handlers keeps pi's `return undefined` no-op behavior.
pub(super) fn install_agent_tool_hooks(agent: &Agent, extension_runner: &Arc<dyn ExtensionRunner>) {
    let runner = Arc::clone(extension_runner);
    agent.set_before_tool_call(Some(Arc::new(
        move |ctx: &mut BeforeToolCallContext, _signal: Option<&AbortSignal>| {
            if !runner.has_handlers("tool_call") {
                return None;
            }
            let event = ToolCallEvent {
                tool_call_id: ctx.tool_call.id.clone(),
                tool_name: ctx.tool_call.name.clone(),
                input: ctx.args.clone(),
            };
            let result = runner.emit_tool_call(&event)?;
            Some(BeforeToolCallResult {
                block: result.block,
                reason: result.reason,
            })
        },
    )));

    let runner = Arc::clone(extension_runner);
    agent.set_after_tool_call(Some(Arc::new(
        move |ctx: &AfterToolCallContext, _signal: Option<&AbortSignal>| {
            if !runner.has_handlers("tool_result") {
                return None;
            }
            let content: Vec<Value> = ctx
                .result
                .content
                .iter()
                .map(|block| serde_json::to_value(block).unwrap_or(Value::Null))
                .collect();
            let event = ToolResultEvent {
                tool_call_id: ctx.tool_call.id.clone(),
                tool_name: ctx.tool_call.name.clone(),
                input: ctx.args.clone(),
                content,
                is_error: ctx.is_error,
                details: ctx.result.details.clone(),
            };
            let hook_result = runner.emit_tool_result(&event)?;
            // pi: `isError: hookResult.isError ?? isError`; `content` / `details`
            // pass straight through (`undefined` keeps the executed value, which
            // the loop honors via `unwrap_or`).
            let content = hook_result.content.map(|blocks| {
                blocks
                    .into_iter()
                    .map(|value| serde_json::from_value(value).unwrap_or(ContentBlock::Unknown))
                    .collect::<Vec<ContentBlock>>()
            });
            Some(AfterToolCallResult {
                content,
                details: hook_result.details,
                is_error: Some(hook_result.is_error.unwrap_or(ctx.is_error)),
                terminate: None,
            })
        },
    )));
}

/// Install the next-turn context refresh on `agent` (pi's
/// `_installAgentNextTurnRefresh`, `agent-session.ts:499`).
///
/// Before each subsequent provider request the loop calls this hook, which
/// re-reads the live system prompt (override, else base), tools, model, and
/// thinking level from the session/agent so mid-run changes take effect on the
/// next turn.
pub(super) fn install_agent_next_turn_refresh(
    agent: &Agent,
    base_system_prompt: &Arc<Mutex<String>>,
    system_prompt_override: &Arc<Mutex<Option<String>>>,
) {
    // pi merges its refresh over any previously-installed `prepareNextTurn` /
    // `prepareNextTurnWithContext` hook. AgentSession installs no such hook
    // elsewhere, and the Agent exposes no getter to read an existing one, so there
    // is no previous snapshot to chain — this refresh is the sole producer.
    // unit5: previous-hook chaining is intentionally omitted for that reason; if a
    // future slice installs another prepare hook it must be composed here.
    let hook_agent = agent.clone();
    let base = Arc::clone(base_system_prompt);
    let override_prompt = Arc::clone(system_prompt_override);
    agent.set_prepare_next_turn_with_context(Some(Arc::new(
        move |turn: &PrepareNextTurnContext, _signal: Option<&AbortSignal>| {
            let system_prompt = override_prompt
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| base.lock().unwrap().clone());
            let mut context = turn.context.clone();
            context.system_prompt = system_prompt;
            context.tools = Some(hook_agent.tools());
            Some(AgentLoopTurnUpdate {
                context: Some(context),
                model: Some(hook_agent.model()),
                thinking_level: Some(hook_agent.thinking_level()),
            })
        },
    )));
}
