//! Bounded property tests for the terminal-width primitives in
//! `pidgin-tui/src/width.rs`.
//!
//! Every invariant asserted here was verified against the module's real
//! behavior (see the per-test comments). Tests are deterministic and bounded:
//! 128 cases each, no on-disk failure persistence.

use pidgin_tui::{truncate_to_width, visible_width, wrap_text_with_ansi};
use proptest::prelude::*;

/// A string of ANSI-free printable ASCII (`0x20..=0x7e`) — the range for which
/// `is_printable_ascii` holds, so `visible_width(s) == s.len()`.
fn ascii_printable() -> impl Strategy<Value = String> {
    "[ -~]{0,200}"
}

/// An arbitrary string that may contain control chars, wide graphemes, ANSI
/// escapes, tabs, and newlines — for panic-freedom checks.
fn arbitrary_text() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..400).prop_map(|cs| cs.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, failure_persistence: None, ..ProptestConfig::default() })]

    /// `visible_width` is additive over concatenation of ANSI-free ASCII, and
    /// equals the byte length for that range.
    #[test]
    fn visible_width_additive_over_ascii(a in ascii_printable(), b in ascii_printable()) {
        prop_assert_eq!(visible_width(&a), a.len());
        prop_assert_eq!(visible_width(&b), b.len());
        let joined = format!("{a}{b}");
        prop_assert_eq!(visible_width(&joined), visible_width(&a) + visible_width(&b));
    }

    /// `visible_width` never panics on arbitrary input.
    #[test]
    fn visible_width_never_panics(s in arbitrary_text()) {
        let _ = visible_width(&s);
    }

    /// Every wrapped line of ANSI-free ASCII fits within the target width
    /// (verified: long words are hard-broken and trailing whitespace trimmed).
    #[test]
    fn wrap_ascii_lines_fit_width(s in ascii_printable(), w in 1usize..=200) {
        for line in wrap_text_with_ansi(&s, w) {
            prop_assert!(
                visible_width(&line) <= w,
                "line {:?} (width {}) exceeds wrap width {}",
                line,
                visible_width(&line),
                w
            );
        }
    }

    /// `wrap_text_with_ansi` never panics and always returns at least one line
    /// on arbitrary input.
    #[test]
    fn wrap_never_panics(s in arbitrary_text(), w in 1usize..=200) {
        let lines = wrap_text_with_ansi(&s, w);
        prop_assert!(!lines.is_empty());
    }

    /// `truncate_to_width` (no padding) never exceeds the target width, for any
    /// input and ASCII ellipsis (verified: the kept prefix plus ellipsis width
    /// is bounded by `max_width` in every code path).
    #[test]
    fn truncate_stays_within_width(
        s in arbitrary_text(),
        w in 1i64..=200,
        ellipsis in prop_oneof![Just(""), Just("."), Just("...")],
    ) {
        let out = truncate_to_width(&s, w, ellipsis, false);
        prop_assert!(
            visible_width(&out) as i64 <= w,
            "truncated {:?} has visible width {} > target {}",
            out,
            visible_width(&out),
            w
        );
    }

    /// `truncate_to_width` never panics across padding and ellipsis choices.
    #[test]
    fn truncate_never_panics(
        s in arbitrary_text(),
        w in -4i64..=200,
        ellipsis in prop_oneof![Just(""), Just("."), Just("...")],
        pad in any::<bool>(),
    ) {
        let _ = truncate_to_width(&s, w, ellipsis, pad);
    }
}
