//! Deferred port of pi's `utils/syntax-highlight.ts`.
//!
//! `renderHighlightedHtml` is pure, but it consumes highlight.js token spans
//! and the pi-tui theme formatter surface. Porting it depends on the tui theme
//! layer and a Rust highlighting engine, so it is deferred. Note: its
//! HTML-entity decoding dependency is already available in [`super::html`].
