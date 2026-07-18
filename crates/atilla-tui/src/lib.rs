//! `atilla-tui` — a bit-exact Rust port of pi's TUI pure-function layer.
//!
//! Part 1 ports the width module from `vendor/pi/packages/tui/src/utils.ts`.
//! Correctness means byte-identical results versus pi: pi's inline renderer
//! crashes on any width mismatch, so the port is validated against vectors
//! extracted from pi itself (see `tests/width_vectors.rs`).

mod eaw_table;
mod unicode_tables;
pub mod width;

pub use width::{
    extract_ansi_code, extract_segments, normalize_terminal_output, slice_by_column,
    slice_with_width, truncate_to_width, visible_width, wrap_text_with_ansi, ExtractSegments,
};
