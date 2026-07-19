// straitjacket-allow-file:duplication — the grapheme and word marker-merge
// passes (`segment_graphemes_with_markers` / `segment_words_with_markers`) share
// the same segmentWithMarkers control flow from pi's `editor.ts`; each mirrors
// its base-segment source faithfully and keeping them as two explicit functions
// (rather than a generic over a trait) matches the source and stays legible.
//! Paste-marker-aware segmentation and marker bookkeeping for the editor,
//! ported from `vendor/pi/packages/tui/src/components/editor.ts`.
//!
//! pi's editor wraps `Intl.Segmenter` so that valid paste markers such as
//! `[paste #1 +20 lines]` are merged into a single atomic segment for cursor
//! movement, deletion and word wrap. Cursor arithmetic is in UTF-16 code units
//! (JS string indices), so every index here is a UTF-16 offset.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use fancy_regex::Regex;
use unicode_segmentation::UnicodeSegmentation;

use crate::text_util::word_segment;

/// A grapheme (or merged-marker) segment: the substring and its UTF-16 index.
#[derive(Debug, Clone)]
pub struct Segment {
    /// The segment text.
    pub segment: String,
    /// UTF-16 offset of the segment within the source string.
    pub index: usize,
}

/// A word segment carrying both text and word-likeness (for word navigation).
#[derive(Debug, Clone)]
pub struct WordSeg {
    pub segment: String,
    pub is_word_like: bool,
}

/// UTF-16 length of a string (JS `string.length`).
pub fn u16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// UTF-16 slice `[start..end]` reconstructed as an owned `String`.
///
/// `start`/`end` are valid UTF-16 boundaries in every editor path (cursors and
/// segment boundaries land on grapheme boundaries, which never split a surrogate
/// pair).
pub fn u16_slice(units: &[u16], start: usize, end: usize) -> String {
    String::from_utf16(&units[start..end]).expect("slice on a valid UTF-16 boundary")
}

/// `/\[paste #(\d+)( (\+\d+ lines|\d+ chars))?\]/g` — global marker matcher.
static PASTE_MARKER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[paste #(\d+)( (\+\d+ lines|\d+ chars))?\]").unwrap());

/// `/^\[paste #(\d+)( (\+\d+ lines|\d+ chars))?\]$/` — single-segment matcher.
static PASTE_MARKER_SINGLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[paste #(\d+)( (\+\d+ lines|\d+ chars))?\]$").unwrap());

/// `isPasteMarker` — whether `segment` is a paste marker (length >= 10 and the
/// full string matches the single-marker pattern).
pub fn is_paste_marker(segment: &str) -> bool {
    u16_len(segment) >= 10 && PASTE_MARKER_SINGLE.is_match(segment).unwrap_or(false)
}

/// If `segment` is a paste marker, return its numeric id (`PASTE_MARKER_SINGLE`
/// capture group 1).
pub fn paste_marker_id(segment: &str) -> Option<u64> {
    let caps = PASTE_MARKER_SINGLE.captures(segment).ok().flatten()?;
    caps.get(1)?.as_str().parse::<u64>().ok()
}

// A marker span in UTF-16 units.
struct MarkerSpan {
    start: usize,
    end: usize,
}

// Byte offset -> UTF-16 offset within `text`.
fn byte_to_u16(text: &str, byte: usize) -> usize {
    u16_len(&text[..byte])
}

// Collect the UTF-16 spans of paste markers whose id is in `valid_ids`.
fn marker_spans(text: &str, valid_ids: &BTreeSet<u64>) -> Vec<MarkerSpan> {
    let mut spans = Vec::new();
    for caps in PASTE_MARKER_REGEX.captures_iter(text).flatten() {
        let Some(m0) = caps.get(0) else { continue };
        let Some(id_m) = caps.get(1) else { continue };
        let Ok(id) = id_m.as_str().parse::<u64>() else {
            continue;
        };
        if !valid_ids.contains(&id) {
            continue;
        }
        spans.push(MarkerSpan {
            start: byte_to_u16(text, m0.start()),
            end: byte_to_u16(text, m0.end()),
        });
    }
    spans
}

/// Grapheme segmentation of `text` with valid paste markers merged into single
/// atomic segments (pi's `segment(text, "grapheme")`).
pub fn segment_graphemes_with_markers(text: &str, valid_ids: &BTreeSet<u64>) -> Vec<Segment> {
    // Base grapheme segments with UTF-16 indices.
    let mut base: Vec<Segment> = Vec::new();
    let mut idx = 0usize;
    for g in text.graphemes(true) {
        base.push(Segment {
            segment: g.to_string(),
            index: idx,
        });
        idx += u16_len(g);
    }

    // Fast path: no markers to merge.
    if valid_ids.is_empty() || !text.contains("[paste #") {
        return base;
    }
    let markers = marker_spans(text, valid_ids);
    if markers.is_empty() {
        return base;
    }

    let units: Vec<u16> = text.encode_utf16().collect();
    let mut result: Vec<Segment> = Vec::new();
    let mut marker_idx = 0usize;
    for seg in base {
        while marker_idx < markers.len() && markers[marker_idx].end <= seg.index {
            marker_idx += 1;
        }
        let marker = markers.get(marker_idx);
        if let Some(m) = marker {
            if seg.index >= m.start && seg.index < m.end {
                if seg.index == m.start {
                    result.push(Segment {
                        segment: u16_slice(&units, m.start, m.end),
                        index: m.start,
                    });
                }
                continue;
            }
        }
        result.push(seg);
    }
    result
}

/// Word segmentation of `text` with valid paste markers merged into single
/// atomic segments (pi's `segment(text, "word")`).
pub fn segment_words_with_markers(text: &str, valid_ids: &BTreeSet<u64>) -> Vec<WordSeg> {
    // Base word segments with UTF-16 indices.
    let raw = word_segment(text);
    let mut base: Vec<(usize, WordSeg)> = Vec::with_capacity(raw.len());
    let mut idx = 0usize;
    for w in raw {
        let len = u16_len(&w.segment);
        base.push((
            idx,
            WordSeg {
                segment: w.segment,
                is_word_like: w.is_word_like,
            },
        ));
        idx += len;
    }

    if valid_ids.is_empty() || !text.contains("[paste #") {
        return base.into_iter().map(|(_, w)| w).collect();
    }
    let markers = marker_spans(text, valid_ids);
    if markers.is_empty() {
        return base.into_iter().map(|(_, w)| w).collect();
    }

    let units: Vec<u16> = text.encode_utf16().collect();
    let mut result: Vec<WordSeg> = Vec::new();
    let mut marker_idx = 0usize;
    for (index, seg) in base {
        while marker_idx < markers.len() && markers[marker_idx].end <= index {
            marker_idx += 1;
        }
        let marker = markers.get(marker_idx);
        if let Some(m) = marker {
            if index >= m.start && index < m.end {
                if index == m.start {
                    result.push(WordSeg {
                        segment: u16_slice(&units, m.start, m.end),
                        is_word_like: false,
                    });
                }
                continue;
            }
        }
        result.push(seg);
    }
    result
}

/// Expand every valid paste marker in `text` to its stored content, in the
/// insertion order of `pastes` (pi's `expandPasteMarkers`).
pub fn expand_paste_markers(text: &str, pastes: &[(u64, String)]) -> String {
    let mut result = text.to_string();
    for (paste_id, content) in pastes {
        let re = Regex::new(&format!(
            r"\[paste #{paste_id}( (\+\d+ lines|\d+ chars))?\]"
        ))
        .unwrap();
        // JS `String.replace(regex-with-g, () => content)` replaces all matches
        // with the literal content (no `$` substitution because a function
        // replacer is used).
        result = replace_all_literal(&re, &result, content);
    }
    result
}

// Replace every match of `re` in `text` with the literal `replacement`
// (no `$`-group substitution), mirroring a JS function replacer.
fn replace_all_literal(re: &Regex, text: &str, replacement: &str) -> String {
    let mut out = String::new();
    let mut last = 0usize;
    for m in re.find_iter(text).flatten() {
        out.push_str(&text[last..m.start()]);
        out.push_str(replacement);
        last = m.end();
    }
    out.push_str(&text[last..]);
    out
}

/// Global paste-marker matches within a single line, each as
/// `(byte_start, byte_end, id, suffix)` where `suffix` is capture group 2
/// (e.g. `" +20 lines"`, possibly empty). Used by the backspace renumber pass.
pub fn paste_marker_matches(line: &str) -> Vec<(usize, usize, u64, String)> {
    let mut out = Vec::new();
    for caps in PASTE_MARKER_REGEX.captures_iter(line).flatten() {
        let Some(m0) = caps.get(0) else { continue };
        let Some(id_m) = caps.get(1) else { continue };
        let Ok(id) = id_m.as_str().parse::<u64>() else {
            continue;
        };
        let suffix = caps
            .get(2)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        out.push((m0.start(), m0.end(), id, suffix));
    }
    out
}
