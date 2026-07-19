//! Port of pi's `core/tools/tool-definition-wrapper.ts`
//! (`vendor/pi/packages/coding-agent/src/core/tools/tool-definition-wrapper.ts`).
//!
//! [`wrap_tool_definition`] / [`wrap_tool_definitions`] adapt a coding-agent
//! [`ToolDefinition`] into the [`AgentTool`] shape the agent runtime consumes
//! (copying `name`/`label`/`description`/`parameters`/`prepare_arguments`/
//! `execution_mode` and wrapping `execute` to inject an [`ExtensionContext`] via
//! an optional `ctx_factory`), and [`create_tool_definition_from_agent_tool`]
//! synthesizes a minimal [`ToolDefinition`] back from a plain [`AgentTool`].
//!
//! # `ctx_factory` semantics
//!
//! pi's `wrapToolDefinition(definition, ctxFactory?)` calls
//! `definition.execute(…, ctxFactory?.() as ExtensionContext)`: when no factory
//! is supplied the context is `undefined`. pidgin's [`ToolDefinition::execute`]
//! takes a non-optional `&dyn ExtensionContext`, so the port passes a
//! [`DefaultExtensionContext`] no-op value in that case, matching pi's
//! "definition ignores the ctx when none is threaded" behavior.
//!
//! # `prepare_arguments` adaptation
//!
//! The two `prepare_arguments` shapes differ by borrow: pi's coding-agent
//! [`ToolDefinition`] takes the raw args **by value** ([`PrepareArguments`] =
//! `Arc<dyn Fn(Value) -> Value>`), while [`AgentTool`]'s takes them **by
//! reference** ([`pidgin_agent::types::PrepareArguments`] =
//! `Arc<dyn Fn(&Value) -> Value>`). The wrappers bridge the two by cloning at the
//! boundary, exactly where pi's structural-typed assignment would be a no-op.

use std::sync::Arc;

use serde_json::Value;

use pidgin_agent::types::{AgentTool, AgentToolExecute, PrepareArguments as AgentPrepareArguments};

use crate::core::extensions::types::{
    ExtensionContext, PrepareArguments, ToolDefinition, ToolDefinitionExecute,
};

/// A factory that produces the [`ExtensionContext`] threaded into a wrapped
/// tool's `execute` (pi's `ctxFactory?: () => ExtensionContext`).
///
/// Returns an owned boxed context each call so the wrapper can hand a borrow to
/// [`ToolDefinition::execute`], mirroring pi's fresh `ctxFactory()` per call.
pub type CtxFactory = Arc<dyn Fn() -> Box<dyn ExtensionContext> + Send + Sync>;

/// The no-op [`ExtensionContext`] used when [`wrap_tool_definition`] is called
/// without a `ctx_factory` (pi's `undefined as ExtensionContext`).
pub struct DefaultExtensionContext;

impl ExtensionContext for DefaultExtensionContext {}

/// Wrap a [`ToolDefinition`] into an [`AgentTool`] for the core runtime (pi's
/// `wrapToolDefinition`).
pub fn wrap_tool_definition(
    definition: ToolDefinition,
    ctx_factory: Option<CtxFactory>,
) -> AgentTool {
    let ToolDefinition {
        name,
        label,
        description,
        parameters,
        execution_mode,
        execute,
        prepare_arguments,
        // Prompt/render hints have no counterpart on `AgentTool`; dropped here as
        // pi's structural wrap simply does not copy them.
        prompt_snippet: _,
        prompt_guidelines: _,
        render_shell: _,
    } = definition;

    let definition_execute: ToolDefinitionExecute = execute;
    let execute: AgentToolExecute =
        Arc::new(
            move |tool_call_id, params, signal, on_update| match &ctx_factory {
                Some(factory) => {
                    let ctx = factory();
                    definition_execute(tool_call_id, params, signal, on_update, ctx.as_ref())
                }
                None => {
                    let ctx = DefaultExtensionContext;
                    definition_execute(tool_call_id, params, signal, on_update, &ctx)
                }
            },
        );

    AgentTool {
        name,
        description,
        parameters,
        label,
        prepare_arguments: prepare_arguments.map(adapt_prepare_to_agent),
        execute,
        execution_mode,
    }
}

/// Wrap multiple [`ToolDefinition`]s into [`AgentTool`]s (pi's
/// `wrapToolDefinitions`).
pub fn wrap_tool_definitions(
    definitions: Vec<ToolDefinition>,
    ctx_factory: Option<CtxFactory>,
) -> Vec<AgentTool> {
    definitions
        .into_iter()
        .map(|definition| wrap_tool_definition(definition, ctx_factory.clone()))
        .collect()
}

/// Synthesize a minimal [`ToolDefinition`] from an [`AgentTool`] (pi's
/// `createToolDefinitionFromAgentTool`).
///
/// The 5-arg [`ToolDefinition::execute`] ignores its injected
/// `&dyn ExtensionContext` and calls the 4-arg [`AgentTool`]'s `execute`.
/// Prompt/render fields default to `None`.
pub fn create_tool_definition_from_agent_tool(tool: AgentTool) -> ToolDefinition {
    let AgentTool {
        name,
        description,
        parameters,
        label,
        prepare_arguments,
        execute,
        execution_mode,
    } = tool;

    let tool_execute: AgentToolExecute = execute;
    let execute: ToolDefinitionExecute =
        Arc::new(move |tool_call_id, params, signal, on_update, _ctx| {
            tool_execute(tool_call_id, params, signal, on_update)
        });

    ToolDefinition {
        name,
        label,
        description,
        parameters,
        execution_mode,
        execute,
        prepare_arguments: prepare_arguments.map(adapt_prepare_to_definition),
        prompt_snippet: None,
        prompt_guidelines: None,
        render_shell: None,
    }
}

/// Adapt a by-value [`PrepareArguments`] into the by-reference
/// [`AgentPrepareArguments`] `AgentTool` expects (clones at the boundary).
fn adapt_prepare_to_agent(prepare: PrepareArguments) -> AgentPrepareArguments {
    Arc::new(move |value: &Value| prepare(value.clone()))
}

/// Adapt a by-reference [`AgentPrepareArguments`] into the by-value
/// [`PrepareArguments`] a [`ToolDefinition`] expects.
fn adapt_prepare_to_definition(prepare: AgentPrepareArguments) -> PrepareArguments {
    Arc::new(move |value: Value| prepare(&value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pidgin_agent::types::AgentToolResult;
    use pidgin_ai::ContentBlock;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// An [`ExtensionContext`] that records that it was constructed.
    struct MarkerCtx;
    impl ExtensionContext for MarkerCtx {}

    fn text_result(text: &str) -> AgentToolResult {
        AgentToolResult {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                text_signature: None,
            }],
            details: json!(null),
            added_tool_names: None,
            terminate: None,
        }
    }

    fn sample_definition() -> ToolDefinition {
        ToolDefinition {
            name: "sample".into(),
            label: "Sample".into(),
            description: "A sample tool".into(),
            parameters: json!({ "type": "object" }),
            execution_mode: None,
            execute: Arc::new(|id, args, _signal, _on_update, _ctx| {
                text_result(&format!("id={id} args={args}"))
            }),
            prepare_arguments: Some(Arc::new(|mut args| {
                if let Value::Object(map) = &mut args {
                    map.insert("prepared".into(), json!(true));
                }
                args
            })),
            prompt_snippet: Some("snippet".into()),
            prompt_guidelines: Some(vec!["guideline".into()]),
            render_shell: None,
        }
    }

    #[test]
    fn wrap_maps_metadata_one_to_one() {
        let def = sample_definition();
        let tool = wrap_tool_definition(def, None);
        assert_eq!(tool.name, "sample");
        assert_eq!(tool.label, "Sample");
        assert_eq!(tool.description, "A sample tool");
        assert_eq!(tool.parameters, json!({ "type": "object" }));
        assert!(tool.execution_mode.is_none());
        // prepare_arguments carries through (by-ref adapter).
        let prepared = (tool.prepare_arguments.as_ref().unwrap())(&json!({ "a": 1 }));
        assert_eq!(prepared, json!({ "a": 1, "prepared": true }));
    }

    #[test]
    fn wrap_executes_underlying_with_default_ctx() {
        let tool = wrap_tool_definition(sample_definition(), None);
        let result = (tool.execute)("call_1", &json!({ "x": 1 }), None, None);
        assert_eq!(
            result.content,
            vec![ContentBlock::Text {
                text: "id=call_1 args={\"x\":1}".into(),
                text_signature: None,
            }]
        );
    }

    #[test]
    fn wrap_invokes_ctx_factory_per_call() {
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();
        let factory: CtxFactory = Arc::new(move || {
            called_clone.store(true, Ordering::SeqCst);
            Box::new(MarkerCtx)
        });
        let tool = wrap_tool_definition(sample_definition(), Some(factory));
        let _ = (tool.execute)("call_2", &json!({}), None, None);
        assert!(called.load(Ordering::SeqCst), "ctx_factory was invoked");
    }

    #[test]
    fn wrap_tool_definitions_maps_all() {
        let defs = vec![sample_definition(), sample_definition()];
        let tools = wrap_tool_definitions(defs, None);
        assert_eq!(tools.len(), 2);
        assert!(tools.iter().all(|t| t.name == "sample"));
    }

    #[test]
    fn create_definition_from_agent_tool_reverses_mapping() {
        let executed = Arc::new(AtomicBool::new(false));
        let executed_clone = executed.clone();
        let tool = AgentTool {
            name: "agent".into(),
            description: "desc".into(),
            parameters: json!({ "type": "object" }),
            label: "Agent".into(),
            prepare_arguments: Some(Arc::new(|value: &Value| value.clone())),
            execute: Arc::new(move |_id, _args, _signal, _on_update| {
                executed_clone.store(true, Ordering::SeqCst);
                text_result("agent-ran")
            }),
            execution_mode: None,
        };

        let def = create_tool_definition_from_agent_tool(tool);
        assert_eq!(def.name, "agent");
        assert_eq!(def.label, "Agent");
        assert_eq!(def.description, "desc");
        assert!(def.prompt_snippet.is_none());
        assert!(def.prompt_guidelines.is_none());
        assert!(def.render_shell.is_none());
        // The by-value prepare adapter round-trips.
        let prepared = (def.prepare_arguments.as_ref().unwrap())(json!({ "k": "v" }));
        assert_eq!(prepared, json!({ "k": "v" }));
        // Executing ignores the injected ctx and drives the AgentTool execute.
        let ctx = DefaultExtensionContext;
        let result = (def.execute)("id", &json!({}), None, None, &ctx);
        assert!(executed.load(Ordering::SeqCst));
        assert_eq!(result.content, text_result("agent-ran").content);
    }
}
