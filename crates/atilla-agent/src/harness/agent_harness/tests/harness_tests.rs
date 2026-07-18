//! Direct ports of `test/harness/agent-harness.test.ts` (13 cases).
//!
//! Real-async-interleaving cases (`abort`, `waitForIdle`) are ADAPTED to the
//! deterministic synchronous model, flagged inline.

// straitjacket-allow-file:duplication — faithful parallel-structure test
// bodies repeat near-identical faux/session/harness scaffolding per scenario,
// mirroring pi's one-`it`-per-shape suite; not extractable duplication.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::*;
use crate::harness::agent_harness::{AgentHarness, AgentHarnessEvent};
use crate::harness::events::{AgentHarnessEventResult, AgentHarnessOwnEvent, ToolResultPatch};
use crate::harness::options::{AgentHarnessErrorCode, SystemPromptContext, SystemPromptSource};
use crate::harness::session::Session;
use crate::harness::skills::Skill;
use crate::types::{AgentEvent, QueueMode, ThinkingLevel};

/// pi: "constructs directly and exposes queue modes"
#[test]
fn constructs_directly_and_exposes_queue_modes() {
    let faux = new_faux();
    let model = faux.get_model(None).unwrap();
    let session = Session::new(new_storage());
    let mut options = base_options(session, faux, model.clone());
    options.thinking_level = Some(ThinkingLevel::High);
    options.system_prompt = Some(SystemPromptSource::Static("You are helpful.".into()));
    options.steering_mode = Some(QueueMode::All);
    options.follow_up_mode = Some(QueueMode::All);
    let harness = AgentHarness::new(options).unwrap();

    assert_eq!(harness.get_model().id, model.id);
    assert_eq!(harness.get_thinking_level(), ThinkingLevel::High);
    assert_eq!(harness.get_steering_mode(), QueueMode::All);
    assert_eq!(harness.get_follow_up_mode(), QueueMode::All);
    harness.set_steering_mode(QueueMode::OneAtATime);
    harness.set_follow_up_mode(QueueMode::OneAtATime);
    assert_eq!(harness.get_steering_mode(), QueueMode::OneAtATime);
    assert_eq!(harness.get_follow_up_mode(), QueueMode::OneAtATime);
}

/// pi: "drains one queued steering message at a time and emits queue updates"
#[test]
fn drains_one_queued_steering_message_at_a_time() {
    let faux = new_faux();
    let user_counts = Arc::new(Mutex::new(Vec::new()));
    faux.set_responses(vec![
        counting_response("first", user_counts.clone()),
        counting_response("second", user_counts.clone()),
        counting_response("third", user_counts.clone()),
    ]);
    let model = faux.get_model(None).unwrap();
    let session = Session::new(new_storage());
    let mut options = base_options(session, faux, model);
    options.steering_mode = Some(QueueMode::OneAtATime);
    let harness = AgentHarness::new(options).unwrap();

    let steer_lengths = Rc::new(RefCell::new(Vec::new()));
    let sl = steer_lengths.clone();
    let queued = Rc::new(Cell::new(false));
    let h = harness.clone();
    let _sub = harness.subscribe(Rc::new(move |event: &AgentHarnessEvent, _| match event {
        AgentHarnessEvent::Own(own) => {
            if let AgentHarnessOwnEvent::QueueUpdate(q) = own.as_ref() {
                sl.borrow_mut().push(q.steer.len());
            }
        }
        AgentHarnessEvent::Loop(AgentEvent::MessageStart { message }) => {
            if role(message) == Some("assistant") && !queued.get() {
                queued.set(true);
                h.steer("one", None).unwrap();
                h.steer("two", None).unwrap();
            }
        }
        _ => {}
    }));

    harness.prompt("hello", None).unwrap();

    assert_eq!(*user_counts.lock().unwrap(), vec![1, 2, 3]);
    assert_eq!(*steer_lengths.borrow(), vec![1, 2, 1, 0]);
}

/// pi: "appends before_agent_start messages and persists them"
#[test]
fn appends_before_agent_start_messages() {
    let faux = new_faux();
    let request_text = Arc::new(Mutex::new(Vec::new()));
    faux.set_responses(vec![capturing_response(request_text.clone())]);
    let model = faux.get_model(None).unwrap();
    let storage = new_storage();
    let harness =
        AgentHarness::new(base_options(Session::new(storage.clone()), faux, model)).unwrap();

    let _sub = harness.on(
        "before_agent_start",
        Rc::new(|_event| {
            Ok(Some(AgentHarnessEventResult::BeforeAgentStart(Some(
                crate::harness::events::BeforeAgentStartResult {
                    messages: Some(vec![json!({
                        "role": "user",
                        "content": [{ "type": "text", "text": "hook" }],
                        "timestamp": 0,
                    })]),
                    system_prompt: None,
                },
            ))))
        }),
    );

    harness.prompt("hello", None).unwrap();

    let inspect = Session::new(storage);
    assert_eq!(*request_text.lock().unwrap(), vec!["hello", "hook"]);
    assert_eq!(persisted_user_texts(&inspect), vec!["hello", "hook"]);
}

/// pi: "abort clears steer and follow-up queues but preserves next-turn messages"
///
/// ADAPTED: pi aborts an in-flight provider mid-await. The synchronous port has
/// no real interleaving, so the scenario is reproduced by queueing steer/follow
/// up/next-turn from a mid-run subscriber and calling `abort()` re-entrantly at
/// the same point. The load-bearing outcomes are preserved: `abort()` clears the
/// steer/follow-up queues (returning them) while `nextTurn` survives to the next
/// prompt.
#[test]
fn abort_clears_steer_and_follow_up_preserves_next_turn() {
    let faux = new_faux();
    let second_request_text = Arc::new(Mutex::new(Vec::new()));
    faux.set_responses(vec![
        text_response("first-turn"),
        capturing_response(second_request_text.clone()),
    ]);
    let model = faux.get_model(None).unwrap();
    let storage = new_storage();
    let harness =
        AgentHarness::new(base_options(Session::new(storage.clone()), faux, model)).unwrap();

    let queue_updates = Rc::new(RefCell::new(Vec::new()));
    let qu = queue_updates.clone();
    let abort_result = Rc::new(RefCell::new(None));
    let ar = abort_result.clone();
    let acted = Rc::new(Cell::new(false));
    let h = harness.clone();
    let _sub = harness.subscribe(Rc::new(move |event: &AgentHarnessEvent, _| match event {
        AgentHarnessEvent::Own(own) => {
            if let AgentHarnessOwnEvent::QueueUpdate(q) = own.as_ref() {
                qu.borrow_mut()
                    .push((q.steer.len(), q.follow_up.len(), q.next_turn.len()));
            }
        }
        AgentHarnessEvent::Loop(AgentEvent::MessageStart { message }) => {
            if role(message) == Some("assistant") && !acted.get() {
                acted.set(true);
                h.steer("steer", None).unwrap();
                h.follow_up("follow", None).unwrap();
                h.next_turn("next", None).unwrap();
                *ar.borrow_mut() = Some(h.abort().unwrap());
            }
        }
        _ => {}
    }));

    harness.prompt("first", None).unwrap();

    let abort_result = abort_result.borrow_mut().take().unwrap();
    assert_eq!(abort_result.cleared_steer.len(), 1);
    assert_eq!(abort_result.cleared_follow_up.len(), 1);
    assert!(queue_updates.borrow().contains(&(0, 0, 1)));

    // nextTurn survives; the follow-up prompt prepends it before the new prompt.
    harness.prompt("second", None).unwrap();
    assert_eq!(
        *second_request_text.lock().unwrap(),
        vec!["first", "next", "second"]
    );
}

/// pi: "drains follow-up messages one at a time after the agent would otherwise
/// stop"
#[test]
fn drains_follow_up_messages_one_at_a_time() {
    let faux = new_faux();
    let user_counts = Arc::new(Mutex::new(Vec::new()));
    faux.set_responses(vec![
        counting_response("first", user_counts.clone()),
        counting_response("second", user_counts.clone()),
        counting_response("third", user_counts.clone()),
    ]);
    let model = faux.get_model(None).unwrap();
    let mut options = base_options(Session::new(new_storage()), faux, model);
    options.follow_up_mode = Some(QueueMode::OneAtATime);
    let harness = AgentHarness::new(options).unwrap();

    let follow_up_lengths = Rc::new(RefCell::new(Vec::new()));
    let fl = follow_up_lengths.clone();
    let queued = Rc::new(Cell::new(false));
    let h = harness.clone();
    let _sub = harness.subscribe(Rc::new(move |event: &AgentHarnessEvent, _| match event {
        AgentHarnessEvent::Own(own) => {
            if let AgentHarnessOwnEvent::QueueUpdate(q) = own.as_ref() {
                fl.borrow_mut().push(q.follow_up.len());
            }
        }
        AgentHarnessEvent::Loop(AgentEvent::MessageStart { message }) => {
            if role(message) == Some("assistant") && !queued.get() {
                queued.set(true);
                h.follow_up("one", None).unwrap();
                h.follow_up("two", None).unwrap();
            }
        }
        _ => {}
    }));

    harness.prompt("hello", None).unwrap();

    assert_eq!(*user_counts.lock().unwrap(), vec![1, 2, 3]);
    assert_eq!(*follow_up_lengths.borrow(), vec![1, 2, 1, 0]);
}

/// pi: "settles thrown hook failures with persisted assistant error messages"
#[test]
fn settles_thrown_hook_failures() {
    let faux = new_faux();
    faux.set_responses(vec![text_response("should not be used")]);
    let model = faux.get_model(None).unwrap();
    let storage = new_storage();
    let harness =
        AgentHarness::new(base_options(Session::new(storage.clone()), faux, model)).unwrap();

    let events = Rc::new(RefCell::new(Vec::new()));
    let _sub = harness.subscribe(recording_subscriber(events.clone()));
    let _sub = harness.on(
        "context",
        Rc::new(|_event| Err("context exploded".to_string())),
    );

    let response = harness.prompt("hello", None).unwrap();
    let after = harness.prompt("after failure", None).unwrap();

    assert_eq!(role(&after), Some("assistant"));
    assert_eq!(response["stopReason"], json!("error"));
    assert_eq!(response["errorMessage"], json!("context exploded"));

    let inspect = Session::new(storage);
    let roles = persisted_roles(&inspect);
    assert_eq!(roles.first().map(String::as_str), Some("user"));
    let entries = inspect.get_entries();
    let assistant = entries
        .iter()
        .find_map(|e| match e {
            SessionTreeEntry::Message(m) if role(&m.message) == Some("assistant") => {
                Some(m.message.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(assistant["stopReason"], json!("error"));
    assert_eq!(assistant["errorMessage"], json!("context exploded"));
    assert!(events.borrow().iter().any(|e| e == "agent_end"));
    assert!(events.borrow().iter().any(|e| e == "settled"));
}

/// pi: "refreshes model, thinking level, resources, system prompt, and active
/// tools at save points"
#[test]
fn refreshes_state_at_save_points() {
    let faux = new_faux_two_models();
    faux.set_responses(vec![
        tool_call_response("calculate", json!({ "expression": "1 + 1" }), "call-1"),
        text_response("done"),
    ]);
    let first = faux.get_model(Some("first")).unwrap();
    let second = faux.get_model(Some("second")).unwrap();

    let recorder = Rc::new(RefCell::new(Recorder::default()));
    let mut options = base_options(Session::new(new_storage()), faux.clone(), first);
    options.stream = recording_stream(faux, recorder.clone());
    options.thinking_level = Some(ThinkingLevel::Off);
    options.resources = Some(crate::harness::events::AgentHarnessResources {
        skills: Some(vec![skill("prompt", "first prompt")]),
        prompt_templates: None,
    });
    options.system_prompt = Some(SystemPromptSource::Dynamic(Box::new(
        |ctx: SystemPromptContext| {
            ctx.resources
                .skills
                .as_ref()
                .and_then(|s| s.first())
                .map(|s| s.content.clone())
                .unwrap_or_else(|| "missing prompt".to_string())
        },
    )));
    options.tools = Some(vec![calculate_tool()]);
    let harness = AgentHarness::new(options).unwrap();

    let h = harness.clone();
    let acted = Rc::new(Cell::new(false));
    let _sub = harness.subscribe(Rc::new(move |event: &AgentHarnessEvent, _| {
        if let AgentHarnessEvent::Loop(AgentEvent::ToolExecutionStart { .. }) = event {
            if !acted.get() {
                acted.set(true);
                h.set_model(second.clone()).unwrap();
                h.set_thinking_level(ThinkingLevel::High).unwrap();
                h.set_resources(crate::harness::events::AgentHarnessResources {
                    skills: Some(vec![skill("prompt", "second prompt")]),
                    prompt_templates: None,
                });
                h.set_tools(
                    vec![calculate_tool(), get_current_time_tool()],
                    Some(vec!["get_current_time".to_string()]),
                )
                .unwrap();
            }
        }
    }));

    harness.prompt("hello", None).unwrap();

    let rec = recorder.borrow();
    assert_eq!(
        rec.model_ids,
        vec!["first".to_string(), "second".to_string()]
    );
    assert_eq!(rec.reasonings, vec![None, Some(ThinkingLevel::High)]);
    assert_eq!(
        rec.system_prompts,
        vec!["first prompt".to_string(), "second prompt".to_string()]
    );
    assert_eq!(
        rec.tool_names,
        vec![
            vec!["calculate".to_string()],
            vec!["get_current_time".to_string()]
        ]
    );
}

/// pi: "orders pending listener session writes after agent-emitted messages"
#[test]
fn orders_pending_listener_writes_after_agent_messages() {
    let faux = new_faux();
    faux.set_responses(vec![text_response("ok")]);
    let model = faux.get_model(None).unwrap();
    let storage = new_storage();
    let harness =
        AgentHarness::new(base_options(Session::new(storage.clone()), faux, model)).unwrap();

    let wrote = Rc::new(Cell::new(false));
    let h = harness.clone();
    let _sub = harness.subscribe(Rc::new(move |event: &AgentHarnessEvent, _| {
        if let AgentHarnessEvent::Loop(AgentEvent::MessageEnd { message }) = event {
            if role(message) == Some("assistant") && !wrote.get() {
                wrote.set(true);
                h.append_message(json!({
                    "role": "custom",
                    "customType": "listener",
                    "content": "listener write",
                    "display": true,
                    "timestamp": 0,
                }))
                .unwrap();
            }
        }
    }));

    harness.prompt("hello", None).unwrap();

    let inspect = Session::new(storage);
    assert_eq!(
        persisted_roles(&inspect),
        vec![
            "user".to_string(),
            "assistant".to_string(),
            "custom".to_string()
        ]
    );
}

/// pi: "waitForIdle waits for external run settlement and awaited listeners"
///
/// ADAPTED: pi's `waitForIdle` awaits a background `runPromise` and any awaited
/// listeners. The synchronous port runs the whole turn (including synchronous
/// listeners) to completion inside `prompt()`, so `waitForIdle` is a no-op that
/// returns after the listener already ran.
#[test]
fn wait_for_idle_returns_after_listeners_run() {
    let faux = new_faux();
    faux.set_responses(vec![text_response("ok")]);
    let model = faux.get_model(None).unwrap();
    let harness =
        AgentHarness::new(base_options(Session::new(new_storage()), faux, model)).unwrap();

    let listener_finished = Rc::new(Cell::new(false));
    let lf = listener_finished.clone();
    let _sub = harness.subscribe(Rc::new(move |event: &AgentHarnessEvent, _| {
        if let AgentHarnessEvent::Loop(AgentEvent::AgentEnd { .. }) = event {
            lf.set(true);
        }
    }));

    harness.prompt("hello", None).unwrap();
    harness.wait_for_idle();
    assert!(listener_finished.get());
}

/// pi: "runs tool_call and tool_result hooks through the direct loop"
#[test]
fn runs_tool_call_and_tool_result_hooks() {
    let faux = new_faux();
    faux.set_responses(vec![tool_call_response(
        "calculate",
        json!({ "expression": "2 + 2" }),
        "call-1",
    )]);
    let model = faux.get_model(None).unwrap();
    let storage = new_storage();
    let mut options = base_options(Session::new(storage.clone()), faux, model);
    options.tools = Some(vec![calculate_tool()]);
    let harness = AgentHarness::new(options).unwrap();

    let seen = Rc::new(RefCell::new(Vec::new()));
    let s = seen.clone();
    let _sub = harness.on(
        "tool_call",
        Rc::new(move |event| {
            if let AgentHarnessOwnEvent::ToolCall(ev) = event {
                s.borrow_mut().push((
                    ev.tool_call_id.clone(),
                    ev.tool_name.clone(),
                    ev.input
                        .get("expression")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                ));
            }
            Ok(None)
        }),
    );
    let _sub = harness.on(
        "tool_result",
        Rc::new(|event| {
            if let AgentHarnessOwnEvent::ToolResult(ev) = event {
                assert_eq!(ev.tool_call_id, "call-1");
                assert_eq!(ev.tool_name, "calculate");
            }
            Ok(Some(AgentHarnessEventResult::ToolResult(Some(
                ToolResultPatch {
                    content: Some(vec![json!({ "type": "text", "text": "patched result" })]),
                    details: Some(json!({ "patched": true })),
                    is_error: None,
                    terminate: Some(true),
                },
            ))))
        }),
    );

    harness.prompt("hello", None).unwrap();

    assert_eq!(
        *seen.borrow(),
        vec![(
            "call-1".to_string(),
            "calculate".to_string(),
            "2 + 2".to_string()
        )]
    );

    let inspect = Session::new(storage);
    let tool_result = inspect
        .get_entries()
        .into_iter()
        .find_map(|e| match e {
            SessionTreeEntry::Message(m) if role(&m.message) == Some("toolResult") => {
                Some(m.message)
            }
            _ => None,
        })
        .expect("tool result persisted");
    assert_eq!(
        tool_result["content"],
        json!([{ "type": "text", "text": "patched result" }])
    );
    assert_eq!(tool_result["details"], json!({ "patched": true }));
}

/// pi: "preserves app tool types for getters and update events"
#[test]
fn preserves_tool_types_for_getters_and_updates() {
    let faux = new_faux();
    let model = faux.get_model(None).unwrap();
    let inspect = calculate_named("inspect");
    let search = calculate_named("search");
    let mut options = base_options(Session::new(new_storage()), faux, model);
    options.tools = Some(vec![inspect.clone(), search.clone()]);
    options.active_tool_names = Some(vec!["inspect".to_string()]);
    let harness = AgentHarness::new(options).unwrap();

    let updates = Rc::new(RefCell::new(Vec::new()));
    let u = updates.clone();
    let _sub = harness.subscribe(Rc::new(move |event: &AgentHarnessEvent, _| {
        if let AgentHarnessEvent::Own(own) = event {
            if let AgentHarnessOwnEvent::ToolsUpdate(ev) = own.as_ref() {
                u.borrow_mut().push((
                    ev.tool_names.clone(),
                    ev.previous_tool_names.clone(),
                    ev.active_tool_names.clone(),
                    ev.previous_active_tool_names.clone(),
                ));
            }
        }
    }));

    assert_eq!(names(&harness.get_tools()), vec!["inspect", "search"]);
    assert_eq!(names(&harness.get_active_tools()), vec!["inspect"]);

    harness
        .set_active_tools(vec!["search".to_string()])
        .unwrap();
    harness
        .set_tools(vec![search.clone()], Some(vec!["search".to_string()]))
        .unwrap();
    assert_eq!(
        harness
            .set_active_tools(vec!["missing".to_string()])
            .unwrap_err()
            .code,
        AgentHarnessErrorCode::InvalidArgument
    );
    assert_eq!(
        harness
            .set_active_tools(vec!["search".to_string(), "search".to_string()])
            .unwrap_err()
            .code,
        AgentHarnessErrorCode::InvalidArgument
    );
    assert_eq!(
        harness
            .set_tools(vec![inspect.clone()], None)
            .unwrap_err()
            .code,
        AgentHarnessErrorCode::InvalidArgument
    );
    assert_eq!(
        harness
            .set_tools(
                vec![inspect.clone(), inspect.clone()],
                Some(vec!["inspect".to_string()])
            )
            .unwrap_err()
            .code,
        AgentHarnessErrorCode::InvalidArgument
    );

    assert_eq!(
        *updates.borrow(),
        vec![
            (
                vec!["inspect".to_string(), "search".to_string()],
                vec!["inspect".to_string(), "search".to_string()],
                vec!["search".to_string()],
                vec!["inspect".to_string()],
            ),
            (
                vec!["search".to_string()],
                vec!["inspect".to_string(), "search".to_string()],
                vec!["search".to_string()],
                vec!["search".to_string()],
            ),
        ]
    );
    assert_eq!(names(&harness.get_tools()), vec!["search"]);
}

/// pi: "validates constructor tool names"
#[test]
fn validates_constructor_tool_names() {
    let faux = new_faux();
    let model = faux.get_model(None).unwrap();

    let mut options = base_options(Session::new(new_storage()), faux.clone(), model.clone());
    options.tools = Some(vec![calculate_tool()]);
    options.active_tool_names = Some(vec!["missing".to_string()]);
    assert!(AgentHarness::new(options)
        .unwrap_err()
        .message
        .contains("Unknown tool"));

    let mut options = base_options(Session::new(new_storage()), faux.clone(), model.clone());
    options.tools = Some(vec![calculate_tool(), calculate_tool()]);
    options.active_tool_names = Some(vec!["calculate".to_string()]);
    assert!(AgentHarness::new(options)
        .unwrap_err()
        .message
        .contains("Duplicate tool"));

    let mut options = base_options(Session::new(new_storage()), faux, model);
    options.tools = Some(vec![calculate_tool()]);
    options.active_tool_names = Some(vec!["calculate".to_string(), "calculate".to_string()]);
    assert!(AgentHarness::new(options)
        .unwrap_err()
        .message
        .contains("Duplicate active tool"));
}

/// pi: "preserves app resource types for getters and update events"
#[test]
fn preserves_resource_types_for_getters_and_updates() {
    let faux = new_faux();
    let model = faux.get_model(None).unwrap();
    let harness =
        AgentHarness::new(base_options(Session::new(new_storage()), faux, model)).unwrap();

    let updates = Rc::new(RefCell::new(Vec::new()));
    let u = updates.clone();
    let _sub = harness.subscribe(Rc::new(move |event: &AgentHarnessEvent, _| {
        if let AgentHarnessEvent::Own(own) = event {
            if let AgentHarnessOwnEvent::ResourcesUpdate(ev) = own.as_ref() {
                let current = first_skill_name(&ev.resources);
                let previous = first_skill_name(&ev.previous_resources);
                u.borrow_mut().push((current, previous));
            }
        }
    }));

    let resources = crate::harness::events::AgentHarnessResources {
        skills: Some(vec![skill("inspect", "Use inspection tools.")]),
        prompt_templates: Some(vec![crate::harness::prompt_templates::PromptTemplate {
            name: "review".into(),
            description: None,
            content: "Review $1".into(),
        }]),
    };
    harness.set_resources(resources.clone());
    harness.set_resources(resources);
    let resolved = harness.get_resources();

    assert_eq!(
        *updates.borrow(),
        vec![
            (Some("inspect".to_string()), None),
            (Some("inspect".to_string()), Some("inspect".to_string())),
        ]
    );
    assert_eq!(resolved.skills.unwrap()[0].name, "inspect");
    assert_eq!(resolved.prompt_templates.unwrap()[0].name, "review");
}

// ---------------------------------------------------------------------------
// Local fixtures.
// ---------------------------------------------------------------------------

fn skill(name: &str, content: &str) -> Skill {
    Skill {
        name: name.to_string(),
        description: name.to_string(),
        content: content.to_string(),
        file_path: format!("/skills/{name}"),
        disable_model_invocation: false,
    }
}

fn calculate_named(name: &str) -> crate::types::AgentTool {
    let mut tool = calculate_tool();
    tool.name = name.to_string();
    tool
}

fn names(tools: &[crate::types::AgentTool]) -> Vec<String> {
    tools.iter().map(|t| t.name.clone()).collect()
}

fn first_skill_name(resources: &crate::harness::events::AgentHarnessResources) -> Option<String> {
    resources
        .skills
        .as_ref()
        .and_then(|s| s.first())
        .map(|s| s.name.clone())
}
