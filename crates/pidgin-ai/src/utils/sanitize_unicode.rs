// straitjacket-allow-file:emoji — a faithful transcription of pi's
// `sanitize-unicode.ts`: the 🙈/🚀 literals are load-bearing test corpus that
// exercise paired-surrogate preservation and must stay verbatim.
//! Lone-surrogate stripping, ported from pi-ai's
//! `packages/ai/src/utils/sanitize-unicode.ts` at pinned commit `3da591ab`.
//!
//! pi's `sanitizeSurrogates` removes *unpaired* UTF-16 surrogate code units — a
//! high surrogate (`0xD800..=0xDBFF`) not followed by a low surrogate, or a low
//! surrogate (`0xDC00..=0xDFFF`) not preceded by a high one — because such units
//! make JSON serialization fail at several providers. Properly paired surrogates
//! (every non-BMP character, including emoji) are preserved.
//!
//! # JS vs. Rust surrogate semantics
//!
//! In JavaScript a `string` is a sequence of UTF-16 code units and *can* hold a
//! lone surrogate, so `sanitizeSurrogates` has real work to do. A Rust `&str` is
//! guaranteed well-formed UTF-8 and therefore can never contain a lone surrogate
//! — the moment text becomes a `String`, any lone surrogate has already been
//! replaced (typically with U+FFFD) or rejected. For every valid Rust string
//! this function is consequently the identity.
//!
//! To stay faithful to pi's semantics rather than assuming the identity, the
//! port still performs the pass over UTF-16 code units: it re-encodes the input
//! with [`str::encode_utf16`], drops any unpaired surrogate exactly as pi's
//! regex does, and rebuilds a string. On well-formed input this reconstructs the
//! original bytes; the surrogate-dropping branches are exercised in tests via a
//! `&[u16]` helper that can express the lone-surrogate cases a `&str` cannot.

/// Removes unpaired Unicode surrogate characters from a string
/// (`sanitize-unicode.ts:21`).
///
/// Valid emoji and other non-BMP characters use properly paired surrogates and
/// are not affected.
pub fn sanitize_surrogates(text: &str) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let sanitized = strip_lone_surrogates(&units);
    // Every retained surrogate is part of a valid pair, so decoding cannot fail.
    String::from_utf16(&sanitized).expect("sanitized code units form valid UTF-16")
}

/// The core of pi's regex over a UTF-16 code-unit sequence: keep matched
/// high/low surrogate pairs, drop any unpaired surrogate, pass everything else
/// through unchanged.
fn strip_lone_surrogates(units: &[u16]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let unit = units[i];
        if is_high_surrogate(unit) {
            // Keep a high surrogate only when a low surrogate follows it.
            if units.get(i + 1).is_some_and(|next| is_low_surrogate(*next)) {
                out.push(unit);
                out.push(units[i + 1]);
                i += 2;
                continue;
            }
            // Unpaired high surrogate: drop it.
            i += 1;
        } else if is_low_surrogate(unit) {
            // Any low surrogate reached here is unpaired (paired ones are
            // consumed by the high-surrogate branch above): drop it.
            i += 1;
        } else {
            out.push(unit);
            i += 1;
        }
    }
    out
}

fn is_high_surrogate(unit: u16) -> bool {
    (0xD800..=0xDBFF).contains(&unit)
}

fn is_low_surrogate(unit: u16) -> bool {
    (0xDC00..=0xDFFF).contains(&unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_valid_emoji() {
        // Properly paired surrogates (🙈 is U+1F648) are untouched.
        assert_eq!(sanitize_surrogates("Hello 🙈 World"), "Hello 🙈 World");
    }

    #[test]
    fn identity_on_bmp_text() {
        assert_eq!(sanitize_surrogates("plain ascii"), "plain ascii");
        assert_eq!(
            sanitize_surrogates("こんにちは 你好 ∑∫∂√"),
            "こんにちは 你好 ∑∫∂√"
        );
        assert_eq!(sanitize_surrogates(""), "");
    }

    #[test]
    fn drops_unpaired_high_surrogate() {
        // "Text " + lone 0xD83D + " here" → the surrogate is removed.
        let units: Vec<u16> = "Text "
            .encode_utf16()
            .chain(std::iter::once(0xD83D))
            .chain(" here".encode_utf16())
            .collect();
        let sanitized = strip_lone_surrogates(&units);
        assert_eq!(String::from_utf16(&sanitized).unwrap(), "Text  here");
    }

    #[test]
    fn drops_unpaired_low_surrogate() {
        let units: Vec<u16> = "a"
            .encode_utf16()
            .chain(std::iter::once(0xDC00))
            .chain("b".encode_utf16())
            .collect();
        let sanitized = strip_lone_surrogates(&units);
        assert_eq!(String::from_utf16(&sanitized).unwrap(), "ab");
    }

    #[test]
    fn preserves_paired_surrogates_in_code_units() {
        // High + low surrogate pair for 🚀 (U+1F680) survives intact.
        let units: Vec<u16> = std::iter::once(0xD83D_u16)
            .chain(std::iter::once(0xDE80))
            .collect();
        let sanitized = strip_lone_surrogates(&units);
        assert_eq!(sanitized, units);
        assert_eq!(String::from_utf16(&sanitized).unwrap(), "🚀");
    }

    #[test]
    fn drops_high_surrogate_followed_by_non_low() {
        // 0xD83D followed by an ordinary BMP char: the surrogate is unpaired.
        let units: Vec<u16> = std::iter::once(0xD83D_u16)
            .chain("x".encode_utf16())
            .collect();
        let sanitized = strip_lone_surrogates(&units);
        assert_eq!(String::from_utf16(&sanitized).unwrap(), "x");
    }
}
