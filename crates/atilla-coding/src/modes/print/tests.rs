//! Tests for [`run_print_mode`](super::run_print_mode).
//!
//! The offline proof-of-life drives a real completion through the provider seam
//! with a **registered faux provider** (pi's `registerFauxProvider`, the
//! provider the conformance suite drives) and no network. Both scenarios (faux
//! completion, real-builtin provider-unavailable) live in one test so the
//! process-global api registry is mutated serially.

use serde_json::Value;

use atilla_ai::providers::faux::{
    faux_assistant_message, faux_text, FauxAssistantOptions, RegisterFauxProviderOptions,
};
use atilla_ai::{register_faux_provider, reset_api_providers};

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
fn faux_reply(text: &str) -> atilla_ai::providers::faux::FauxResponseStep {
    faux_assistant_message(vec![faux_text(text)], FauxAssistantOptions::default(), 0).into()
}

/// A text-mode print options with a single initial message.
fn text_options(initial: &str) -> PrintModeOptions {
    PrintModeOptions {
        mode: PrintOutputMode::Text,
        messages: Vec::new(),
        initial_message: Some(initial.to_string()),
    }
}

#[test]
fn print_mode_drives_the_provider_seam() {
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
