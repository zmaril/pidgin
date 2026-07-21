#![cfg(feature = "python")]
//! End-to-end proof that a Python extension `tool_call` guardrail BLOCKS a tool
//! in a real `AgentSession` turn: the real PyO3 runner is installed on a faux
//! session, the model attempts a tool call, and the agent loop's
//! before-tool-call hook short-circuits execution with the guardrail's reason.
// straitjacket-allow-file:duplication

mod python_support;

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use pidgin_agent::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback};
use pidgin_ai::providers::faux::{faux_assistant_message, faux_tool_call, FauxAssistantOptions};
use pidgin_ai::seams::AbortSignal;
use pidgin_ai::{AssistantMessage, ContentBlock, Context, StopReason};
use pidgin_coding::core::agent_session::{build_faux_session_with_runner, FauxResponse};

// The guardrail also registers the `list_tasks` tool + `task` command the shared
// `python_support::load_runner` parity check expects; only `on_tool_call` matters
// to the enforcement assertions below.
const GUARDRAIL_PY: &str = r#"
def extension(pi):
    def list_tasks(args):
        return {"content": [{"type": "text", "text": "no tasks"}], "details": None}

    pi.register_tool({
        "name": "list_tasks",
        "label": "List tasks",
        "description": "list the current tasks",
        "parameters": {"type": "object", "properties": {}},
        "execute": list_tasks,
    })

    def task_handler(args, ctx):
        return None

    pi.register_command("task", description="manage the task list", handler=task_handler)

    def on_tool_call(event, ctx):
        if event.get("toolName") != "probe_bash":
            return None
        payload = event.get("input")
        command = payload.get("command", "") if isinstance(payload, dict) else ""
        if "rm -rf" in (command or ""):
            return {"block": True, "reason": "Blocked destructive rm -rf command by python guardrail"}
        return None
    pi.on("tool_call", on_tool_call)
"#;

const BLOCK_REASON: &str = "Blocked destructive rm -rf command by python guardrail";

/// A stand-in "bash" tool that only RECORDS the command it was asked to run and
/// never executes anything (safe even if a block were to fail).
fn probe_bash_tool(runs: Arc<Mutex<Vec<String>>>) -> AgentTool {
    AgentTool {
        name: "probe_bash".to_string(),
        description: "Records the command; never executes anything.".to_string(),
        parameters: json!({ "type": "object" }),
        label: "Probe Bash".to_string(),
        prepare_arguments: None,
        execution_mode: None,
        execute: Arc::new(
            move |_id: &str,
                  params: &Value,
                  _signal: Option<&AbortSignal>,
                  _on_update: Option<&AgentToolUpdateCallback>| {
                let command = params
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                runs.lock().unwrap().push(command.clone());
                AgentToolResult {
                    content: vec![ContentBlock::Text {
                        text: format!("ran: {command}"),
                        text_signature: None,
                    }],
                    details: json!({ "command": command }),
                    added_tool_names: None,
                    terminate: None,
                }
            },
        ),
    }
}

fn assistant_tool_use(content: Vec<ContentBlock>) -> AssistantMessage {
    faux_assistant_message(
        content,
        FauxAssistantOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
        0,
    )
}

fn assistant_text(text: &str) -> AssistantMessage {
    faux_assistant_message(
        vec![ContentBlock::Text {
            text: text.to_string(),
            text_signature: None,
        }],
        FauxAssistantOptions::default(),
        0,
    )
}

/// Pull the text of the first `toolResult` message out of the request context so
/// a follow-up faux turn can echo it (mirrors the in-crate helper).
fn context_tool_result_text(context: &Context) -> String {
    let messages = serde_json::to_value(&context.messages).unwrap_or(Value::Null);
    messages
        .as_array()
        .and_then(|list| {
            list.iter()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("toolResult"))
        })
        .and_then(|m| m.get("content").and_then(Value::as_array).cloned())
        .and_then(|blocks| {
            blocks
                .into_iter()
                .find_map(|b| b.get("text").and_then(Value::as_str).map(str::to_string))
        })
        .unwrap_or_default()
}

fn write_guardrail(dir: &std::path::Path) -> String {
    let path = dir.join("guardrail.py");
    std::fs::write(&path, GUARDRAIL_PY).expect("write guardrail.py");
    path.to_str().expect("utf8 path").to_string()
}

#[test]
fn python_tool_call_guardrail_blocks_execution_end_to_end() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_str().expect("utf8 cwd").to_string();
    let ext_path = write_guardrail(dir.path());

    let runner = python_support::load_runner(&ext_path, &cwd);
    let runs = Arc::new(Mutex::new(Vec::<String>::new()));

    let session = build_faux_session_with_runner(
        cwd,
        vec![
            FauxResponse::Message(Box::new(assistant_tool_use(vec![faux_tool_call(
                "probe_bash",
                json!({ "command": "rm -rf /tmp/pidgin-victim" }),
                Some("call-1".to_string()),
            )]))),
            FauxResponse::Fn(Box::new(|context: &Context| {
                assistant_text(&context_tool_result_text(context))
            })),
        ],
        vec![probe_bash_tool(Arc::clone(&runs))],
        runner,
    )
    .expect("build faux session with python runner");

    session
        .prompt("please clean up", None, None)
        .expect("prompt");

    // 1. The tool never executed: the guardrail blocked it before `execute`.
    assert!(
        runs.lock().unwrap().is_empty(),
        "blocked tool must not execute, got {:?}",
        runs.lock().unwrap()
    );

    // 2. The block became an error tool-result (pi semantics: isError == true).
    let messages = session.messages();
    assert!(
        messages.iter().any(|m| {
            m.get("role").and_then(Value::as_str) == Some("toolResult")
                && m.get("isError").and_then(Value::as_bool) == Some(true)
        }),
        "expected an error tool-result message, got {messages:#?}"
    );

    // 3. The guardrail's reason surfaced (in the tool-result and echoed back).
    let serialized = serde_json::to_string(&messages).unwrap_or_default();
    assert!(
        serialized.contains(BLOCK_REASON),
        "block reason must surface in the turn, got {serialized}"
    );
}

#[test]
fn python_tool_call_guardrail_allows_benign_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_str().expect("utf8 cwd").to_string();
    let ext_path = write_guardrail(dir.path());

    let runner = python_support::load_runner(&ext_path, &cwd);
    let runs = Arc::new(Mutex::new(Vec::<String>::new()));

    let session = build_faux_session_with_runner(
        cwd,
        vec![
            FauxResponse::Message(Box::new(assistant_tool_use(vec![faux_tool_call(
                "probe_bash",
                json!({ "command": "ls -la" }),
                Some("call-2".to_string()),
            )]))),
            FauxResponse::Fn(Box::new(|context: &Context| {
                assistant_text(&context_tool_result_text(context))
            })),
        ],
        vec![probe_bash_tool(Arc::clone(&runs))],
        runner,
    )
    .expect("build faux session with python runner");

    session.prompt("list files", None, None).expect("prompt");

    // The benign command is allowed through and the tool actually executes.
    assert_eq!(
        runs.lock().unwrap().as_slice(),
        &["ls -la".to_string()],
        "benign command must execute"
    );
    assert!(
        !session
            .messages()
            .iter()
            .any(|m| m.get("isError").and_then(Value::as_bool) == Some(true)),
        "benign tool call must not produce an error result"
    );
}
