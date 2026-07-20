//! Model- and thinking-level management tests, ported from pi's
//! `test/suite/agent-session-model-extension.test.ts` (the model-management
//! cases: `setModel`, scoped `cycleModel`, thinking-level clamping / cycling, and
//! the configured-auth gate).
//!
//! Each `#[test]` mirrors a pi characterization case using the shared in-memory
//! harness ([`super::super::test_support`]): a multi-model faux provider, a
//! recording [`TestExtensionRunner`], and the session-event sink. No case is
//! `#[ignore]`d — every scenario is reachable under the sync/eager,
//! session-actor model.

// straitjacket-allow-file:duplication

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use pidgin_agent::types::ThinkingLevel;
use pidgin_ai::ThinkingLevel as RequestThinkingLevel;

use crate::core::agent_session::events::AgentSessionEvent;
use crate::core::agent_session::test_support::TestExtensionRunner;
use crate::core::agent_session::test_support::{create_harness, HarnessModel, HarnessOptions};
use crate::core::agent_session::ScopedModel;

/// The `model_change` session entries as `"<provider>/<modelId>"`, in order.
fn model_change_entries(
    harness: &crate::core::agent_session::test_support::Harness,
) -> Vec<String> {
    harness
        .session
        .session_manager()
        .get_entries()
        .iter()
        .filter_map(|entry| serde_json::to_value(entry).ok())
        .filter(|value| value.get("type").and_then(Value::as_str) == Some("model_change"))
        .map(|value| {
            format!(
                "{}/{}",
                value.get("provider").and_then(Value::as_str).unwrap_or(""),
                value.get("modelId").and_then(Value::as_str).unwrap_or(""),
            )
        })
        .collect()
}

#[test]
fn set_model_saves_the_model_and_emits_model_select() {
    let model_events = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&model_events);
    let mut harness = create_harness(HarnessOptions {
        models: vec![
            HarnessModel::new("faux-1", true),
            HarnessModel::new("faux-2", true),
        ],
        make_runner: Some(Box::new(move |_agent| {
            Box::new(TestExtensionRunner::new().with_model_select_recording(sink))
        })),
        ..Default::default()
    });
    let next_model = harness.get_model("faux-2").expect("faux-2 is known");

    harness
        .session
        .set_model(next_model.clone())
        .expect("set_model succeeds with configured auth");

    assert_eq!(
        harness.session.model().map(|model| model.id),
        Some("faux-2".to_string())
    );
    assert_eq!(
        *model_events.lock().unwrap(),
        vec!["faux-1->faux-2:set".to_string()]
    );
    assert_eq!(
        model_change_entries(&harness),
        vec![format!("{}/{}", next_model.provider, next_model.id)]
    );
}

#[test]
fn cycles_through_scoped_models_and_preserves_the_scoped_thinking_preference() {
    let mut harness = create_harness(HarnessOptions {
        models: vec![
            HarnessModel::new("faux-1", true),
            HarnessModel::new("faux-2", false),
        ],
        ..Default::default()
    });
    let model_one = harness.get_model("faux-1").expect("faux-1 is known");
    let model_two = harness.get_model("faux-2").expect("faux-2 is known");
    harness.session.set_scoped_models(vec![
        ScopedModel {
            model: model_one,
            thinking_level: Some(RequestThinkingLevel::High),
        },
        ScopedModel {
            model: model_two,
            thinking_level: None,
        },
    ]);
    harness.session.set_thinking_level(ThinkingLevel::High);

    let first = harness
        .session
        .cycle_model(super::CycleDirection::Forward)
        .expect("scoped cycle returns a result");
    assert_eq!(first.model.id, "faux-2");
    assert!(first.is_scoped);
    assert_eq!(
        harness.session.model().map(|model| model.id),
        Some("faux-2".to_string())
    );
    // faux-2 is non-reasoning, so `high` clamps to `off`.
    assert_eq!(harness.session.thinking_level(), ThinkingLevel::Off);

    let second = harness
        .session
        .cycle_model(super::CycleDirection::Forward)
        .expect("scoped cycle returns a result");
    assert_eq!(second.model.id, "faux-1");
    assert_eq!(
        harness.session.model().map(|model| model.id),
        Some("faux-1".to_string())
    );
    // faux-1's explicit scoped `high` preference is restored.
    assert_eq!(harness.session.thinking_level(), ThinkingLevel::High);
}

#[test]
fn clamps_thinking_levels_to_model_capabilities_and_cycles_available_levels() {
    let mut harness = create_harness(HarnessOptions {
        models: vec![HarnessModel::new("faux-1", false)],
        ..Default::default()
    });

    // A non-reasoning model supports only `off`, so `high` clamps down.
    harness.session.set_thinking_level(ThinkingLevel::High);
    assert_eq!(harness.session.thinking_level(), ThinkingLevel::Off);
    assert_eq!(harness.session.cycle_thinking_level(), None);
}

#[test]
fn cycles_xhigh_before_max_when_both_are_supported() {
    let mut harness = create_harness(HarnessOptions {
        models: vec![HarnessModel::new("faux-1", true)],
        ..Default::default()
    });
    // Mutate the active model to advertise `xhigh` and `max`, mirroring pi's
    // `harness.getModel().thinkingLevelMap = { xhigh, max }`.
    let mut model = harness.get_model("faux-1").expect("faux-1 is known");
    let mut map: BTreeMap<ThinkingLevel, Option<String>> = BTreeMap::new();
    map.insert(ThinkingLevel::Xhigh, Some("xhigh".to_string()));
    map.insert(ThinkingLevel::Max, Some("max".to_string()));
    model.thinking_level_map = Some(map);
    harness.agent.set_model(model);

    assert_eq!(
        harness.session.get_available_thinking_levels(),
        vec![
            ThinkingLevel::Off,
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::Xhigh,
            ThinkingLevel::Max,
        ]
    );
    harness.session.set_thinking_level(ThinkingLevel::High);
    assert_eq!(
        harness.session.cycle_thinking_level(),
        Some(ThinkingLevel::Xhigh)
    );
    assert_eq!(
        harness.session.cycle_thinking_level(),
        Some(ThinkingLevel::Max)
    );
    assert_eq!(
        harness.session.cycle_thinking_level(),
        Some(ThinkingLevel::Off)
    );
}

#[test]
fn throws_when_set_model_is_called_without_configured_auth() {
    let mut harness = create_harness(HarnessOptions {
        models: vec![
            HarnessModel::new("faux-1", true),
            HarnessModel::new("faux-2", true),
        ],
        with_configured_auth: false,
        ..Default::default()
    });
    let next_model = harness.get_model("faux-2").expect("faux-2 is known");
    let default_provider = harness.default_model().provider;

    let error = harness
        .session
        .set_model(next_model)
        .expect_err("set_model rejects an unconfigured provider");
    assert_eq!(
        error.to_string(),
        format!("No API key for {default_provider}/faux-2")
    );
}

#[test]
fn set_thinking_level_emits_changed_and_dispatches_thinking_select() {
    let thinking_events = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&thinking_events);
    let mut harness = create_harness(HarnessOptions {
        models: vec![HarnessModel::new("faux-1", true)],
        make_runner: Some(Box::new(move |_agent| {
            Box::new(TestExtensionRunner::new().with_thinking_select_recording(sink))
        })),
        ..Default::default()
    });

    harness.session.set_thinking_level(ThinkingLevel::High);

    assert_eq!(harness.session.thinking_level(), ThinkingLevel::High);
    let changed: Vec<ThinkingLevel> = harness
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            AgentSessionEvent::ThinkingLevelChanged { level } => Some(*level),
            _ => None,
        })
        .collect();
    assert_eq!(changed, vec![ThinkingLevel::High]);
    assert_eq!(
        *thinking_events.lock().unwrap(),
        vec!["off->high".to_string()]
    );
}
