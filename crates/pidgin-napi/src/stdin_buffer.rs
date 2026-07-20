//! Node-API surface for the stdin escape-sequence splitter (`StdinBufferCore`).
//!
//! This exposes the Rust [`pidgin_tui::StdinBuffer`] — a faithful port of pi's
//! `StdinBuffer` (`vendor/pi/packages/tui/src/stdin-buffer.ts`) — to pi's
//! `packages/tui` `stdin-buffer.test.ts`. The Rust type owns the whole splitting
//! state machine: partial-escape reassembly, CSI/OSC/DCS/APC/SS3 completion,
//! old-style + SGR mouse handling, the WezTerm double-ESC split, bracketed-paste
//! extraction, and the Kitty printable-codepoint dedup.
//!
//! # The seam: state in Rust, plumbing in TS
//!
//! pi's public `StdinBuffer` is an `EventEmitter` that fires `"data"` / `"paste"`
//! synchronously during `process()` and arms a 10 ms completion timer. Those two
//! concerns are inherently JS-runtime (an event target's identity, a
//! `setTimeout`), so the shim keeps them in TS. Everything that decides *what*
//! the events are crosses here: [`StdinBufferCore::process`] returns the ordered
//! list of events (`{kind, value}`) the buffer produced for this chunk, and the
//! shim replays them onto pi's `EventEmitter`. The incomplete remainder is read
//! back with [`StdinBufferCore::get_buffer`] (pi's `getBuffer()`), flushed with
//! [`StdinBufferCore::flush`], and reset with [`StdinBufferCore::clear`].
//!
//! # Marshaling
//!
//! Everything crosses as strings/numbers/plain objects. `process` takes a JS
//! string (the shim converts `Buffer` → string first, including pi's single
//! high-byte → `ESC`+char rewrite) and returns `Vec<StdinEventJs>`; whole strings
//! cross the boundary intact, so there is no byte-boundary / lone-surrogate risk
//! (the splitter never re-slices a sequence JS-side). No JS closures, streams, or
//! stable object identity are required across the boundary.

use napi_derive::napi;

use pidgin_tui::{StdinBuffer, StdinBufferOptions, StdinEvent};

/// One event emitted by [`StdinBufferCore::process`], mirroring pi's
/// `"data"` / `"paste"` EventEmitter channels. `kind` is `"data"` for a complete
/// input sequence (one keypress / escape sequence) or `"paste"` for the body of
/// a bracketed paste (markers already stripped); `value` is the payload string.
#[napi(object)]
pub struct StdinEventJs {
    pub kind: String,
    pub value: String,
}

impl From<StdinEvent> for StdinEventJs {
    fn from(event: StdinEvent) -> Self {
        match event {
            StdinEvent::Data(value) => Self {
                kind: "data".to_string(),
                value,
            },
            StdinEvent::Paste(value) => Self {
                kind: "paste".to_string(),
                value,
            },
        }
    }
}

/// The Rust-backed stdin splitter, exposed to JavaScript as `StdinBufferCore`.
///
/// The JS `StdinBuffer` shim owns one of these, keeps pi's `EventEmitter` surface
/// and completion timer in TS, and per chunk: converts any `Buffer` to a string,
/// calls [`StdinBufferCore::process`], and replays the returned events onto the
/// emitter. All cross-chunk state (buffer, paste mode, pending Kitty codepoint)
/// lives entirely in the wrapped [`StdinBuffer`].
#[napi(js_name = "StdinBufferCore")]
pub struct StdinBufferCore {
    inner: StdinBuffer,
}

#[napi]
impl StdinBufferCore {
    /// Build a buffer. `timeout_ms` mirrors pi's `StdinBufferOptions.timeout`
    /// (default 10); it is retained for parity — the completion timer itself is
    /// driven by the JS shim, so this value does not affect native splitting.
    #[napi(constructor)]
    pub fn new(timeout_ms: Option<i64>) -> Self {
        let options = match timeout_ms {
            Some(ms) => StdinBufferOptions {
                timeout_ms: ms.max(0) as u64,
            },
            None => StdinBufferOptions::default(),
        };
        Self {
            inner: StdinBuffer::new(options),
        }
    }

    /// pi's `StdinBuffer.process`. Accumulate `data`, and return every event the
    /// buffer produced for this chunk (complete `data` sequences and any
    /// `paste`), in emission order. The incomplete remainder is retained and can
    /// be read with [`StdinBufferCore::get_buffer`].
    #[napi(js_name = "process")]
    pub fn process(&mut self, data: String) -> Vec<StdinEventJs> {
        self.inner
            .process(&data)
            .into_iter()
            .map(StdinEventJs::from)
            .collect()
    }

    /// pi's `StdinBuffer.flush`: emit the buffered incomplete remainder verbatim
    /// as a list of `data` payload strings (empty when nothing is buffered), and
    /// reset the remainder.
    #[napi(js_name = "flush")]
    pub fn flush(&mut self) -> Vec<String> {
        self.inner
            .flush()
            .into_iter()
            .map(|event| match event {
                StdinEvent::Data(value) | StdinEvent::Paste(value) => value,
            })
            .collect()
    }

    /// pi's `StdinBuffer.getBuffer`: the currently buffered incomplete remainder.
    #[napi(js_name = "getBuffer")]
    pub fn get_buffer(&self) -> String {
        self.inner.buffer().to_string()
    }

    /// pi's `StdinBuffer.clear` / `destroy`: drop all buffered state.
    #[napi(js_name = "clear")]
    pub fn clear(&mut self) {
        self.inner.clear();
    }
}
