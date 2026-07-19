//! Shared byte-level primitives used by the ported utilities.
//!
//! These helpers centralize three families of duplicated logic:
//! - percent-decoding (`%XX`) shared by [`git_url`](super::git_url) and
//!   [`paths`](super::paths);
//! - bounds-safe byte access and little/big-endian multi-byte reads shared by
//!   [`exif`](super::exif) and [`mime`](super::mime);
//! - lexical POSIX path normalization shared by
//!   [`changelog`](super::changelog) and [`paths`](super::paths).
//!
//! The byte readers deliberately treat out-of-range indices as `0` (mirroring
//! the JavaScript sources these were ported from). Do not replace them with
//! `u32::from_be_bytes`/`from_le_bytes`, which panic on truncated buffers.

/// Percent-decode a value into UTF-8; `None` signals a malformed escape or
/// invalid UTF-8, mirroring a `decodeURIComponent` that throws.
pub(crate) fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Read the byte at `pos`, returning `0` when out of range.
pub(crate) fn byte_at(bytes: &[u8], pos: usize) -> u32 {
    bytes.get(pos).copied().unwrap_or(0) as u32
}

/// Read a little-endian `u16` at `offset`, treating missing bytes as `0`.
pub(crate) fn read_u16_le(bytes: &[u8], offset: usize) -> u32 {
    byte_at(bytes, offset) | (byte_at(bytes, offset + 1) << 8)
}

/// Read a big-endian `u16` at `offset`, treating missing bytes as `0`.
pub(crate) fn read_u16_be(bytes: &[u8], offset: usize) -> u32 {
    (byte_at(bytes, offset) << 8) | byte_at(bytes, offset + 1)
}

/// Read a little-endian `u32` at `offset`, treating missing bytes as `0`.
pub(crate) fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    byte_at(bytes, offset)
        | (byte_at(bytes, offset + 1) << 8)
        | (byte_at(bytes, offset + 2) << 16)
        | (byte_at(bytes, offset + 3) << 24)
}

/// Read a big-endian `u32` at `offset`, treating missing bytes as `0`.
pub(crate) fn read_u32_be(bytes: &[u8], offset: usize) -> u32 {
    (byte_at(bytes, offset) << 24)
        | (byte_at(bytes, offset + 1) << 16)
        | (byte_at(bytes, offset + 2) << 8)
        | byte_at(bytes, offset + 3)
}

/// Return true when `buffer` begins with `prefix`.
pub(crate) fn starts_with(buffer: &[u8], prefix: &[u8]) -> bool {
    buffer.len() >= prefix.len() && buffer[..prefix.len()] == *prefix
}

/// Return true when the ASCII `text` appears in `buffer` at `offset`.
pub(crate) fn starts_with_ascii(buffer: &[u8], offset: usize, text: &str) -> bool {
    let text_bytes = text.as_bytes();
    if buffer.len() < offset + text_bytes.len() {
        return false;
    }
    &buffer[offset..offset + text_bytes.len()] == text_bytes
}

/// Lexically normalize a POSIX (`/`-separated) path, resolving `.` and `..`
/// without touching the filesystem (mirrors `path.posix.normalize`).
///
/// When `preserve_trailing` is true, a trailing slash on the input is kept and
/// an empty result collapses to `"."` even for absolute inputs (the behavior
/// changelog link rewriting relies on). When false, an empty absolute result
/// stays `"/"` (the behavior path resolution relies on).
pub(crate) fn posix_normalize(path: &str, preserve_trailing: bool) -> String {
    let is_abs = path.starts_with('/');
    let has_trailing = preserve_trailing && path.len() > 1 && path.ends_with('/');
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                if let Some(last) = out.last() {
                    if *last != ".." {
                        out.pop();
                        continue;
                    }
                }
                if !is_abs {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    if preserve_trailing {
        if joined.is_empty() {
            return ".".to_string();
        }
        let mut result = if is_abs { format!("/{joined}") } else { joined };
        if has_trailing {
            result.push('/');
        }
        result
    } else if is_abs {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_roundtrips_and_rejects_malformed() {
        assert_eq!(percent_decode("a%20b").as_deref(), Some("a b"));
        assert_eq!(percent_decode("plain").as_deref(), Some("plain"));
        assert_eq!(percent_decode("%A"), None);
        assert_eq!(percent_decode("%ZZ"), None);
        assert_eq!(percent_decode("%E0%A4%A"), None);
    }

    #[test]
    fn multibyte_reads_are_bounds_safe() {
        let buf = [0x12, 0x34, 0x56, 0x78];
        assert_eq!(read_u16_le(&buf, 0), 0x3412);
        assert_eq!(read_u16_be(&buf, 0), 0x1234);
        assert_eq!(read_u32_le(&buf, 0), 0x78563412);
        assert_eq!(read_u32_be(&buf, 0), 0x12345678);
        // Out-of-range indices read as 0 instead of panicking.
        assert_eq!(read_u32_be(&buf, 3), 0x78000000);
        assert_eq!(byte_at(&buf, 99), 0);
    }

    #[test]
    fn prefix_helpers_match_bytes() {
        assert!(starts_with(b"RIFFxxxx", b"RIFF"));
        assert!(!starts_with(b"RI", b"RIFF"));
        assert!(starts_with_ascii(b"..WEBP..", 2, "WEBP"));
        assert!(!starts_with_ascii(b"WEB", 0, "WEBP"));
    }

    #[test]
    fn posix_normalize_matches_both_modes() {
        // Shared core.
        assert_eq!(posix_normalize("a/./b/../c", false), "a/c");
        assert_eq!(posix_normalize("a/./b/../c", true), "a/c");
        assert_eq!(posix_normalize("../a", false), "../a");
        // Trailing-slash and empty-path edge cases differ per mode.
        assert_eq!(posix_normalize("a/b/", true), "a/b/");
        assert_eq!(posix_normalize("a/b/", false), "a/b");
        assert_eq!(posix_normalize("/..", false), "/");
        assert_eq!(posix_normalize("/..", true), ".");
        assert_eq!(posix_normalize("", false), ".");
        assert_eq!(posix_normalize("", true), ".");
    }
}
