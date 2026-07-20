// straitjacket-allow-file:duplication — the `load()` vector-reading helper and
// the per-component replay loops intentionally mirror the two-line boilerplate
// the pidgin-tui vector-test binaries use (widget_vectors.rs etc.); each
// integration-test binary is standalone.
//! Drives the Rust port of pi's interactive message-render components
//! (AssistantMessage, UserMessage, ToolExecution) against vectors extracted from
//! pi itself (`crates/pidgin-coding/vectors/gen/generate_interactive_messages.mjs`).
//! Every assertion is byte-identical: pi's `render(width)` output is the source
//! of truth. The theme is baked at 256-color to match the generator.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;

use pidgin_agent::types::AgentToolResult;
use pidgin_ai::types::{AssistantMessage as AiAssistantMessage, ContentBlock};
use pidgin_coding::core::extensions::types::{RenderShell, ToolDefinition};
use pidgin_coding::modes::interactive::components::{
    AssistantMessage, ToolExecution, ToolExecutionOptions, ToolExecutionResult, UserMessage,
};
use pidgin_coding::modes::interactive::theme::{create_theme, parse_theme_json, ColorMode, Theme};
use pidgin_tui::renderer::Component;

fn load<T: serde::de::DeserializeOwned>(name: &str) -> Vec<T> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join(format!("{name}.json"));
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

/// Build the runtime `dark` theme baked at 256-color — the same theme the
/// generator loads (the JSON is byte-identical to pi's).
fn dark_theme() -> Theme {
    let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("modes")
        .join("interactive")
        .join("theme")
        .join("dark.json");
    let content = std::fs::read_to_string(&json_path).expect("read dark.json");
    let theme_json = parse_theme_json(&content).expect("parse dark.json");
    create_theme(&theme_json, Some(ColorMode::Color256), None).expect("create dark theme")
}

// --- AssistantMessage -------------------------------------------------------

#[derive(Deserialize)]
struct AssistantVec {
    label: String,
    message: AiAssistantMessage,
    #[serde(rename = "hideThinkingBlock")]
    hide_thinking_block: bool,
    #[serde(rename = "hiddenThinkingLabel")]
    hidden_thinking_label: String,
    #[serde(rename = "outputPad")]
    output_pad: usize,
    width: usize,
    expected: Vec<String>,
}

#[test]
fn interactive_assistant_message_vectors() {
    let theme = dark_theme();
    let vectors: Vec<AssistantVec> = load("interactive_assistant_message");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let component = AssistantMessage::new(
            Some(&v.message),
            theme.clone(),
            v.hide_thinking_block,
            v.hidden_thinking_label.clone(),
            v.output_pad,
        );
        assert_eq!(
            component.render(v.width),
            v.expected,
            "AssistantMessage[{}] width={}",
            v.label,
            v.width
        );
    }
}

// --- UserMessage ------------------------------------------------------------

#[derive(Deserialize)]
struct UserVec {
    label: String,
    text: String,
    #[serde(rename = "outputPad")]
    output_pad: usize,
    width: usize,
    expected: Vec<String>,
}

#[test]
fn interactive_user_message_vectors() {
    let theme = dark_theme();
    let vectors: Vec<UserVec> = load("interactive_user_message");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let component = UserMessage::new(v.text.clone(), theme.clone(), v.output_pad);
        assert_eq!(
            component.render(v.width),
            v.expected,
            "UserMessage[{}] width={}",
            v.label,
            v.width
        );
    }
}

// --- ToolExecution ----------------------------------------------------------

#[derive(Deserialize)]
struct ToolDefSpec {
    #[serde(rename = "renderShell")]
    render_shell: String,
}

#[derive(Deserialize)]
struct ResultBlock {
    #[serde(rename = "type")]
    ty: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct ToolResultSpec {
    content: Vec<ResultBlock>,
    #[serde(rename = "isError")]
    is_error: bool,
}

#[derive(Deserialize)]
struct ToolVec {
    label: String,
    #[serde(rename = "toolName")]
    tool_name: String,
    args: Value,
    #[serde(rename = "toolDefinition")]
    tool_definition: Option<ToolDefSpec>,
    cwd: String,
    result: Option<ToolResultSpec>,
    #[serde(rename = "isPartial")]
    is_partial: bool,
    width: usize,
    expected: Vec<String>,
}

/// A minimal [`ToolDefinition`] carrying only a `render_shell` — the shape pi's
/// generator uses for the "definition without renderers" cases. The `execute`
/// closure is never invoked by the render path.
fn tool_def(render_shell: RenderShell) -> ToolDefinition {
    ToolDefinition {
        name: "customTool".into(),
        label: "Custom".into(),
        description: String::new(),
        parameters: serde_json::json!({ "type": "object" }),
        execution_mode: None,
        execute: Arc::new(|_id, _args, _signal, _on_update, _ctx| AgentToolResult {
            content: Vec::new(),
            details: Value::Null,
            added_tool_names: None,
            terminate: None,
        }),
        prepare_arguments: None,
        prompt_snippet: None,
        prompt_guidelines: None,
        render_shell: Some(render_shell),
        render_call: None,
        render_result: None,
    }
}

#[test]
fn interactive_tool_execution_vectors() {
    let theme = dark_theme();
    let vectors: Vec<ToolVec> = load("interactive_tool_execution");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let tool_definition = v.tool_definition.as_ref().map(|spec| {
            let shell = match spec.render_shell.as_str() {
                "self" => RenderShell::SelfRender,
                _ => RenderShell::Default,
            };
            tool_def(shell)
        });

        let mut component = ToolExecution::new(
            v.tool_name.clone(),
            "tool_call_id_1",
            v.args.clone(),
            ToolExecutionOptions::default(),
            tool_definition,
            &v.cwd,
            theme.clone(),
        );

        if let Some(result) = &v.result {
            let content = result
                .content
                .iter()
                .map(|b| {
                    assert_eq!(b.ty, "text", "only text result blocks are covered");
                    ContentBlock::Text {
                        text: b.text.clone().unwrap_or_default(),
                        text_signature: None,
                    }
                })
                .collect();
            component.update_result(
                ToolExecutionResult {
                    content,
                    is_error: result.is_error,
                    details: Value::Null,
                },
                v.is_partial,
            );
        }

        assert_eq!(
            component.render(v.width),
            v.expected,
            "ToolExecution[{}] width={}",
            v.label,
            v.width
        );
    }
}
