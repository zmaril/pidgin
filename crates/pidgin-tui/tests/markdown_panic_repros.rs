//! Deterministic regression tests for the two real `markdown_render` defects the
//! hardening crew's markdown property tests surfaced in PR #126
//! (`test-hardening/v3-golden-and-proptest`, `tests/prop_markdown.rs`). There the
//! source was deliberately left unmodified and each defect was captured as an
//! `#[ignore]`d reproduction:
//!
//!   * `blockquote_list_line_panics_out_of_bounds` — `markdown_render(">-\n*", 80)`
//!     aborted with an out-of-bounds slice panic at `src/markdown/lexer.rs:446`.
//!   * `deeply_nested_blockquote_overflows_stack` — `markdown_render(&"> ".repeat(n))`
//!     overflowed the stack and aborted the process for deep `n`.
//!
//! Both are now fixed in the lexer/renderer, so these tests are **re-enabled**
//! (no `#[ignore]`) and, rather than merely asserting panic-freedom, they assert
//! the *exact* output pi's own `Markdown` renderer produces for the same inputs.
//!
//! Ground truth was captured by driving pi's `Markdown`
//! (`vendor/pi/packages/tui/src/components/markdown.ts`) with `defaultMarkdownTheme`
//! — the identical path `markdown_render` mirrors — over these inputs:
//!
//! ```text
//! ">-\n*"           @ w=80 -> ["\x1b[2m│ \x1b[22m…- …" (×2, padded to 80)]
//! "> ".repeat(124)  @ w=80 -> [""]
//! "> ".repeat(200)  @ w=80 -> [""]
//! "> ".repeat(500)  @ w=80 -> [""]
//! ```
//!
//! For the deep-nesting input pi collapses the empty nested blockquotes to a
//! single empty line at every depth it can process (marked's own recursion only
//! throws `RangeError: Maximum call stack size exceeded` past several thousand
//! levels in Node); the Rust port reproduces `[""]` at all of these depths without
//! overflowing.

use pidgin_tui::markdown_render;

/// Re-enables PR #126's `blockquote_list_line_panics_out_of_bounds`.
///
/// A blockquote whose content starts a list (`>-`) followed by a bare list-marker
/// line (`*`) made marked's blockquote tokenizer synthesize a `raw` longer than
/// the remaining source (`">-\n\n*"`, 6 bytes over the 4-byte input). The old
/// `src[tok.raw.len()..]` slice panicked; JS's `String.prototype.substring`
/// clamps instead, so marked emits a blockquote containing two normalized list
/// items. The port now matches that byte-for-byte.
#[test]
fn blockquote_then_list_line_matches_marked() {
    // pi: two identical lines, each `│ - ` padded to width 80 (76 trailing
    // spaces), with the dim quote border + italic-quote + cyan list-bullet ANSI.
    let line = format!(
        "\u{1b}[2m│ \u{1b}[22m\u{1b}[3m\u{1b}[3m\u{1b}[36m- \u{1b}[39m\u{1b}[23m\u{1b}[3m\u{1b}[23m{}",
        " ".repeat(76)
    );
    let expected = vec![line.clone(), line];
    assert_eq!(markdown_render(">-\n*", 80), expected);
}

/// Re-enables PR #126's `deeply_nested_blockquote_overflows_stack`.
///
/// Deeply nested empty blockquotes previously overflowed the recursive
/// lexer/renderer and aborted the process. pi collapses them to a single empty
/// line; the port now reproduces `[""]` at every tested depth without overflow.
#[test]
fn deeply_nested_blockquote_matches_marked() {
    for depth in [124usize, 200, 500] {
        let input = "> ".repeat(depth);
        assert_eq!(
            markdown_render(&input, 80),
            vec![String::new()],
            "deep blockquote nesting depth {depth} must match pi's empty-line output"
        );
    }
}
