//! Tests the additive bridge `impl compaction::Models for pidgin_ai::Models`:
//! compaction can now run against pidgin-ai's real `completeSimple` wrapper,
//! driven deterministically by a `FauxProvider` backend instead of the
//! hand-rolled `FauxModels` fake in `compaction.rs`.
//!
//! These mirror pi's models-runtime expectations at the compaction call site:
//! `completeSimple` resolves to the faux provider's final message, and
//! `generate_summary` returns that message's text through the trait bridge.

use std::sync::Arc;

use pidgin_ai::providers::faux::{
    faux_assistant_message, faux_text, FauxAssistantOptions, FauxModelDefinition, FauxProvider,
    RegisterFauxProviderOptions,
};
use pidgin_ai::{
    create_models, create_provider, ApiRouting, CreateProviderOptions, Model, MutableModels,
    StopReason,
};

use pidgin_agent::harness::compaction::{
    generate_summary, CompletionOptions, Models as CompactionModels,
};

/// Build an `pidgin_ai::Models` whose single provider streams through a
/// `FauxProvider` seeded with `responses`. Returns the collection and the faux
/// model to drive requests with. Mirrors the compaction test's
/// `createModels() + fauxProvider()` setup, but against the real wrapper.
fn faux_models(responses: Vec<pidgin_ai::AssistantMessage>) -> (pidgin_ai::Models, Model) {
    let faux = FauxProvider::new(RegisterFauxProviderOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            name: None,
            reasoning: Some(false),
            input: None,
            cost: None,
            context_window: Some(200_000),
            max_tokens: Some(16_384),
        }]),
        ..RegisterFauxProviderOptions::default()
    });
    faux.set_responses(responses.into_iter().map(Into::into));
    let model = faux.get_model(None).expect("faux model");

    let mut models = create_models();
    models.set_provider(create_provider(CreateProviderOptions {
        id: model.provider.clone(),
        name: None,
        base_url: None,
        headers: None,
        auth: pidgin_ai::ProviderAuth::default(),
        models: vec![model.clone()],
        fetch_models: None,
        api: ApiRouting::Single(Arc::new(faux)),
    }));
    (models, model)
}

fn text_message(text: &str) -> pidgin_ai::AssistantMessage {
    faux_assistant_message(
        vec![faux_text(text.to_string())],
        FauxAssistantOptions::default(),
        0,
    )
}

/// The bridge exposes pidgin-ai's `completeSimple` as compaction's
/// `complete_simple`: it returns the faux provider's final message.
#[test]
fn complete_simple_bridges_to_pidgin_ai() {
    let (models, model) = faux_models(vec![text_message("bridged summary")]);
    let dyn_models: &dyn CompactionModels = &models;

    let message = dyn_models.complete_simple(
        &model,
        &pidgin_ai::Context::default(),
        &CompletionOptions::default(),
    );

    assert_eq!(message.stop_reason, StopReason::Stop);
    // The faux provider streams text deltas; the accumulated message carries the
    // queued text verbatim.
    let text: String = message
        .content
        .iter()
        .filter_map(|block| match block {
            pidgin_ai::ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "bridged summary");
}

/// Compaction's `generate_summary` runs end-to-end against the real wrapper: the
/// faux response text is returned as the summary.
#[test]
fn generate_summary_runs_through_the_bridge() {
    let (models, model) = faux_models(vec![text_message("## Goal\nBridge works")]);

    let summary = generate_summary(
        &[],     // empty history is enough to exercise the model call
        &models, // the pidgin-ai wrapper, via the new trait impl
        &model,
        16_384, // reserve tokens
        None,   // signal
        None,   // custom instructions
        None,   // previous summary
        None,   // thinking level
    )
    .expect("summary generated");

    assert_eq!(summary, "## Goal\nBridge works");
}

/// An unconfigured/unknown provider surfaces as a summarization failure rather
/// than a panic: the bridge forwards pidgin-ai's error stream.
#[test]
fn generate_summary_maps_unknown_provider_to_error() {
    let models = create_models();
    let mut model = {
        let (_m, model) = faux_models(vec![]);
        model
    };
    model.provider = "ghost".to_string();

    let error = generate_summary(&[], &models, &model, 16_384, None, None, None, None)
        .expect_err("unknown provider fails");
    assert!(error.to_string().contains("Unknown provider: ghost"));
}
