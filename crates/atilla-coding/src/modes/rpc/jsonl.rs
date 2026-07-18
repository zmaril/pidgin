//! LF-only JSONL framing, ported from pi's `modes/rpc/jsonl.ts`.
//!
//! Framing is LF-only: one JSON object per line, terminated by a single `\n`.
//! Records are split on `\n` **only** — a trailing `\r` (CRLF input) is stripped
//! before parse, and other Unicode separators (U+2028/U+2029) that are valid
//! inside JSON strings are never treated as record boundaries. This mirrors pi's
//! deliberate avoidance of Node's `readline`, which would split on those.

use std::io::{self, BufRead};

/// Serialize a single strict JSONL record: `JSON.stringify(value) + "\n"`.
///
/// Mirrors `serializeJsonLine`. `serde_json` emits ` `/` ` as escaped
/// sequences (never raw), so the framing is safe. Never pretty-prints.
pub fn serialize_json_line<T: serde::Serialize>(value: &T) -> String {
    let mut s = serde_json::to_string(value).expect("rpc value must serialize");
    s.push('\n');
    s
}

/// Read LF-framed JSONL records from `r`, invoking `on_line` for each.
///
/// Splits on `\n` only and strips a single trailing `\r`. A final unterminated
/// line (no newline before EOF) is emitted as its own record, matching pi's
/// `onEnd` residual-buffer flush.
pub fn read_json_lines<R: BufRead>(mut r: R, mut on_line: impl FnMut(&str)) -> io::Result<()> {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = r.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break; // EOF
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        let line = String::from_utf8_lossy(&buf);
        on_line(&line);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_appends_single_lf() {
        let line = serialize_json_line(&serde_json::json!({"a": 1}));
        assert_eq!(line, "{\"a\":1}\n");
    }

    #[test]
    fn serialize_preserves_line_separators_and_frames_on_lf_only() {
        // Like pi, U+2028 is emitted verbatim (not escaped); framing safety
        // comes from splitting records on `\n` only. The single record must
        // therefore contain exactly one `\n` (the terminator) and the raw
        // separator inside the string.
        let line = serialize_json_line(&serde_json::json!({"t": "a\u{2028}b"}));
        assert!(line.contains('\u{2028}'));
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
    }

    #[test]
    fn reads_lf_only_and_strips_cr() {
        let input = b"{\"a\":1}\r\n{\"b\":2}\n" as &[u8];
        let mut lines = Vec::new();
        read_json_lines(input, |l| lines.push(l.to_string())).unwrap();
        assert_eq!(lines, vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]);
    }

    #[test]
    fn emits_final_unterminated_line() {
        let input = b"{\"a\":1}\n{\"b\":2}" as &[u8];
        let mut lines = Vec::new();
        read_json_lines(input, |l| lines.push(l.to_string())).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], "{\"b\":2}");
    }

    #[test]
    fn line_separator_inside_string_is_not_a_boundary() {
        // A raw U+2028 embedded in a JSON string is a single record.
        let input = "{\"t\":\"a\u{2028}b\"}\n".as_bytes();
        let mut lines = Vec::new();
        read_json_lines(input, |l| lines.push(l.to_string())).unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains('\u{2028}'));
    }
}
