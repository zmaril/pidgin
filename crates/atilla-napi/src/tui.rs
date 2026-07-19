//! Node-API surface for the differential TUI renderer (`TuiCore`).
//!
//! This exposes the Rust renderer ([`atilla_tui::Tui`], backed by the in-memory
//! [`atilla_tui::LoggingTerminal`] sink) to pi's `packages/tui` renderer tests.
//! It is the native half of pi's `TUI` differential render path
//! (`vendor/pi/packages/tui/src/tui.ts`, `TUI::doRender`).
//!
//! # The seam: pre-rendered lines, not a component tree
//!
//! The Rust renderer consumes **pre-rendered lines**, never a component
//! prop-tree: pi's TS components render themselves to `string[]` on the JS side
//! (exactly as they do today), and the shim feeds those lines into Rust via
//! [`TuiCore::set_base_lines`]. Rust then does the diff / composite / write-stream
//! that `doRender` performs, and the shim drains the resulting bytes with
//! [`TuiCore::take_writes`] and forwards them to pi's `VirtualTerminal` (an xterm
//! emulator) so `getViewport`/cell assertions replay JS-side.
//!
//! # Why no threadsafe callback is needed here
//!
//! The base render tree crosses as an already-flattened `string[]` per frame, and
//! the write stream comes back as a `String`. There is no re-render of props in
//! Rust and no callback into JS during a render, so the boundary is plain
//! synchronous calls — no `ThreadsafeFunction`. Overlays (which Rust would
//! re-render at a resolved width, requiring a JS callback) are intentionally out
//! of scope: the shim keeps pi's overlay/focus/input path in TS and only routes
//! the base render path through this core.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use atilla_tui::{LoggingTerminal, Tui};

/// The Rust-backed differential renderer, exposed to JavaScript as `TuiCore`.
///
/// The JS `TUI` shim owns one of these, keeps pi's component tree / overlay /
/// focus / input logic in JS, and per frame: renders its components to lines,
/// pushes them via [`TuiCore::set_base_lines`], drives one render with
/// [`TuiCore::tick`], drains the write stream with [`TuiCore::take_writes`], and
/// forwards it to the JS terminal. The renderer's cross-frame diff state
/// (previous lines, cursor rows, full-redraw ladder) lives entirely in the
/// wrapped [`Tui`].
#[napi(js_name = "TuiCore")]
pub struct TuiCore {
    tui: Tui<LoggingTerminal>,
}

fn to_usize(value: i64) -> usize {
    value.max(0) as usize
}

#[napi]
impl TuiCore {
    /// Build a renderer over an in-memory logging terminal of `cols` x `rows`.
    /// `show_hardware_cursor` mirrors pi's `PI_HARDWARE_CURSOR` opt-in.
    #[napi(constructor)]
    pub fn new(cols: i64, rows: i64, show_hardware_cursor: bool) -> Self {
        let terminal = LoggingTerminal::new(to_usize(cols), to_usize(rows));
        Self {
            tui: Tui::new(terminal, show_hardware_cursor),
        }
    }

    /// Update the terminal dimensions the next render reads (pi reads
    /// `terminal.columns`/`rows` each `doRender`; the width/height-change full
    /// redraw is decided inside `tick` by comparing against the previous frame).
    #[napi(js_name = "setSize")]
    pub fn set_size(&mut self, cols: i64, rows: i64) {
        self.tui
            .terminal_mut()
            .resize(to_usize(cols), to_usize(rows));
    }

    /// pi's `setClearOnShrink`: clear empty rows when content shrinks.
    #[napi(js_name = "setClearOnShrink")]
    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        self.tui.set_clear_on_shrink(enabled);
    }

    /// Model `isTermuxSession()`: on Termux, height changes do not force a full
    /// redraw. The shim calls this per frame from `process.env.TERMUX_VERSION`.
    #[napi(js_name = "setTermux")]
    pub fn set_termux(&mut self, termux: bool) {
        self.tui.set_termux(termux);
    }

    /// Model terminal image capability (gates pi's `queryCellSize`). Not needed
    /// for the base render diff, but kept for surface parity.
    #[napi(js_name = "setImagesCapable")]
    pub fn set_images_capable(&mut self, capable: bool) {
        self.tui.set_images_capable(capable);
    }

    /// Replace the entire base frame with pre-rendered `lines` (pi's TS
    /// components rendered to `string[]`). Backed by a single line-buffer child.
    #[napi(js_name = "setBaseLines")]
    pub fn set_base_lines(&mut self, lines: Vec<String>) {
        self.tui.set_base_lines(lines);
    }

    /// Drive exactly one render: `request_render(force)` then `flush()`. This is
    /// the deterministic, timer-free equivalent of a single fire of pi's
    /// coalesced scheduler (one `waitForRender()`). `force` resets the diff state
    /// for a full redraw (pi's `requestRender(true)`).
    #[napi(js_name = "tick")]
    pub fn tick(&mut self, force: bool) -> Result<()> {
        self.tui.request_render(force);
        self.tui
            .flush()
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    /// Drain the accumulated write stream (pi's
    /// `LoggingVirtualTerminal.getWrites()` + `clearWrites()`): the exact bytes
    /// this frame emitted, then reset the sink.
    #[napi(js_name = "takeWrites")]
    pub fn take_writes(&mut self) -> String {
        self.tui.take_writes()
    }

    /// Number of full redraws performed (pi's `fullRedraws` getter).
    #[napi(js_name = "fullRedraws")]
    pub fn full_redraws(&self) -> i64 {
        self.tui.full_redraws() as i64
    }

    /// Logical cursor row (end of rendered content).
    #[napi(js_name = "cursorRow")]
    pub fn cursor_row(&self) -> i64 {
        self.tui.cursor_row()
    }

    /// Actual hardware cursor row (pi's `hardwareCursorRow`).
    #[napi(js_name = "hardwareCursorRow")]
    pub fn hardware_cursor_row(&self) -> i64 {
        self.tui.hardware_cursor_row()
    }

    /// Previous viewport top (resize-aware cursor bookkeeping).
    #[napi(js_name = "previousViewportTop")]
    pub fn previous_viewport_top(&self) -> i64 {
        self.tui.previous_viewport_top()
    }

    /// High-water working area (max lines ever rendered since last clear).
    #[napi(js_name = "maxLinesRendered")]
    pub fn max_lines_rendered(&self) -> i64 {
        self.tui.max_lines_rendered()
    }
}
