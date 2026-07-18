//! Drives the Rust width module against vectors extracted from pi itself
//! (`crates/atilla-tui/vectors/gen/generate.mjs`). Every assertion is
//! byte-identical: pi is the source of truth, and any disagreement is a bug in
//! the port, not the vectors.

use std::path::PathBuf;

use serde::Deserialize;
use unicode_segmentation::UnicodeSegmentation;

use atilla_tui::{
    extract_ansi_code, extract_segments, normalize_terminal_output, slice_by_column,
    slice_with_width, truncate_to_width, visible_width, wrap_text_with_ansi,
};

fn load<T: serde::de::DeserializeOwned>(name: &str) -> Vec<T> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join(format!("{name}.json"));
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

#[derive(Deserialize)]
struct WidthVec {
    input: String,
    expected: usize,
}

#[test]
fn visible_width_vectors() {
    let vectors: Vec<WidthVec> = load("visible_width");
    assert!(!vectors.is_empty());
    let mut fails = Vec::new();
    for v in &vectors {
        let got = visible_width(&v.input);
        if got != v.expected {
            fails.push(format!(
                "visible_width({:?}) = {got}, want {}",
                v.input, v.expected
            ));
        }
    }
    report("visible_width", vectors.len(), fails);
}

#[test]
fn grapheme_width_vectors() {
    let vectors: Vec<WidthVec> = load("grapheme_width");
    let mut fails = Vec::new();
    for v in &vectors {
        let got = visible_width(&v.input);
        if got != v.expected {
            fails.push(format!(
                "grapheme visible_width({:?}) = {got}, want {}",
                v.input, v.expected
            ));
        }
    }
    report("grapheme_width", vectors.len(), fails);
}

#[derive(Deserialize)]
struct EawVec {
    codepoint: u32,
    input: String,
    expected: usize,
}

#[test]
fn east_asian_width_vectors() {
    let vectors: Vec<EawVec> = load("east_asian_width");
    let mut fails = Vec::new();
    for v in &vectors {
        let got = visible_width(&v.input);
        if got != v.expected {
            fails.push(format!(
                "east_asian visible_width(U+{:04X} {:?}) = {got}, want {}",
                v.codepoint, v.input, v.expected
            ));
        }
    }
    report("east_asian_width", vectors.len(), fails);
}

#[derive(Deserialize)]
struct AnsiExpected {
    code: String,
    length: usize,
}

#[derive(Deserialize)]
struct AnsiVec {
    input: String,
    pos: usize,
    expected: Option<AnsiExpected>,
}

#[test]
fn extract_ansi_code_vectors() {
    let vectors: Vec<AnsiVec> = load("extract_ansi_code");
    let mut fails = Vec::new();
    for v in &vectors {
        let got = extract_ansi_code(&v.input, v.pos);
        let ok = match (&got, &v.expected) {
            (None, None) => true,
            (Some((code, len)), Some(exp)) => code == &exp.code && *len == exp.length,
            _ => false,
        };
        if !ok {
            fails.push(format!(
                "extract_ansi_code({:?}, {}) = {:?}, want {:?}",
                v.input,
                v.pos,
                got.as_ref().map(|(c, l)| (c, l)),
                v.expected.as_ref().map(|e| (&e.code, e.length))
            ));
        }
    }
    report("extract_ansi_code", vectors.len(), fails);
}

#[derive(Deserialize)]
struct NormVec {
    input: String,
    expected: String,
}

#[test]
fn normalize_terminal_output_vectors() {
    let vectors: Vec<NormVec> = load("normalize_terminal_output");
    let mut fails = Vec::new();
    for v in &vectors {
        let got = normalize_terminal_output(&v.input);
        if got != v.expected {
            fails.push(format!(
                "normalize_terminal_output({:?}) = {:?}, want {:?}",
                v.input, got, v.expected
            ));
        }
    }
    report("normalize_terminal_output", vectors.len(), fails);
}

#[derive(Deserialize)]
struct TruncVec {
    text: String,
    #[serde(rename = "maxWidth")]
    max_width: i64,
    ellipsis: String,
    pad: bool,
    expected: String,
}

#[test]
fn truncate_to_width_vectors() {
    let vectors: Vec<TruncVec> = load("truncate_to_width");
    let mut fails = Vec::new();
    for v in &vectors {
        let got = truncate_to_width(&v.text, v.max_width, &v.ellipsis, v.pad);
        if got != v.expected {
            fails.push(format!(
                "truncate_to_width({:?}, {}, {:?}, {}) = {:?}, want {:?}",
                v.text, v.max_width, v.ellipsis, v.pad, got, v.expected
            ));
        }
    }
    report("truncate_to_width", vectors.len(), fails);
}

#[derive(Deserialize)]
struct WrapVec {
    text: String,
    width: usize,
    expected: Vec<String>,
}

#[test]
fn wrap_text_with_ansi_vectors() {
    let vectors: Vec<WrapVec> = load("wrap_text_with_ansi");
    let mut fails = Vec::new();
    for v in &vectors {
        let got = wrap_text_with_ansi(&v.text, v.width);
        if got != v.expected {
            fails.push(format!(
                "wrap_text_with_ansi({:?}, {}) = {:?}, want {:?}",
                v.text, v.width, got, v.expected
            ));
        }
    }
    report("wrap_text_with_ansi", vectors.len(), fails);
}

#[derive(Deserialize)]
struct SliceVec {
    line: String,
    #[serde(rename = "startCol")]
    start_col: i64,
    length: i64,
    strict: bool,
    #[serde(rename = "expectedText")]
    expected_text: String,
    #[serde(rename = "expectedWidth")]
    expected_width: i64,
}

#[test]
fn slice_by_column_vectors() {
    let vectors: Vec<SliceVec> = load("slice_by_column");
    let mut fails = Vec::new();
    for v in &vectors {
        let text = slice_by_column(&v.line, v.start_col, v.length, v.strict);
        let (wtext, width) = slice_with_width(&v.line, v.start_col, v.length, v.strict);
        if text != v.expected_text || wtext != v.expected_text || width != v.expected_width {
            fails.push(format!(
                "slice({:?}, {}, {}, {}) = ({:?}, {}), want ({:?}, {})",
                v.line,
                v.start_col,
                v.length,
                v.strict,
                text,
                width,
                v.expected_text,
                v.expected_width
            ));
        }
    }
    report("slice_by_column", vectors.len(), fails);
}

#[derive(Deserialize)]
struct ExtractSegVec {
    line: String,
    #[serde(rename = "beforeEnd")]
    before_end: i64,
    #[serde(rename = "afterStart")]
    after_start: i64,
    #[serde(rename = "afterLen")]
    after_len: i64,
    #[serde(rename = "strictAfter")]
    strict_after: bool,
    #[serde(rename = "expectedBefore")]
    expected_before: String,
    #[serde(rename = "expectedBeforeWidth")]
    expected_before_width: i64,
    #[serde(rename = "expectedAfter")]
    expected_after: String,
    #[serde(rename = "expectedAfterWidth")]
    expected_after_width: i64,
}

#[test]
fn extract_segments_vectors() {
    let vectors: Vec<ExtractSegVec> = load("extract_segments");
    let mut fails = Vec::new();
    for v in &vectors {
        let r = extract_segments(
            &v.line,
            v.before_end,
            v.after_start,
            v.after_len,
            v.strict_after,
        );
        if r.before != v.expected_before
            || r.before_width != v.expected_before_width
            || r.after != v.expected_after
            || r.after_width != v.expected_after_width
        {
            fails.push(format!(
                "extract_segments({:?}, {}, {}, {}, {}) = (before {:?}/{}, after {:?}/{}), want (before {:?}/{}, after {:?}/{})",
                v.line, v.before_end, v.after_start, v.after_len, v.strict_after,
                r.before, r.before_width, r.after, r.after_width,
                v.expected_before, v.expected_before_width, v.expected_after, v.expected_after_width
            ));
        }
    }
    report("extract_segments", vectors.len(), fails);
}

#[derive(Deserialize)]
struct SegVec {
    input: String,
    graphemes: Vec<String>,
}

/// Verifies unicode-segmentation's extended grapheme clusters match pi's
/// `Intl.Segmenter` on the emoji/CJK/combining corpus the width module relies
/// on. A mismatch here would silently corrupt every width above it.
#[test]
fn grapheme_segmentation_vectors() {
    let vectors: Vec<SegVec> = load("grapheme_segmentation");
    let mut fails = Vec::new();
    for v in &vectors {
        let got: Vec<String> = v.input.graphemes(true).map(|s| s.to_string()).collect();
        if got != v.graphemes {
            fails.push(format!(
                "segment({:?}) = {:?}, want {:?}",
                v.input, got, v.graphemes
            ));
        }
    }
    report("grapheme_segmentation", vectors.len(), fails);
}

fn report(name: &str, total: usize, fails: Vec<String>) {
    if !fails.is_empty() {
        let shown: Vec<_> = fails.iter().take(20).cloned().collect();
        panic!(
            "{name}: {}/{total} vectors FAILED\n{}",
            fails.len(),
            shown.join("\n")
        );
    }
    eprintln!("{name}: {total}/{total} vectors passed");
}
