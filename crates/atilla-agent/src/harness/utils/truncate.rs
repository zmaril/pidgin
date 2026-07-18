// straitjacket-allow-file:duplication — a faithful transcription of pi's
// `truncate.ts`: `truncateHead` and `truncateTail` share pi's identical
// no-truncation `TruncationResult` return block and the same
// collect/limit-check shape by design, and the ported test bodies mirror pi's
// parallel `it(...)` cases. The clone detector reads these as duplicates;
// collapsing them would diverge from the source structure this port tracks.
//! Shared truncation utilities for tool outputs, mirroring
//! `packages/agent/src/harness/utils/truncate.ts`.
//!
//! Truncation is based on two independent limits — whichever is hit first wins:
//! a line limit (default 2000) and a byte limit (default 50KB). Output never
//! contains partial lines, except the bash tail-truncation edge case where a
//! single oversized final line is cut on a UTF-8 boundary.
//!
//! # Faithful divergence from pi
//!
//! pi's source treats strings as UTF-16 and hand-rolls a `utf8ByteLength` that
//! reconstructs surrogate pairs, because JS strings can contain lone surrogates.
//! Rust `str`/`String` are always well-formed UTF-8 and cannot hold a lone
//! surrogate, so byte length is simply [`str::len`] and the surrogate-repair
//! paths (`replaceUnpairedSurrogates`, the surrogate branches of
//! `truncateStringToBytesFromEnd`) collapse to natural UTF-8 char-boundary
//! handling. The observable behavior is identical for every well-formed input.

/// Default maximum number of lines.
pub const DEFAULT_MAX_LINES: usize = 2000;
/// Default maximum number of bytes (50KB).
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
/// Maximum characters per grep match line.
pub const GREP_MAX_LINE_LENGTH: usize = 500;

/// Which limit triggered truncation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

impl TruncatedBy {
    /// The wire string pi uses (`"lines" | "bytes"`).
    pub fn as_str(self) -> &'static str {
        match self {
            TruncatedBy::Lines => "lines",
            TruncatedBy::Bytes => "bytes",
        }
    }
}

/// The outcome of a truncation. Mirrors pi's `TruncationResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncationResult {
    /// The truncated content.
    pub content: String,
    /// Whether truncation occurred.
    pub truncated: bool,
    /// Which limit was hit, or `None` if not truncated.
    pub truncated_by: Option<TruncatedBy>,
    /// Total number of lines in the original content.
    pub total_lines: usize,
    /// Total number of bytes in the original content.
    pub total_bytes: usize,
    /// Number of complete lines in the truncated output.
    pub output_lines: usize,
    /// Number of bytes in the truncated output.
    pub output_bytes: usize,
    /// Whether the last line was partially truncated (tail edge case only).
    pub last_line_partial: bool,
    /// Whether the first line exceeded the byte limit (head truncation).
    pub first_line_exceeds_limit: bool,
    /// The max lines limit that was applied.
    pub max_lines: usize,
    /// The max bytes limit that was applied.
    pub max_bytes: usize,
}

/// Truncation limits. Mirrors pi's `TruncationOptions`; `None` fields fall back
/// to [`DEFAULT_MAX_LINES`]/[`DEFAULT_MAX_BYTES`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TruncationOptions {
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
}

/// UTF-8 byte length. Mirrors pi's `utf8ByteLength`; in Rust a `str` is already
/// UTF-8, so this is simply the byte length.
fn utf8_byte_length(content: &str) -> usize {
    content.len()
}

/// Format bytes as a human-readable size. Mirrors pi's `formatSize`.
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Truncate content from the head (keep the first N lines/bytes). Suitable for
/// file reads. Never returns partial lines; if the first line exceeds the byte
/// limit, returns empty content with `first_line_exceeds_limit == true`.
/// Mirrors pi's `truncateHead`.
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = utf8_byte_length(content);
    let lines: Vec<&str> = content.split('\n').collect();
    let total_lines = lines.len();

    // No truncation needed.
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    // First line alone exceeds the byte limit.
    let first_line_bytes = utf8_byte_length(lines[0]);
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    // Collect complete lines that fit.
    let mut output_lines_arr: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    for (i, line) in lines.iter().enumerate() {
        if i >= max_lines {
            break;
        }
        let line_bytes = utf8_byte_length(line) + usize::from(i > 0); // +1 for newline
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output_lines_arr.push(line);
        output_bytes_count += line_bytes;
    }

    // Exited due to the line limit.
    if output_lines_arr.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output_lines_arr.join("\n");
    let final_output_bytes = utf8_byte_length(&output_content);

    TruncationResult {
        output_lines: output_lines_arr.len(),
        output_bytes: final_output_bytes,
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Truncate content from the tail (keep the last N lines/bytes). Suitable for
/// bash output. May return a partial first line if the last line of the
/// original content exceeds the byte limit. Mirrors pi's `truncateTail`.
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = utf8_byte_length(content);
    let mut lines: Vec<&str> = content.split('\n').collect();
    if lines.len() > 1 && lines[lines.len() - 1].is_empty() {
        lines.pop();
    }
    let total_lines = lines.len();

    // No truncation needed.
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    // Work backwards from the end. Lines are collected in reverse and reversed
    // at the end, matching pi's `unshift` ordering.
    let mut output_rev: Vec<String> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;

    let mut i = lines.len();
    while i > 0 && output_rev.len() < max_lines {
        i -= 1;
        let line = lines[i];
        let line_bytes = utf8_byte_length(line) + usize::from(!output_rev.is_empty()); // +1 for newline

        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            // Edge case: no lines added yet and this line exceeds max_bytes —
            // take the end of the line (partial).
            if output_rev.is_empty() {
                let truncated_line = truncate_string_to_bytes_from_end(line, max_bytes);
                output_bytes_count = utf8_byte_length(&truncated_line);
                output_rev.push(truncated_line);
                last_line_partial = true;
            }
            break;
        }

        output_rev.push(line.to_string());
        output_bytes_count += line_bytes;
    }

    // Exited due to the line limit.
    if output_rev.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    output_rev.reverse();
    let output_content = output_rev.join("\n");
    let final_output_bytes = utf8_byte_length(&output_content);

    TruncationResult {
        output_lines: output_rev.len(),
        output_bytes: final_output_bytes,
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Truncate a string to fit within a byte limit, keeping the end. Handles
/// multi-byte UTF-8 characters correctly. Mirrors pi's
/// `truncateStringToBytesFromEnd` (its surrogate branches are unreachable for
/// well-formed Rust `str`).
fn truncate_string_to_bytes_from_end(s: &str, max_bytes: usize) -> String {
    if max_bytes == 0 {
        return String::new();
    }

    let mut output_bytes = 0usize;
    let mut start = s.len();
    for (idx, ch) in s.char_indices().rev() {
        let character_bytes = ch.len_utf8();
        if output_bytes + character_bytes > max_bytes {
            break;
        }
        output_bytes += character_bytes;
        start = idx;
    }
    s[start..].to_string()
}

/// Truncate a single line to `max_chars` characters, adding a `[truncated]`
/// suffix. Used for grep match lines. Mirrors pi's `truncateLine`.
///
/// pi measures with JS `String.length` (UTF-16 code units) and `slice`; this
/// port measures and slices on Unicode scalar (`char`) boundaries, which agrees
/// for the BMP text grep lines carry and never splits a code point.
pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    if line.chars().count() <= max_chars {
        return (line.to_string(), false);
    }
    let head: String = line.chars().take(max_chars).collect();
    (format!("{head}... [truncated]"), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(max_bytes: usize, max_lines: usize) -> TruncationOptions {
        TruncationOptions {
            max_bytes: Some(max_bytes),
            max_lines: Some(max_lines),
        }
    }

    // Reference tail truncation matching Node's `Buffer` byte-tail semantics,
    // ported from the test helper `bufferTail`: take the last `max_bytes` bytes,
    // then advance past UTF-8 continuation bytes to a char boundary.
    fn buffer_tail(content: &str, max_bytes: usize) -> String {
        let bytes = content.as_bytes();
        if bytes.len() <= max_bytes {
            return content.to_string();
        }
        let mut start = bytes.len() - max_bytes;
        while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
            start += 1;
        }
        std::str::from_utf8(&bytes[start..]).unwrap().to_string()
    }

    fn assert_matches_buffer_tail(input: &str, max_byte_values: &[usize]) {
        for &max_bytes in max_byte_values {
            let result = truncate_tail(input, opts(max_bytes, 10));
            let expected = buffer_tail(input, max_bytes);
            assert_eq!(
                result.content, expected,
                "tail mismatch input={input:?} max_bytes={max_bytes}"
            );
            assert!(
                result.content.len() <= max_bytes,
                "tail output exceeded byte limit input={input:?} max_bytes={max_bytes}"
            );
        }
    }

    #[test]
    fn counts_utf8_bytes() {
        let content = "aé\u{1F642}\nb";
        let result = truncate_head(content, opts(100, 10));
        assert!(!result.truncated);
        assert_eq!(result.total_bytes, content.len());
        assert_eq!(result.output_bytes, content.len());
        assert_eq!(result.total_bytes, 9);
    }

    #[test]
    fn truncates_head_on_utf8_byte_limits_without_partial_lines() {
        let content = "éé\nabc";
        let result = truncate_head(content, opts(4, 10));
        assert_eq!(result.content, "éé");
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(result.output_bytes, 4);
        assert!(!result.first_line_exceeds_limit);
    }

    #[test]
    fn reports_head_truncation_when_first_line_exceeds_byte_limit() {
        let result = truncate_head("éé\nabc", opts(3, 10));
        assert_eq!(result.content, "");
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert!(result.first_line_exceeds_limit);
    }

    #[test]
    fn truncates_tail_on_utf8_boundaries_when_only_partial_last_line_fits() {
        let result = truncate_tail("aé\u{1F642}b", opts(5, 10));
        assert_eq!(result.content, "\u{1F642}b");
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert!(result.last_line_partial);
        assert_eq!(result.output_bytes, 5);
    }

    #[test]
    fn truncates_oversized_single_line_with_trailing_newline() {
        let input = format!("{}\n", "X".repeat(300_000));
        let result = truncate_tail(&input, opts(1024, 100));
        assert_eq!(result.content, "X".repeat(1024));
        assert_eq!(result.output_bytes, 1024);
        assert_eq!(result.output_lines, 1);
        assert!(result.last_line_partial);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
    }

    #[test]
    fn drops_oversized_trailing_character_when_it_cannot_fit_in_tail_byte_limit() {
        let result = truncate_tail("abc\u{1F642}", opts(3, 10));
        assert_eq!(result.content, "");
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert!(result.last_line_partial);
        assert_eq!(result.output_bytes, 0);
    }

    #[test]
    fn matches_buffer_tail_semantics_for_multibyte_edge_cases() {
        // pi's surrogate-specific inputs (lone `\ud83d` etc.) cannot exist in a
        // Rust `str`; these well-formed multi-byte inputs exercise the same
        // UTF-8 boundary-alignment path.
        let inputs = [
            "a\u{1F642}",
            "\u{1F642}b",
            "a\u{1F642}b",
            "\u{1F642}\u{1F642}\u{1F642}",
            "\u{1F469}\u{200D}\u{1F4BB}",
            "中中中",
        ];
        for input in inputs {
            let total = input.len();
            let values: Vec<usize> = (0..=total + 5).collect();
            assert_matches_buffer_tail(input, &values);
        }
    }

    #[test]
    fn matches_buffer_tail_semantics_across_deterministic_fuzz_cases() {
        // Well-formed subset of pi's alphabet (lone surrogates excluded — they
        // are unrepresentable in Rust `str`).
        let alphabet = [
            "a",
            "\u{7f}",
            "\u{80}",
            "é",
            "\u{7ff}",
            "\u{800}",
            "中",
            "\u{d7ff}",
            "\u{1F642}",
            "\u{e000}",
            "\u{ffff}",
        ];

        fn sampled_byte_limits(input: &str) -> Vec<usize> {
            let total = input.len() as i64;
            let candidates = [
                0,
                1,
                2,
                3,
                4,
                5,
                8,
                total / 2 - 1,
                total / 2,
                total / 2 + 1,
                total - 8,
                total - 5,
                total - 4,
                total - 3,
                total - 2,
                total - 1,
                total,
                total + 1,
                total + 4,
            ];
            let mut values: Vec<usize> = candidates
                .into_iter()
                .filter(|value| *value >= 0)
                .map(|value| value as usize)
                .collect();
            values.sort_unstable();
            values.dedup();
            values
        }

        fn check_exhaustive(prefix: &str, depth: usize, alphabet: &[&str]) {
            let values = sampled_byte_limits(prefix);
            for &max_bytes in &values {
                let result = truncate_tail(
                    prefix,
                    TruncationOptions {
                        max_bytes: Some(max_bytes),
                        max_lines: Some(10),
                    },
                );
                let expected = buffer_tail(prefix, max_bytes);
                assert_eq!(
                    result.content, expected,
                    "prefix={prefix:?} max_bytes={max_bytes}"
                );
                assert!(result.content.len() <= max_bytes);
            }
            if depth == 0 {
                return;
            }
            for character in alphabet {
                let next = format!("{prefix}{character}");
                check_exhaustive(&next, depth - 1, alphabet);
            }
        }
        check_exhaustive("", 3, &alphabet);

        // Deterministic LCG fuzz, mirroring the test's `random()`.
        let mut seed: u64 = 0x1234_5678;
        let mut random = || {
            seed = (seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223)) & 0xffff_ffff;
            seed as f64 / 4_294_967_296.0
        };
        for _ in 0..1_000 {
            let mut input = String::new();
            let length = (random() * 80.0).floor() as usize;
            for _ in 0..length {
                input.push_str(alphabet[(random() * alphabet.len() as f64).floor() as usize]);
            }
            let values = sampled_byte_limits(&input);
            assert_matches_buffer_tail(&input, &values);
        }
    }

    #[test]
    fn format_size_matches_pi() {
        assert_eq!(format_size(512), "512B");
        assert_eq!(format_size(1536), "1.5KB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.0MB");
    }

    #[test]
    fn truncate_line_adds_suffix_past_limit() {
        assert_eq!(
            truncate_line("short", GREP_MAX_LINE_LENGTH),
            ("short".to_string(), false)
        );
        let long = "x".repeat(GREP_MAX_LINE_LENGTH + 10);
        let (text, was) = truncate_line(&long, GREP_MAX_LINE_LENGTH);
        assert!(was);
        assert_eq!(
            text,
            format!("{}... [truncated]", "x".repeat(GREP_MAX_LINE_LENGTH))
        );
    }
}
