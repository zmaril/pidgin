// straitjacket-allow-file:duplication — the `load()` vector-reading helper is
// intentionally the same two-line boilerplate as in width_vectors.rs /
// keys_vectors.rs; each integration-test binary is standalone and cannot share
// a private helper without a common module, more indirection than it warrants.
//! Drives the Rust port of pi's TUI component support layer (fuzzy, word
//! navigation, word segmentation, kill ring, undo stack, keybindings, and the
//! util gaps) against vectors extracted from pi itself
//! (`crates/pidgin-tui/vectors/gen/generate_components.mjs`). Every assertion
//! is byte/shape-identical: pi is the source of truth.

use std::path::PathBuf;

use serde::Deserialize;

use pidgin_tui::text_util::WordSegment;
use pidgin_tui::{
    apply_background_to_line, find_word_backward, find_word_forward, fuzzy_filter, fuzzy_match,
    is_punctuation_char, is_whitespace_char, set_kitty_protocol_active, tui_keybindings,
    word_segment, KeybindingConflict, KeybindingsManager, KillRing, PushOpts, UndoStack,
    WordNavOptions,
};

fn load<T: serde::de::DeserializeOwned>(name: &str) -> Vec<T> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join(format!("{name}.json"));
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

// --- fuzzy ----------------------------------------------------------------

#[derive(Deserialize)]
struct FuzzyMatchVec {
    query: String,
    text: String,
    matches: bool,
    // Exact IEEE-754 bit pattern of pi's score (decimal string of the u64).
    bits: String,
}

#[test]
fn fuzzy_match_vectors() {
    let vectors: Vec<FuzzyMatchVec> = load("fuzzy_match");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let r = fuzzy_match(&v.query, &v.text);
        assert_eq!(
            r.matches, v.matches,
            "fuzzy_match({:?}, {:?}).matches",
            v.query, v.text
        );
        // Compare raw IEEE-754 bits for byte-exact score parity with pi,
        // sidestepping any decimal float round-trip in the JSON reader.
        let expected_bits: u64 = v.bits.parse().expect("score bits");
        assert_eq!(
            r.score.to_bits(),
            expected_bits,
            "fuzzy_match({:?}, {:?}).score bits (got {})",
            v.query,
            v.text,
            r.score
        );
    }
}

#[derive(Deserialize)]
struct FuzzyFilterVec {
    items: Vec<String>,
    query: String,
    result: Vec<String>,
}

#[test]
fn fuzzy_filter_vectors() {
    let vectors: Vec<FuzzyFilterVec> = load("fuzzy_filter");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let got = fuzzy_filter(v.items.clone(), &v.query, |x: &String| x.clone());
        assert_eq!(got, v.result, "fuzzy_filter(query={:?})", v.query);
    }
}

// --- word segmentation ----------------------------------------------------

#[derive(Deserialize)]
struct SegVec {
    text: String,
    segments: Vec<SegEntry>,
}

#[derive(Deserialize)]
struct SegEntry {
    segment: String,
    #[serde(rename = "isWordLike")]
    is_word_like: bool,
}

#[test]
fn word_segmentation_vectors() {
    let vectors: Vec<SegVec> = load("word_segmentation");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let got = word_segment(&v.text);
        let expected: Vec<WordSegment> = v
            .segments
            .iter()
            .map(|s| WordSegment {
                segment: s.segment.clone(),
                is_word_like: s.is_word_like,
            })
            .collect();
        assert_eq!(got, expected, "word_segment({:?})", v.text);
    }
}

#[derive(Deserialize)]
struct ClassifyVec {
    text: String,
    #[serde(rename = "isWhitespace")]
    is_whitespace: bool,
    #[serde(rename = "isPunctuation")]
    is_punctuation: bool,
}

#[test]
fn char_classification_vectors() {
    let vectors: Vec<ClassifyVec> = load("char_classification");
    assert!(!vectors.is_empty());
    for v in &vectors {
        assert_eq!(
            is_whitespace_char(&v.text),
            v.is_whitespace,
            "is_whitespace_char({:?})",
            v.text
        );
        assert_eq!(
            is_punctuation_char(&v.text),
            v.is_punctuation,
            "is_punctuation_char({:?})",
            v.text
        );
    }
}

// --- word navigation ------------------------------------------------------

#[derive(Deserialize)]
struct NavVec {
    text: String,
    cursor: usize,
    backward: usize,
    forward: usize,
}

#[test]
fn word_navigation_vectors() {
    let vectors: Vec<NavVec> = load("word_navigation");
    assert!(!vectors.is_empty());
    let opts = WordNavOptions::default();
    for v in &vectors {
        assert_eq!(
            find_word_backward(&v.text, v.cursor, &opts),
            v.backward,
            "find_word_backward({:?}, {})",
            v.text,
            v.cursor
        );
        assert_eq!(
            find_word_forward(&v.text, v.cursor, &opts),
            v.forward,
            "find_word_forward({:?}, {})",
            v.text,
            v.cursor
        );
    }
}

// The atomic-segment cases from word-navigation.test.ts drive the functions
// with a custom segmenter (a pre-split map) plus an `isAtomicSegment`
// predicate, so they are asserted directly rather than via JSON vectors.
#[test]
fn word_navigation_atomic_segments() {
    let marker = "[paste #1 +5 lines]";
    let text = format!("hello {marker} world");

    // Pre-split segment maps keyed by the exact slice the functions pass.
    fn seg(items: &[(&str, bool)]) -> Vec<WordSegment> {
        items
            .iter()
            .map(|(s, w)| WordSegment {
                segment: s.to_string(),
                is_word_like: *w,
            })
            .collect()
    }

    let text_full = text.clone();
    let text_26 = text.clone();
    let text_from6 = text.clone();
    let marker_owned = marker.to_string();

    let segment = move |input: &str| -> Vec<WordSegment> {
        if input == text_full.as_str() {
            seg(&[
                ("hello", true),
                (" ", false),
                (&marker_owned, true),
                (" ", false),
                ("world", true),
            ])
        } else if input == &text_26[..26] {
            seg(&[
                ("hello", true),
                (" ", false),
                (&marker_owned, true),
                (" ", false),
            ])
        } else if input == &text_from6[6..] {
            seg(&[(&marker_owned, true), (" ", false), ("world", true)])
        } else {
            Vec::new()
        }
    };

    let marker_atomic = marker.to_string();
    let opts = WordNavOptions {
        segment: Some(Box::new(segment)),
        is_atomic_segment: Some(Box::new(move |s: &str| s == marker_atomic)),
    };

    let len = text.encode_utf16().count();
    // backward skips word then stops before atomic marker.
    assert_eq!(find_word_backward(&text, len, &opts), 26);
    // backward skips whitespace then atomic marker as one unit.
    assert_eq!(find_word_backward(&text, 26, &opts), 6);
    // forward skips atomic marker as one unit.
    assert_eq!(
        find_word_forward(&text, 6, &opts),
        6 + marker.encode_utf16().count()
    );
}

// --- kill ring ------------------------------------------------------------

#[derive(Deserialize)]
struct KillRingScenario {
    name: String,
    ops: Vec<KillRingOp>,
    states: Vec<KillRingState>,
}

#[derive(Deserialize)]
struct KillRingOp {
    op: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    prepend: Option<bool>,
    #[serde(default)]
    accumulate: Option<bool>,
}

#[derive(Deserialize)]
struct KillRingState {
    peek: Option<String>,
    length: usize,
}

#[test]
fn kill_ring_vectors() {
    let scenarios: Vec<KillRingScenario> = load("kill_ring");
    assert!(!scenarios.is_empty());
    for sc in &scenarios {
        let mut ring = KillRing::new();
        assert_eq!(sc.ops.len(), sc.states.len());
        for (op, expected) in sc.ops.iter().zip(&sc.states) {
            match op.op.as_str() {
                "push" => ring.push(
                    op.text.as_deref().unwrap_or(""),
                    PushOpts {
                        prepend: op.prepend.unwrap_or(false),
                        accumulate: op.accumulate.unwrap_or(false),
                    },
                ),
                "rotate" => ring.rotate(),
                "peek" => {}
                other => panic!("unknown kill-ring op {other}"),
            }
            assert_eq!(
                ring.peek().map(str::to_string),
                expected.peek,
                "{}: peek",
                sc.name
            );
            assert_eq!(ring.len(), expected.length, "{}: length", sc.name);
        }
    }
}

// --- undo stack -----------------------------------------------------------

#[derive(Deserialize)]
struct UndoScenario {
    name: String,
    ops: Vec<UndoOp>,
    states: Vec<UndoState>,
}

#[derive(Deserialize)]
struct UndoOp {
    op: String,
    #[serde(default)]
    value: Option<String>,
}

#[derive(Deserialize)]
struct UndoState {
    op: String,
    popped: Option<String>,
    length: usize,
}

#[test]
fn undo_stack_vectors() {
    let scenarios: Vec<UndoScenario> = load("undo_stack");
    assert!(!scenarios.is_empty());
    for sc in &scenarios {
        let mut stack: UndoStack<String> = UndoStack::new();
        assert_eq!(sc.ops.len(), sc.states.len());
        for (op, expected) in sc.ops.iter().zip(&sc.states) {
            let mut popped: Option<String> = None;
            match op.op.as_str() {
                "push" => stack.push(op.value.as_ref().expect("push value")),
                "pop" => popped = stack.pop(),
                "clear" => stack.clear(),
                other => panic!("unknown undo op {other}"),
            }
            assert_eq!(expected.op, op.op);
            assert_eq!(popped, expected.popped, "{}: popped", sc.name);
            assert_eq!(stack.len(), expected.length, "{}: length", sc.name);
        }
    }
}

// --- applyBackgroundToLine ------------------------------------------------

#[derive(Deserialize)]
struct BgVec {
    line: String,
    width: usize,
    result: String,
}

#[test]
fn apply_background_vectors() {
    let vectors: Vec<BgVec> = load("apply_background");
    assert!(!vectors.is_empty());
    let bg = |t: &str| format!("\x1b[41m{t}\x1b[49m");
    for v in &vectors {
        assert_eq!(
            apply_background_to_line(&v.line, v.width, bg),
            v.result,
            "apply_background_to_line({:?}, {})",
            v.line,
            v.width
        );
    }
}

// --- keybindings ----------------------------------------------------------

#[derive(Deserialize)]
#[serde(untagged)]
enum StringOrVec {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct KbScenario {
    name: String,
    // null (default), or ordered array of [id, keysOrNull].
    #[serde(rename = "userBindings")]
    user_bindings: Option<Vec<(String, Option<StringOrVec>)>>,
    #[serde(rename = "getKeys")]
    get_keys: std::collections::HashMap<String, Vec<String>>,
    resolved: std::collections::HashMap<String, StringOrVec>,
    conflicts: Vec<KbConflict>,
    matches: Vec<KbMatch>,
}

#[derive(Deserialize)]
struct KbConflict {
    key: String,
    keybindings: Vec<String>,
}

#[derive(Deserialize)]
struct KbMatch {
    data: String,
    binding: String,
    expected: bool,
}

#[test]
fn keybindings_vectors() {
    // The generator ran with pi's default (inactive) Kitty protocol state.
    set_kitty_protocol_active(false);

    let scenarios: Vec<KbScenario> = load("keybindings");
    assert!(!scenarios.is_empty());

    for sc in scenarios {
        let user: Vec<(&str, Option<Vec<String>>)> = match &sc.user_bindings {
            None => Vec::new(),
            Some(entries) => entries
                .iter()
                .map(|(id, keys)| {
                    let keys = keys.as_ref().map(|sv| match sv {
                        StringOrVec::One(s) => vec![s.clone()],
                        StringOrVec::Many(v) => v.clone(),
                    });
                    (id.as_str(), keys)
                })
                .collect(),
        };
        let mgr = KeybindingsManager::new(tui_keybindings(), user);

        // getKeys per binding.
        for (id, expected) in &sc.get_keys {
            assert_eq!(&mgr.get_keys(id), expected, "{}: getKeys({id})", sc.name);
        }

        // getResolvedBindings.
        let resolved = mgr.get_resolved_bindings();
        for (id, keys) in &resolved {
            let expected = sc
                .resolved
                .get(id)
                .unwrap_or_else(|| panic!("{}: resolved missing {id}", sc.name));
            let expected_vec = match expected {
                StringOrVec::One(s) => vec![s.clone()],
                StringOrVec::Many(v) => v.clone(),
            };
            assert_eq!(keys, &expected_vec, "{}: resolved({id})", sc.name);
        }

        // getConflicts (order-sensitive).
        let got_conflicts: Vec<KeybindingConflict> = mgr.get_conflicts();
        assert_eq!(
            got_conflicts.len(),
            sc.conflicts.len(),
            "{}: conflict count",
            sc.name
        );
        for (got, exp) in got_conflicts.iter().zip(&sc.conflicts) {
            assert_eq!(got.key, exp.key, "{}: conflict key", sc.name);
            assert_eq!(
                got.keybindings, exp.keybindings,
                "{}: conflict keybindings",
                sc.name
            );
        }

        // matches.
        for m in &sc.matches {
            assert_eq!(
                mgr.matches(&m.data, &m.binding),
                m.expected,
                "{}: matches({:?}, {})",
                sc.name,
                m.data,
                m.binding
            );
        }
    }
}
