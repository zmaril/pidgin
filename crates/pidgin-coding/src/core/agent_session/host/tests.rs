//! Host-bridge tests: drive the four `bindCore` host-trait impls directly against
//! a session's shared state (the offline harness can build the `Send + Sync`
//! bridge but cannot run the deno runner that would call it end-to-end), plus a
//! `bind_extensions` wiring check through the test runner.

// straitjacket-allow-file:duplication

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;

use pidgin_agent::types::ThinkingLevel;

use crate::core::agent_session::events::AgentSessionEvent;
use crate::core::agent_session::test_support::{
    create_harness, events_of_type, HarnessOptions, TestExtensionRunner,
};
use crate::core::extensions::runner::{SessionContextHost, SessionControlHost};

#[test]
fn context_host_reports_idle_and_reads_agent_model_and_prompt() {
    let harness = create_harness(HarnessOptions::default());
    let bridge = harness.session.host_bridge();

    assert!(bridge.is_idle());
    assert_eq!(
        bridge.get_model().get("id").and_then(Value::as_str),
        Some("faux-1")
    );
    assert_eq!(bridge.get_system_prompt(), "You are a test assistant.");
    // With no run active, a fresh non-aborted signal is minted.
    assert!(!bridge.get_signal().is_aborted());
    assert!(!bridge.has_pending_messages());
}

#[test]
fn context_host_reflects_the_project_trust_snapshot() {
    let harness = create_harness(HarnessOptions::default());
    let bridge = harness.session.host_bridge();
    // The harness settings manager and the bridge snapshot agree.
    assert_eq!(
        bridge.is_project_trusted(),
        harness.session.settings_manager.is_project_trusted()
    );
}

#[test]
fn context_host_reports_pending_queued_messages() {
    let harness = create_harness(HarnessOptions::default());
    let bridge = harness.session.host_bridge();
    assert!(!bridge.has_pending_messages());

    // Queueing a steering message while idle pushes the UI mirror the bridge reads.
    harness.session.steer("later", None).unwrap();
    assert!(bridge.has_pending_messages());
}

#[test]
fn control_host_appends_a_custom_entry_and_emits_entry_appended() {
    let harness = create_harness(HarnessOptions::default());
    let bridge = harness.session.host_bridge();

    let entry_id = bridge.append_entry("note", &serde_json::json!({ "k": "v" }));
    assert!(!entry_id.is_empty());
    assert!(harness
        .session
        .session_manager()
        .get_entry(&entry_id)
        .is_some());
    assert_eq!(
        events_of_type(&harness, |event| matches!(
            event,
            AgentSessionEvent::EntryAppended { .. }
        )),
        1
    );
}

#[test]
fn control_host_sets_and_reads_the_session_name() {
    let harness = create_harness(HarnessOptions::default());
    let bridge = harness.session.host_bridge();
    assert_eq!(bridge.get_session_name(), None);

    bridge.set_session_name("My Session");
    assert_eq!(bridge.get_session_name().as_deref(), Some("My Session"));
}

#[test]
fn control_host_sets_the_thinking_level_on_the_agent() {
    let harness = create_harness(HarnessOptions::default());
    let bridge = harness.session.host_bridge();

    bridge.set_thinking_level(ThinkingLevel::Medium);
    assert_eq!(bridge.get_thinking_level(), ThinkingLevel::Medium);
    assert_eq!(
        harness.session.agent.thinking_level(),
        ThinkingLevel::Medium
    );
}

#[test]
fn bind_extensions_binds_core_into_the_runner() {
    let flag = Arc::new(AtomicBool::new(false));
    let runner_flag = Arc::clone(&flag);
    let harness = create_harness(HarnessOptions {
        make_runner: Some(Box::new(move |_agent| {
            Box::new(TestExtensionRunner::new().with_bind_core_flag(runner_flag))
        })),
        ..Default::default()
    });

    assert!(!flag.load(Ordering::SeqCst));
    harness.session.bind_extensions();
    assert!(flag.load(Ordering::SeqCst));
}

// ---------------------------------------------------------------------------
// dynamic-tools + dynamic-provider suites: end-to-end cases that drive a live
// extension through `createAgentSession` + `bindExtensions`. They require the
// deno-backed ExtensionRunner (v8, CI-only) plus subsystems not ported by this
// slice (the tool registry, ModelRuntime provider overrides). The host-trait
// callbacks they exercise ARE implemented here (SessionControlHost tool methods,
// ProviderRegistrationHost); only the live-driven end-to-end path is deferred.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "unit5: live deno runner + runtime tool-registry slice — registerTool via a session_start extension + `_refreshToolRegistry`/`getAllTools` are not reachable offline"]
fn refreshes_tool_registry_when_tools_are_registered_after_initialization() {}

#[test]
#[ignore = "unit5: live deno runner + runtime tool-registry slice — SDK custom-tool source metadata is built by `_refreshToolRegistry`, not ported by this slice"]
fn returns_source_metadata_for_sdk_custom_tools() {}

#[test]
#[ignore = "unit5: live deno runner + runtime tool-registry slice — promptSnippet gating of available tools is built by `_rebuildSystemPrompt`, not ported by this slice"]
fn keeps_custom_tools_active_but_omits_from_available_when_no_prompt_snippet() {}

#[test]
#[ignore = "unit5: live deno runner + provider-registration slice — top-level registerProvider overrides the !Send ModelRuntime and rebuilds the active model, not reachable offline"]
fn applies_top_level_register_provider_overrides_to_the_active_model() {}

#[test]
#[ignore = "unit5: live deno runner + provider-registration slice — session_start registerProvider + bindExtensions provider rebuild are not reachable offline"]
fn applies_session_start_register_provider_overrides_to_the_active_model() {}

#[test]
#[ignore = "unit5: live deno runner + provider-registration slice — native pi-ai provider registration during extension loading is not reachable offline"]
fn registers_native_pi_ai_providers_during_extension_loading() {}

#[test]
#[ignore = "unit5: live deno runner + provider-registration slice — command-time registerProvider without reload drives the !Send ModelRuntime, not reachable offline"]
fn applies_command_time_register_provider_overrides_without_reload() {}

#[test]
#[ignore = "unit5: live deno runner + provider-registration slice — command-time native provider registration is not reachable offline"]
fn registers_native_pi_ai_providers_at_command_time() {}
