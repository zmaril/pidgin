//! Compaction tests, ported from pi's
//! `test/suite/agent-session-compaction.test.ts` (14 cases) and
//! `test/agent-session-auto-compaction-queue.test.ts` (6 cases), plus the
//! `describe.skipIf(!API_KEY)` e2e suite `test/agent-session-compaction.test.ts`
//! (5 cases, represented as `#[ignore]`d — they call a real LLM and pi skips them
//! without `API_KEY`).
//!
//! Each `#[test]` mirrors a pi case over the in-memory harness in
//! [`super::super::test_support`]. pi forces the compaction decision by seeding
//! `agent.state.messages` / the session manager with usage-carrying assistant
//! messages and overriding the compaction threshold; where a pi case spies on
//! `_runAutoCompaction`, the Rust port asserts the equivalent
//! [`CompactionPlan`](super::CompactionPlan) the decision produces (the same
//! choice the spy observed) or the observable `compaction_start`/`compaction_end`
//! sequence.
//!
//! The summarization provider is threaded as a [`Models`](crate::core::compaction::Models)
//! seam (pi passes `agent.streamFn`): the [`SummaryModels`] harness seam is the
//! "custom `streamFn`" analog. Cases whose premise needs genuine mid-compaction
//! concurrency (cancelling a compaction whose extension handler *awaits* the abort
//! signal) or a real LLM are `#[ignore]`d with a precise reason.

// straitjacket-allow-file:duplication

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use atilla_ai::providers::faux::{faux_assistant_message, FauxAssistantOptions};
use atilla_ai::{AssistantMessage, Model, StopReason, Usage, UsageCost};

use crate::core::agent_session::events::AgentSessionEvent;
use crate::core::agent_session::test_support::{
    create_harness, faux_model, text_block, BeforeCompactHandler, FauxResponse, Harness,
    HarnessOptions, SummaryModels, TestExtensionRunner,
};
use crate::core::extensions::events::session::{CompactionReason, SessionBeforeCompactResult};
use crate::core::session_manager::SessionEntry;
use crate::core::settings_manager::Settings;

use super::CompactionPlan;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A millisecond wall-clock timestamp (pi's `Date.now()`).
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A usage block reporting `total` context tokens (pi's `createUsage`).
fn usage_with_total(total: u64) -> Usage {
    Usage {
        input: total,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: total,
        cost: UsageCost::default(),
    }
}

/// Options for [`assistant_value`] (pi's `createAssistant` inputs).
#[derive(Default)]
struct AssistantOpts<'a> {
    stop_reason: Option<StopReason>,
    error_message: Option<&'a str>,
    total_tokens: u64,
    timestamp: i64,
    text: Option<&'a str>,
}

/// Build an assistant [`AgentMessage`](atilla_agent::types::AgentMessage) value
/// stamped with the harness model's `api`/`provider`/`model` (pi's
/// `createAssistant`), carrying `usage`, `stopReason`, `timestamp`, and text
/// content.
fn assistant_value(model: &Model, opts: AssistantOpts) -> Value {
    let content = match opts.text {
        Some(text) => vec![text_block(text)],
        None => Vec::new(),
    };
    let mut message: AssistantMessage = faux_assistant_message(
        content,
        FauxAssistantOptions {
            stop_reason: opts.stop_reason,
            error_message: opts.error_message.map(str::to_string),
            timestamp: Some(opts.timestamp),
            ..Default::default()
        },
        0,
    );
    message.api = model.api.clone();
    message.provider = model.provider.clone();
    message.model = model.id.clone();
    message.usage = usage_with_total(opts.total_tokens);
    serde_json::to_value(message).expect("assistant value")
}

/// A `{ role: "user", content: [text], timestamp }` message value.
fn user_value(text: &str, timestamp: i64) -> Value {
    json!({
        "role": "user",
        "content": [{ "type": "text", "text": text }],
        "timestamp": timestamp,
    })
}

/// A plain-text assistant response stamped with the harness model
/// (`fauxAssistantMessage("text")`), for scripted prompt turns.
fn assistant_plain(text: &str) -> AssistantMessage {
    let model = faux_model();
    let mut message =
        faux_assistant_message(vec![text_block(text)], FauxAssistantOptions::default(), 0);
    message.api = model.api.clone();
    message.provider = model.provider.clone();
    message.model = model.id.clone();
    message
}

/// The `faux` model with its context window overridden (pi's
/// `createHarness({ models: [{ contextWindow }] })`).
fn faux_model_with_window(context_window: u64) -> Model {
    Model {
        context_window,
        ..faux_model()
    }
}

/// A `{ compaction: { enabled?, reserveTokens?, keepRecentTokens? } }` settings
/// override (pi's `createHarness({ settings: { compaction } })`).
fn compaction_override(
    enabled: Option<bool>,
    reserve_tokens: Option<i64>,
    keep_recent_tokens: Option<i64>,
) -> Settings {
    let mut compaction = serde_json::Map::new();
    if let Some(enabled) = enabled {
        compaction.insert("enabled".to_string(), json!(enabled));
    }
    if let Some(reserve_tokens) = reserve_tokens {
        compaction.insert("reserveTokens".to_string(), json!(reserve_tokens));
    }
    if let Some(keep_recent_tokens) = keep_recent_tokens {
        compaction.insert("keepRecentTokens".to_string(), json!(keep_recent_tokens));
    }
    let mut map = serde_json::Map::new();
    map.insert("compaction".to_string(), Value::Object(compaction));
    Settings::from_map(map)
}

/// A `session_before_compact` handler that supplies a replacement compaction whose
/// `summary` is `summary` and whose `firstKeptEntryId`/`tokensBefore` echo the
/// event's preparation (pi's example extension in the suite).
fn extension_summary_handler(summary: &'static str) -> BeforeCompactHandler {
    Arc::new(move |event| {
        let first_kept = event
            .preparation
            .get("firstKeptEntryId")
            .cloned()
            .unwrap_or(Value::Null);
        let tokens_before = event
            .preparation
            .get("tokensBefore")
            .cloned()
            .unwrap_or(json!(0));
        SessionBeforeCompactResult {
            cancel: None,
            compaction: Some(json!({
                "summary": summary,
                "firstKeptEntryId": first_kept,
                "tokensBefore": tokens_before,
                "details": {},
            })),
        }
    })
}

/// Seed a small compactable conversation into the session and mirror it into agent
/// state (pi's `seedCompactableSession`).
fn seed_compactable_session(harness: &Harness) {
    let model = harness.session.model().expect("model");
    let now = now_ms();
    let context_messages = {
        let mut manager = harness.session.session_manager();
        manager.append_message(user_value("message to compact", now - 1000));
        manager.append_message(assistant_value(
            &model,
            AssistantOpts {
                stop_reason: Some(StopReason::Stop),
                total_tokens: 100,
                timestamp: now - 500,
                text: Some("assistant response to compact"),
                ..Default::default()
            },
        ));
        manager.build_session_context().messages
    };
    harness.session.agent.set_messages(context_messages);
}

/// The number of `compaction` entries in the session (pi's
/// `sessionManager.getEntries().filter((e) => e.type === "compaction")`).
fn compaction_entry_count(harness: &Harness) -> usize {
    harness
        .session
        .session_manager()
        .get_entries()
        .iter()
        .filter(|entry| matches!(entry, SessionEntry::Compaction(_)))
        .count()
}

/// The recorded `compaction_start`/`compaction_end` events, in order.
fn compaction_events(harness: &Harness) -> Vec<AgentSessionEvent> {
    harness
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentSessionEvent::CompactionStart { .. } | AgentSessionEvent::CompactionEnd { .. }
            )
        })
        .cloned()
        .collect()
}

/// The `error_message`s of every `compaction_end` event.
fn compaction_end_errors(harness: &Harness) -> Vec<String> {
    harness
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            AgentSessionEvent::CompactionEnd {
                error_message: Some(message),
                ..
            } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// suite/agent-session-compaction.test.ts (14 cases)
// ---------------------------------------------------------------------------

#[test]
fn manually_compacts_using_an_extension_provided_summary() {
    let harness = create_harness(HarnessOptions {
        settings: Some(compaction_override(None, None, Some(1))),
        make_runner: Some(Box::new(|_agent| {
            Box::new(
                TestExtensionRunner::new()
                    .with_before_compact(extension_summary_handler("summary from extension")),
            )
        })),
        ..Default::default()
    });
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_plain("one"))),
        FauxResponse::Message(Box::new(assistant_plain("two"))),
    ]);

    harness
        .session
        .prompt("one", None, None)
        .expect("prompt one");
    harness
        .session
        .prompt("two", None, None)
        .expect("prompt two");

    let result = harness.session.compact(None).expect("compact");

    assert_eq!(result.summary, "summary from extension");
    assert_eq!(compaction_entry_count(&harness), 1);
    assert_eq!(
        harness.session.messages()[0]
            .get("role")
            .and_then(Value::as_str),
        Some("compactionSummary"),
    );
}

#[test]
fn throws_when_compacting_without_a_model() {
    let harness = create_harness(HarnessOptions {
        with_model: false,
        ..Default::default()
    });

    let error = harness.session.compact(None).expect_err("no model");
    assert!(
        error.to_string().contains("No model selected"),
        "got: {error}"
    );
}

#[test]
fn throws_when_compacting_without_configured_auth() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: false,
        ..Default::default()
    });

    let provider = harness.session.model().expect("model").provider;
    let error = harness.session.compact(None).expect_err("no auth");
    assert!(
        error
            .to_string()
            .contains(&format!("No API key found for {provider}.")),
        "got: {error}"
    );
}

#[test]
fn manually_compacts_with_a_custom_stream_fn_when_registry_auth_absent() {
    let (models, calls) = SummaryModels::build("summary from custom stream");
    let harness = create_harness(HarnessOptions {
        with_configured_auth: false,
        settings: Some(compaction_override(None, None, Some(1))),
        summarization_models: Some(models),
        ..Default::default()
    });
    seed_compactable_session(&harness);

    let result = harness.session.compact(None).expect("compact");

    assert!(
        result.summary.contains("summary from custom stream"),
        "got: {}",
        result.summary
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn auto_compacts_with_a_custom_stream_fn_when_registry_auth_absent() {
    let (models, calls) = SummaryModels::build("auto summary from custom stream");
    let harness = create_harness(HarnessOptions {
        with_configured_auth: false,
        settings: Some(compaction_override(None, None, Some(1))),
        summarization_models: Some(models),
        ..Default::default()
    });
    seed_compactable_session(&harness);

    let continued = harness
        .session
        .run_auto_compaction(CompactionReason::Threshold, false);

    assert!(!continued);
    assert_eq!(compaction_entry_count(&harness), 1);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    // pi asserts `compaction_end.result.estimatedTokensAfter > 0`; the ported
    // `CompactionResult` omits `estimatedTokensAfter` (documented on the type), so
    // assert the terminal `compaction_end` carries a result instead.
    let last = compaction_events(&harness).pop().expect("compaction_end");
    assert!(matches!(
        last,
        AgentSessionEvent::CompactionEnd {
            result: Some(_),
            aborted: false,
            ..
        }
    ));
}

#[test]
#[ignore = "pi cancels a manual compaction whose session_before_compact handler \
            AWAITS the abort signal, then calls abortCompaction() from another \
            task; the sync/eager !Send model runs the extension emit inline with no \
            way to trip the signal mid-handler (see module docs)"]
fn cancels_in_progress_manual_compaction_when_abort_compaction_is_called() {}

#[test]
fn resumes_after_threshold_compaction_when_only_agent_queued_messages_exist() {
    let harness = create_harness(HarnessOptions {
        settings: Some(compaction_override(None, None, Some(1))),
        make_runner: Some(Box::new(|_agent| {
            Box::new(
                TestExtensionRunner::new()
                    .with_before_compact(extension_summary_handler("auto compacted")),
            )
        })),
        ..Default::default()
    });
    harness.set_responses(vec![
        FauxResponse::Message(Box::new(assistant_plain("one"))),
        FauxResponse::Message(Box::new(assistant_plain("two"))),
    ]);
    harness
        .session
        .prompt("first", None, None)
        .expect("prompt first");
    harness
        .session
        .prompt("second", None, None)
        .expect("prompt second");

    // A custom message queued directly on the agent (pi's `agent.followUp`).
    harness.agent.follow_up(json!({
        "role": "custom",
        "customType": "test",
        "content": [{ "type": "text", "text": "queued custom" }],
        "display": false,
        "timestamp": now_ms(),
    }));

    let continued = harness
        .session
        .run_auto_compaction(CompactionReason::Threshold, false);
    assert!(continued);
}

#[test]
fn does_not_retry_overflow_recovery_more_than_once() {
    let harness = create_harness(HarnessOptions::default());
    let model = harness.session.model().expect("model");
    let overflow = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Error),
            error_message: Some("prompt is too long"),
            timestamp: now_ms(),
            ..Default::default()
        },
    );

    // The session manager is empty, so `run_auto_compaction` no-ops (prepareCompaction
    // returns None) — the first check sets the one-shot guard without emitting; the
    // second short-circuits to the "recovery failed" compaction_end (pi spies
    // `_runAutoCompaction` to observe the single attempt).
    harness.session.check_compaction(&overflow, true);
    let overflow2 = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Error),
            error_message: Some("prompt is too long"),
            timestamp: now_ms() + 1,
            ..Default::default()
        },
    );
    harness.session.check_compaction(&overflow2, true);

    assert_eq!(
        compaction_end_errors(&harness),
        vec![super::OVERFLOW_RECOVERY_EXHAUSTED.to_string()],
    );
    // The first attempt short-circuited before `compaction_start` (empty session),
    // and the second never re-entered the run.
    assert!(!compaction_events(&harness)
        .iter()
        .any(|event| matches!(event, AgentSessionEvent::CompactionStart { .. })));
}

#[test]
fn compacts_successful_overflow_responses_without_retrying() {
    let harness = create_harness(HarnessOptions {
        settings: Some(compaction_override(Some(true), Some(0), Some(1))),
        make_runner: Some(Box::new(|_agent| {
            Box::new(
                TestExtensionRunner::new().with_before_compact(extension_summary_handler(
                    "successful overflow compacted",
                )),
            )
        })),
        ..Default::default()
    });
    // A one-token context window makes any usage a silent overflow.
    harness.agent.set_model(faux_model_with_window(1));
    let model = harness.session.model().expect("model");
    // A completed answer whose reported usage exceeds the window (pi's silent
    // overflow): stopReason "stop" → compact but do not retry.
    let response: AssistantMessage = serde_json::from_value(assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Stop),
            total_tokens: 100,
            timestamp: now_ms(),
            text: Some("completed answer"),
            ..Default::default()
        },
    ))
    .expect("assistant message");
    harness.set_responses(vec![FauxResponse::Message(Box::new(response))]);

    harness.session.prompt("hello", None, None).expect("prompt");

    let last = compaction_events(&harness).pop().expect("compaction_end");
    assert!(
        matches!(
            last,
            AgentSessionEvent::CompactionEnd {
                reason: CompactionReason::Overflow,
                aborted: false,
                will_retry: false,
                ..
            }
        ),
        "unexpected terminal compaction event"
    );
    // Only the original prompt streamed; there was no retry request.
    assert_eq!(harness.call_count(), 1);
}

#[test]
fn ignores_stale_pre_compaction_assistant_usage_on_pre_prompt_checks() {
    let harness = create_harness(HarnessOptions::default());
    let model = harness.session.model().expect("model");
    let stale_ts = now_ms() - 10_000;
    let stale = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Stop),
            total_tokens: 610_000,
            timestamp: stale_ts,
            ..Default::default()
        },
    );

    {
        let mut manager = harness.session.session_manager();
        manager.append_message(user_value("before compaction", stale_ts - 1000));
        manager.append_message(stale.clone());
        let id = manager.get_entries()[0].id().to_string();
        manager.append_compaction("summary", &id, 610_000, None, Some(false));
        manager.append_message(user_value("after compaction", now_ms()));
    }

    // Stale assistant is older than the compaction boundary → no compaction.
    assert_eq!(
        harness.session.compaction_plan(&stale, false),
        CompactionPlan::None,
    );
}

#[test]
fn triggers_threshold_compaction_for_error_messages_using_the_last_successful_usage() {
    let harness = create_harness(HarnessOptions::default());
    let model = harness.session.model().expect("model");
    let successful = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Stop),
            total_tokens: 190_000,
            timestamp: now_ms(),
            ..Default::default()
        },
    );
    let error = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Error),
            error_message: Some("529 overloaded"),
            timestamp: now_ms() + 1000,
            ..Default::default()
        },
    );
    harness.agent.set_messages(vec![
        user_value("hello", now_ms() - 1000),
        successful,
        user_value("retry", now_ms() + 500),
        error.clone(),
    ]);

    assert_eq!(
        harness.session.compaction_plan(&error, true),
        CompactionPlan::Run {
            reason: CompactionReason::Threshold,
            will_retry: false,
            set_overflow_guard: false,
        },
    );
}

#[test]
fn does_not_trigger_threshold_compaction_for_error_messages_when_no_prior_usage_exists() {
    let harness = create_harness(HarnessOptions::default());
    let model = harness.session.model().expect("model");
    let error = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Error),
            error_message: Some("529 overloaded"),
            timestamp: now_ms(),
            ..Default::default()
        },
    );
    harness
        .agent
        .set_messages(vec![user_value("hello", now_ms() - 1000), error.clone()]);

    assert_eq!(
        harness.session.compaction_plan(&error, true),
        CompactionPlan::None,
    );
}

#[test]
fn does_not_trigger_threshold_compaction_when_only_kept_pre_compaction_usage_exists() {
    let harness = create_harness(HarnessOptions::default());
    let model = harness.session.model().expect("model");
    let pre_ts = now_ms() - 10_000;
    let kept = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Stop),
            total_tokens: 190_000,
            timestamp: pre_ts,
            ..Default::default()
        },
    );

    {
        let mut manager = harness.session.session_manager();
        manager.append_message(user_value("before compaction", pre_ts - 1000));
        manager.append_message(kept.clone());
        let id = manager.get_entries()[0].id().to_string();
        manager.append_compaction("summary", &id, 190_000, None, Some(false));
    }

    let error = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Error),
            error_message: Some("529 overloaded"),
            timestamp: now_ms(),
            ..Default::default()
        },
    );
    harness.agent.set_messages(vec![
        user_value("kept user", pre_ts - 1000),
        kept,
        user_value("new prompt", now_ms() - 500),
        error.clone(),
    ]);

    assert_eq!(
        harness.session.compaction_plan(&error, true),
        CompactionPlan::None,
    );
}

#[test]
fn does_not_trigger_threshold_compaction_below_the_threshold_or_when_disabled() {
    // Below threshold: a large context window keeps a small response under the cap.
    let below = create_harness(HarnessOptions {
        settings: Some(compaction_override(Some(true), Some(1000), None)),
        ..Default::default()
    });
    below.agent.set_model(faux_model_with_window(200_000));
    let below_model = below.session.model().expect("model");
    let below_msg = assistant_value(
        &below_model,
        AssistantOpts {
            stop_reason: Some(StopReason::Stop),
            total_tokens: 1_000,
            timestamp: now_ms(),
            ..Default::default()
        },
    );
    assert_eq!(
        below.session.compaction_plan(&below_msg, true),
        CompactionPlan::None,
    );

    // Disabled: even a huge context does not compact.
    let disabled = create_harness(HarnessOptions {
        settings: Some(compaction_override(Some(false), None, None)),
        ..Default::default()
    });
    let disabled_model = disabled.session.model().expect("model");
    let disabled_msg = assistant_value(
        &disabled_model,
        AssistantOpts {
            stop_reason: Some(StopReason::Stop),
            total_tokens: 1_000_000,
            timestamp: now_ms(),
            ..Default::default()
        },
    );
    assert_eq!(
        disabled.session.compaction_plan(&disabled_msg, true),
        CompactionPlan::None,
    );
}

// ---------------------------------------------------------------------------
// agent-session-compaction.test.ts — describe.skipIf(!API_KEY) e2e (5 cases)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "pi e2e (describe.skipIf(!API_KEY)): drives real anthropic LLM turns \
            then compact(); skipped without an API key. No offline faux equivalent \
            for the live summarization/tool turns it exercises"]
fn e2e_should_trigger_manual_compaction_via_compact() {}

#[test]
#[ignore = "pi e2e (describe.skipIf(!API_KEY)): real LLM turns; skipped without \
            an API key"]
fn e2e_should_maintain_valid_session_state_after_compaction() {}

#[test]
#[ignore = "pi e2e (describe.skipIf(!API_KEY)): real LLM turns; skipped without \
            an API key"]
fn e2e_should_persist_compaction_to_session_file() {}

#[test]
#[ignore = "pi e2e (describe.skipIf(!API_KEY)): real LLM turns; skipped without \
            an API key"]
fn e2e_should_work_with_no_session_mode_in_memory_only() {}

#[test]
#[ignore = "pi e2e (describe.skipIf(!API_KEY)): real LLM turns; skipped without \
            an API key"]
fn e2e_should_emit_compaction_events_during_manual_compaction() {}

// ---------------------------------------------------------------------------
// agent-session-auto-compaction-queue.test.ts (6 cases)
// ---------------------------------------------------------------------------

#[test]
fn queue_resumes_after_threshold_compaction_when_only_agent_queued_messages_exist() {
    // Same scenario as the suite "resume" case but driven through the summarization
    // Models seam (pi overrides `agent.streamFn` here instead of using an extension).
    let (models, _calls) = SummaryModels::build("compacted");
    let harness = create_harness(HarnessOptions {
        settings: Some(compaction_override(None, None, Some(1))),
        summarization_models: Some(models),
        ..Default::default()
    });
    seed_compactable_session(&harness);

    harness.agent.follow_up(json!({
        "role": "custom",
        "customType": "test",
        "content": [{ "type": "text", "text": "Queued custom" }],
        "display": false,
        "timestamp": now_ms(),
    }));

    assert_eq!(harness.session.pending_message_count(), 0);
    assert!(harness.agent.has_queued_messages());

    let continued = harness
        .session
        .run_auto_compaction(CompactionReason::Threshold, false);
    assert!(continued);
}

#[test]
fn queue_should_not_compact_repeatedly_after_overflow_recovery_already_attempted() {
    let harness = create_harness(HarnessOptions::default());
    let model = harness.session.model().expect("model");
    let overflow = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Error),
            error_message: Some("prompt is too long"),
            timestamp: now_ms(),
            ..Default::default()
        },
    );

    harness.session.check_compaction(&overflow, true);
    let overflow2 = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Error),
            error_message: Some("prompt is too long"),
            timestamp: now_ms() + 1,
            ..Default::default()
        },
    );
    harness.session.check_compaction(&overflow2, true);

    assert_eq!(
        compaction_end_errors(&harness),
        vec![super::OVERFLOW_RECOVERY_EXHAUSTED.to_string()],
    );
}

#[test]
fn queue_should_ignore_stale_pre_compaction_assistant_usage_on_pre_prompt_checks() {
    let harness = create_harness(HarnessOptions::default());
    let model = harness.session.model().expect("model");
    let stale_ts = now_ms() - 10_000;
    let stale = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Stop),
            total_tokens: 610_000,
            timestamp: stale_ts,
            text: Some("large response before compaction"),
            ..Default::default()
        },
    );

    {
        let mut manager = harness.session.session_manager();
        manager.append_message(user_value("before compaction", stale_ts - 1000));
        manager.append_message(stale.clone());
        let id = manager.get_entries()[0].id().to_string();
        manager.append_compaction("summary", &id, 610_000, None, Some(false));
        manager.append_message(user_value("session recovery payload", now_ms()));
    }

    assert_eq!(
        harness.session.compaction_plan(&stale, false),
        CompactionPlan::None,
    );
}

#[test]
fn queue_should_trigger_threshold_compaction_for_error_messages_using_last_successful_usage() {
    let harness = create_harness(HarnessOptions::default());
    let model = harness.session.model().expect("model");
    // Usage just over the compaction threshold, computed from the model (pi does the
    // same so catalog context-window changes do not break the test).
    let settings = harness.session.settings_manager.get_compaction_settings();
    let threshold_tokens = (model.context_window as i64 - settings.reserve_tokens + 1) as u64;
    let successful = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Stop),
            total_tokens: threshold_tokens,
            timestamp: now_ms(),
            text: Some("large successful response"),
            ..Default::default()
        },
    );
    let error = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Error),
            error_message: Some("529 overloaded"),
            timestamp: now_ms() + 1000,
            ..Default::default()
        },
    );
    harness.agent.set_messages(vec![
        user_value("hello", now_ms() - 1000),
        successful,
        user_value("another prompt", now_ms() + 500),
        error.clone(),
    ]);

    assert_eq!(
        harness.session.compaction_plan(&error, true),
        CompactionPlan::Run {
            reason: CompactionReason::Threshold,
            will_retry: false,
            set_overflow_guard: false,
        },
    );
}

#[test]
fn queue_should_not_trigger_threshold_compaction_for_error_messages_when_no_prior_usage_exists() {
    let harness = create_harness(HarnessOptions::default());
    let model = harness.session.model().expect("model");
    let error = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Error),
            error_message: Some("529 overloaded"),
            timestamp: now_ms(),
            ..Default::default()
        },
    );
    harness
        .agent
        .set_messages(vec![user_value("hello", now_ms() - 1000), error.clone()]);

    assert_eq!(
        harness.session.compaction_plan(&error, true),
        CompactionPlan::None,
    );
}

#[test]
fn queue_should_not_trigger_threshold_compaction_when_only_kept_pre_compaction_usage_exists() {
    let harness = create_harness(HarnessOptions::default());
    let model = harness.session.model().expect("model");
    let pre_ts = now_ms() - 10_000;
    let kept = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Stop),
            total_tokens: 190_000,
            timestamp: pre_ts,
            text: Some("kept response from before compaction"),
            ..Default::default()
        },
    );

    {
        let mut manager = harness.session.session_manager();
        manager.append_message(user_value("before compaction", pre_ts - 1000));
        manager.append_message(kept.clone());
        let id = manager.get_entries()[0].id().to_string();
        manager.append_compaction("summary", &id, 190_000, None, Some(false));
    }

    let error = assistant_value(
        &model,
        AssistantOpts {
            stop_reason: Some(StopReason::Error),
            error_message: Some("529 overloaded"),
            timestamp: now_ms(),
            ..Default::default()
        },
    );
    harness.agent.set_messages(vec![
        user_value("kept user msg", pre_ts - 1000),
        kept,
        user_value("new prompt", now_ms() - 500),
        error.clone(),
    ]);

    assert_eq!(
        harness.session.compaction_plan(&error, true),
        CompactionPlan::None,
    );
}
