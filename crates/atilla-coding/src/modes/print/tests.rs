//! Tests for [`run_print_mode`](super::run_print_mode).
//!
//! The offline proof-of-life drives a real completion through the provider seam
//! with a **registered faux provider** (pi's `registerFauxProvider`, the
//! provider the conformance suite drives) and no network. It asserts both the
//! final assistant text the harness produces and that `run_print_mode` exits 0.

use std::rc::Rc;
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde_json::Value;

use atilla_agent::harness::agent_harness::AgentHarness;
use atilla_agent::harness::env::MemoryExecutionEnv;
use atilla_agent::harness::options::AgentHarnessOptions;
use atilla_agent::harness::session::{InMemorySessionStorage, Session};
use atilla_ai::providers::faux::{
    faux_assistant_message, faux_text, FauxAssistantOptions, RegisterFauxProviderOptions,
};
use atilla_ai::{register_faux_provider, reset_api_providers};

use super::{
    builtin_models_registry, provider_stream, run_print_mode, PrintModeOptions, PrintOutputMode,
    RegistryCompaction,
};

/// The process api registry (pi's module-level map) is global, so registry-
/// touching tests must not run concurrently. Each takes this lock from a cleared
/// registry.
fn serialized() -> MutexGuard<'static, ()> {
    static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reset_api_providers();
    guard
}

/// Concatenate the text blocks of an assistant [`Value`] message.
fn assistant_text(message: &Value) -> String {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<String>()
        })
        .unwrap_or_default()
}

#[test]
fn faux_provider_completes_offline_through_the_seam() {
    let _guard = serialized();

    // Register a faux provider (offline) and queue a canned assistant response.
    let registration = register_faux_provider(RegisterFauxProviderOptions::default());
    registration.set_responses(vec![faux_assistant_message(
        vec![faux_text("hello from the faux provider")],
        FauxAssistantOptions::default(),
        0,
    )
    .into()]);
    let model = registration.get_model(None).expect("faux model");
    assert_eq!(model.api, "faux");

    // Assemble the harness against the provider seam exactly as the CLI does.
    let registry = builtin_models_registry();
    let harness = AgentHarness::new(AgentHarnessOptions {
        env: Box::new(MemoryExecutionEnv::new("/work")),
        session: Session::new(Rc::new(InMemorySessionStorage::new())),
        models: Box::new(RegistryCompaction::new(registry.clone())),
        stream: provider_stream(registry),
        tools: None,
        resources: None,
        system_prompt: None,
        stream_options: None,
        model,
        thinking_level: None,
        active_tool_names: None,
        steering_mode: None,
        follow_up_mode: None,
    })
    .expect("harness constructs");

    // Drive the completion directly to prove the offline text, then confirm
    // run_print_mode reports success on the same seam.
    let message = harness.prompt("hello", None).expect("prompt completes");
    assert_eq!(message["role"], "assistant");
    assert_eq!(assistant_text(&message), "hello from the faux provider");

    // Queue a fresh response for the run_print_mode drive on the same seam.
    registration.append_responses(vec![faux_assistant_message(
        vec![faux_text("hello from the faux provider")],
        FauxAssistantOptions::default(),
        0,
    )
    .into()]);
    let options = PrintModeOptions {
        mode: PrintOutputMode::Text,
        messages: Vec::new(),
        initial_message: Some("hello again".to_string()),
    };
    let code = run_print_mode(&harness, None, &options);
    assert_eq!(code, 0, "faux completion exits 0");

    registration.unregister();
}

#[test]
fn real_builtin_model_surfaces_provider_unavailable_error() {
    let _guard = serialized();

    // No faux provider registered: a real builtin model has no native transport,
    // so the completion must surface a terminal error (exit 1), never a panic
    // or a network call.
    let registry = builtin_models_registry();
    let model = registry
        .get_providers()
        .iter()
        .find_map(|p| p.get_models().into_iter().next())
        .expect("a builtin model exists");

    let harness = AgentHarness::new(AgentHarnessOptions {
        env: Box::new(MemoryExecutionEnv::new("/work")),
        session: Session::new(Rc::new(InMemorySessionStorage::new())),
        models: Box::new(RegistryCompaction::new(registry.clone())),
        stream: provider_stream(registry),
        tools: None,
        resources: None,
        system_prompt: None,
        stream_options: None,
        model,
        thinking_level: None,
        active_tool_names: None,
        steering_mode: None,
        follow_up_mode: None,
    })
    .expect("harness constructs");

    let message = harness.prompt("hello", None).expect("prompt returns");
    assert_eq!(message["stopReason"], "error");

    let options = PrintModeOptions {
        mode: PrintOutputMode::Text,
        messages: Vec::new(),
        initial_message: Some("hello".to_string()),
    };
    assert_eq!(run_print_mode(&harness, None, &options), 1);
}
