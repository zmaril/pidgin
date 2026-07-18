//! Bit-exact port of pi's `fuzzy.ts`
//! (`vendor/pi/packages/tui/src/fuzzy.ts`).
//!
//! Bespoke fuzzy scorer. Matches if all query characters appear in order (not
//! necessarily consecutive). Lower score = better match. The scoring constants
//! (`-5 * consecutive`, `+2 * gap`, `-10` word boundary, `+0.1 * i` position,
//! `-100` exact, `+5` swapped alpha/numeric) are reproduced exactly, including
//! the floating-point position term, so scores are byte-identical to pi.
//!
//! pi operates on JavaScript strings, which index by UTF-16 code unit. The port
//! therefore scores over UTF-16 code units (`i`, lengths, and single-code-unit
//! character comparisons all use UTF-16), so scores match pi even for
//! non-ASCII text.

/// Result of a fuzzy match: whether it matched and the (lower-is-better) score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FuzzyMatch {
    /// Whether the query matched the text.
    pub matches: bool,
    /// Match score; lower is better. Meaningful only when `matches` is true.
    pub score: f64,
}

use crate::text_util::is_js_space_unit;

// Word-boundary test on the preceding code unit: `/[\s\-_./:]/`.
fn is_word_boundary_unit(u: u16) -> bool {
    // ASCII: `-` 0x2d, `_` 0x5f, `.` 0x2e, `/` 0x2f, `:` 0x3a.
    is_js_space_unit(u) || matches!(u, 0x2d | 0x5f | 0x2e | 0x2f | 0x3a)
}

// Match `query` (already lowercased UTF-16) against `text_lower` (UTF-16).
fn match_query(normalized_query: &[u16], text_lower: &[u16]) -> FuzzyMatch {
    if normalized_query.is_empty() {
        return FuzzyMatch {
            matches: true,
            score: 0.0,
        };
    }

    if normalized_query.len() > text_lower.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    let mut query_index: usize = 0;
    let mut score: f64 = 0.0;
    let mut last_match_index: i64 = -1;
    let mut consecutive_matches: i64 = 0;

    let mut i: usize = 0;
    while i < text_lower.len() && query_index < normalized_query.len() {
        if text_lower[i] == normalized_query[query_index] {
            let is_word_boundary = i == 0 || is_word_boundary_unit(text_lower[i - 1]);

            // Reward consecutive matches
            if last_match_index == i as i64 - 1 {
                consecutive_matches += 1;
                score -= (consecutive_matches * 5) as f64;
            } else {
                consecutive_matches = 0;
                // Penalize gaps
                if last_match_index >= 0 {
                    score += ((i as i64 - last_match_index - 1) * 2) as f64;
                }
            }

            // Reward word boundary matches
            if is_word_boundary {
                score -= 10.0;
            }

            // Slight penalty for later matches
            score += i as f64 * 0.1;

            last_match_index = i as i64;
            query_index += 1;
        }
        i += 1;
    }

    if query_index < normalized_query.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    if normalized_query == text_lower {
        score -= 100.0;
    }

    FuzzyMatch {
        matches: true,
        score,
    }
}

const ASCII_LOWER: (u16, u16) = (b'a' as u16, b'z' as u16);
const ASCII_DIGIT: (u16, u16) = (b'0' as u16, b'9' as u16);

// Parse `^(<first>+)(<second>+)$` (each an inclusive code-unit range) on a
// lowercased UTF-16 slice, returning the two non-empty runs if the whole slice
// is consumed. Backs both `letters+digits` and `digits+letters` splits.
fn split_two_runs(q: &[u16], first: (u16, u16), second: (u16, u16)) -> Option<(&[u16], &[u16])> {
    let mut i = 0;
    while i < q.len() && (first.0..=first.1).contains(&q[i]) {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let boundary = i;
    while i < q.len() && (second.0..=second.1).contains(&q[i]) {
        i += 1;
    }
    if i == boundary || i != q.len() {
        return None;
    }
    Some((&q[..boundary], &q[boundary..]))
}

/// Fuzzy-match `query` against `text`. See module docs for the scoring model.
pub fn fuzzy_match(query: &str, text: &str) -> FuzzyMatch {
    let query_lower: Vec<u16> = query.to_lowercase().encode_utf16().collect();
    let text_lower: Vec<u16> = text.to_lowercase().encode_utf16().collect();

    let primary_match = match_query(&query_lower, &text_lower);
    if primary_match.matches {
        return primary_match;
    }

    // Swapped alpha/numeric fallback: `letters+digits` -> `digits+letters`
    // (and vice versa). In both orderings the swap is `second_run ++ first_run`.
    let swapped_query: Vec<u16> = split_two_runs(&query_lower, ASCII_LOWER, ASCII_DIGIT)
        .or_else(|| split_two_runs(&query_lower, ASCII_DIGIT, ASCII_LOWER))
        .map(|(first, second)| second.iter().chain(first.iter()).copied().collect())
        .unwrap_or_default();

    if swapped_query.is_empty() {
        return primary_match;
    }

    let swapped_match = match_query(&swapped_query, &text_lower);
    if !swapped_match.matches {
        return primary_match;
    }

    FuzzyMatch {
        matches: true,
        score: swapped_match.score + 5.0,
    }
}

/// Filter and sort items by fuzzy match quality (best matches first). Supports
/// whitespace- and slash-separated tokens: all tokens must match.
pub fn fuzzy_filter<T, F>(items: Vec<T>, query: &str, get_text: F) -> Vec<T>
where
    F: Fn(&T) -> String,
{
    let trimmed = js_trim(query);
    if trimmed.is_empty() {
        return items;
    }

    let tokens: Vec<String> = split_ws_or_slash(trimmed);

    if tokens.is_empty() {
        return items;
    }

    // (index-into-items via ownership, totalScore). We keep items owned and
    // stable-sort a parallel list of (item, score).
    let mut results: Vec<(T, f64)> = Vec::new();

    for item in items {
        let text = get_text(&item);
        let mut total_score: f64 = 0.0;
        let mut all_match = true;

        for token in &tokens {
            let m = fuzzy_match(token, &text);
            if m.matches {
                total_score += m.score;
            } else {
                all_match = false;
                break;
            }
        }

        if all_match {
            results.push((item, total_score));
        }
    }

    // Stable sort ascending by totalScore, matching JS `Array.prototype.sort`.
    results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    results.into_iter().map(|(item, _)| item).collect()
}

// JS `\s` as a `char` predicate (all such code points are in the BMP, so the
// UTF-16-unit and `char` views coincide here).
fn is_js_space_char(ch: char) -> bool {
    (ch as u32) <= 0xffff && is_js_space_unit(ch as u16)
}

// JS `String.prototype.trim`: strip leading/trailing JS whitespace. (JS's trim
// set is `\s` plus line terminators; all are covered by `is_js_space_char`.)
fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space_char)
}

// Split on runs of whitespace or `/`, dropping empty pieces: `/[\s/]+/`.
fn split_ws_or_slash(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if is_js_space_char(ch) || ch == '/' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
