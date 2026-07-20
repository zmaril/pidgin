//! Tests for [`run_print_mode`](super::run_print_mode).
//!
//! The offline proof-of-life drives a real completion through the provider seam
//! with a **registered faux provider** (pi's `registerFauxProvider`, the
//! provider the conformance suite drives) and no network. Both scenarios (faux
//! completion, real-builtin provider-unavailable) live in one test so the
//! process-global api registry is mutated serially.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use serde_json::Value;

use pidgin_ai::providers::faux::{
    faux_assistant_message, faux_text, FauxAssistantOptions, FauxResponseStep,
    RegisterFauxProviderOptions,
};
use pidgin_ai::{register_faux_provider, reset_api_providers};

use super::{
    build_harness, builtin_models_registry, run_print_mode, PrintModeOptions, PrintOutputMode,
};

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

/// A single canned faux assistant response.
fn faux_reply(text: &str) -> pidgin_ai::providers::faux::FauxResponseStep {
    faux_assistant_message(vec![faux_text(text)], FauxAssistantOptions::default(), 0).into()
}

/// Serialize the tests that mutate the process-global api provider registry
/// (`register_faux_provider` / `reset_api_providers`), so cargo's parallel test
/// threads cannot interleave their registrations. Mirrors compat.rs's test lock.
fn serialized() -> MutexGuard<'static, ()> {
    static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A text-mode print options with a single initial message.
fn text_options(initial: &str) -> PrintModeOptions {
    PrintModeOptions {
        mode: PrintOutputMode::Text,
        messages: Vec::new(),
        initial_message: Some(initial.to_string()),
    }
}

/// The tool name of each entry in a captured `Context.tools` value. Each tool
/// is projected to `{name, description, parameters}` by the agent loop's
/// `context_tools`, so the `name` key is the stable assertion target.
fn tool_names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

#[test]
fn print_harness_sends_coding_tools_in_the_request() {
    let _guard = serialized();
    // The api registry is process-global; start from a clean slate.
    reset_api_providers();

    // Capture the request context the provider actually receives, so the
    // assertion sees the exact `tools` array reaching `build_params`.
    let captured_tools: Arc<Mutex<Option<Vec<Value>>>> = Arc::new(Mutex::new(None));
    let captured_system: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let registration = register_faux_provider(RegisterFauxProviderOptions::default());
    let tools_slot = Arc::clone(&captured_tools);
    let system_slot = Arc::clone(&captured_system);
    registration.set_responses(vec![FauxResponseStep::Factory(Box::new(
        move |context, _options, _state, _model| {
            *tools_slot.lock().unwrap() = context.tools.clone();
            *system_slot.lock().unwrap() = context.system_prompt.clone();
            faux_assistant_message(vec![faux_text("done")], FauxAssistantOptions::default(), 0)
        },
    ))]);
    let faux_model = registration.get_model(None).expect("faux model");

    let harness =
        build_harness(faux_model, "/work", builtin_models_registry()).expect("harness constructs");
    harness
        .prompt("Use the read tool to read foo", None)
        .expect("prompt completes");

    registration.unregister();

    // The request must carry a non-empty `tools` array (the drop bug left it
    // empty, so `build_params` omitted the key and the model hallucinated a
    // fake tool call). The default coding tool set is read, bash, edit, write.
    let tools = captured_tools
        .lock()
        .unwrap()
        .clone()
        .expect("provider received a tools array");
    assert_eq!(
        tool_names(&tools),
        ["read", "bash", "edit", "write"],
        "print mode attaches pi's default coding tool set"
    );

    // The coding system prompt reaches the request too.
    let system = captured_system
        .lock()
        .unwrap()
        .clone()
        .expect("provider received a system prompt");
    assert!(
        system.contains("expert coding assistant"),
        "print mode attaches the coding system prompt"
    );
    assert!(
        system.contains("- read: Read file contents"),
        "the system prompt lists the read tool snippet"
    );
}

#[test]
fn print_mode_drives_the_provider_seam() {
    let _guard = serialized();
    // The api registry is process-global; start from a clean slate.
    reset_api_providers();

    // --- Offline proof-of-life: a registered faux provider completes. ---
    let registration = register_faux_provider(RegisterFauxProviderOptions::default());
    registration.set_responses(vec![
        faux_reply("hello from the faux provider"),
        faux_reply("hello from the faux provider"),
    ]);
    let faux_model = registration.get_model(None).expect("faux model");
    assert_eq!(faux_model.api, "faux");

    let harness =
        build_harness(faux_model, "/work", builtin_models_registry()).expect("harness constructs");

    let message = harness.prompt("hello", None).expect("prompt completes");
    assert_eq!(message["role"], "assistant");
    assert_eq!(assistant_text(&message), "hello from the faux provider");

    let code = run_print_mode(&harness, None, &text_options("hello again"));
    assert_eq!(code, 0, "faux completion exits 0");

    registration.unregister();

    // --- Real builtin model: no native transport, so a terminal error. ---
    let registry = builtin_models_registry();
    let real_model = registry
        .get_providers()
        .iter()
        .find_map(|p| p.get_models().into_iter().next())
        .expect("a builtin model exists");

    let harness = build_harness(real_model, "/work", registry).expect("harness constructs");

    let message = harness.prompt("hello", None).expect("prompt returns");
    assert_eq!(message["stopReason"], "error");
    assert_eq!(run_print_mode(&harness, None, &text_options("hello")), 1);
}
