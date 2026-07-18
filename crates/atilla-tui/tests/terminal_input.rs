// straitjacket-allow-file:duplication — the `load()` vector-reading helper is
// intentionally the same two-line boilerplate as in keys_vectors.rs /
// width_vectors.rs; each integration test binary is standalone and cannot share
// a private helper without a common module, which is more indirection than a
// two-line reader warrants.
//! Drives the real terminal backend's input pipeline
//! ([`atilla_tui::ProcessTerminal::feed`]) against the very same `parse_key`
//! vectors extracted from pi (`vectors/gen/generate_keys.mjs`). The terminal
//! path is a thin wrapper: it splits raw bytes into complete sequences and
//! forwards them; all key *decoding* is [`atilla_tui::parse_key`]. So for every
//! vector whose input the terminal forwards verbatim as a single sequence, the
//! forwarded string must decode to pi's expected KeyId.
//!
//! Sequences the terminal deliberately consumes — Kitty/DA keyboard-protocol
//! replies and their prefixes — are excluded here; that swallowing behaviour is
//! covered by the `ProcessTerminal` unit tests. Like `keys_vectors.rs`, these
//! run serially in one test because `parse_key` reads ambient global state
//! (the Kitty flag and `isWindowsTerminalSession()` env vars).

use std::path::PathBuf;

use serde::Deserialize;

use atilla_tui::{
    is_negotiation_sequence_prefix, parse_key, parse_negotiation_sequence,
    set_kitty_protocol_active, ProcessTerminal, TerminalInput,
};

#[derive(Debug, Deserialize)]
struct ParseKeyVector {
    input: String,
    kitty: bool,
    wt: bool,
    expected: Option<String>,
}

fn load<T: serde::de::DeserializeOwned>(name: &str) -> Vec<T> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join(format!("{name}.json"));
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

// Realize a desired isWindowsTerminalSession() result by setting WT_SESSION
// (SSH_* cleared), matching how the generator produced the `wt` flag.
fn set_env_for_wt(wt: bool) {
    for v in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"] {
        std::env::remove_var(v);
    }
    if wt {
        std::env::set_var("WT_SESSION", "test-session");
    } else {
        std::env::remove_var("WT_SESSION");
    }
}

/// A vector is a thin-wrapper case when its input is not something the terminal
/// consumes as keyboard-protocol negotiation.
fn is_forwardable(input: &str) -> bool {
    !input.is_empty()
        && parse_negotiation_sequence(input).is_none()
        && !is_negotiation_sequence_prefix(input)
}

#[test]
fn terminal_feed_forwards_sequences_that_decode_like_keys_rs() {
    let vectors: Vec<ParseKeyVector> = load("keys_parse_key");
    let mut checked = 0usize;

    for v in &vectors {
        if !is_forwardable(&v.input) {
            continue;
        }

        // Fresh terminal per vector so per-instance StdinBuffer state (e.g. the
        // Kitty printable-dedup) never bleeds between cases. Raw-mode management
        // is off so no TTY is required.
        let mut term = ProcessTerminal::with_size(Vec::new(), 80, 24).manage_raw_mode(false);
        let events = term.feed(&v.input);

        // Only assert when the terminal forwards the input unchanged as exactly
        // one key event (the thin-wrapper case). Batched/multi-sequence inputs
        // are exercised separately below.
        if events.as_slice() != [TerminalInput::Key(v.input.clone())] {
            continue;
        }

        // parse_key reads ambient global state; set it exactly as the vector.
        set_kitty_protocol_active(v.kitty);
        set_env_for_wt(v.wt);

        let decoded = parse_key(&v.input);
        assert_eq!(
            decoded, v.expected,
            "terminal-forwarded sequence {:?} decoded to {:?}, expected {:?}",
            v.input, decoded, v.expected
        );
        checked += 1;
    }

    set_kitty_protocol_active(false);
    set_env_for_wt(false);

    // Guard against the filters silently excluding everything.
    assert!(
        checked > 100,
        "expected the terminal path to cover many key vectors, only checked {checked}"
    );
}

#[test]
fn terminal_feed_splits_batched_keys_into_matching_keyids() {
    set_kitty_protocol_active(false);
    set_env_for_wt(false);

    // A batch of distinct keypresses arriving in one read is split into one
    // event each, and every event decodes via keys.rs to its own KeyId.
    let mut term = ProcessTerminal::with_size(Vec::new(), 80, 24).manage_raw_mode(false);
    let events = term.feed("a\x1b[A\x03");
    assert_eq!(
        events,
        vec![
            TerminalInput::Key("a".to_string()),
            TerminalInput::Key("\x1b[A".to_string()),
            TerminalInput::Key("\x03".to_string()),
        ]
    );

    let decoded: Vec<Option<String>> = events
        .iter()
        .map(|e| match e {
            TerminalInput::Key(s) => parse_key(s),
            TerminalInput::Paste(_) => None,
        })
        .collect();
    assert_eq!(
        decoded,
        vec![
            Some("a".to_string()),
            Some("up".to_string()),
            Some("ctrl+c".to_string()),
        ]
    );
}
