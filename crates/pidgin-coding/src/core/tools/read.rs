//! Pure text-slicing and continuation-notice layer of the read tool.
//!
//! Ported from pi's `core/tools/read.ts`. The offset/limit slicing (1-indexed
//! to 0-indexed), `truncate_head` application, and the exact continuation
//! notices are pure and live in [`format_text_read`]. The surrounding tool
//! (filesystem read, image detection/processing, syntax highlighting, and TUI
//! rendering) is a deferred seam: those need an execution environment plus the
//! image and theme layers, so this module takes the already-read file content
//! as input and returns the formatted text + truncation details.

use super::truncate::{
    format_size, truncate_head, TruncatedBy, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES,
};

/// The formatted output of a text read: display text plus optional truncation
/// details (present only when truncation or a first-line-too-long condition
/// occurred, matching pi's `details` assignment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadTextOutput {
    /// The text to return to the model.
    pub text: String,
    /// Truncation accounting, when applicable.
    pub details: Option<TruncationResult>,
}

/// Format a text-file read given its full `content`.
///
/// `offset` is a 1-indexed start line; `limit` caps the number of lines. Errors
/// mirror pi's out-of-bounds message. `path` is only used to build the
/// bash-fallback hint for over-long first lines.
pub fn format_text_read(
    content: &str,
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ReadTextOutput, String> {
    let all_lines: Vec<&str> = content.split('\n').collect();
    let total_file_lines = all_lines.len();

    // Convert 1-indexed offset to 0-indexed. offset==0 is treated as no offset,
    // matching pi's `offset ? Math.max(0, offset - 1) : 0`.
    let start_line = match offset {
        Some(o) if o > 0 => o - 1,
        _ => 0,
    };
    let start_line_display = start_line + 1;

    if start_line >= all_lines.len() {
        let shown = offset.unwrap_or(0);
        return Err(format!(
            "Offset {shown} is beyond end of file ({total_file_lines} lines total)"
        ));
    }

    let mut user_limited_lines: Option<usize> = None;
    let selected_content: String = if let Some(lim) = limit {
        let end_line = (start_line + lim).min(all_lines.len());
        user_limited_lines = Some(end_line - start_line);
        all_lines[start_line..end_line].join("\n")
    } else {
        all_lines[start_line..].join("\n")
    };

    let truncation = truncate_head(&selected_content, TruncationOptions::default());

    let mut details: Option<TruncationResult> = None;
    let text: String;

    if truncation.first_line_exceeds_limit {
        let first_line_size = format_size(all_lines[start_line].len());
        text = format!(
            "[Line {start_line_display} is {first_line_size}, exceeds {} limit. Use bash: sed -n '{start_line_display}p' {path} | head -c {DEFAULT_MAX_BYTES}]",
            format_size(DEFAULT_MAX_BYTES)
        );
        details = Some(truncation);
    } else if truncation.truncated {
        let end_line_display = start_line_display + truncation.output_lines - 1;
        let next_offset = end_line_display + 1;
        let mut out = truncation.content.clone();
        if truncation.truncated_by == Some(TruncatedBy::Lines) {
            out += &format!(
                "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines}. Use offset={next_offset} to continue.]"
            );
        } else {
            out += &format!(
                "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines} ({} limit). Use offset={next_offset} to continue.]",
                format_size(DEFAULT_MAX_BYTES)
            );
        }
        text = out;
        details = Some(truncation);
    } else if let Some(limited) = user_limited_lines {
        if start_line + limited < all_lines.len() {
            let remaining = all_lines.len() - (start_line + limited);
            let next_offset = start_line + limited + 1;
            text = format!(
                "{}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]",
                truncation.content
            );
        } else {
            text = truncation.content.clone();
        }
    } else {
        text = truncation.content.clone();
    }

    Ok(ReadTextOutput { text, details })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_content_that_fits() {
        let content = "Hello, world!\nLine 2\nLine 3";
        let out = format_text_read(content, "test.txt", None, None).unwrap();
        assert_eq!(out.text, content);
        assert!(!out.text.contains("Use offset="));
        assert!(out.details.is_none());
    }

    #[test]
    fn truncates_files_exceeding_line_limit() {
        let lines: Vec<String> = (1..=2500).map(|i| format!("Line {i}")).collect();
        let content = lines.join("\n");
        let out = format_text_read(&content, "large.txt", None, None).unwrap();
        assert!(out.text.contains("Line 1"));
        assert!(out.text.contains("Line 2000"));
        assert!(!out.text.contains("Line 2001"));
        assert!(out
            .text
            .contains("[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]"));
    }

    #[test]
    fn truncates_when_byte_limit_exceeded() {
        // 500 lines, each ~207 bytes -> exceeds 50KB before 2000 lines.
        let lines: Vec<String> = (1..=500)
            .map(|i| format!("Line {i}: {}", "x".repeat(200)))
            .collect();
        let content = lines.join("\n");
        let out = format_text_read(&content, "large-bytes.txt", None, None).unwrap();
        assert!(out.text.contains("Line 1:"));
        // Matches the byte-limit notice shape.
        let re = regex::Regex::new(
            r"\[Showing lines 1-\d+ of 500 \(.* limit\)\. Use offset=\d+ to continue\.\]",
        )
        .unwrap();
        assert!(re.is_match(&out.text), "notice not found in: {}", out.text);
    }

    #[test]
    fn handles_offset() {
        let lines: Vec<String> = (1..=100).map(|i| format!("Line {i}")).collect();
        let content = lines.join("\n");
        let out = format_text_read(&content, "offset.txt", Some(51), None).unwrap();
        assert!(!out.text.contains("Line 50"));
        assert!(out.text.contains("Line 51"));
        assert!(out.text.contains("Line 100"));
        assert!(!out.text.contains("Use offset="));
    }

    #[test]
    fn handles_limit() {
        let lines: Vec<String> = (1..=100).map(|i| format!("Line {i}")).collect();
        let content = lines.join("\n");
        let out = format_text_read(&content, "limit.txt", None, Some(10)).unwrap();
        assert!(out.text.contains("Line 1"));
        assert!(out.text.contains("Line 10"));
        assert!(!out.text.contains("Line 11"));
        assert!(out
            .text
            .contains("[90 more lines in file. Use offset=11 to continue.]"));
    }

    #[test]
    fn handles_offset_plus_limit() {
        let lines: Vec<String> = (1..=100).map(|i| format!("Line {i}")).collect();
        let content = lines.join("\n");
        let out = format_text_read(&content, "ol.txt", Some(41), Some(20)).unwrap();
        assert!(!out.text.contains("Line 40"));
        assert!(out.text.contains("Line 41"));
        assert!(out.text.contains("Line 60"));
        assert!(!out.text.contains("Line 61"));
        assert!(out
            .text
            .contains("[40 more lines in file. Use offset=61 to continue.]"));
    }

    #[test]
    fn errors_when_offset_beyond_end() {
        let content = "Line 1\nLine 2\nLine 3";
        let err = format_text_read(content, "short.txt", Some(100), None).unwrap_err();
        assert_eq!(err, "Offset 100 is beyond end of file (3 lines total)");
    }

    #[test]
    fn includes_truncation_details_when_truncated() {
        let lines: Vec<String> = (1..=2500).map(|i| format!("Line {i}")).collect();
        let content = lines.join("\n");
        let out = format_text_read(&content, "large.txt", None, None).unwrap();
        let details = out.details.expect("details present");
        assert!(details.truncated);
        assert_eq!(details.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(details.total_lines, 2500);
        assert_eq!(details.output_lines, 2000);
    }
}
