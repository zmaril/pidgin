//! PKCE utilities, ported from pi-ai's
//! `packages/ai/src/auth/oauth/pkce.ts` at pinned commit `3da591ab`.
//!
//! pi generates a 32-byte random verifier, base64url-encodes it, and derives the
//! challenge as base64url(SHA-256(utf8(verifier))) (`pkce.ts:21-34`). The
//! base64url encoding is standard base64 with `+`->`-`, `/`->`_`, and `=`
//! stripped (`pkce.ts:9-15`).
//!
//! The 32 random bytes are injectable ([`generate_pkce_from_bytes`]) so tests
//! are deterministic; [`generate_pkce`] draws them from the OS CSPRNG.

use base64::Engine;
use sha2::{Digest, Sha256};

/// A generated PKCE verifier / challenge pair (`pkce.ts:21`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    /// The code verifier (base64url of 32 random bytes).
    pub verifier: String,
    /// The code challenge (base64url of SHA-256 of the verifier).
    pub challenge: String,
}

/// Encode bytes as a base64url string (`pkce.ts:9-15`): standard base64, then
/// `+`->`-`, `/`->`_`, and `=` padding stripped.
fn base64url_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD
        .encode(bytes)
        .replace('+', "-")
        .replace('/', "_")
        .replace('=', "")
}

/// Generate a PKCE pair from a fixed 32-byte verifier seed (`pkce.ts:21-34`).
///
/// Deterministic: the same input always yields the same verifier/challenge.
pub fn generate_pkce_from_bytes(bytes: [u8; 32]) -> Pkce {
    let verifier = base64url_encode(&bytes);
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = base64url_encode(&hasher.finalize());
    Pkce {
        verifier,
        challenge,
    }
}

/// Generate a PKCE pair from OS randomness (`pkce.ts:21-34`,
/// `crypto.getRandomValues`).
pub fn generate_pkce() -> Pkce {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable");
    generate_pkce_from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector_matches_web_crypto() {
        // Verifier bytes 0..32; verifier + challenge computed independently
        // (base64url(bytes) and base64url(sha256(utf8(verifier)))).
        let mut bytes = [0u8; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = i as u8;
        }
        let pkce = generate_pkce_from_bytes(bytes);
        assert_eq!(pkce.verifier, "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8");
        assert_eq!(
            pkce.challenge,
            "6oZqdX5MOLq_qBJ8vppAnT4fk6AP8UiP9zX8-Rev_9A"
        );
    }

    #[test]
    fn base64url_has_no_padding_or_standard_alphabet_chars() {
        // 0xFB 0xFF encodes to "+/" in standard base64 -> "-_" in base64url.
        assert_eq!(base64url_encode(&[0xFB, 0xFF]), "-_8");
        assert!(!base64url_encode(&[0u8; 32]).contains('='));
    }

    #[test]
    fn generate_pkce_is_well_formed() {
        let pkce = generate_pkce();
        // 32 bytes -> 43 base64url chars (no padding).
        assert_eq!(pkce.verifier.len(), 43);
        // SHA-256 -> 32 bytes -> 43 base64url chars.
        assert_eq!(pkce.challenge.len(), 43);
        assert!(!pkce.verifier.contains(['+', '/', '=']));
        assert!(!pkce.challenge.contains(['+', '/', '=']));
    }
}
