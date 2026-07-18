//! Strip ANSI escape sequences from a string.
//!
//! Ported from pi's `utils/ansi.ts`, which itself derives from chalk's
//! `ansi-regex` / `strip-ansi` (MIT, Sindre Sorhus). Removes OSC sequences
//! (`ESC ] ... ST`) and CSI / C1 sequences, matching chalk's behavior.
//!
//! Unlike the TypeScript original, there is no runtime type guard: the Rust
//! type system guarantees a `&str`, so pi's `TypeError`-on-non-string test is
//! not applicable and is intentionally dropped.

use regex::Regex;
use std::sync::OnceLock;

/// Build the ANSI-stripping regex, mirroring pi's `ansiRegex()`.
fn ansi_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Valid string terminator sequences are BEL, ESC\, and 0x9c.
        let st = "(?:\\u{0007}|\\u{001B}\\u{005C}|\\u{009C})";
        // OSC sequences only: ESC ] ... ST (non-greedy until the first ST).
        let osc = format!("(?:\\u{{001B}}\\][\\s\\S]*?{st})");
        // CSI and related: ESC/C1, optional intermediates, optional params
        // (supports ; and :) then final byte.
        // The `[` in the intermediates class is escaped (`\[`); the `regex`
        // crate treats a bare `[[` as the start of a nested character class.
        let csi = "[\\u{001B}\\u{009B}][\\[\\]()#;?]*(?:\\d{1,4}(?:[;:]\\d{0,4})*)?[\\dA-PR-TZcf-nq-uy=><~]";
        Regex::new(&format!("{osc}|{csi}")).expect("valid ANSI regex")
    })
}

/// Remove ANSI escape sequences (OSC + CSI) from `value`.
pub fn strip_ansi(value: &str) -> String {
    // Fast path: ANSI codes require the ESC (7-bit) or CSI (8-bit) introducer.
    if !value.contains('\u{001B}') && !value.contains('\u{009B}') {
        return value.to_string();
    }
    ansi_regex().replace_all(value, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_color_sequences() {
        assert_eq!(strip_ansi("a\x1b[31mred\x1b[0mz"), "aredz");
    }

    #[test]
    fn strips_osc_hyperlink_sequences() {
        let input = "a\x1b]8;;https://example.com\x07link\x1b]8;;\x07z";
        assert_eq!(strip_ansi(input), "alinkz");
    }

    #[test]
    fn strips_ris_without_leaking_the_final_byte() {
        assert_eq!(strip_ansi("\x1bcdone"), "done");
    }

    #[test]
    fn strips_single_byte_esc_sequences_without_leaking_final_bytes() {
        for code in b'g'..=b'm' {
            let seq = format!("\x1b{}ok", code as char);
            assert_eq!(strip_ansi(&seq), "ok");
        }
        for code in b'r'..=b't' {
            let seq = format!("\x1b{}ok", code as char);
            assert_eq!(strip_ansi(&seq), "ok");
        }
    }

    #[test]
    fn strips_common_ansi_sequences_used_in_tool_output() {
        let input = "a\x1b[31mred\x1b[0m\x1b]8;;https://example.com\x07link\x1b]8;;\x07z";
        assert_eq!(strip_ansi(input), "aredlinkz");
    }

    #[test]
    fn passes_through_plain_strings() {
        assert_eq!(strip_ansi("plain"), "plain");
    }
}
