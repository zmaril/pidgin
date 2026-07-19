//! HTML entity decoding.
//!
//! Ported from pi's `utils/html.ts`. Decodes the five predefined named
//! entities (`amp`, `lt`, `gt`, `quot`, `apos`) plus decimal (`#nn`) and
//! hexadecimal (`#xNN`) numeric character references. pi has no dedicated
//! vitest file (it is exercised via the syntax-highlight tests); the tests
//! below are written fresh for the Rust port.

/// A decoded HTML entity: the resulting text and how many source characters
/// were consumed (including the leading `&` and trailing `;`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedHtmlEntity {
    pub text: String,
    pub length: usize,
}

/// Convert a Unicode scalar value to a string, rejecting values outside the
/// valid range (`0..=0x10FFFF`) and surrogate code points.
///
/// `char::from_u32` already returns `None` for out-of-range values and
/// surrogate code points, so no separate range guard is needed.
fn decode_code_point(code_point: u32) -> Option<String> {
    char::from_u32(code_point).map(|c| c.to_string())
}

/// Decode a bare entity name (the text between `&` and `;`, exclusive).
///
/// Returns `None` for unknown names, empty numeric references, or numeric
/// references that fail to parse or fall outside the valid code-point range.
pub fn decode_html_entity(entity: &str) -> Option<String> {
    match entity {
        "amp" => return Some("&".to_string()),
        "lt" => return Some("<".to_string()),
        "gt" => return Some(">".to_string()),
        "quot" => return Some("\"".to_string()),
        "apos" => return Some("'".to_string()),
        _ => {}
    }

    if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        let code_point = u32::from_str_radix(hex, 16).ok()?;
        return decode_code_point(code_point);
    }

    if let Some(dec) = entity.strip_prefix('#') {
        let code_point: u32 = dec.parse().ok()?;
        return decode_code_point(code_point);
    }

    None
}

/// Decode an entity starting at byte index `index` (which must point at `&`).
///
/// Scans forward to the next `;`, giving up if none is found within 16 bytes.
/// On success returns the decoded text and the number of bytes consumed
/// (`&` through `;` inclusive).
pub fn decode_html_entity_at(html: &str, index: usize) -> Option<DecodedHtmlEntity> {
    let bytes = html.as_bytes();
    let semicolon_index = bytes
        .iter()
        .enumerate()
        .skip(index + 1)
        .find(|(_, &b)| b == b';')
        .map(|(i, _)| i)?;

    if semicolon_index - index > 16 {
        return None;
    }

    let entity = &html[index + 1..semicolon_index];
    let decoded = decode_html_entity(entity)?;

    Some(DecodedHtmlEntity {
        text: decoded,
        length: semicolon_index - index + 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_named_entities() {
        assert_eq!(decode_html_entity("amp").as_deref(), Some("&"));
        assert_eq!(decode_html_entity("lt").as_deref(), Some("<"));
        assert_eq!(decode_html_entity("gt").as_deref(), Some(">"));
        assert_eq!(decode_html_entity("quot").as_deref(), Some("\""));
        assert_eq!(decode_html_entity("apos").as_deref(), Some("'"));
    }

    #[test]
    fn decodes_decimal_references() {
        assert_eq!(decode_html_entity("#65").as_deref(), Some("A"));
        assert_eq!(decode_html_entity("#0").as_deref(), Some("\u{0}"));
        assert_eq!(decode_html_entity("#128512").as_deref(), Some("\u{1F600}"));
    }

    #[test]
    fn decodes_hex_references() {
        assert_eq!(decode_html_entity("#x41").as_deref(), Some("A"));
        assert_eq!(decode_html_entity("#X41").as_deref(), Some("A"));
        assert_eq!(decode_html_entity("#x1F600").as_deref(), Some("\u{1F600}"));
    }

    #[test]
    fn rejects_unknown_and_invalid() {
        assert_eq!(decode_html_entity("nbsp"), None);
        assert_eq!(decode_html_entity(""), None);
        assert_eq!(decode_html_entity("#"), None);
        assert_eq!(decode_html_entity("#x"), None);
        assert_eq!(decode_html_entity("#zz"), None);
        assert_eq!(decode_html_entity("#xZZ"), None);
        // Above the maximum Unicode code point.
        assert_eq!(decode_html_entity("#x110000"), None);
        assert_eq!(decode_html_entity("#1114112"), None);
        // Surrogate code point is not a valid scalar value.
        assert_eq!(decode_html_entity("#xD800"), None);
    }

    #[test]
    fn decodes_entity_at_index() {
        let html = "a&amp;b";
        let decoded = decode_html_entity_at(html, 1).unwrap();
        assert_eq!(decoded.text, "&");
        assert_eq!(decoded.length, 5);
    }

    #[test]
    fn decodes_numeric_entity_at_index() {
        let html = "&#65;";
        let decoded = decode_html_entity_at(html, 0).unwrap();
        assert_eq!(decoded.text, "A");
        assert_eq!(decoded.length, 5);
    }

    #[test]
    fn returns_none_without_semicolon() {
        assert_eq!(decode_html_entity_at("&amp", 0), None);
    }

    #[test]
    fn returns_none_when_semicolon_too_far() {
        // 17 characters before the semicolon exceeds the 16-byte limit.
        let html = "&aaaaaaaaaaaaaaaaa;";
        assert_eq!(decode_html_entity_at(html, 0), None);
    }

    #[test]
    fn returns_none_for_unknown_entity_at_index() {
        assert_eq!(decode_html_entity_at("&nbsp;", 0), None);
    }
}
