//! Byte-exact round-trip of the pi-driven v3 golden corpus for agent-core.
//!
//! `tests/fixtures/v3-pi-generated.jsonl` is a reproducible corpus emitted by
//! `tests/gen/generate_sessions.mjs` (which drives pi's own session writer). It
//! covers the header plus every agent-core entry variant. This test proves the
//! Rust serializer (`serialize_header_line` / `serialize_entry_line`) reproduces
//! pi's bytes exactly — the same entry points `golden_vectors.rs` exercises over
//! the hand-authored vector, applied here to the machine-generated corpus.

use std::path::Path;

use atilla_agent::harness::session::{
    serialize_entry_line, serialize_header_line, JsonlSessionStorage, SessionStorage,
};

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

#[test]
fn pi_generated_corpus_round_trips_byte_for_byte() {
    let path = fixture("v3-pi-generated.jsonl");
    let original = std::fs::read_to_string(&path).expect("read pi corpus");

    let storage = JsonlSessionStorage::open(&path).expect("open pi corpus");
    let metadata = storage.get_metadata();

    let mut reserialized = serialize_header_line(&metadata);
    for entry in storage.get_entries() {
        reserialized.push_str(&serialize_entry_line(&entry));
    }

    assert_eq!(
        reserialized, original,
        "re-serialized pi corpus must be byte-identical to the generated fixture"
    );
}

#[test]
fn pi_generated_corpus_exercises_every_line_type() {
    let path = fixture("v3-pi-generated.jsonl");
    let storage = JsonlSessionStorage::open(&path).expect("open pi corpus");
    let mut seen: Vec<&'static str> = storage.get_entries().iter().map(|e| e.type_str()).collect();
    seen.sort_unstable();
    seen.dedup();
    for expected in [
        "active_tools_change",
        "branch_summary",
        "compaction",
        "custom",
        "custom_message",
        "label",
        "leaf",
        "message",
        "model_change",
        "session_info",
        "thinking_level_change",
    ] {
        assert!(
            seen.contains(&expected),
            "pi corpus missing a {expected} line"
        );
    }
}
