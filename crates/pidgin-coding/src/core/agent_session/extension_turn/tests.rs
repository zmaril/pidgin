//! Extension-turn tests, ported from pi's `test/suite/agent-session-prompt.test.ts`
//! (skill / prompt-template expansion, `/`-command dispatch) and
//! `test/suite/agent-session-model-extension.test.ts` (input transform/handle,
//! `before_agent_start` injection + system-prompt override, `message_end`
//! replacement).
//!
//! Each `#[test]` mirrors a pi characterization case: same setup (a faux stream
//! fn with an in-memory session/settings/model runtime, from
//! [`super::super::test_support`]), same assertions. Cases that need a subsystem
//! this slice does not port (model management, the tool registry, the live deno
//! runner, the out-of-seam `context` hook) live as `#[ignore]`d markers in the
//! suite modules with a precise reason.

// straitjacket-allow-file:duplication

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use pidgin_ai::providers::faux::faux_tool_call;
use pidgin_ai::Context;

use crate::core::agent_session::test_support::{
    assistant_text, assistant_texts, assistant_tool_use, create_harness, echo_tool, message_text,
    HarnessOptions, TestExtensionRunner,
};
use crate::core::extensions::dispatch::BeforeAgentStartCombinedResult;
use crate::core::extensions::events::selection::InputEventResult;
use crate::core::prompt_templates::{self, PromptTemplate};
use crate::core::skills::{self, Skill};

use super::super::test_support::FauxResponse;

/// The text of the first `user` message the provider received.
fn context_user_text(context: &Context) -> String {
    let messages = serde_json::to_value(&context.messages).unwrap_or(Value::Null);
    messages
        .as_array()
        .and_then(|list| {
            list.iter()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        })
        .map(message_text)
        .unwrap_or_default()
}

/// The joined text of the first `toolResult` message the provider received (pi's
/// test helper that reads the tool-result content the follow-up turn sees).
fn context_tool_result_text(context: &Context) -> String {
    let messages = serde_json::to_value(&context.messages).unwrap_or(Value::Null);
    messages
        .as_array()
        .and_then(|list| {
            list.iter()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("toolResult"))
        })
        .map(message_text)
        .unwrap_or_default()
}

/// A skill fixture whose body lives at `file_path` (pi's fake `getSkills`).
fn skill_fixture(name: &str, file_path: &str, base_dir: &str) -> Skill {
    Skill {
        name: name.to_string(),
        description: "Test skill".to_string(),
        file_path: file_path.to_string(),
        base_dir: base_dir.to_string(),
        source_info: skills::SourceInfo {
            path: file_path.to_string(),
            source: "local".to_string(),
            scope: skills::SourceScope::Project,
            origin: skills::SourceOrigin::TopLevel,
            base_dir: Some(base_dir.to_string()),
        },
        disable_model_invocation: false,
    }
}

/// A prompt-template fixture with body `content` (pi's fake `getPrompts`).
fn prompt_template_fixture(name: &str, content: &str) -> PromptTemplate {
    PromptTemplate {
        name: name.to_string(),
        description: "Review template".to_string(),
        argument_hint: None,
        content: content.to_string(),
        source_info: prompt_templates::SourceInfo {
            path: "/virtual/review.md".to_string(),
            source: "local".to_string(),
            scope: prompt_templates::SourceScope::Temporary,
            origin: prompt_templates::SourceOrigin::TopLevel,
            base_dir: None,
        },
        file_path: "/virtual/review.md".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Skill / prompt-template expansion (re-enabled from the PR3 prompt suite)
// ---------------------------------------------------------------------------

#[test]
fn expands_skill_commands_before_sending_the_prompt() {
    let skill_dir = tempfile::tempdir().expect("tempdir");
    let skill_path = skill_dir.path().join("test-skill.md");
    std::fs::write(&skill_path, "# Test Skill\n\nUse the skill body.").expect("write skill");
    let skill = skill_fixture(
        "test",
        &skill_path.to_string_lossy(),
        &skill_dir.path().to_string_lossy(),
    );

    let harness = create_harness(HarnessOptions {
        skills: vec![skill],
        ..Default::default()
    });
    let seen = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&seen);
    harness.set_responses(vec![FauxResponse::Fn(Box::new(
        move |context: &Context| {
            *sink.lock().unwrap() = context_user_text(context);
            assistant_text("ok")
        },
    ))]);

    harness
        .session
        .prompt("/skill:test explain this", None, None)
        .unwrap();

    let expanded = seen.lock().unwrap().clone();
    assert!(
        expanded.contains("<skill name=\"test\" location=\""),
        "missing skill block, got {expanded:?}"
    );
    assert!(expanded.contains("Use the skill body."));
    assert!(expanded.contains("explain this"));
}

#[test]
fn expands_prompt_templates_before_sending_the_prompt() {
    let template = prompt_template_fixture("review", "Review this code: $1");
    let harness = create_harness(HarnessOptions {
        prompt_templates: vec![template],
        ..Default::default()
    });
    let seen = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&seen);
    harness.set_responses(vec![FauxResponse::Fn(Box::new(
        move |context: &Context| {
            *sink.lock().unwrap() = context_user_text(context);
            assistant_text("ok")
        },
    ))]);

    harness
        .session
        .prompt("/review src/index.ts", None, None)
        .unwrap();

    assert_eq!(
        seen.lock().unwrap().clone(),
        "Review this code: src/index.ts"
    );
}

// ---------------------------------------------------------------------------
// `/`-command dispatch (re-enabled from the PR3 prompt suite + PR4 queue suite)
// ---------------------------------------------------------------------------

#[test]
fn dispatches_extension_commands_without_consuming_a_provider_response() {
    let runs = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&runs);
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(move |_agent| {
            Box::new(TestExtensionRunner::new().with_recording_command("testcmd", sink))
        })),
        ..Default::default()
    });
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_text(
        "should stay queued",
    )))]);

    harness
        .session
        .prompt("/testcmd hello world", None, None)
        .unwrap();

    assert_eq!(*runs.lock().unwrap(), vec!["hello world".to_string()]);
    // No user/assistant message was created and the queued provider response is
    // untouched (the command managed its own interaction).
    assert!(harness.session.messages().is_empty());
    assert_eq!(harness.pending_response_count(), 1);
}

#[test]
fn dispatches_extension_commands_immediately_when_prompted_while_idle() {
    let runs = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&runs);
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(move |_agent| {
            Box::new(TestExtensionRunner::new().with_recording_command("testcmd", sink))
        })),
        ..Default::default()
    });

    harness
        .session
        .prompt("/testcmd hello world", None, None)
        .unwrap();

    assert_eq!(*runs.lock().unwrap(), vec!["hello world".to_string()]);
    assert_eq!(harness.pending_response_count(), 0);
    assert!(harness.session.messages().is_empty());
}

#[test]
fn reports_command_handler_errors_through_the_runner_and_still_handles() {
    // An unknown command is not handled (the prompt proceeds normally); a known
    // command whose handler is registered is always handled. We assert the handled
    // path leaves no messages, matching pi's `_tryExecuteExtensionCommand`.
    let runs = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&runs);
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(move |_agent| {
            Box::new(TestExtensionRunner::new().with_recording_command("known", sink))
        })),
        ..Default::default()
    });
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_text("ok")))]);

    // Unknown command: falls through to a normal prompt turn.
    harness
        .session
        .prompt("/unknown please", None, None)
        .unwrap();
    assert_eq!(harness.message_roles(), vec!["user", "assistant"]);
    assert!(runs.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Input transform / handle (ported from the model-extension suite)
// ---------------------------------------------------------------------------

#[test]
fn allows_extension_input_handlers_to_transform_or_handle_input() {
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(move |_agent| {
            Box::new(
                TestExtensionRunner::new().with_input_response(Arc::new(|text: &str| {
                    if text == "ping" {
                        InputEventResult::Handled
                    } else {
                        InputEventResult::Transform {
                            text: format!("transformed:{text}"),
                            images: None,
                        }
                    }
                })),
            )
        })),
        ..Default::default()
    });
    let seen = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&seen);
    harness.set_responses(vec![FauxResponse::Fn(Box::new(
        move |context: &Context| {
            *sink.lock().unwrap() = context_user_text(context);
            assistant_text("done")
        },
    ))]);

    harness.session.prompt("hello", None, None).unwrap();
    harness.session.prompt("ping", None, None).unwrap();

    // "hello" was transformed and sent; "ping" was handled and never sent.
    assert_eq!(seen.lock().unwrap().clone(), "transformed:hello");
    let user_messages = harness
        .session
        .messages()
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .count();
    assert_eq!(user_messages, 1);
}

// ---------------------------------------------------------------------------
// before_agent_start injection + system-prompt override (model-extension suite)
// ---------------------------------------------------------------------------

#[test]
fn before_agent_start_injects_custom_messages_and_overrides_the_system_prompt() {
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(move |_agent| {
            Box::new(TestExtensionRunner::new().with_before_agent_start(Arc::new(
                |_prompt: &str, system_prompt: &str| BeforeAgentStartCombinedResult {
                    messages: Some(vec![json!({
                        "customType": "before-start",
                        "content": "injected",
                        "display": true,
                        "details": { "injected": true },
                    })]),
                    system_prompt: Some(format!("{system_prompt}\n\nextra instructions")),
                },
            )))
        })),
        ..Default::default()
    });

    let seen_prompt = Arc::new(Mutex::new(String::new()));
    let prompt_sink = Arc::clone(&seen_prompt);
    harness.set_responses(vec![FauxResponse::Fn(Box::new(
        move |context: &Context| {
            *prompt_sink.lock().unwrap() = context.system_prompt.clone().unwrap_or_default();
            assistant_text("done")
        },
    ))]);

    harness.session.prompt("hello", None, None).unwrap();

    // The handler's system-prompt override reached the provider.
    assert!(
        seen_prompt.lock().unwrap().contains("extra instructions"),
        "system prompt override not applied: {:?}",
        seen_prompt.lock().unwrap()
    );
    // The injected custom message entered the turn (agent state carries it as a
    // `custom` role message). Whether agent-core forwards custom messages to the
    // LLM is a separate `convert_to_llm` concern outside this slice.
    let has_custom = harness.session.messages().iter().any(|message| {
        message.get("role").and_then(Value::as_str) == Some("custom")
            && message.get("customType").and_then(Value::as_str) == Some("before-start")
    });
    assert!(has_custom, "custom before-start message not in state");
}

#[test]
fn before_agent_start_override_resets_to_base_on_the_next_turn() {
    // First turn overrides the prompt; the second (no override) resets to base.
    let toggle = Arc::new(Mutex::new(true));
    let toggle_runner = Arc::clone(&toggle);
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(move |_agent| {
            Box::new(TestExtensionRunner::new().with_before_agent_start(Arc::new(
                move |_prompt: &str, system_prompt: &str| {
                    let mut first = toggle_runner.lock().unwrap();
                    if *first {
                        *first = false;
                        BeforeAgentStartCombinedResult {
                            messages: None,
                            system_prompt: Some(format!("{system_prompt}\n\noverride")),
                        }
                    } else {
                        BeforeAgentStartCombinedResult {
                            messages: None,
                            system_prompt: None,
                        }
                    }
                },
            )))
        })),
        ..Default::default()
    });

    let prompts = Arc::new(Mutex::new(Vec::<String>::new()));
    let prompt_sink = Arc::clone(&prompts);
    harness.set_responses(vec![
        FauxResponse::Fn(Box::new({
            let sink = Arc::clone(&prompt_sink);
            move |context: &Context| {
                sink.lock()
                    .unwrap()
                    .push(context.system_prompt.clone().unwrap_or_default());
                assistant_text("one")
            }
        })),
        FauxResponse::Fn(Box::new(move |context: &Context| {
            prompt_sink
                .lock()
                .unwrap()
                .push(context.system_prompt.clone().unwrap_or_default());
            assistant_text("two")
        })),
    ]);

    harness.session.prompt("first", None, None).unwrap();
    harness.session.prompt("second", None, None).unwrap();

    let seen = prompts.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].contains("override"), "first turn should override");
    assert!(
        !seen[1].contains("override"),
        "second turn should reset to base, got {:?}",
        seen[1]
    );
}

// ---------------------------------------------------------------------------
// message_end replacement (pi `emitMessageEnd` + `_replaceMessageInPlace`)
// ---------------------------------------------------------------------------

#[test]
fn message_end_replacement_is_applied_to_state_and_persistence() {
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(move |_agent| {
            Box::new(
                TestExtensionRunner::new().with_message_end_replacement(Arc::new(
                    |message: &Value| {
                        if message.get("role").and_then(Value::as_str) == Some("assistant") {
                            let mut replacement = message.clone();
                            replacement["content"] = json!([{ "type": "text", "text": "patched" }]);
                            Some(replacement)
                        } else {
                            None
                        }
                    },
                )),
            )
        })),
        ..Default::default()
    });
    harness.set_responses(vec![FauxResponse::Message(Box::new(assistant_text(
        "original",
    )))]);

    harness.session.prompt("hi", None, None).unwrap();

    // Agent state carries the replacement.
    let assistant = harness
        .session
        .messages()
        .into_iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .expect("assistant message");
    assert_eq!(message_text(&assistant), "patched");

    // Session history persisted the replacement.
    let entries = harness.session.session_manager().get_entries();
    let persisted_assistant_text = entries
        .iter()
        .filter_map(|entry| serde_json::to_value(entry).ok())
        .filter_map(|value| value.get("message").cloned())
        .find(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .map(|message| message_text(&message))
        .unwrap_or_default();
    assert_eq!(persisted_assistant_text, "patched");
}

// ---------------------------------------------------------------------------
// model-extension suite cases deferred to a later slice (structurally not
// reachable by this extension-turn slice) — represented with a precise reason.
// The model-management cases (setModel / cycleModel / thinking cycling) are
// ported in `super::super::model::tests`.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Agent tool-call / tool-result hooks (`_installAgentToolHooks`), ported from
// `agent-session-model-extension.test.ts:116/158`.
// ---------------------------------------------------------------------------

#[test]
fn allows_extension_tool_call_handlers_to_block_tool_execution() {
    let tool_runs = Arc::new(Mutex::new(Vec::new()));
    let harness = create_harness(HarnessOptions {
        tools: vec![echo_tool(Arc::clone(&tool_runs))],
        make_runner: Some(Box::new(|_agent| {
            Box::new(TestExtensionRunner::new().with_tool_call_block("Blocked by test"))
        })),
        ..Default::default()
    });
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_tool_use(vec![faux_tool_call(
            "echo",
            json!({ "text": "hello" }),
            Some("call-1".to_string()),
        )]))),
        FauxResponse::Fn(Box::new(|context: &Context| {
            assistant_text(&context_tool_result_text(context))
        })),
    ]);

    harness.session.prompt("hi", None, None).unwrap();

    // The tool never executed (the hook blocked it before `execute`).
    assert!(
        tool_runs.lock().unwrap().is_empty(),
        "blocked tool must not execute, got {:?}",
        tool_runs.lock().unwrap()
    );
    // The block reason surfaced as the tool-result text the follow-up turn echoed.
    assert!(
        assistant_texts(&harness)
            .iter()
            .any(|text| text.contains("Blocked by test")),
        "missing block reason in assistant texts, got {:?}",
        assistant_texts(&harness)
    );
    // The blocked call became an error tool-result message.
    assert!(
        harness.session.messages().iter().any(|message| {
            message.get("role").and_then(Value::as_str) == Some("toolResult")
                && message.get("isError").and_then(Value::as_bool) == Some(true)
        }),
        "expected an error tool-result message"
    );
}

#[test]
fn allows_extension_tool_result_handlers_to_modify_tool_results() {
    let tool_runs = Arc::new(Mutex::new(Vec::new()));
    let harness = create_harness(HarnessOptions {
        tools: vec![echo_tool(Arc::clone(&tool_runs))],
        make_runner: Some(Box::new(|_agent| {
            Box::new(TestExtensionRunner::new().with_tool_result_override(
                vec![json!({ "type": "text", "text": "patched result" })],
                json!({ "patched": true }),
            ))
        })),
        ..Default::default()
    });
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_tool_use(vec![faux_tool_call(
            "echo",
            json!({ "text": "hello" }),
            Some("call-1".to_string()),
        )]))),
        FauxResponse::Fn(Box::new(|context: &Context| {
            assistant_text(&context_tool_result_text(context))
        })),
    ]);

    harness.session.prompt("hi", None, None).unwrap();

    // The tool executed normally; the hook replaced its result afterward.
    assert_eq!(*tool_runs.lock().unwrap(), vec!["hello".to_string()]);
    // The patched content surfaced as the tool-result text the follow-up echoed.
    assert!(
        assistant_texts(&harness)
            .iter()
            .any(|text| text.contains("patched result")),
        "missing patched result in assistant texts, got {:?}",
        assistant_texts(&harness)
    );
    // The tool-result message carries the replacement details.
    assert!(
        harness.session.messages().iter().any(|message| {
            message.get("role").and_then(Value::as_str) == Some("toolResult")
                && message
                    .get("details")
                    .and_then(|details| details.get("patched"))
                    .and_then(Value::as_bool)
                    == Some(true)
        }),
        "expected a tool-result message with patched details"
    );
}

#[test]
fn runs_tools_normally_when_no_tool_hook_handlers_are_registered() {
    // Passthrough: with the default stub runner (`has_handlers` false), the
    // installed hooks are no-ops and the tool executes with its original result.
    let tool_runs = Arc::new(Mutex::new(Vec::new()));
    let harness = create_harness(HarnessOptions {
        tools: vec![echo_tool(Arc::clone(&tool_runs))],
        ..Default::default()
    });
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_tool_use(vec![faux_tool_call(
            "echo",
            json!({ "text": "hello" }),
            Some("call-1".to_string()),
        )]))),
        FauxResponse::Fn(Box::new(|context: &Context| {
            assistant_text(&context_tool_result_text(context))
        })),
    ]);

    harness.session.prompt("hi", None, None).unwrap();

    // The tool ran with its original arguments.
    assert_eq!(*tool_runs.lock().unwrap(), vec!["hello".to_string()]);
    // The original (unmodified) result surfaced.
    assert!(
        assistant_texts(&harness)
            .iter()
            .any(|text| text.contains("echo:hello")),
        "missing original tool result, got {:?}",
        assistant_texts(&harness)
    );
    // No error tool-result message was produced.
    assert!(
        !harness.session.messages().iter().any(|message| {
            message.get("role").and_then(Value::as_str) == Some("toolResult")
                && message.get("isError").and_then(Value::as_bool) == Some(true)
        }),
        "unexpected error tool-result message"
    );
}

#[test]
#[ignore = "unit5: out of the ExtensionRunner seam — pi's `context` hook is driven from sdk.ts (the provider-request path), not AgentSession; not part of this trait"]
fn allows_extension_context_handlers_to_modify_messages_before_the_llm_call() {}

#[test]
#[ignore = "unit5: runtime tool-registry slice — `_baseSystemPromptOptions` (with selectedTools) is built from the tool registry, not ported by the extension-turn slice"]
fn allows_extension_commands_to_inspect_live_system_prompt_options() {}

#[test]
#[ignore = "unit5: lifecycle slice — bindExtensions/reload + session_start/session_shutdown emit ordering are not ported by the extension-turn slice"]
fn bind_extensions_emits_session_start_and_reload_emits_shutdown_then_start() {}
