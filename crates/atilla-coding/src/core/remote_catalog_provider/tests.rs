//! Tests for the remote-catalog pure helpers.
//!
//! The pi test `test/remote-catalog-provider.test.ts` (118 lines) exercises the
//! network overlay end-to-end (mocked `fetch`, store round-trips, TTL, forced
//! refresh, 501 handling); that behavior is deferred (see the module docs).
//! Reachable here are the two pure helpers the overlay is built from:
//! [`merge_models`] (the `getModels()` merge) and [`parse_catalog`] (the shape
//! parsing pinned by the test's "parses keyed catalogs" case).

use atilla_ai::types::Model;
use serde_json::json;

use super::*;
use crate::core::test_support::model as make_model;

fn model(id: &str) -> Model {
    make_model("test-provider", id)
}

fn ids(models: &[Model]) -> Vec<String> {
    models.iter().map(|m| m.id.clone()).collect()
}

// remote-catalog-provider.test.ts:77 — the merged view keeps the static model
// and appends the dynamic one (["static", "dynamic"]).
#[test]
fn merge_appends_new_ids() {
    let merged = merge_models(&[model("static")], &[model("dynamic")]);
    assert_eq!(ids(&merged), vec!["static", "dynamic"]);
}

// A dynamic entry with the same id replaces the baseline entry in place.
#[test]
fn merge_replaces_by_id() {
    let mut replacement = model("static");
    replacement.name = "Replaced".to_string();
    let merged = merge_models(&[model("static"), model("other")], &[replacement]);
    assert_eq!(ids(&merged), vec!["static", "other"]);
    assert_eq!(merged[0].name, "Replaced");
}

// remote-catalog-provider.test.ts:27 — a keyed object catalog ({ dynamic: {..} })
// parses to its values with the provider stamped.
#[test]
fn parse_keyed_object_catalog_stamps_provider() {
    let value = json!({ "dynamic": serde_json::to_value(model("dynamic")).unwrap() });
    let parsed = parse_catalog("test-provider", &value).unwrap();
    assert_eq!(ids(&parsed), vec!["dynamic"]);
    assert_eq!(parsed[0].provider, "test-provider");
}

// A bare array catalog parses in order; the provider is overwritten on each.
#[test]
fn parse_array_catalog_overwrites_provider() {
    let mut foreign = model("a");
    foreign.provider = "someone-else".to_string();
    let value = json!([
        serde_json::to_value(foreign).unwrap(),
        serde_json::to_value(model("b")).unwrap(),
    ]);
    let parsed = parse_catalog("test-provider", &value).unwrap();
    assert_eq!(ids(&parsed), vec!["a", "b"]);
    assert!(parsed.iter().all(|m| m.provider == "test-provider"));
}

// A `{ models: [...] }` envelope parses the inner array.
#[test]
fn parse_models_envelope() {
    let value = json!({ "models": [serde_json::to_value(model("m")).unwrap()] });
    let parsed = parse_catalog("test-provider", &value).unwrap();
    assert_eq!(ids(&parsed), vec!["m"]);
}

// Entries without an "id" are dropped.
#[test]
fn parse_drops_entries_without_id() {
    let value = json!([{ "name": "no id here" }, serde_json::to_value(model("kept")).unwrap()]);
    let parsed = parse_catalog("test-provider", &value).unwrap();
    assert_eq!(ids(&parsed), vec!["kept"]);
}

// A non-array, non-object payload is an invalid catalog.
#[test]
fn parse_rejects_scalar_payload() {
    let err = parse_catalog("test-provider", &json!("nonsense")).unwrap_err();
    assert!(err.contains("Invalid model catalog for provider \"test-provider\""));
}
