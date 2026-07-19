//! Tests for the models-store seam.
//!
//! Translated from pi's `test/models-store.test.ts` (51 lines): the
//! `FileModelsStore` catalog round-trip. An additional in-memory round-trip
//! pins the [`InMemoryCodingAgentModelsStore`] path.

use tempfile::tempdir;

use super::*;
use crate::core::test_support::model;

fn ids(entry: &Option<ModelsStoreEntry>) -> Option<Vec<String>> {
    entry
        .as_ref()
        .map(|e| e.models.iter().map(|m| m.id.clone()).collect())
}

// models-store.test.ts:32 — persists provider catalogs without replacing
// unrelated providers, survives reload, and deletes one key in isolation.
#[test]
fn file_store_persists_provider_catalogs_independently() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("models-store.json");
    let path_str = path.to_str().unwrap();
    let store = FileModelsStore::new(path_str);

    store.write(
        "one",
        ModelsStoreEntry {
            models: vec![model("one", "m1")],
            checked_at: Some(100),
        },
    );
    store.write(
        "two",
        ModelsStoreEntry {
            models: vec![model("two", "m2")],
            checked_at: Some(200),
        },
    );

    let reloaded = FileModelsStore::new(path_str);
    assert_eq!(ids(&reloaded.read("one")), Some(vec!["m1".to_string()]));
    assert_eq!(reloaded.read("one").and_then(|e| e.checked_at), Some(100));
    assert_eq!(ids(&reloaded.read("two")), Some(vec!["m2".to_string()]));

    reloaded.delete("one");
    assert_eq!(reloaded.read("one"), None);
    assert_eq!(ids(&reloaded.read("two")), Some(vec!["m2".to_string()]));
}

// The in-memory store honors the same read/write/delete contract.
#[test]
fn in_memory_store_round_trips() {
    let store = InMemoryCodingAgentModelsStore::new();
    assert_eq!(store.read("one"), None);

    store.write(
        "one",
        ModelsStoreEntry {
            models: vec![model("one", "m1")],
            checked_at: Some(1),
        },
    );
    assert_eq!(ids(&store.read("one")), Some(vec!["m1".to_string()]));

    store.delete("one");
    assert_eq!(store.read("one"), None);
}
