// straitjacket-allow-file:duplication
//! Shared, dialect-agnostic incremental SSE streaming primitives.
//!
//! This is the transport-agnostic core of pi-ai's `iterateSseMessages` reader
//! loop (`packages/ai/src/api/anthropic-messages.ts` at pinned commit
//! `3da591ab`), lifted out of the Anthropic driver so every dialect can share
//! ONE copy of the SSE framing logic:
//!
//! - [`SseFrameSplitter`] is a faithful port of pi's `iterateSseMessages`
//!   reader loop: `decoder.decode(value, { stream: true })` per arriving chunk,
//!   the `consumeLine` line-splitting loop, and the trailing partial-line +
//!   dangling-event flush at stream end. It is fed bytes incrementally and emits
//!   [`ServerSentEvent`] frames as they complete; feeding the whole body at once
//!   yields the byte-identical frame sequence the buffered path produced before.
//! - [`SseEventDecoder`] is the per-dialect seam: it turns each raw SSE frame
//!   into zero or more [`AssistantMessageEvent`]s and, at stream end, emits the
//!   terminal event(s) and returns the accumulated [`AssistantMessage`].
//! - [`AssistantEventReader`] is the PULL iterator that ties them together with
//!   no async runtime, thread, or channel: each `next()` drains ready events,
//!   else pulls ONE chunk (a blocking `read()` is the real inter-chunk timing),
//!   feeds the splitter, and runs the decoder per new frame. On EOF it flushes
//!   the splitter and the decoder to produce the terminal event(s).

use std::collections::VecDeque;
use std::io;
use std::ops::ControlFlow;

use crate::types::{AssistantMessage, AssistantMessageEvent, StopReason};

/// A decoded server-sent event (pi's `ServerSentEvent`).
///
/// Fields are public so dialect decoders can read the `event:` name, the joined
/// `data:` payload, and the `raw` lines (for error diagnostics) directly.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerSentEvent {
    /// The `event:` field value, if the frame carried one.
    pub event: Option<String>,
    /// The `data:` field value(s), joined with `\n` (pi's `data.join("\n")`).
    pub data: String,
    /// Every non-empty raw line that composed this frame, in order.
    pub raw: Vec<String>,
}

/// In-progress accumulation of a single SSE frame (pi's `SseDecoderState`).
#[derive(Debug, Default)]
struct SseDecoderState {
    event: Option<String>,
    data: Vec<String>,
    raw: Vec<String>,
}

/// Emit the accumulated frame, if any (pi's `flushSseEvent`).
fn flush_sse_event(state: &mut SseDecoderState) -> Option<ServerSentEvent> {
    if state.event.is_none() && state.data.is_empty() {
        return None;
    }
    let event = ServerSentEvent {
        event: state.event.take(),
        data: state.data.join("\n"),
        raw: std::mem::take(&mut state.raw),
    };
    state.data.clear();
    state.raw.clear();
    Some(event)
}

/// Consume one SSE line into `state`, flushing a frame on a blank line (pi's
/// `decodeSseLine`).
fn decode_sse_line(line: &str, state: &mut SseDecoderState) -> Option<ServerSentEvent> {
    if line.is_empty() {
        return flush_sse_event(state);
    }

    state.raw.push(line.to_string());
    if line.starts_with(':') {
        return None;
    }

    let (field_name, mut value) = match line.find(':') {
        None => (line, String::new()),
        Some(idx) => (&line[..idx], line[idx + 1..].to_string()),
    };
    if let Some(stripped) = value.strip_prefix(' ') {
        value = stripped.to_string();
    }

    if field_name == "event" {
        state.event = Some(value);
    } else if field_name == "data" {
        state.data.push(value);
    }

    None
}

fn next_line_break_index(text: &str) -> Option<usize> {
    let cr = text.find('\r');
    let lf = text.find('\n');
    match (cr, lf) {
        (None, lf) => lf,
        (cr, None) => cr,
        (Some(cr), Some(lf)) => Some(cr.min(lf)),
    }
}

/// One line consumed from `text`, plus the remaining tail (pi's `consumeLine`).
/// Byte indices are safe: the delimiters are ASCII `\r`/`\n`.
fn consume_line(text: &str) -> Option<(String, String)> {
    let line_break = next_line_break_index(text)?;
    let bytes = text.as_bytes();
    let mut next = line_break + 1;
    if bytes[line_break] == b'\r' && bytes.get(next) == Some(&b'\n') {
        next += 1;
    }
    Some((text[..line_break].to_string(), text[next..].to_string()))
}

/// Incremental UTF-8 decoder mirroring the browser `TextDecoder` pi feeds its
/// byte chunks through: `decode(bytes, { stream: true })` buffers an incomplete
/// trailing multi-byte sequence across chunks; the final `decode()` flushes any
/// dangling bytes as a replacement character.
#[derive(Debug, Default)]
struct StreamingUtf8Decoder {
    pending: Vec<u8>,
}

impl StreamingUtf8Decoder {
    /// `TextDecoder.decode(bytes, { stream: true })`.
    fn decode(&mut self, bytes: &[u8], out: &mut String) {
        self.pending.extend_from_slice(bytes);
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(valid) => {
                    out.push_str(valid);
                    self.pending.clear();
                    return;
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    if valid_up_to > 0 {
                        // The prefix is verified valid UTF-8 by `valid_up_to`.
                        out.push_str(
                            std::str::from_utf8(&self.pending[..valid_up_to])
                                .expect("valid_up_to prefix is valid UTF-8"),
                        );
                    }
                    match err.error_len() {
                        // Incomplete sequence at the end: buffer it for the next feed.
                        None => {
                            self.pending.drain(..valid_up_to);
                            return;
                        }
                        // Invalid mid-stream sequence: emit a replacement and continue.
                        Some(bad) => {
                            out.push('\u{FFFD}');
                            self.pending.drain(..valid_up_to + bad);
                        }
                    }
                }
            }
        }
    }

    /// `TextDecoder.decode()` (final flush).
    fn finish(&mut self, out: &mut String) {
        if !self.pending.is_empty() {
            out.push('\u{FFFD}');
            self.pending.clear();
        }
    }
}

/// Incremental SSE framer: feed newly-arrived bytes, receive complete frames.
///
/// A faithful port of pi's `iterateSseMessages` reader loop. Feeding the entire
/// body in one `feed` + `finish` reproduces the buffered frame sequence exactly;
/// feeding byte-by-byte yields the same frames as arriving-chunk timing dictates.
#[derive(Debug, Default)]
pub struct SseFrameSplitter {
    decoder: StreamingUtf8Decoder,
    buffer: String,
    state: SseDecoderState,
}

impl SseFrameSplitter {
    /// A fresh splitter with empty buffers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed newly-arrived bytes; push every now-complete frame into `out`.
    ///
    /// Mirrors pi's per-chunk body: `buffer += decoder.decode(value, { stream:
    /// true })` then the `consumeLine` loop.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<ServerSentEvent>) {
        self.decoder.decode(bytes, &mut self.buffer);
        while let Some((line, rest)) = consume_line(&self.buffer) {
            self.buffer = rest;
            if let Some(event) = decode_sse_line(&line, &mut self.state) {
                out.push(event);
            }
        }
    }

    /// Byte stream ended: flush the UTF-8 decoder, decode any trailing partial
    /// line, and flush a dangling event.
    ///
    /// Mirrors pi's post-loop tail: `buffer += decoder.decode()`, the final
    /// `consumeLine` drain, the trailing-line `decodeSseLine`, and the
    /// `flushSseEvent`.
    pub fn finish(&mut self, out: &mut Vec<ServerSentEvent>) {
        self.decoder.finish(&mut self.buffer);
        while let Some((line, rest)) = consume_line(&self.buffer) {
            self.buffer = rest;
            if let Some(event) = decode_sse_line(&line, &mut self.state) {
                out.push(event);
            }
        }
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            if let Some(event) = decode_sse_line(&line, &mut self.state) {
                out.push(event);
            }
        }
        if let Some(event) = flush_sse_event(&mut self.state) {
            out.push(event);
        }
    }
}

/// Per-dialect SSE-frame to assistant-event decoder.
///
/// The shared [`AssistantEventReader`] owns all chunk-pull, line-buffer, and
/// terminal plumbing; a dialect retrofits by supplying only this decoder.
pub trait SseEventDecoder {
    /// Decode ONE raw SSE frame into zero or more assistant events, updating the
    /// decoder's internal accumulation. Return [`ControlFlow::Break`] to signal a
    /// hard terminal error (the reader stops pulling and finalizes); a decoder
    /// that breaks MUST make its subsequent [`finish`](Self::finish) emit an
    /// error terminal.
    fn on_frame(
        &mut self,
        frame: &ServerSentEvent,
        out: &mut Vec<AssistantMessageEvent>,
    ) -> ControlFlow<String>;

    /// The byte stream ended: emit the terminal `done`/`error` event(s) and
    /// return the final accumulated message.
    fn finish(&mut self, out: &mut Vec<AssistantMessageEvent>) -> AssistantMessage;
}

/// The PULL iterator over a streaming assistant response.
///
/// Each `next()` drains a ready spill-buffer, else pulls ONE chunk from the
/// underlying byte iterator (the blocking `read()` there IS the real inter-chunk
/// timing), feeds the [`SseFrameSplitter`], and runs the [`SseEventDecoder`] per
/// new frame. On chunk EOF it flushes the splitter and the decoder to produce the
/// terminal event(s). It never panics and is bounded by the chunk iterator's EOF.
pub struct AssistantEventReader<'a> {
    chunks: Box<dyn Iterator<Item = io::Result<Vec<u8>>> + 'a>,
    splitter: SseFrameSplitter,
    decoder: Box<dyn SseEventDecoder + 'a>,
    ready: VecDeque<AssistantMessageEvent>,
    finished: bool,
    result: Option<Result<AssistantMessage, AssistantMessage>>,
}

impl<'a> AssistantEventReader<'a> {
    /// Build a reader over `chunks`, decoded by `decoder`.
    pub fn new(
        chunks: Box<dyn Iterator<Item = io::Result<Vec<u8>>> + 'a>,
        decoder: Box<dyn SseEventDecoder + 'a>,
    ) -> Self {
        Self {
            chunks,
            splitter: SseFrameSplitter::new(),
            decoder,
            ready: VecDeque::new(),
            finished: false,
            result: None,
        }
    }

    /// The terminal outcome, available once the stream has finished: `Ok` for a
    /// `done` terminal, `Err` for an `error` terminal.
    pub fn result(&self) -> Option<&Result<AssistantMessage, AssistantMessage>> {
        self.result.as_ref()
    }

    /// Run `decoder.finish`, push the terminal event(s), and capture `result`.
    ///
    /// `hard_error` is set when a frame's `on_frame` broke or a read/EOF error
    /// occurred; if the decoder did not itself emit an error terminal, the reader
    /// synthesizes one so a hard error always surfaces as an `error` event.
    fn finalize(&mut self, hard_error: Option<String>, out: &mut Vec<AssistantMessageEvent>) {
        let terminal_start = out.len();
        let message = self.decoder.finish(out);
        let emitted_error = out[terminal_start..]
            .iter()
            .any(|event| matches!(event, AssistantMessageEvent::Error { .. }));

        self.result = Some(match hard_error {
            Some(err) if !emitted_error => {
                let mut errored = message.clone();
                errored.stop_reason = StopReason::Error;
                errored.error_message = Some(err);
                out.push(AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: errored.clone(),
                });
                Err(errored)
            }
            Some(_) => Err(message),
            None if emitted_error => Err(message),
            None => Ok(message),
        });
        self.finished = true;
    }
}

impl Iterator for AssistantEventReader<'_> {
    type Item = AssistantMessageEvent;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.ready.pop_front() {
                return Some(event);
            }
            if self.finished {
                return None;
            }

            match self.chunks.next() {
                Some(Ok(bytes)) => {
                    let mut frames = Vec::new();
                    self.splitter.feed(&bytes, &mut frames);
                    let mut out = Vec::new();
                    let mut hard_error = None;
                    for frame in &frames {
                        if let ControlFlow::Break(err) = self.decoder.on_frame(frame, &mut out) {
                            hard_error = Some(err);
                            break;
                        }
                    }
                    if let Some(err) = hard_error {
                        self.finalize(Some(err), &mut out);
                    }
                    self.ready.extend(out);
                }
                Some(Err(err)) => {
                    let mut out = Vec::new();
                    self.finalize(Some(err.to_string()), &mut out);
                    self.ready.extend(out);
                }
                None => {
                    let mut frames = Vec::new();
                    self.splitter.finish(&mut frames);
                    let mut out = Vec::new();
                    let mut hard_error = None;
                    for frame in &frames {
                        if let ControlFlow::Break(err) = self.decoder.on_frame(frame, &mut out) {
                            hard_error = Some(err);
                            break;
                        }
                    }
                    self.finalize(hard_error, &mut out);
                    self.ready.extend(out);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AssistantRole, ContentBlock, Usage, UsageCost};
    use std::time::{Duration, Instant};

    fn zero_usage() -> Usage {
        Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: UsageCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
        }
    }

    fn frames(body: &[u8]) -> Vec<ServerSentEvent> {
        let mut splitter = SseFrameSplitter::new();
        let mut out = Vec::new();
        splitter.feed(body, &mut out);
        splitter.finish(&mut out);
        out
    }

    fn frames_byte_by_byte(body: &[u8]) -> Vec<ServerSentEvent> {
        let mut splitter = SseFrameSplitter::new();
        let mut out = Vec::new();
        for byte in body {
            splitter.feed(&[*byte], &mut out);
        }
        splitter.finish(&mut out);
        out
    }

    #[test]
    fn incremental_framing_matches_buffered() {
        let body = b"event: message\ndata: hello\n\ndata: line1\ndata: line2\n\n: comment\ndata: tail\n\ndata: dangling";
        let whole = frames(body);
        let dripped = frames_byte_by_byte(body);
        assert_eq!(whole, dripped);
        // Sanity: the framing produced the frames we expect.
        assert_eq!(whole.len(), 4);
        assert_eq!(whole[0].event.as_deref(), Some("message"));
        assert_eq!(whole[0].data, "hello");
        assert_eq!(whole[1].data, "line1\nline2");
        assert_eq!(whole[2].data, "tail");
        assert_eq!(whole[3].data, "dangling");
    }

    #[test]
    fn incremental_framing_handles_split_multibyte_utf8() {
        // The euro sign is three UTF-8 bytes; feeding them across chunk
        // boundaries must not corrupt the frame.
        let body = "data: \u{20ac}\n\n".as_bytes();
        assert_eq!(frames(body), frames_byte_by_byte(body));
        assert_eq!(frames(body)[0].data, "\u{20ac}");
    }

    /// A trivial decoder: every `data:` line is a text delta; `[DONE]` is the
    /// terminal marker. `finish` emits a `Done` with the accumulated text.
    struct TextDeltaDecoder {
        text: String,
    }

    impl TextDeltaDecoder {
        fn new() -> Self {
            Self {
                text: String::new(),
            }
        }

        fn message(&self) -> AssistantMessage {
            AssistantMessage {
                role: AssistantRole::Assistant,
                content: vec![ContentBlock::Text {
                    text: self.text.clone(),
                    text_signature: None,
                }],
                api: "test".to_string(),
                provider: "test".to_string(),
                model: "test".to_string(),
                response_model: None,
                response_id: None,
                diagnostics: None,
                usage: zero_usage(),
                stop_reason: StopReason::Stop,
                error_message: None,
                timestamp: 0,
            }
        }
    }

    impl SseEventDecoder for TextDeltaDecoder {
        fn on_frame(
            &mut self,
            frame: &ServerSentEvent,
            out: &mut Vec<AssistantMessageEvent>,
        ) -> ControlFlow<String> {
            if frame.data == "[DONE]" {
                return ControlFlow::Continue(());
            }
            self.text.push_str(&frame.data);
            out.push(AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: frame.data.clone(),
                partial: self.message(),
            });
            ControlFlow::Continue(())
        }

        fn finish(&mut self, out: &mut Vec<AssistantMessageEvent>) -> AssistantMessage {
            let message = self.message();
            out.push(AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: message.clone(),
            });
            message
        }
    }

    /// An iterator that sleeps `delay` then yields ONE frame's bytes per `next()`,
    /// bounded by the supplied frame list. The sleep simulates real inter-chunk
    /// arrival timing so the reader's PULL behaviour is observable.
    struct SleepingChunks {
        frames: std::vec::IntoIter<Vec<u8>>,
        delay: Duration,
    }

    impl Iterator for SleepingChunks {
        type Item = io::Result<Vec<u8>>;

        fn next(&mut self) -> Option<Self::Item> {
            let bytes = self.frames.next()?;
            std::thread::sleep(self.delay);
            Some(Ok(bytes))
        }
    }

    #[test]
    fn reader_pulls_events_with_inter_chunk_timing() {
        let n = 5usize;
        let delay = Duration::from_millis(15);
        let mut raw_frames: Vec<Vec<u8>> = (0..n)
            .map(|i| format!("data: word{i}\n\n").into_bytes())
            .collect();
        raw_frames.push(b"data: [DONE]\n\n".to_vec());

        // Whole body, for the equivalence check.
        let whole_body: Vec<u8> = raw_frames.iter().flatten().copied().collect();

        let chunks = SleepingChunks {
            frames: raw_frames.into_iter(),
            delay,
        };
        let mut reader =
            AssistantEventReader::new(Box::new(chunks), Box::new(TextDeltaDecoder::new()));

        let start = Instant::now();
        let mut stamped: Vec<(Duration, AssistantMessageEvent)> = Vec::new();
        for event in reader.by_ref() {
            stamped.push((start.elapsed(), event));
        }

        let events: Vec<AssistantMessageEvent> = stamped.iter().map(|(_, e)| e.clone()).collect();

        // n text deltas + 1 terminal Done.
        assert_eq!(events.len(), n + 1);
        assert!(matches!(
            events.last(),
            Some(AssistantMessageEvent::Done { .. })
        ));

        // PULL timing: the yielded events span at least (n-1) delays. Each of the
        // first n text deltas arrives one sleeping chunk apart.
        let spread = stamped.last().unwrap().0 - stamped.first().unwrap().0;
        let lower_bound = delay.mul_f64((n as f64 - 1.0) * 0.5);
        assert!(
            spread >= lower_bound,
            "expected inter-event spread >= {lower_bound:?} (pull timing), got {spread:?}",
        );

        // Equivalence: driving the reader chunk-by-chunk yields the SAME events
        // and message as feeding the whole body at once.
        let mut splitter = SseFrameSplitter::new();
        let mut frames = Vec::new();
        splitter.feed(&whole_body, &mut frames);
        splitter.finish(&mut frames);
        let mut decoder = TextDeltaDecoder::new();
        let mut buffered_events = Vec::new();
        for frame in &frames {
            let _ = decoder.on_frame(frame, &mut buffered_events);
        }
        let buffered_message = decoder.finish(&mut buffered_events);

        assert_eq!(events, buffered_events);
        assert_eq!(
            reader.result().and_then(|r| r.as_ref().ok()),
            Some(&buffered_message)
        );
    }

    /// A decoder that breaks on an `error` frame to exercise the terminal-error
    /// path.
    struct BreakingDecoder;

    impl SseEventDecoder for BreakingDecoder {
        fn on_frame(
            &mut self,
            frame: &ServerSentEvent,
            _out: &mut Vec<AssistantMessageEvent>,
        ) -> ControlFlow<String> {
            if frame.event.as_deref() == Some("error") {
                return ControlFlow::Break(format!("boom: {}", frame.data));
            }
            ControlFlow::Continue(())
        }

        fn finish(&mut self, _out: &mut Vec<AssistantMessageEvent>) -> AssistantMessage {
            AssistantMessage {
                role: AssistantRole::Assistant,
                content: Vec::new(),
                api: "test".to_string(),
                provider: "test".to_string(),
                model: "test".to_string(),
                response_model: None,
                response_id: None,
                diagnostics: None,
                usage: zero_usage(),
                stop_reason: StopReason::Stop,
                error_message: None,
                timestamp: 0,
            }
        }
    }

    #[test]
    fn reader_break_produces_error_terminal() {
        let body = b"data: fine\n\nevent: error\ndata: kaboom\n\n";
        let reader = AssistantEventReader::new(
            Box::new(std::iter::once(Ok(body.to_vec()))),
            Box::new(BreakingDecoder),
        );
        let mut reader = reader;
        let events: Vec<AssistantMessageEvent> = reader.by_ref().collect();
        assert!(matches!(
            events.last(),
            Some(AssistantMessageEvent::Error { .. })
        ));
        let result = reader.result().expect("finished");
        assert!(result.is_err());
        let errored = result.as_ref().err().unwrap();
        assert_eq!(errored.stop_reason, StopReason::Error);
        assert_eq!(errored.error_message.as_deref(), Some("boom: kaboom"));
    }

    #[test]
    fn reader_read_error_produces_error_terminal() {
        let chunks: Vec<io::Result<Vec<u8>>> = vec![
            Ok(b"data: hi\n\n".to_vec()),
            Err(io::Error::new(io::ErrorKind::UnexpectedEof, "truncated")),
        ];
        let mut reader = AssistantEventReader::new(
            Box::new(chunks.into_iter()),
            Box::new(TextDeltaDecoder::new()),
        );
        let events: Vec<AssistantMessageEvent> = reader.by_ref().collect();
        assert!(matches!(
            events.last(),
            Some(AssistantMessageEvent::Error { .. })
        ));
        assert!(reader.result().unwrap().is_err());
    }
}
