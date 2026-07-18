// straitjacket-allow-file:duplication — truncate_head and truncate_tail are a
// deliberate 1:1 mirror of pi's separate truncateHead/truncateTail exports; the
// shared setup preamble and TruncationResult construction are intentional
// head/tail symmetry that keeps each variant auditable against the source.
//! Shared truncation utilities for tool outputs.
//!
//! Ported from pi's `core/tools/truncate.ts`. Truncation is based on two
//! independent limits - whichever is hit first wins:
//! - Line limit (default: 2000 lines)
//! - Byte limit (default: 50KB)
//!
//! Never returns partial lines (except the bash tail truncation edge case).
//!
//! All limits are measured in **UTF-8 bytes**, matching pi's use of
//! `Buffer.byteLength(content, "utf-8")`. In Rust, `str::len()` already yields
//! the UTF-8 byte length, so it is used directly. `truncate_line` is the one
//! exception: it counts Unicode scalar values (Rust `char`s) rather than
//! pi's UTF-16 code units; this differs only for text outside the Basic
//! Multilingual Plane, which grep match lines are not expected to contain.

/// Default maximum number of lines retained before truncation.
pub const DEFAULT_MAX_LINES: usize = 2000;
/// Default maximum number of bytes retained before truncation (50KB).
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
/// Maximum characters per grep match line.
pub const GREP_MAX_LINE_LENGTH: usize = 500;

/// Which limit triggered truncation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    /// The line-count limit was reached first.
    Lines,
    /// The byte-count limit was reached first.
    Bytes,
}

/// Result of a truncation operation, mirroring pi's `TruncationResult`.
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

/// Options controlling truncation limits.
#[derive(Debug, Clone, Copy)]
pub struct TruncationOptions {
    /// Maximum number of lines.
    pub max_lines: usize,
    /// Maximum number of bytes.
    pub max_bytes: usize,
}

impl Default for TruncationOptions {
    fn default() -> Self {
        Self {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// Split content into countable lines: a trailing newline does not create an
/// extra empty line. Empty content yields no lines.
fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Format bytes as a human-readable size (`B` / `KB` / `MB`, one decimal).
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn not_truncated(
    content: &str,
    total_lines: usize,
    total_bytes: usize,
    opts: TruncationOptions,
) -> TruncationResult {
    TruncationResult {
        content: content.to_string(),
        truncated: false,
        truncated_by: None,
        total_lines,
        total_bytes,
        output_lines: total_lines,
        output_bytes: total_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: opts.max_lines,
        max_bytes: opts.max_bytes,
    }
}

/// Truncate content from the head (keep the first N lines/bytes).
///
/// Never returns partial lines. If the first line exceeds the byte limit,
/// returns empty content with `first_line_exceeds_limit = true`.
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines;
    let max_bytes = options.max_bytes;

    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return not_truncated(content, total_lines, total_bytes, options);
    }

    // Check if the first line alone exceeds the byte limit.
    let first_line_bytes = lines[0].len();
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

    let mut output_lines_arr: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    let mut i = 0;
    while i < lines.len() && i < max_lines {
        let line = lines[i];
        let line_bytes = line.len() + if i > 0 { 1 } else { 0 };
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output_lines_arr.push(line);
        output_bytes_count += line_bytes;
        i += 1;
    }

    if output_lines_arr.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output_lines_arr.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines_arr.len(),
        output_bytes: final_output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Truncate content from the tail (keep the last N lines/bytes).
///
/// May return a partial first line if the last line of the original content
/// exceeds the byte limit.
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines;
    let max_bytes = options.max_bytes;

    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return not_truncated(content, total_lines, total_bytes, options);
    }

    let mut output_lines_arr: Vec<String> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;

    let mut i = lines.len() as isize - 1;
    while i >= 0 && output_lines_arr.len() < max_lines {
        let line = lines[i as usize];
        let line_bytes = line.len() + if !output_lines_arr.is_empty() { 1 } else { 0 };
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            if output_lines_arr.is_empty() {
                let truncated_line = truncate_string_to_bytes_from_end(line, max_bytes);
                output_bytes_count = truncated_line.len();
                output_lines_arr.insert(0, truncated_line);
                last_line_partial = true;
            }
            break;
        }
        output_lines_arr.insert(0, line.to_string());
        output_bytes_count += line_bytes;
        i -= 1;
    }

    if output_lines_arr.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output_lines_arr.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines_arr.len(),
        output_bytes: final_output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Truncate a string to fit within a byte limit, keeping the end. Handles
/// multi-byte UTF-8 characters by advancing to the next character boundary.
fn truncate_string_to_bytes_from_end(s: &str, max_bytes: usize) -> String {
    let buf = s.as_bytes();
    if buf.len() <= max_bytes {
        return s.to_string();
    }
    let mut start = buf.len() - max_bytes;
    // Advance past UTF-8 continuation bytes to find a char boundary.
    while start < buf.len() && (buf[start] & 0xc0) == 0x80 {
        start += 1;
    }
    String::from_utf8_lossy(&buf[start..]).into_owned()
}

/// A single line truncated to `max_chars` characters, with a marker suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncatedLine {
    /// The possibly-truncated line text.
    pub text: String,
    /// Whether truncation occurred.
    pub was_truncated: bool,
}

/// Truncate a single line to `max_chars` characters, adding a `[truncated]`
/// suffix. Used for grep match lines.
pub fn truncate_line(line: &str, max_chars: usize) -> TruncatedLine {
    if line.chars().count() <= max_chars {
        return TruncatedLine {
            text: line.to_string(),
            was_truncated: false,
        };
    }
    let head: String = line.chars().take(max_chars).collect();
    TruncatedLine {
        text: format!("{head}... [truncated]"),
        was_truncated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(max_lines: usize, max_bytes: usize) -> TruncationOptions {
        TruncationOptions {
            max_lines,
            max_bytes,
        }
    }

    #[test]
    fn format_size_units() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(512), "512B");
        assert_eq!(format_size(1023), "1023B");
        assert_eq!(format_size(1024), "1.0KB");
        assert_eq!(format_size(1536), "1.5KB");
        assert_eq!(format_size(50 * 1024), "50.0KB");
        assert_eq!(format_size(1024 * 1024), "1.0MB");
        assert_eq!(format_size(3 * 1024 * 1024 / 2), "1.5MB");
    }

    #[test]
    fn empty_content_is_not_truncated() {
        let r = truncate_head("", TruncationOptions::default());
        assert!(!r.truncated);
        assert_eq!(r.total_lines, 0);
        assert_eq!(r.total_bytes, 0);
    }

    #[test]
    fn trailing_newline_not_counted_as_extra_line() {
        let r = truncate_head("a\nb\n", TruncationOptions::default());
        assert!(!r.truncated);
        assert_eq!(r.total_lines, 2);
    }

    #[test]
    fn head_stops_at_line_limit() {
        let content = "l1\nl2\nl3\nl4\nl5";
        let r = truncate_head(content, opts(3, DEFAULT_MAX_BYTES));
        assert!(r.truncated);
        assert_eq!(r.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(r.output_lines, 3);
        assert_eq!(r.content, "l1\nl2\nl3");
        assert!(!r.last_line_partial);
    }

    #[test]
    fn head_stops_at_byte_limit_without_partial_line() {
        // Each "xxxx" line is 4 bytes; plus a newline between lines.
        let content = "xxxx\nyyyy\nzzzz";
        // Room for "xxxx" (4) + "\nyyyy" (5) = 9 bytes; "zzzz" would push to 14.
        let r = truncate_head(content, opts(DEFAULT_MAX_LINES, 9));
        assert!(r.truncated);
        assert_eq!(r.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(r.content, "xxxx\nyyyy");
        // No partial lines on head truncation.
        assert!(!r.content.ends_with("zz"));
    }

    #[test]
    fn head_first_line_exceeds_limit() {
        let content = "this-line-is-way-too-long\nshort";
        let r = truncate_head(content, opts(DEFAULT_MAX_LINES, 5));
        assert!(r.truncated);
        assert!(r.first_line_exceeds_limit);
        assert_eq!(r.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(r.content, "");
        assert_eq!(r.output_lines, 0);
    }

    #[test]
    fn head_line_limit_wins_when_both_exceeded() {
        // 5 lines, small bytes. Line limit 2 hits before the generous byte cap.
        let content = "a\nb\nc\nd\ne";
        let r = truncate_head(content, opts(2, DEFAULT_MAX_BYTES));
        assert_eq!(r.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(r.content, "a\nb");
    }

    #[test]
    fn tail_keeps_last_lines() {
        let content = "l1\nl2\nl3\nl4\nl5";
        let r = truncate_tail(content, opts(2, DEFAULT_MAX_BYTES));
        assert!(r.truncated);
        assert_eq!(r.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(r.content, "l4\nl5");
        assert!(!r.last_line_partial);
    }

    #[test]
    fn tail_partial_last_line_when_single_line_exceeds_bytes() {
        let content = "0123456789";
        let r = truncate_tail(content, opts(DEFAULT_MAX_LINES, 4));
        assert!(r.truncated);
        assert!(r.last_line_partial);
        assert_eq!(r.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(r.content, "6789");
    }

    #[test]
    fn tail_respects_utf8_boundary_on_partial() {
        // "é" is two bytes (0xC3 0xA9). Cutting mid-character must advance to a
        // boundary rather than split it.
        let content = "aéébb"; // bytes: a(1) é(2) é(2) b(1) b(1) = 7 bytes
        let r = truncate_tail(content, opts(DEFAULT_MAX_LINES, 3));
        assert!(r.last_line_partial);
        // Whatever is returned must be valid UTF-8 and a suffix of the input.
        assert!(content.ends_with(&r.content));
        assert!(r.content.len() <= 3);
    }

    #[test]
    fn byte_counts_use_utf8_bytes_not_chars() {
        // Two 3-byte characters = 6 bytes on one line.
        let content = "世界";
        let r = truncate_head(content, TruncationOptions::default());
        assert_eq!(r.total_bytes, 6);
        assert_eq!(r.total_lines, 1);
    }

    #[test]
    fn truncate_line_short_passes_through() {
        let r = truncate_line("hello", GREP_MAX_LINE_LENGTH);
        assert!(!r.was_truncated);
        assert_eq!(r.text, "hello");
    }

    #[test]
    fn truncate_line_long_gets_marker() {
        let line = "x".repeat(600);
        let r = truncate_line(&line, GREP_MAX_LINE_LENGTH);
        assert!(r.was_truncated);
        assert!(r.text.ends_with("... [truncated]"));
        assert_eq!(
            r.text.chars().count(),
            GREP_MAX_LINE_LENGTH + "... [truncated]".len()
        );
    }
}
