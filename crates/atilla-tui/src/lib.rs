//! `atilla-tui` — a bit-exact Rust port of pi's TUI pure-function layer.
//!
//! Part 1 ports the width module from `vendor/pi/packages/tui/src/utils.ts`.
//! Part 2 ports the key parser from `vendor/pi/packages/tui/src/keys.ts`.
//! Correctness means byte-identical results versus pi: pi's inline renderer
//! crashes on any width mismatch, so the port is validated against vectors
//! extracted from pi itself (see `tests/width_vectors.rs` and
//! `tests/keys_vectors.rs`).

mod eaw_table;
pub mod keys;
pub mod renderer;
pub mod terminal;
mod unicode_tables;
pub mod width;

pub use keys::{
    decode_kitty_printable, decode_printable_key, is_key_release, is_key_repeat,
    is_kitty_protocol_active, matches_key, parse_key, set_kitty_protocol_active, KeyEventType,
};
pub use renderer::{
    delete_kitty_image, is_image_line, Component, ComponentId, Container, MarginSpec,
    OverlayAnchor, OverlayHandleId, OverlayMargin, OverlayOptions, ReactionAction, RenderError,
    SharedLines, SizeValue, Tui, CURSOR_MARKER,
};
pub use terminal::{CrosstermTerminal, LoggingTerminal, Terminal};
pub use width::{
    extract_ansi_code, extract_segments, normalize_terminal_output, slice_by_column,
    slice_with_width, truncate_to_width, visible_width, wrap_text_with_ansi, ExtractSegments,
};
