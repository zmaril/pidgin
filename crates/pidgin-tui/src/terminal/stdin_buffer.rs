//! Faithful port of pi's `StdinBuffer`
//! (`vendor/pi/packages/tui/src/stdin-buffer.ts`, pi v0.80.10, submodule pin
//! `3da591a`).
//!
//! `StdinBuffer` accumulates raw terminal input and splits it into complete
//! escape sequences, exactly as pi does before feeding the key parser. Terminal
//! `data` events can arrive in partial chunks (a single SGR mouse sequence like
//! `\x1b[<35;20;5m` may span three reads); without buffering, partial sequences
//! would be misread as individual keypresses. It also recognises bracketed
//! paste and emits the pasted body as a single [`StdinEvent::Paste`].
//!
//! This module is pure (no I/O), so the whole splitter runs and is unit-tested
//! on Linux CI. Only the 10 ms completion timer in pi is not modelled as a
//! timer here: [`StdinBuffer::process`] returns the sequences it can complete
//! immediately and retains the remainder, and [`StdinBuffer::flush`] performs
//! the timeout flush the caller schedules.
//!
//! Char/UTF-16 note: pi operates on JS strings (UTF-16 code units); this port
//! operates on `&str` at Unicode-scalar (char) granularity. Every escape
//! terminator pi inspects is ASCII (one UTF-16 unit == one `char`), so the
//! sequence-splitting is identical. The only reachable difference is the
//! single-`char` dedup in [`StdinBuffer::emit_data_sequence`] for astral
//! codepoints (2 UTF-16 units in JS, 1 `char` here) — those are never Kitty
//! printable-codepoint duplicates, so the observable behaviour matches.

use super::negotiation::{BRACKETED_PASTE_END, BRACKETED_PASTE_START};

const ESC: &str = "\x1b";

/// An event emitted by [`StdinBuffer`], mirroring pi's `"data"` / `"paste"`
/// EventEmitter channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinEvent {
    /// A single complete input sequence (one keypress / escape sequence).
    Data(String),
    /// The body of a bracketed paste (markers stripped), as pi's `"paste"`.
    Paste(String),
}

/// Completion status of a candidate escape sequence, mirroring pi's
/// `isCompleteSequence` result union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Completion {
    Complete,
    Incomplete,
    NotEscape,
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// True when every char in `s` is an ASCII digit (`\d`).
fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `^<\d+;\d+;\d+[Mm]$` over the CSI payload (leading `<` already present).
fn is_sgr_mouse_payload(payload: &str) -> bool {
    // payload begins with '<' and ends with 'M' or 'm'.
    let inner = &payload[1..payload.len() - 1];
    let parts: Vec<&str> = inner.split(';').collect();
    parts.len() == 3 && parts.iter().all(|p| all_digits(p))
}

/// Port of pi's `isCompleteSequence`.
fn is_complete_sequence(data: &str) -> Completion {
    if !data.starts_with(ESC) {
        return Completion::NotEscape;
    }
    if char_len(data) == 1 {
        return Completion::Incomplete;
    }

    let after_esc = &data[ESC.len()..];

    if after_esc.starts_with('[') {
        if after_esc.starts_with("[M") {
            // Old-style mouse: ESC[M + 3 bytes = 6 total.
            return if char_len(data) >= 6 {
                Completion::Complete
            } else {
                Completion::Incomplete
            };
        }
        return is_complete_csi_sequence(data);
    }
    if after_esc.starts_with(']') {
        return is_complete_osc_sequence(data);
    }
    if after_esc.starts_with('P') {
        return is_complete_dcs_or_apc_sequence(data);
    }
    if after_esc.starts_with('_') {
        return is_complete_dcs_or_apc_sequence(data);
    }
    if after_esc.starts_with('O') {
        // SS3: ESC O followed by a single character.
        return if char_len(after_esc) >= 2 {
            Completion::Complete
        } else {
            Completion::Incomplete
        };
    }
    if char_len(after_esc) == 1 {
        // Meta key: ESC followed by a single character.
        return Completion::Complete;
    }
    // Unknown escape sequence — treat as complete.
    Completion::Complete
}

/// Port of pi's `isCompleteCsiSequence`.
fn is_complete_csi_sequence(data: &str) -> Completion {
    if !data.starts_with("\x1b[") {
        return Completion::Complete;
    }
    if char_len(data) < 3 {
        return Completion::Incomplete;
    }
    let payload = &data[2..];
    let last_char = payload.chars().last().unwrap();
    let last_code = last_char as u32;

    if (0x40..=0x7e).contains(&last_code) {
        if payload.starts_with('<') {
            if is_sgr_mouse_payload(payload) {
                return Completion::Complete;
            }
            // Ends with M/m but structurally incomplete.
            return Completion::Incomplete;
        }
        return Completion::Complete;
    }
    Completion::Incomplete
}

/// Port of pi's `isCompleteOscSequence` (OSC terminates on ST or BEL).
fn is_complete_osc_sequence(data: &str) -> Completion {
    if !data.starts_with("\x1b]") {
        return Completion::Complete;
    }
    if data.ends_with("\x1b\\") || data.ends_with('\x07') {
        return Completion::Complete;
    }
    Completion::Incomplete
}

/// Port of pi's `isCompleteDcsSequence` / `isCompleteApcSequence`; both DCS
/// (`ESC P`) and APC (`ESC _`) terminate on ST (`ESC \`).
fn is_complete_dcs_or_apc_sequence(data: &str) -> Completion {
    if data.ends_with("\x1b\\") {
        return Completion::Complete;
    }
    Completion::Incomplete
}

/// Port of pi's `parseUnmodifiedKittyPrintableCodepoint`:
/// `^\x1b\[(\d+)(?::\d*)?(?::\d+)?u$` → codepoint if `>= 32`.
fn parse_unmodified_kitty_printable_codepoint(sequence: &str) -> Option<u32> {
    let inner = sequence
        .strip_prefix("\x1b[")
        .and_then(|s| s.strip_suffix('u'))?;

    let mut rest = inner;
    // (\d+)
    let digits_end = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits_end == 0 {
        return None;
    }
    let num: u32 = rest[..digits_end].parse().ok()?;
    rest = &rest[digits_end..];

    // (?::\d*)?
    if let Some(after) = rest.strip_prefix(':') {
        let n = after.bytes().take_while(|b| b.is_ascii_digit()).count();
        rest = &after[n..];
    }
    // (?::\d+)?
    if let Some(after) = rest.strip_prefix(':') {
        let n = after.bytes().take_while(|b| b.is_ascii_digit()).count();
        if n == 0 {
            return None;
        }
        rest = &after[n..];
    }
    if !rest.is_empty() {
        return None;
    }
    if num >= 32 {
        Some(num)
    } else {
        None
    }
}

/// Port of pi's `extractCompleteSequences`: returns the completed sequences and
/// any incomplete remainder still buffered.
fn extract_complete_sequences(buffer: &str) -> (Vec<String>, String) {
    let chars: Vec<char> = buffer.chars().collect();
    let mut sequences: Vec<String> = Vec::new();
    let mut pos = 0usize;

    while pos < chars.len() {
        if chars[pos] == '\x1b' {
            let mut advanced = false;
            let mut seq_end = 1usize;
            while seq_end <= chars.len() - pos {
                let candidate: String = chars[pos..pos + seq_end].iter().collect();
                match is_complete_sequence(&candidate) {
                    Completion::Complete => {
                        // WezTerm emits a bare ESC (press) immediately followed by
                        // a full Kitty CSI-u release, arriving as `\x1b\x1b[...u`.
                        // Treat the leading `\x1b\x1b` as ESC + a new sequence
                        // start rather than a meta-key.
                        if candidate == "\x1b\x1b" {
                            let next = chars.get(pos + seq_end).copied();
                            if matches!(
                                next,
                                Some('[') | Some(']') | Some('O') | Some('P') | Some('_')
                            ) {
                                sequences.push(ESC.to_string());
                                pos += 1;
                                advanced = true;
                                break;
                            }
                        }
                        sequences.push(candidate);
                        pos += seq_end;
                        advanced = true;
                        break;
                    }
                    Completion::Incomplete => {
                        seq_end += 1;
                    }
                    Completion::NotEscape => {
                        // Should not happen when starting with ESC.
                        sequences.push(candidate);
                        pos += seq_end;
                        advanced = true;
                        break;
                    }
                }
            }
            if !advanced {
                let remainder: String = chars[pos..].iter().collect();
                return (sequences, remainder);
            }
        } else {
            sequences.push(chars[pos].to_string());
            pos += 1;
        }
    }

    (sequences, String::new())
}

/// Options for [`StdinBuffer`], mirroring pi's `StdinBufferOptions`.
#[derive(Debug, Clone, Copy)]
pub struct StdinBufferOptions {
    /// Maximum time (ms) to wait for sequence completion before a timeout
    /// flush. Retained for parity; the timer is driven by the caller.
    pub timeout_ms: u64,
}

impl Default for StdinBufferOptions {
    fn default() -> Self {
        Self { timeout_ms: 10 }
    }
}

/// Faithful port of pi's `StdinBuffer`. Feed raw input via [`StdinBuffer::process`]
/// and flush a stalled remainder via [`StdinBuffer::flush`].
#[derive(Debug)]
pub struct StdinBuffer {
    buffer: String,
    #[allow(dead_code)]
    timeout_ms: u64,
    paste_mode: bool,
    paste_buffer: String,
    pending_kitty_printable_codepoint: Option<u32>,
}

impl Default for StdinBuffer {
    fn default() -> Self {
        Self::new(StdinBufferOptions::default())
    }
}

impl StdinBuffer {
    /// Construct a buffer with the given options.
    pub fn new(options: StdinBufferOptions) -> Self {
        Self {
            buffer: String::new(),
            timeout_ms: options.timeout_ms,
            paste_mode: false,
            paste_buffer: String::new(),
            pending_kitty_printable_codepoint: None,
        }
    }

    /// True when a partial (incomplete) sequence is still buffered — the caller
    /// should arm a [`StdinBuffer::flush`] timer, as pi does.
    pub fn has_pending(&self) -> bool {
        !self.buffer.is_empty()
    }

    /// Port of pi's `StdinBuffer.process`. Accumulates `data`, emits every
    /// complete sequence (and any paste), and retains an incomplete remainder.
    pub fn process(&mut self, data: &str) -> Vec<StdinEvent> {
        let mut out = Vec::new();
        self.process_into(data, &mut out);
        out
    }

    fn process_into(&mut self, data: &str, out: &mut Vec<StdinEvent>) {
        if data.is_empty() && self.buffer.is_empty() {
            self.emit_data_sequence("", out);
            return;
        }

        self.buffer.push_str(data);

        if self.paste_mode {
            let combined = std::mem::take(&mut self.buffer);
            self.paste_buffer.push_str(&combined);
            self.try_finish_paste(out);
            return;
        }

        if let Some(start_index) = self.buffer.find(BRACKETED_PASTE_START) {
            if start_index > 0 {
                let before_paste = self.buffer[..start_index].to_string();
                let (sequences, _remainder) = extract_complete_sequences(&before_paste);
                for sequence in sequences {
                    self.emit_data_sequence(&sequence, out);
                }
            }
            self.pending_kitty_printable_codepoint = None;
            let rest = self.buffer[start_index + BRACKETED_PASTE_START.len()..].to_string();
            self.buffer.clear();
            self.paste_mode = true;
            self.paste_buffer = rest;
            self.try_finish_paste(out);
            return;
        }

        let (sequences, remainder) = extract_complete_sequences(&self.buffer);
        self.buffer = remainder;
        for sequence in sequences {
            self.emit_data_sequence(&sequence, out);
        }
        // pi arms a timeout here when a remainder is pending; the caller drives
        // that timer and calls `flush()`.
    }

    fn try_finish_paste(&mut self, out: &mut Vec<StdinEvent>) {
        if let Some(end_index) = self.paste_buffer.find(BRACKETED_PASTE_END) {
            let pasted = self.paste_buffer[..end_index].to_string();
            let remaining = self.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();
            self.paste_mode = false;
            self.paste_buffer.clear();
            self.pending_kitty_printable_codepoint = None;
            out.push(StdinEvent::Paste(pasted));
            if !remaining.is_empty() {
                self.process_into(&remaining, out);
            }
        }
    }

    fn emit_data_sequence(&mut self, sequence: &str, out: &mut Vec<StdinEvent>) {
        let raw_codepoint = if char_len(sequence) == 1 {
            sequence.chars().next().map(|c| c as u32)
        } else {
            None
        };
        if let (Some(raw), Some(pending)) = (raw_codepoint, self.pending_kitty_printable_codepoint)
        {
            if raw == pending {
                self.pending_kitty_printable_codepoint = None;
                return;
            }
        }
        self.pending_kitty_printable_codepoint =
            parse_unmodified_kitty_printable_codepoint(sequence);
        out.push(StdinEvent::Data(sequence.to_string()));
    }

    /// Port of pi's `StdinBuffer.flush`: the timeout path. Emits any buffered
    /// remainder verbatim as a single `data` event.
    pub fn flush(&mut self) -> Vec<StdinEvent> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let sequence = std::mem::take(&mut self.buffer);
        self.pending_kitty_printable_codepoint = None;
        vec![StdinEvent::Data(sequence)]
    }

    /// Port of pi's `StdinBuffer.clear`: drop all buffered state.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.paste_mode = false;
        self.paste_buffer.clear();
        self.pending_kitty_printable_codepoint = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_events(events: Vec<StdinEvent>) -> Vec<String> {
        events
            .into_iter()
            .filter_map(|e| match e {
                StdinEvent::Data(s) => Some(s),
                StdinEvent::Paste(_) => None,
            })
            .collect()
    }

    #[test]
    fn splits_batched_simple_keys() {
        let mut b = StdinBuffer::default();
        let ev = b.process("abc");
        assert_eq!(data_events(ev), vec!["a", "b", "c"]);
    }

    #[test]
    fn reassembles_split_sgr_mouse_sequence() {
        let mut b = StdinBuffer::default();
        assert!(data_events(b.process("\x1b")).is_empty());
        assert!(data_events(b.process("[<35")).is_empty());
        let ev = b.process(";20;5m");
        assert_eq!(data_events(ev), vec!["\x1b[<35;20;5m"]);
    }

    #[test]
    fn emits_single_csi_arrow() {
        let mut b = StdinBuffer::default();
        let ev = b.process("\x1b[A");
        assert_eq!(data_events(ev), vec!["\x1b[A"]);
    }

    #[test]
    fn extracts_bracketed_paste_body() {
        let mut b = StdinBuffer::default();
        let ev = b.process("\x1b[200~hello world\x1b[201~");
        assert_eq!(ev, vec![StdinEvent::Paste("hello world".to_string())]);
    }

    #[test]
    fn paste_split_across_chunks() {
        let mut b = StdinBuffer::default();
        assert!(b.process("\x1b[200~foo").is_empty());
        let ev = b.process(" bar\x1b[201~");
        assert_eq!(ev, vec![StdinEvent::Paste("foo bar".to_string())]);
    }

    #[test]
    fn key_before_paste_is_emitted_first() {
        let mut b = StdinBuffer::default();
        let ev = b.process("a\x1b[200~body\x1b[201~");
        assert_eq!(
            ev,
            vec![
                StdinEvent::Data("a".to_string()),
                StdinEvent::Paste("body".to_string()),
            ]
        );
    }

    #[test]
    fn wezterm_double_escape_splits_esc_then_release() {
        // `\x1b\x1b[27;...u` -> bare ESC, then the Kitty CSI-u release.
        let mut b = StdinBuffer::default();
        let ev = data_events(b.process("\x1b\x1b[27u"));
        assert_eq!(ev, vec!["\x1b", "\x1b[27u"]);
    }

    #[test]
    fn dedups_kitty_printable_then_raw_duplicate() {
        // A Kitty printable `\x1b[97u` (codepoint 97 = 'a') sets pending; the
        // immediately following raw 'a' is swallowed as the duplicate.
        let mut b = StdinBuffer::default();
        let ev = data_events(b.process("\x1b[97u"));
        assert_eq!(ev, vec!["\x1b[97u"]);
        let ev2 = data_events(b.process("a"));
        assert!(ev2.is_empty(), "raw duplicate should be swallowed");
    }

    #[test]
    fn flush_emits_incomplete_remainder() {
        let mut b = StdinBuffer::default();
        assert!(b.process("\x1b[").is_empty());
        assert!(b.has_pending());
        let flushed = b.flush();
        assert_eq!(flushed, vec![StdinEvent::Data("\x1b[".to_string())]);
    }

    #[test]
    fn parse_kitty_printable_variants() {
        assert_eq!(
            parse_unmodified_kitty_printable_codepoint("\x1b[97u"),
            Some(97)
        );
        assert_eq!(
            parse_unmodified_kitty_printable_codepoint("\x1b[97:0u"),
            Some(97)
        );
        assert_eq!(
            parse_unmodified_kitty_printable_codepoint("\x1b[97::5u"),
            Some(97)
        );
        // Control codepoint < 32 is rejected.
        assert_eq!(parse_unmodified_kitty_printable_codepoint("\x1b[13u"), None);
        // Not a CSI-u sequence.
        assert_eq!(parse_unmodified_kitty_printable_codepoint("\x1b[A"), None);
    }
}
