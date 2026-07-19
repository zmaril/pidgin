//! Byte-exact round-trip of hand-authored v3 golden vectors, plus proof that
//! legacy v1 headers are rejected by the version-3 surface.
//!
//! The v1 fixtures committed upstream (`before-compaction.jsonl`,
//! `large-session.jsonl` under pi's coding-agent test tree) are legacy v1
//! sessions that agent-core rejects by design, so they cannot round-trip
//! through this surface. `v3-all-line-types.jsonl` is a fresh v3 vector
//! covering the header plus every entry variant (present and absent optional
//! fields) and both leaf shapes; `legacy-v1-header.jsonl` reproduces a v1
//! header for the rejection test.

use std::path::Path;

use pidgin_agent::harness::session::{
    serialize_entry_line, serialize_header_line, JsonlSessionStorage, SessionStorage,
};
use pidgin_agent::harness::types::SessionErrorCode;

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

#[test]
fn v3_golden_vector_round_trips_byte_for_byte() {
    let path = fixture("v3-all-line-types.jsonl");
    let original = std::fs::read_to_string(&path).expect("read fixture");

    let storage = JsonlSessionStorage::open(&path).expect("open v3 fixture");
    let metadata = storage.get_metadata();

    let mut reserialized = serialize_header_line(&metadata);
    for entry in storage.get_entries() {
        reserialized.push_str(&serialize_entry_line(&entry));
    }

    assert_eq!(
        reserialized, original,
        "re-serialized v3 vector must be byte-identical to the authored fixture"
    );
}

#[test]
fn v3_fixture_exercises_every_line_type() {
    let path = fixture("v3-all-line-types.jsonl");
    let storage = JsonlSessionStorage::open(&path).expect("open v3 fixture");
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
            "fixture missing a {expected} line"
        );
    }
}

#[test]
fn legacy_v1_header_is_rejected_as_not_version_3() {
    let path = fixture("legacy-v1-header.jsonl");
    let error = match JsonlSessionStorage::open(&path) {
        Ok(_) => panic!("v1 session must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, SessionErrorCode::InvalidSession);
    assert!(
        error.message.contains("unsupported session version"),
        "unexpected error: {}",
        error.message
    );
}
