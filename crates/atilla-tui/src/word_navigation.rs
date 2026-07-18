//! Bit-exact port of pi's `word-navigation.ts`
//! (`vendor/pi/packages/tui/src/word-navigation.ts`).
//!
//! `find_word_backward` / `find_word_forward` compute the cursor position after
//! moving one word. pi's cursors are JavaScript string indices — UTF-16 code
//! units — so this port does all cursor arithmetic in UTF-16 units and slices
//! the text on UTF-16 boundaries. Segment lengths and `PUNCTUATION_REGEX`
//! match offsets are likewise measured in UTF-16 units.
//!
//! Word segmentation goes through [`crate::text_util::word_segment`], which
//! matches pi's `Intl.Segmenter(granularity: "word")` (see that module for the
//! colon tailoring).

use crate::text_util::{is_punctuation_unit, is_whitespace_char, word_segment, WordSegment};

/// A custom segmenter: maps a (sliced) text to its word segments.
pub type SegmentFn<'a> = Box<dyn Fn(&str) -> Vec<WordSegment> + 'a>;
/// A predicate marking atomic (indivisible) segments, e.g. paste markers.
pub type IsAtomicFn<'a> = Box<dyn Fn(&str) -> bool + 'a>;

/// Options for word navigation, mirroring pi's `WordNavigationOptions`.
///
/// When `segment` is `None`, the default `Intl.Segmenter`-equivalent
/// segmentation is used. `is_atomic_segment` marks segments (e.g. paste
/// markers) that must be treated as single indivisible units.
#[derive(Default)]
pub struct WordNavOptions<'a> {
    /// Custom segmenter returning word segments for the given text.
    pub segment: Option<SegmentFn<'a>>,
    /// Predicate identifying atomic segments to treat as single units.
    pub is_atomic_segment: Option<IsAtomicFn<'a>>,
}

impl WordNavOptions<'_> {
    fn segment(&self, text: &str) -> Vec<WordSegment> {
        match &self.segment {
            Some(f) => f(text),
            None => word_segment(text),
        }
    }

    fn is_atomic(&self, segment: &str) -> bool {
        match &self.is_atomic_segment {
            Some(f) => f(segment),
            None => false,
        }
    }
}

// UTF-16 length of a string (JS `string.length`).
fn u16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

// UTF-16 slice `[0..end]` reconstructed as a `String`. `end` is a valid UTF-16
// boundary in the word-navigation corpus (cursors never split a surrogate).
fn u16_slice_to(text_units: &[u16], end: usize) -> String {
    String::from_utf16(&text_units[..end]).expect("cursor on a valid UTF-16 boundary")
}

fn u16_slice_from(text_units: &[u16], start: usize) -> String {
    String::from_utf16(&text_units[start..]).expect("cursor on a valid UTF-16 boundary")
}

// Index (UTF-16, within `s`) just past the last punctuation code unit, i.e.
// `lastMatch.index + lastMatch[0].length` for the final `PUNCTUATION_REGEX`
// match. `None` when there is no punctuation.
fn last_punct_end_u16(s: &str) -> Option<usize> {
    let mut result = None;
    for (i, u) in s.encode_utf16().enumerate() {
        if is_punctuation_unit(u) {
            result = Some(i + 1);
        }
    }
    result
}

// Index (UTF-16, within `s`) of the first punctuation code unit
// (`PUNCTUATION_REGEX.exec(s)?.index`). `None` when there is no punctuation.
fn first_punct_index_u16(s: &str) -> Option<usize> {
    s.encode_utf16().position(is_punctuation_unit)
}

/// Find the cursor position after moving one word backward from `cursor`.
pub fn find_word_backward(text: &str, cursor: usize, options: &WordNavOptions) -> usize {
    if cursor == 0 {
        return 0;
    }

    let units: Vec<u16> = text.encode_utf16().collect();
    let text_before = u16_slice_to(&units, cursor);
    let mut segments = options.segment(&text_before);
    let mut new_cursor = cursor;

    // Skip trailing whitespace.
    while let Some(last) = segments.last() {
        if !options.is_atomic(&last.segment) && is_whitespace_char(&last.segment) {
            new_cursor -= u16_len(&last.segment);
            segments.pop();
        } else {
            break;
        }
    }

    let Some(last) = segments.last() else {
        return new_cursor;
    };

    if options.is_atomic(&last.segment) {
        // Skip one atomic segment.
        new_cursor -= u16_len(&last.segment);
    } else if last.is_word_like {
        // Skip inside one word-like segment, preserving ASCII punctuation
        // boundaries.
        let seg_len = u16_len(&last.segment);
        match last_punct_end_u16(&last.segment) {
            None => new_cursor -= seg_len,
            Some(end) => new_cursor -= seg_len - end,
        }
    } else {
        // Skip a non-word non-whitespace run (punctuation).
        while let Some(last) = segments.last() {
            if !options.is_atomic(&last.segment)
                && !last.is_word_like
                && !is_whitespace_char(&last.segment)
            {
                new_cursor -= u16_len(&last.segment);
                segments.pop();
            } else {
                break;
            }
        }
    }

    new_cursor
}

/// Find the cursor position after moving one word forward from `cursor`.
pub fn find_word_forward(text: &str, cursor: usize, options: &WordNavOptions) -> usize {
    let units: Vec<u16> = text.encode_utf16().collect();
    let text_len = units.len();
    if cursor >= text_len {
        return text_len;
    }

    let text_after = u16_slice_from(&units, cursor);
    let segments = options.segment(&text_after);
    let mut idx = 0usize;
    let mut new_cursor = cursor;

    // Skip leading whitespace.
    while idx < segments.len() {
        let seg = &segments[idx];
        if !options.is_atomic(&seg.segment) && is_whitespace_char(&seg.segment) {
            new_cursor += u16_len(&seg.segment);
            idx += 1;
        } else {
            break;
        }
    }

    if idx >= segments.len() {
        return new_cursor;
    }

    let cur = &segments[idx];
    if options.is_atomic(&cur.segment) {
        // Skip one atomic segment.
        new_cursor += u16_len(&cur.segment);
    } else if cur.is_word_like {
        // Skip inside one word-like segment, preserving ASCII punctuation
        // boundaries.
        new_cursor += first_punct_index_u16(&cur.segment).unwrap_or_else(|| u16_len(&cur.segment));
    } else {
        // Skip a non-word non-whitespace run (punctuation).
        while idx < segments.len() {
            let seg = &segments[idx];
            if !options.is_atomic(&seg.segment)
                && !seg.is_word_like
                && !is_whitespace_char(&seg.segment)
            {
                new_cursor += u16_len(&seg.segment);
                idx += 1;
            } else {
                break;
            }
        }
    }

    new_cursor
}
