//! Ports of `test/harness/agent-harness-stream.test.ts` (4 cases).
//!
//! The crate's [`AgentHarnessStreamOptionsPatch`] models delete only for
//! header/metadata *keys* (a scalar or whole-map delete is not representable, by
//! prior-wave design), so the "deletion semantics" case is ADAPTED to the
//! representable key-level deletes; the non-representable scalar/whole-map
//! deletes are covered as documented gaps.

// straitjacket-allow-file:duplication — faithful parallel-structure test
// bodies repeat near-identical faux/session/harness scaffolding per scenario,
// mirroring pi's one-`it`-per-shape suite; not extractable duplication.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::{json, Map, Value};

use super::*;
use crate::harness::agent_harness::{AgentHarness, AgentHarnessEvent};
use crate::harness::events::{
    AgentHarnessEventResult, AgentHarnessOwnEvent, AgentHarnessStreamOptions,
    AgentHarnessStreamOptionsPatch, BeforeProviderPayloadResult, BeforeProviderRequestResult,
};
use crate::harness::options::{ProviderStream, ProviderStreamRequest};
use crate::harness::session::Session;
use crate::types::AgentEvent;

fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn metadata(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

/// pi: "snapshots stream options before provider request hooks"
#[test]
fn snapshots_stream_options_before_provider_request_hooks() {
    let faux = new_faux();
    faux.set_responses(vec![text_response("ok")]);
    let model = faux.get_model(None).unwrap();
    let recorder = Rc::new(RefCell::new(Recorder::default()));

    let mut options = base_options(
        Session::new(storage_with_id("session-1")),
        faux.clone(),
        model,
    );
    options.stream = recording_stream(faux, recorder.clone());
    options.stream_options = Some(AgentHarnessStreamOptions {
        timeout_ms: Some(1000),
        max_retries: Some(2),
        max_retry_delay_ms: Some(3000),
        headers: Some(headers(&[("x-base", "base")])),
        metadata: Some(metadata(&[("base", json!(true))])),
        cache_retention: Some("none".into()),
        transport: None,
    });
    let harness = AgentHarness::new(options).unwrap();

    let seen_session = Rc::new(RefCell::new(String::new()));
    let seen_headers = Rc::new(RefCell::new(BTreeMap::new()));
    let ss = seen_session.clone();
    let sh = seen_headers.clone();
    let _sub = harness.on(
        "before_provider_request",
        Rc::new(move |event| {
            if let AgentHarnessOwnEvent::BeforeProviderRequest(ev) = event {
                *ss.borrow_mut() = ev.session_id.clone();
                *sh.borrow_mut() = ev.stream_options.headers.clone().unwrap_or_default();
            }
            Ok(Some(AgentHarnessEventResult::BeforeProviderRequest(Some(
                BeforeProviderRequestResult {
                    stream_options: Some(AgentHarnessStreamOptionsPatch {
                        headers: Some(
                            [("x-hook".to_string(), Some("hook".to_string()))]
                                .into_iter()
                                .collect(),
                        ),
                        metadata: Some(metadata(&[("hook", json!(true))])),
                        ..AgentHarnessStreamOptionsPatch::default()
                    }),
                },
            ))))
        }),
    );

    harness.prompt("hello", None).unwrap();

    assert_eq!(*seen_session.borrow(), "session-1");
    assert_eq!(*seen_headers.borrow(), headers(&[("x-base", "base")]));

    let rec = recorder.borrow();
    let captured = &rec.options[0];
    assert_eq!(captured.timeout_ms, Some(1000));
    assert_eq!(captured.max_retries, Some(2));
    assert_eq!(captured.max_retry_delay_ms, Some(3000));
    assert_eq!(captured.cache_retention.as_deref(), Some("none"));
    assert_eq!(
        captured.headers,
        Some(headers(&[("x-base", "base"), ("x-hook", "hook")]))
    );
    assert_eq!(
        captured.metadata,
        Some(metadata(&[("base", json!(true)), ("hook", json!(true))]))
    );
}

/// pi: "chains provider request patches and supports deletion semantics"
///
/// ADAPTED: pi's scalar `timeoutMs: undefined` delete and whole-`metadata:
/// undefined` clear are not representable in the crate's patch type (scalar/whole
/// deletes were deliberately not modeled — only header/metadata *key* deletes
/// are). This port exercises the representable behavior: chaining two hooks,
/// adding header keys across both, and deleting a header key and a metadata key.
#[test]
fn chains_provider_request_patches_with_key_deletes() {
    let faux = new_faux();
    faux.set_responses(vec![text_response("ok")]);
    let model = faux.get_model(None).unwrap();
    let recorder = Rc::new(RefCell::new(Recorder::default()));

    let mut options = base_options(Session::new(new_storage()), faux.clone(), model);
    options.stream = recording_stream(faux, recorder.clone());
    options.stream_options = Some(AgentHarnessStreamOptions {
        timeout_ms: Some(1000),
        max_retries: Some(2),
        headers: Some(headers(&[("keep", "base"), ("remove", "base")])),
        metadata: Some(metadata(&[
            ("keep", json!("base")),
            ("remove", json!("base")),
        ])),
        ..AgentHarnessStreamOptions::default()
    });
    let harness = AgentHarness::new(options).unwrap();

    let _sub = harness.on(
        "before_provider_request",
        Rc::new(|event| {
            if let AgentHarnessOwnEvent::BeforeProviderRequest(ev) = event {
                assert_eq!(
                    ev.stream_options.headers,
                    Some(headers(&[("keep", "base"), ("remove", "base")]))
                );
            }
            Ok(Some(AgentHarnessEventResult::BeforeProviderRequest(Some(
                BeforeProviderRequestResult {
                    stream_options: Some(AgentHarnessStreamOptionsPatch {
                        headers: Some(
                            [
                                ("first".to_string(), Some("1".to_string())),
                                ("remove".to_string(), None),
                            ]
                            .into_iter()
                            .collect(),
                        ),
                        metadata: Some(metadata(&[("first", json!(1)), ("remove", Value::Null)])),
                        ..AgentHarnessStreamOptionsPatch::default()
                    }),
                },
            ))))
        }),
    );
    let _sub = harness.on(
        "before_provider_request",
        Rc::new(|event| {
            if let AgentHarnessOwnEvent::BeforeProviderRequest(ev) = event {
                assert_eq!(
                    ev.stream_options.headers,
                    Some(headers(&[("keep", "base"), ("first", "1")]))
                );
                assert_eq!(
                    ev.stream_options.metadata,
                    Some(metadata(&[("keep", json!("base")), ("first", json!(1))]))
                );
            }
            Ok(Some(AgentHarnessEventResult::BeforeProviderRequest(Some(
                BeforeProviderRequestResult {
                    stream_options: Some(AgentHarnessStreamOptionsPatch {
                        headers: Some(
                            [("second".to_string(), Some("2".to_string()))]
                                .into_iter()
                                .collect(),
                        ),
                        ..AgentHarnessStreamOptionsPatch::default()
                    }),
                },
            ))))
        }),
    );

    harness.prompt("hello", None).unwrap();

    let rec = recorder.borrow();
    let captured = &rec.options[0];
    assert_eq!(captured.max_retries, Some(2));
    assert_eq!(
        captured.headers,
        Some(headers(&[
            ("keep", "base"),
            ("first", "1"),
            ("second", "2")
        ]))
    );
    assert_eq!(
        captured.metadata,
        Some(metadata(&[("keep", json!("base")), ("first", json!(1))]))
    );
}

/// pi: "uses updated stream options for save-point snapshots without mutating the
/// active request"
#[test]
fn updated_stream_options_apply_at_save_points() {
    let faux = new_faux();
    faux.set_responses(vec![
        tool_call_response("calculate", json!({ "expression": "1 + 1" }), "call-1"),
        text_response("done"),
    ]);
    let model = faux.get_model(None).unwrap();
    let recorder = Rc::new(RefCell::new(Recorder::default()));
    let mut options = base_options(Session::new(new_storage()), faux.clone(), model);
    options.stream = recording_stream(faux, recorder.clone());
    options.tools = Some(vec![calculate_tool()]);
    options.stream_options = Some(AgentHarnessStreamOptions {
        timeout_ms: Some(1000),
        headers: Some(headers(&[("turn", "first")])),
        ..AgentHarnessStreamOptions::default()
    });
    let harness = AgentHarness::new(options).unwrap();

    let h = harness.clone();
    let _sub = harness.subscribe(Rc::new(move |event: &AgentHarnessEvent, _| {
        if let AgentHarnessEvent::Loop(AgentEvent::ToolExecutionStart { .. }) = event {
            h.set_stream_options(AgentHarnessStreamOptions {
                timeout_ms: Some(2000),
                headers: Some(headers(&[("turn", "second")])),
                ..AgentHarnessStreamOptions::default()
            });
        }
    }));

    harness.prompt("hello", None).unwrap();

    let rec = recorder.borrow();
    assert_eq!(rec.options.len(), 2);
    assert_eq!(rec.options[0].timeout_ms, Some(1000));
    assert_eq!(rec.options[0].headers, Some(headers(&[("turn", "first")])));
    assert_eq!(rec.options[1].timeout_ms, Some(2000));
    assert_eq!(rec.options[1].headers, Some(headers(&[("turn", "second")])));
}

/// pi: "chains provider payload hooks"
#[test]
fn chains_provider_payload_hooks() {
    use pidgin_ai::seams::Provider;

    let faux = new_faux();
    faux.set_responses(vec![text_response("ok")]);
    let model = faux.get_model(None).unwrap();

    let final_payload = Rc::new(RefCell::new(Value::Null));
    let fp = final_payload.clone();
    let stream: ProviderStream = {
        let faux = faux.clone();
        Rc::new(move |req: ProviderStreamRequest| {
            let result = (req.on_payload)(json!({ "steps": ["provider"] }));
            *fp.borrow_mut() = result;
            faux.stream(req.model, req.context, None, req.signal)
        })
    };
    let mut options = base_options(Session::new(new_storage()), faux, model);
    options.stream = stream;
    let harness = AgentHarness::new(options).unwrap();

    let seen = Rc::new(RefCell::new(Vec::new()));
    let s1 = seen.clone();
    let _sub = harness.on(
        "before_provider_payload",
        Rc::new(move |event| {
            if let AgentHarnessOwnEvent::BeforeProviderPayload(ev) = event {
                s1.borrow_mut().push(ev.payload.clone());
            }
            Ok(Some(AgentHarnessEventResult::BeforeProviderPayload(Some(
                BeforeProviderPayloadResult {
                    payload: json!({ "steps": ["provider", "first"] }),
                },
            ))))
        }),
    );
    let s2 = seen.clone();
    let _sub = harness.on(
        "before_provider_payload",
        Rc::new(move |event| {
            if let AgentHarnessOwnEvent::BeforeProviderPayload(ev) = event {
                s2.borrow_mut().push(ev.payload.clone());
            }
            Ok(Some(AgentHarnessEventResult::BeforeProviderPayload(Some(
                BeforeProviderPayloadResult {
                    payload: json!({ "steps": ["provider", "first", "second"] }),
                },
            ))))
        }),
    );

    harness.prompt("hello", None).unwrap();

    assert_eq!(
        *seen.borrow(),
        vec![
            json!({ "steps": ["provider"] }),
            json!({ "steps": ["provider", "first"] }),
        ]
    );
    assert_eq!(
        *final_payload.borrow(),
        json!({ "steps": ["provider", "first", "second"] })
    );
}
