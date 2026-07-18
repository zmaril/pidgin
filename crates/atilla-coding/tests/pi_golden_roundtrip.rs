//! Byte-exact round-trip of the pi-driven v3 golden corpus for the
//! coding-agent session writer — the cross-crate parity check.
//!
//! `tests/fixtures/v3-pi-coding-generated.jsonl` is emitted by
//! `../../atilla-agent/tests/gen/generate_sessions.mjs` driving pi's
//! coding-agent session writer. The coding-agent `custom` / `custom_message`
//! entries put their type-specific fields *before* `id`/`parentId`/`timestamp`
//! — the byte-level divergence from agent-core. This test loads each line
//! through the coding crate's own serde types (the exact parse+serialize path
//! `SessionManager` uses in `core/session_manager/io.rs`: typed
//! deserialize + `serde_json::to_string`) and asserts the re-emitted bytes are
//! identical to pi's, proving the coding serializer honors that key order.

use std::path::Path;

use atilla_coding::core::session_manager::{SessionEntry, SessionHeader};

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// Serialize a coding session header exactly as `io.rs::json_line` does.
fn serialize_header(header: &SessionHeader) -> String {
    serde_json::to_string(header).expect("header serializes")
}

/// Serialize a coding session entry exactly as `io.rs::json_line` does.
fn serialize_entry(entry: &SessionEntry) -> String {
    serde_json::to_string(entry).expect("entry serializes")
}

#[test]
fn pi_coding_corpus_round_trips_byte_for_byte() {
    let path = fixture("v3-pi-coding-generated.jsonl");
    let original = std::fs::read_to_string(&path).expect("read pi coding corpus");
    let lines: Vec<&str> = original.lines().collect();
    assert!(!lines.is_empty(), "corpus must be non-empty");

    // Line 0 is the session header; every subsequent line is a session entry.
    // Assert each re-serializes byte-for-byte to the original line.
    let header: SessionHeader =
        serde_json::from_str(lines[0]).expect("first line parses as a session header");
    assert_eq!(
        serialize_header(&header),
        lines[0],
        "re-serialized header must be byte-identical to the pi corpus line 0"
    );

    for (idx, line) in lines.iter().enumerate().skip(1) {
        let entry: SessionEntry = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("line {idx} does not parse as a SessionEntry: {e}"));
        assert_eq!(
            serialize_entry(&entry),
            *line,
            "re-serialized entry on line {idx} must be byte-identical to the pi corpus\n\
             (this is the custom/custom_message key-order parity check)"
        );
    }
}

#[test]
fn pi_coding_corpus_exercises_the_coding_entry_variants() {
    let path = fixture("v3-pi-coding-generated.jsonl");
    let original = std::fs::read_to_string(&path).expect("read pi coding corpus");
    let mut seen: Vec<&'static str> = original
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str::<SessionEntry>(l)
                .expect("entry parses")
                .type_str()
        })
        .collect();
    seen.sort_unstable();
    seen.dedup();
    // The coding-agent union has no `leaf` or `active_tools_change` variant.
    for expected in [
        "branch_summary",
        "compaction",
        "custom",
        "custom_message",
        "label",
        "message",
        "model_change",
        "session_info",
        "thinking_level_change",
    ] {
        assert!(
            seen.contains(&expected),
            "pi coding corpus missing a {expected} line"
        );
    }
}
