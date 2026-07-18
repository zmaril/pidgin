//! Bounded streaming output accounting for the bash tool.
//!
//! Ported from pi's `core/tools/output-accumulator.ts`. This tracks streaming
//! output with bounded memory: it decodes raw byte chunks with an incremental
//! UTF-8 decoder (so multi-byte sequences split across chunks decode
//! correctly), keeps only a decoded rolling tail for display snapshots, and
//! accounts total lines/bytes. [`OutputAccumulator::snapshot`] composes
//! [`truncate_tail`] over the rolling tail.
//!
//! The temp-file streaming sink (pi opens a `createWriteStream` when the full
//! output must be preserved) is deferred and injected behind the [`OutputSink`]
//! trait: this crate has no execution environment to own the file yet. Without
//! a sink, `full_output_path` stays `None`; the persistence *decision* is still
//! computed and exposed so the seam can be wired later.

use super::truncate::{
    truncate_tail, TruncatedBy, TruncationResult, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
};

/// Injected sink used to persist the full output when it must be retained.
/// Deferred: the default accumulator has no sink and reports no path.
pub trait OutputSink {
    /// Persist the accumulated raw chunks and return a path identifying them.
    fn persist(&mut self, chunks: &[Vec<u8>]) -> String;
}

/// Options controlling the accumulator's limits.
#[derive(Debug, Clone, Copy)]
pub struct OutputAccumulatorOptions {
    /// Line limit for snapshots.
    pub max_lines: usize,
    /// Byte limit for snapshots.
    pub max_bytes: usize,
}

impl Default for OutputAccumulatorOptions {
    fn default() -> Self {
        Self {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// A point-in-time view of accumulated output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSnapshot {
    /// The (possibly truncated) display content.
    pub content: String,
    /// Truncation accounting for the full output.
    pub truncation: TruncationResult,
    /// Path to the persisted full output, if a sink persisted it.
    pub full_output_path: Option<String>,
}

/// Incrementally tracks streaming output with bounded memory.
pub struct OutputAccumulator {
    max_lines: usize,
    max_bytes: usize,
    max_rolling_bytes: usize,

    pending: Vec<u8>,
    raw_chunks: Vec<Vec<u8>>,
    tail_text: String,
    tail_bytes: usize,
    tail_starts_at_line_boundary: bool,
    total_raw_bytes: usize,
    total_decoded_bytes: usize,
    completed_lines: usize,
    total_lines: usize,
    current_line_bytes: usize,
    has_open_line: bool,
    finished: bool,

    full_output_path: Option<String>,
    sink: Option<Box<dyn OutputSink>>,
}

impl OutputAccumulator {
    /// Create an accumulator with the given limits.
    pub fn new(options: OutputAccumulatorOptions) -> Self {
        Self {
            max_lines: options.max_lines,
            max_bytes: options.max_bytes,
            max_rolling_bytes: (options.max_bytes * 2).max(1),
            pending: Vec::new(),
            raw_chunks: Vec::new(),
            tail_text: String::new(),
            tail_bytes: 0,
            tail_starts_at_line_boundary: true,
            total_raw_bytes: 0,
            total_decoded_bytes: 0,
            completed_lines: 0,
            total_lines: 0,
            current_line_bytes: 0,
            has_open_line: false,
            finished: false,
            full_output_path: None,
            sink: None,
        }
    }

    /// Attach a persistence sink (the deferred temp-file seam).
    pub fn set_sink(&mut self, sink: Box<dyn OutputSink>) {
        self.sink = Some(sink);
    }

    /// Append a raw byte chunk.
    pub fn append(&mut self, data: &[u8]) {
        assert!(
            !self.finished,
            "cannot append to a finished output accumulator"
        );
        self.total_raw_bytes += data.len();
        let decoded = self.feed(data, false);
        self.append_decoded_text(&decoded);
        if !data.is_empty() {
            self.raw_chunks.push(data.to_vec());
        }
    }

    /// Flush the decoder and mark the stream finished.
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let decoded = self.feed(&[], true);
        self.append_decoded_text(&decoded);
    }

    /// Compute a display snapshot. When `persist_if_truncated` is set and the
    /// output is truncated, the injected sink (if any) persists the full output.
    pub fn snapshot(&mut self, persist_if_truncated: bool) -> OutputSnapshot {
        let tail_truncation = truncate_tail(
            &self.get_snapshot_text(),
            super::truncate::TruncationOptions {
                max_lines: self.max_lines,
                max_bytes: self.max_bytes,
            },
        );
        let truncated =
            self.total_lines > self.max_lines || self.total_decoded_bytes > self.max_bytes;
        let truncated_by = if truncated {
            tail_truncation
                .truncated_by
                .or(Some(if self.total_decoded_bytes > self.max_bytes {
                    TruncatedBy::Bytes
                } else {
                    TruncatedBy::Lines
                }))
        } else {
            None
        };
        let truncation = TruncationResult {
            truncated,
            truncated_by,
            total_lines: self.total_lines,
            total_bytes: self.total_decoded_bytes,
            max_lines: self.max_lines,
            max_bytes: self.max_bytes,
            ..tail_truncation
        };

        if persist_if_truncated && truncation.truncated {
            self.ensure_temp_file();
        }

        OutputSnapshot {
            content: truncation.content.clone(),
            truncation,
            full_output_path: self.full_output_path.clone(),
        }
    }

    /// Bytes accumulated on the current (incomplete) line.
    pub fn last_line_bytes(&self) -> usize {
        self.current_line_bytes
    }

    /// Whether the full output should be persisted (byte/line limit exceeded).
    pub fn should_use_temp_file(&self) -> bool {
        self.total_raw_bytes > self.max_bytes
            || self.total_decoded_bytes > self.max_bytes
            || self.total_lines > self.max_lines
    }

    fn ensure_temp_file(&mut self) {
        if self.full_output_path.is_some() {
            return;
        }
        if let Some(sink) = self.sink.as_mut() {
            let path = sink.persist(&self.raw_chunks);
            self.full_output_path = Some(path);
        }
    }

    fn append_decoded_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let bytes = text.len();
        self.total_decoded_bytes += bytes;
        self.tail_text.push_str(text);
        self.tail_bytes += bytes;
        if self.tail_bytes > self.max_rolling_bytes * 2 {
            self.trim_tail();
        }

        let mut newlines = 0usize;
        let mut last_newline: isize = -1;
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                newlines += 1;
                last_newline = i as isize;
            }
        }
        if newlines == 0 {
            self.current_line_bytes += bytes;
            self.has_open_line = true;
        } else {
            self.completed_lines += newlines;
            let tail = &text[(last_newline as usize) + 1..];
            self.current_line_bytes = tail.len();
            self.has_open_line = !tail.is_empty();
        }
        self.total_lines = self.completed_lines + usize::from(self.has_open_line);
    }

    fn trim_tail(&mut self) {
        let buffer = self.tail_text.as_bytes().to_vec();
        if buffer.len() <= self.max_rolling_bytes {
            self.tail_bytes = buffer.len();
            return;
        }
        let mut start = buffer.len() - self.max_rolling_bytes;
        while start < buffer.len() && (buffer[start] & 0xc0) == 0x80 {
            start += 1;
        }
        self.tail_starts_at_line_boundary = if start == 0 {
            self.tail_starts_at_line_boundary
        } else {
            buffer[start - 1] == 0x0a
        };
        self.tail_text = String::from_utf8_lossy(&buffer[start..]).into_owned();
        self.tail_bytes = self.tail_text.len();
    }

    fn get_snapshot_text(&self) -> String {
        if self.tail_starts_at_line_boundary {
            return self.tail_text.clone();
        }
        match self.tail_text.find('\n') {
            None => self.tail_text.clone(),
            Some(idx) => self.tail_text[idx + 1..].to_string(),
        }
    }

    /// Incremental UTF-8 decoder mirroring `TextDecoder`'s streaming behavior:
    /// incomplete trailing sequences are held in `pending`, invalid sequences
    /// become U+FFFD.
    fn feed(&mut self, data: &[u8], flush: bool) -> String {
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(data);
        let mut out = String::new();
        let mut idx = 0;
        loop {
            if idx >= buf.len() {
                break;
            }
            match std::str::from_utf8(&buf[idx..]) {
                Ok(s) => {
                    out.push_str(s);
                    idx = buf.len();
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        // Safe: `valid_up_to` is a valid UTF-8 boundary.
                        out.push_str(std::str::from_utf8(&buf[idx..idx + valid]).unwrap());
                        idx += valid;
                    }
                    match e.error_len() {
                        Some(n) => {
                            out.push('\u{FFFD}');
                            idx += n;
                        }
                        None => {
                            if flush {
                                out.push('\u{FFFD}');
                                idx = buf.len();
                            }
                            break;
                        }
                    }
                }
            }
        }
        if idx < buf.len() {
            self.pending = buf[idx..].to_vec();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordingSink {
        path: String,
        persisted_bytes: usize,
    }

    impl OutputSink for RecordingSink {
        fn persist(&mut self, chunks: &[Vec<u8>]) -> String {
            self.persisted_bytes = chunks.iter().map(|c| c.len()).sum();
            self.path.clone()
        }
    }

    #[test]
    fn decodes_utf8_split_across_chunks() {
        let mut acc = OutputAccumulator::new(OutputAccumulatorOptions::default());
        // "héllo": é is 0xC3 0xA9 - split it across two appends.
        acc.append(b"h\xc3");
        acc.append(b"\xa9llo");
        acc.finish();
        let snap = acc.snapshot(false);
        assert_eq!(snap.content, "héllo");
        assert_eq!(snap.truncation.total_lines, 1);
    }

    #[test]
    fn trailing_newline_not_counted_as_extra_line() {
        let mut acc = OutputAccumulator::new(OutputAccumulatorOptions::default());
        acc.append(b"a\nb\n");
        acc.finish();
        let snap = acc.snapshot(false);
        assert_eq!(snap.truncation.total_lines, 2);
        assert!(!snap.truncation.truncated);
    }

    #[test]
    fn open_line_counts_toward_total() {
        let mut acc = OutputAccumulator::new(OutputAccumulatorOptions::default());
        acc.append(b"a\nb\nc");
        acc.finish();
        let snap = acc.snapshot(false);
        assert_eq!(snap.truncation.total_lines, 3);
        assert_eq!(acc.last_line_bytes(), 1);
    }

    #[test]
    fn line_only_truncation_flags_full_output_for_persistence() {
        let mut acc = OutputAccumulator::new(OutputAccumulatorOptions {
            max_lines: 2,
            max_bytes: DEFAULT_MAX_BYTES,
        });
        acc.set_sink(Box::new(RecordingSink {
            path: "/tmp/full-output.log".to_string(),
            persisted_bytes: 0,
        }));
        acc.append(b"l1\nl2\nl3\n");
        acc.finish();
        let snap = acc.snapshot(true);
        assert!(snap.truncation.truncated);
        assert_eq!(snap.truncation.truncated_by, Some(TruncatedBy::Lines));
        // Rolling tail keeps the last two lines.
        assert_eq!(snap.content, "l2\nl3");
        // Persistence fired even though only the line limit was hit.
        assert_eq!(
            snap.full_output_path.as_deref(),
            Some("/tmp/full-output.log")
        );
    }

    #[test]
    fn no_sink_means_no_persisted_path() {
        let mut acc = OutputAccumulator::new(OutputAccumulatorOptions {
            max_lines: 1,
            max_bytes: DEFAULT_MAX_BYTES,
        });
        acc.append(b"l1\nl2\n");
        acc.finish();
        let snap = acc.snapshot(true);
        assert!(snap.truncation.truncated);
        assert_eq!(snap.full_output_path, None);
        assert!(acc.should_use_temp_file());
    }

    #[test]
    fn invalid_bytes_become_replacement_char() {
        let mut acc = OutputAccumulator::new(OutputAccumulatorOptions::default());
        acc.append(b"a\xff b");
        acc.finish();
        let snap = acc.snapshot(false);
        assert!(snap.content.contains('\u{FFFD}'));
        assert!(snap.content.starts_with('a'));
    }
}
