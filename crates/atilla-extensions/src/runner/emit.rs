//! The `emitXxx` dispatch methods — the faithful port of pi's `runner.ts`
//! emitters, one per acceptance-asserted hook.
//!
//! Each emitter follows the same shape as its `runner.ts` counterpart: seed a
//! shaping fold from the initial value, then for each registered handler (in
//! order) build the JSON event from the current fold state, invoke the handler
//! over the rendezvous, isolate a throw into an `onError` record and continue,
//! and fold the result in. The fold decides the accumulated outcome and (for
//! `input`) whether to short-circuit.
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/runner.ts`.

// straitjacket-allow-file:duplication -- each emitter is a deliberate parallel
// mirror of one emitXxx method in runner.ts (seed fold, loop handlers, build
// event, invoke, isolate error, fold result); the shape recurs per hook, so it
// is faithful-port duplication, not an accident to hoist away.

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};

use atilla_coding::core::extensions::dispatch::{
    next_headers, BeforeAgentStartCombinedResult, BeforeAgentStartFold, ContextFold, InputFold,
    ToolResultFold,
};
use atilla_coding::core::extensions::events::{
    BeforeAgentStartEventResult, ContextEventResult, InputEventResult, InputSource,
    StreamingBehavior, ToolResultEvent, ToolResultEventResult,
};
use atilla_coding::core::extensions::hook::HookEvent;

use super::ExtensionRunner;

/// Deserialize a handler's JSON return value into the typed result, treating a
/// `null` (the JS `undefined` a handler returns) and any malformed value as "no
/// result" — pi's `result?.…` optional access.
fn parse_result<T: DeserializeOwned>(value: &Value) -> Option<T> {
    if value.is_null() {
        return None;
    }
    serde_json::from_value(value.clone()).ok()
}

impl ExtensionRunner {
    /// `emitInput` (`runner.ts:1174`): transforms chain across handlers and a
    /// `handled` result short-circuits the rest.
    pub async fn emit_input(
        &self,
        text: &str,
        images: Option<Vec<Value>>,
        source: InputSource,
        streaming_behavior: Option<StreamingBehavior>,
    ) -> Result<InputEventResult> {
        let sites = self.sites(HookEvent::Input);
        let mut fold = InputFold::new(text, images);
        let ctx = self.context.to_json();

        for (index, extension_path) in sites.into_iter().enumerate() {
            let mut event = Map::new();
            event.insert("type".into(), json!("input"));
            event.insert("text".into(), json!(fold.current_text()));
            if let Some(images) = fold.current_images() {
                event.insert("images".into(), json!(images));
            }
            event.insert("source".into(), serde_json::to_value(source)?);
            if let Some(behavior) = streaming_behavior {
                event.insert("streamingBehavior".into(), serde_json::to_value(behavior)?);
            }

            let invocation = self
                .plane()
                .invoke_hook("input", index, &Value::Object(event), &ctx)
                .await?;
            if !invocation.ok {
                self.record_error("input", extension_path, invocation);
                continue;
            }
            let result: Option<InputEventResult> = parse_result(&invocation.result);
            if fold.apply(result) {
                break;
            }
        }

        Ok(fold.finish())
    }

    /// `emitBeforeAgentStart` (`runner.ts:1059`): the system prompt chains across
    /// handlers (each sees the running value via `ctx.getSystemPrompt()` and
    /// `event.systemPrompt`) and injected messages are collected in order.
    pub async fn emit_before_agent_start(
        &self,
        prompt: &str,
        images: Option<Vec<Value>>,
        system_prompt: &str,
        system_prompt_options: Value,
    ) -> Result<Option<BeforeAgentStartCombinedResult>> {
        let sites = self.sites(HookEvent::BeforeAgentStart);
        let mut fold = BeforeAgentStartFold::new(system_prompt);

        for (index, extension_path) in sites.into_iter().enumerate() {
            let mut event = Map::new();
            event.insert("type".into(), json!("before_agent_start"));
            event.insert("prompt".into(), json!(prompt));
            if let Some(images) = &images {
                event.insert("images".into(), json!(images));
            }
            event.insert("systemPrompt".into(), json!(fold.current_system_prompt()));
            event.insert("systemPromptOptions".into(), system_prompt_options.clone());
            // ctx.getSystemPrompt() must report the running (chained) prompt.
            let ctx = self
                .context
                .to_json_with_prompt(fold.current_system_prompt());

            let invocation = self
                .plane()
                .invoke_hook("before_agent_start", index, &Value::Object(event), &ctx)
                .await?;
            if !invocation.ok {
                self.record_error("before_agent_start", extension_path, invocation);
                continue;
            }
            let result: Option<BeforeAgentStartEventResult> = parse_result(&invocation.result);
            fold.apply(result);
        }

        Ok(fold.finish())
    }

    /// `emitToolResult` (`runner.ts:860`): each handler returns a partial patch;
    /// `content` / `details` / `isError` merge, so a later partial patch
    /// preserves earlier fields.
    pub async fn emit_tool_result(
        &self,
        event: ToolResultEvent,
    ) -> Result<Option<ToolResultEventResult>> {
        let ToolResultEvent {
            tool_call_id,
            tool_name,
            input,
            content,
            details,
            is_error,
        } = event;
        let sites = self.sites(HookEvent::ToolResult);
        let mut fold = ToolResultFold::new(content, details, is_error);
        let ctx = self.context.to_json();

        for (index, extension_path) in sites.into_iter().enumerate() {
            let event_json = json!({
                "type": "tool_result",
                "toolCallId": tool_call_id,
                "toolName": tool_name,
                "input": input,
                "content": fold.content(),
                "details": fold.details(),
                "isError": fold.is_error(),
            });

            let invocation = self
                .plane()
                .invoke_hook("tool_result", index, &event_json, &ctx)
                .await?;
            if !invocation.ok {
                self.record_error("tool_result", extension_path, invocation);
                continue;
            }
            fold.apply(parse_result(&invocation.result));
        }

        Ok(fold.finish())
    }

    /// `emitBeforeProviderHeaders` (`runner.ts:1028`): handlers mutate the headers
    /// object in place and their return value is ignored; the runner threads the
    /// mutated headers forward to the next handler.
    pub async fn emit_before_provider_headers(&self, headers: Value) -> Result<Value> {
        let sites = self.sites(HookEvent::BeforeProviderHeaders);
        let ctx = self.context.to_json();
        let mut current = headers;

        for (index, extension_path) in sites.into_iter().enumerate() {
            let event_json = json!({
                "type": "before_provider_headers",
                "headers": current,
            });

            let invocation = self
                .plane()
                .invoke_hook("before_provider_headers", index, &event_json, &ctx)
                .await?;
            if !invocation.ok {
                self.record_error("before_provider_headers", extension_path, invocation);
                continue;
            }
            // The handler mutated event.headers in place; carry them forward.
            if let Some(mutated) = invocation.event.get("headers") {
                current = next_headers(mutated.clone());
            }
        }

        Ok(current)
    }

    /// `emitContext` (`runner.ts:962`): each handler may replace the message
    /// array wholesale, and the next handler sees the replacement.
    pub async fn emit_context(&self, messages: Vec<Value>) -> Result<Vec<Value>> {
        let sites = self.sites(HookEvent::Context);
        let mut fold = ContextFold::new(messages);
        let ctx = self.context.to_json();

        for (index, extension_path) in sites.into_iter().enumerate() {
            let event_json = json!({
                "type": "context",
                "messages": fold.current_messages(),
            });

            let invocation = self
                .plane()
                .invoke_hook("context", index, &event_json, &ctx)
                .await?;
            if !invocation.ok {
                self.record_error("context", extension_path, invocation);
                continue;
            }
            let result: Option<ContextEventResult> = parse_result(&invocation.result);
            fold.apply(result);
        }

        Ok(fold.finish())
    }
}
