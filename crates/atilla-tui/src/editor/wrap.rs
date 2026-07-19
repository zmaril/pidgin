//! Word-wrap layout for the editor, ported from `wordWrapLine` in
//! `vendor/pi/packages/tui/src/components/editor.ts`.
//!
//! Greedy grapheme-based wrapping with a "last wrap opportunity" backtrack,
//! whitespace/CJK break rules, force-break, and recursive splitting of an atomic
//! segment (e.g. a paste marker) wider than the available width. All indices are
//! UTF-16 offsets to match pi's JavaScript string slicing.

use unicode_segmentation::UnicodeSegmentation;

use crate::text_util::{is_cjk_script, is_whitespace_char};
use crate::width::visible_width;

use super::segment::{is_paste_marker, u16_len, u16_slice, Segment};

/// A chunk of text produced by word wrapping, tracking its UTF-16 span in the
/// original line (pi's `TextChunk`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    /// The chunk text.
    pub text: String,
    /// UTF-16 start offset within the source line.
    pub start_index: usize,
    /// UTF-16 end offset within the source line.
    pub end_index: usize,
}

// `cjkBreakRegex.test(s)` — true when any character of `s` is in the Han,
// Hiragana, Katakana, Hangul, or Bopomofo scripts (breaking is allowed between
// adjacent CJK characters).
fn is_cjk_break(s: &str) -> bool {
    s.chars().any(is_cjk_script)
}

/// Split a line into word-wrapped chunks.
///
/// `pre_segmented` optionally supplies paste-marker-aware grapheme segments;
/// when `None`, the line is segmented with the default grapheme segmenter.
pub fn word_wrap_line(
    line: &str,
    max_width: i64,
    pre_segmented: Option<&[Segment]>,
) -> Vec<TextChunk> {
    if line.is_empty() || max_width <= 0 {
        return vec![TextChunk {
            text: String::new(),
            start_index: 0,
            end_index: 0,
        }];
    }

    let line_width = visible_width(line) as i64;
    let line_u16_len = u16_len(line);
    if line_width <= max_width {
        return vec![TextChunk {
            text: line.to_string(),
            start_index: 0,
            end_index: line_u16_len,
        }];
    }

    let units: Vec<u16> = line.encode_utf16().collect();

    // Default grapheme segmentation when no pre-segmented graphemes are given.
    let owned_segments: Vec<Segment>;
    let segments: &[Segment] = match pre_segmented {
        Some(s) => s,
        None => {
            let mut base = Vec::new();
            let mut idx = 0usize;
            for g in line.graphemes(true) {
                base.push(Segment {
                    segment: g.to_string(),
                    index: idx,
                });
                idx += u16_len(g);
            }
            owned_segments = base;
            &owned_segments
        }
    };

    let mut chunks: Vec<TextChunk> = Vec::new();
    let mut current_width: i64 = 0;
    let mut chunk_start: usize = 0;
    let mut wrap_opp_index: i64 = -1;
    let mut wrap_opp_width: i64 = 0;

    for i in 0..segments.len() {
        let seg = &segments[i];
        let grapheme = seg.segment.as_str();
        let g_width = visible_width(grapheme) as i64;
        let char_index = seg.index;
        let is_ws = !is_paste_marker(grapheme) && is_whitespace_char(grapheme);

        // Overflow check before advancing.
        if current_width + g_width > max_width {
            if wrap_opp_index >= 0 && current_width - wrap_opp_width + g_width <= max_width {
                let wrap_opp = wrap_opp_index as usize;
                chunks.push(TextChunk {
                    text: u16_slice(&units, chunk_start, wrap_opp),
                    start_index: chunk_start,
                    end_index: wrap_opp,
                });
                chunk_start = wrap_opp;
                current_width -= wrap_opp_width;
            } else if chunk_start < char_index {
                chunks.push(TextChunk {
                    text: u16_slice(&units, chunk_start, char_index),
                    start_index: chunk_start,
                    end_index: char_index,
                });
                chunk_start = char_index;
                current_width = 0;
            }
            wrap_opp_index = -1;
        }

        if g_width > max_width {
            // Single atomic segment wider than max_width. Re-wrap at grapheme
            // granularity; it remains logically atomic (the split is visual).
            let sub_chunks = word_wrap_line(grapheme, max_width, None);
            let sub_len = sub_chunks.len();
            for sc in &sub_chunks[..sub_len.saturating_sub(1)] {
                chunks.push(TextChunk {
                    text: sc.text.clone(),
                    start_index: char_index + sc.start_index,
                    end_index: char_index + sc.end_index,
                });
            }
            let last = &sub_chunks[sub_len - 1];
            chunk_start = char_index + last.start_index;
            current_width = visible_width(&last.text) as i64;
            wrap_opp_index = -1;
            continue;
        }

        // Advance.
        current_width += g_width;

        // Record a wrap opportunity.
        if let Some(next) = segments.get(i + 1) {
            if is_ws && (is_paste_marker(&next.segment) || !is_whitespace_char(&next.segment)) {
                wrap_opp_index = next.index as i64;
                wrap_opp_width = current_width;
            } else if !is_ws && !is_whitespace_char(&next.segment) {
                let is_cjk = !is_paste_marker(grapheme) && is_cjk_break(grapheme);
                let next_is_cjk = !is_paste_marker(&next.segment) && is_cjk_break(&next.segment);
                if is_cjk || next_is_cjk {
                    wrap_opp_index = next.index as i64;
                    wrap_opp_width = current_width;
                }
            }
        }
    }

    // Push final chunk.
    chunks.push(TextChunk {
        text: u16_slice(&units, chunk_start, line_u16_len),
        start_index: chunk_start,
        end_index: line_u16_len,
    });

    chunks
}
