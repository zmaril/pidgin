// straitjacket-allow-file:duplication — a faithful transcription of pi's
// `utils/hash.ts`: the two-lane mixing steps are near-identical by design.
// straitjacket-allow-file:emoji — TODO(straitjacket): a test input string below contains an
// emoji (🙈) on purpose, exercising non-ASCII hashing. Declared explicitly so it suppresses
// only emoji, not every rule (the old bracket form was a silent catch-all).
//! Fast deterministic string hashing, ported from pi-ai's
//! `packages/ai/src/utils/hash.ts` at pinned commit `3da591ab`.
//!
//! [`short_hash`] mirrors pi's `shortHash` byte-for-byte: a two-lane
//! multiply-xor mixer (the "cyrb53"-style hash) over the string's UTF-16 code
//! units, emitting two base-36 unsigned 32-bit words concatenated.
//!
//! Parity notes:
//! - JS `charCodeAt(i)` yields a UTF-16 code unit, so the port iterates
//!   `str.encode_utf16()` rather than Unicode scalar values.
//! - JS `Math.imul` is a 32-bit wrapping multiply; `u32::wrapping_mul` matches
//!   it. XOR and the unsigned right shift (`>>>`) are bit-identical on `u32`, so
//!   carrying every lane as `u32` reproduces the signed/unsigned dance in the
//!   source exactly.
//! - `(x >>> 0).toString(36)` renders the unsigned 32-bit word in base 36 with
//!   digits `0-9a-z` and no padding; [`to_base36`] reproduces that.

/// Fast deterministic hash to shorten long strings (`hash.ts:2`).
pub fn short_hash(str: &str) -> String {
    let mut h1: u32 = 0xdead_beef;
    let mut h2: u32 = 0x41c6_ce57;
    for ch in str.encode_utf16() {
        let code = u32::from(ch);
        h1 = (h1 ^ code).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ code).wrapping_mul(1_597_334_677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
    format!("{}{}", to_base36(h2), to_base36(h1))
}

/// JS `(n >>> 0).toString(36)`: base-36 with digits `0-9a-z`, no padding.
fn to_base36(mut n: u32) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut buf = Vec::new();
    while n > 0 {
        buf.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    buf.reverse();
    String::from_utf8(buf).expect("base-36 digits are ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_deterministic() {
        assert_eq!(short_hash("hello world"), short_hash("hello world"));
    }

    #[test]
    fn distinct_inputs_differ() {
        assert_ne!(short_hash("hello"), short_hash("world"));
        assert_ne!(short_hash("a"), short_hash("b"));
    }

    #[test]
    fn output_uses_only_base36_digits() {
        let h = short_hash("Any input string with unicode 🙈 and more");
        assert!(!h.is_empty());
        assert!(h
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
    }

    #[test]
    fn base36_of_zero_is_zero() {
        assert_eq!(to_base36(0), "0");
        assert_eq!(to_base36(35), "z");
        assert_eq!(to_base36(36), "10");
    }

    #[test]
    fn matches_pi_reference_vectors() {
        // Golden values produced by pi's `shortHash` (Node) at commit 3da591ab.
        assert_eq!(short_hash(""), "k4n83c7h0j2b");
        assert_eq!(short_hash("hello"), "1h6qa0qrowduu");
        assert_eq!(short_hash("The quick brown fox"), "na9l2t124bi96");
        assert_eq!(short_hash("a"), "m8735310ae7sx");
        assert_eq!(short_hash("b"), "jbf49n1hx4dkv");
    }

    #[test]
    fn empty_string_is_stable_and_nonempty() {
        // With no input the mixing loop is skipped and only the seed fold runs,
        // yielding a fixed value; recomputing must agree.
        let a = short_hash("");
        assert!(!a.is_empty());
        assert_eq!(a, short_hash(""));
    }
}
