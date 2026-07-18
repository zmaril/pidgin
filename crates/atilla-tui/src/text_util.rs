//! Port of the character-classification, word-segmentation, and
//! background-fill helpers from `vendor/pi/packages/tui/src/utils.ts` that the
//! editor-support components (word navigation, kill ring, etc.) depend on.
//!
//! # Word segmentation parity
//!
//! pi segments words with `Intl.Segmenter(undefined, { granularity: "word" })`,
//! which in V8 is backed by ICU4C. The Rust port uses the `icu_segmenter`
//! crate (ICU4X). The two agree on the entire word-navigation corpus **except**
//! one tailoring: ICU4C classifies U+003A COLON as `MidLetter`, so a lone `:`
//! between two ALetter runs joins them into a single word (`"foo:bar"` ->
//! one word-like segment); ICU4X does not, and splits (`"foo"`, `":"`, `"bar"`).
//! Digits (`"12:30"`) and CJK (`"你:好"`) never join in either engine.
//!
//! [`word_segment`] reproduces pi exactly by running ICU4X and then applying a
//! single left-to-right colon-merge pass matching ICU4C's `MidLetter`
//! behaviour. This is the same "match pi, not the crate" approach `width.rs`
//! took for grapheme/EAW parity, validated by the `word_segmentation` vectors.

use icu_segmenter::options::WordType;
use icu_segmenter::WordSegmenter;

use crate::width::visible_width;

// JS `\s` (single UTF-16 code unit) matches this set of code points.
pub(crate) fn is_js_space_unit(u: u16) -> bool {
    matches!(
        u,
        0x0009 | 0x000a | 0x000b | 0x000c | 0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000
            ..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

// `PUNCTUATION_REGEX = /[(){}[\]<>.,;:'"!?+\-=*/\\|&%^$#@~`]/` — all ASCII, so
// a single UTF-16 code unit fully determines membership.
pub(crate) fn is_punctuation_unit(u: u16) -> bool {
    // ASCII of `[(){}[\]<>.,;:'"!?+\-=*/\\|&%^$#@~`]`.
    matches!(
        u,
        0x28 | 0x29
            | 0x7b
            | 0x7d
            | 0x5b
            | 0x5d
            | 0x3c
            | 0x3e
            | 0x2e
            | 0x2c
            | 0x3b
            | 0x3a
            | 0x27
            | 0x22
            | 0x21
            | 0x3f
            | 0x2b
            | 0x2d
            | 0x3d
            | 0x2a
            | 0x2f
            | 0x5c
            | 0x7c
            | 0x26
            | 0x25
            | 0x5e
            | 0x24
            | 0x23
            | 0x40
            | 0x7e
            | 0x60
    )
}

/// `isWhitespaceChar` — `true` if the string contains any JS-`\s` code unit
/// (`/\s/.test(char)` is a "contains" test).
pub fn is_whitespace_char(s: &str) -> bool {
    s.encode_utf16().any(is_js_space_unit)
}

/// `isPunctuationChar` — `true` if the string contains any punctuation code
/// unit (`PUNCTUATION_REGEX.test(char)`).
pub fn is_punctuation_char(s: &str) -> bool {
    s.encode_utf16().any(is_punctuation_unit)
}

/// A single word segment: the substring and whether it is word-like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordSegment {
    /// The segment text.
    pub segment: String,
    /// Whether ICU classifies the segment as word-like (letter or number).
    pub is_word_like: bool,
}

// A char is UAX#29 ALetter for the colon-merge if it is alphabetic but not one
// of the ideographic / syllabic scripts that ICU dictionary-segments (Han,
// Hiragana, Katakana, Hangul, Bopomofo — the cjkBreakRegex scripts). Those are
// not ALetter, so `:` never joins them, matching ICU4C.
fn is_cjk_script(ch: char) -> bool {
    let c = ch as u32;
    (0x2e80..=0x2eff).contains(&c) // CJK Radicals Supplement
        || (0x2f00..=0x2fdf).contains(&c) // Kangxi Radicals
        || (0x3040..=0x309f).contains(&c) // Hiragana
        || (0x30a0..=0x30ff).contains(&c) // Katakana
        || (0x3100..=0x312f).contains(&c) // Bopomofo
        || (0x3130..=0x318f).contains(&c) // Hangul Compatibility Jamo
        || (0x31a0..=0x31bf).contains(&c) // Bopomofo Extended
        || (0x31f0..=0x31ff).contains(&c) // Katakana Phonetic Extensions
        || (0x3400..=0x4dbf).contains(&c) // CJK Ext A
        || (0x4e00..=0x9fff).contains(&c) // CJK Unified
        || (0x1100..=0x11ff).contains(&c) // Hangul Jamo
        || (0xa960..=0xa97f).contains(&c) // Hangul Jamo Extended-A
        || (0xac00..=0xd7ff).contains(&c) // Hangul Syllables + Extended-B
        || (0xf900..=0xfaff).contains(&c) // CJK Compatibility Ideographs
        || (0x20000..=0x2fa1f).contains(&c) // CJK Ext B..F + compat supplement
}

fn is_aletter(ch: char) -> bool {
    ch.is_alphabetic() && !is_cjk_script(ch)
}

fn ends_with_aletter(s: &str) -> bool {
    s.chars().next_back().is_some_and(is_aletter)
}

fn starts_with_aletter(s: &str) -> bool {
    s.chars().next().is_some_and(is_aletter)
}

/// Segment `text` into word segments, matching pi's
/// `Intl.Segmenter(granularity: "word")` (see module docs for the colon
/// tailoring that reconciles ICU4X with ICU4C).
pub fn word_segment(text: &str) -> Vec<WordSegment> {
    if text.is_empty() {
        return Vec::new();
    }

    // Raw ICU4X segmentation. `new_auto` returns a cheap `Copy` handle that
    // borrows statically-compiled data, so there is nothing to cache.
    let seg = WordSegmenter::new_auto(Default::default());
    let mut iter = seg.segment_str(text);
    let mut raw: Vec<WordSegment> = Vec::new();
    let mut prev = 0usize;
    while let Some(idx) = iter.next() {
        if idx == 0 {
            continue;
        }
        let piece = &text[prev..idx];
        // ICU4X's `word_type()` is unreliable for complex-script (CJK
        // dictionary) segments — it reports `None` for e.g. the second word of
        // "你好世界" — whereas ICU4C/`Intl.Segmenter` always marks
        // ideograph/kana segments as word-like. Force word-like for any segment
        // containing a CJK-script character to match pi.
        let is_word_like = matches!(iter.word_type(), WordType::Number | WordType::Letter)
            || piece.chars().any(is_cjk_script);
        raw.push(WordSegment {
            segment: piece.to_string(),
            is_word_like,
        });
        prev = idx;
    }

    // Colon-merge pass: `<ALetter word> ":" <ALetter word>` -> one word.
    let mut out: Vec<WordSegment> = Vec::with_capacity(raw.len());
    let mut i = 0usize;
    while i < raw.len() {
        let seg = &raw[i];
        if seg.segment == ":" && !seg.is_word_like && i + 1 < raw.len() {
            if let Some(last) = out.last() {
                let next = &raw[i + 1];
                if last.is_word_like
                    && ends_with_aletter(&last.segment)
                    && next.is_word_like
                    && starts_with_aletter(&next.segment)
                {
                    let mut merged = out.pop().expect("out non-empty");
                    merged.segment.push(':');
                    merged.segment.push_str(&next.segment);
                    out.push(merged);
                    i += 2;
                    continue;
                }
            }
        }
        out.push(seg.clone());
        i += 1;
    }

    out
}

/// `applyBackgroundToLine` — pad `line` to `width` visible columns, then apply
/// `bg_fn` to the padded content.
pub fn apply_background_to_line<F>(line: &str, width: usize, bg_fn: F) -> String
where
    F: Fn(&str) -> String,
{
    let visible_len = visible_width(line);
    let padding_needed = width.saturating_sub(visible_len);
    let padding = " ".repeat(padding_needed);
    let with_padding = format!("{line}{padding}");
    bg_fn(&with_padding)
}
