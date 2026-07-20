//! Strip `//` line comments and trailing commas from JSON.
//!
//! Ported from pi's `utils/json.ts`. Two regex passes remove `//` line
//! comments and trailing commas while leaving string literals untouched (so a
//! `//` sequence inside a string survives). pi has no dedicated vitest file;
//! the tests below are written fresh for the Rust port. Used by pi's
//! model-config loader to accept JSONC-style config.

use regex::Regex;
use std::sync::OnceLock;

fn comment_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""(?:\\.|[^"\\])*"|//[^\n]*"#).expect("valid comment regex"))
}

fn trailing_comma_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#""(?:\\.|[^"\\])*"|,(\s*[}\]])"#).expect("valid trailing-comma regex")
    })
}

/// Strip `//` line comments and trailing commas from `input`, leaving string
/// literals intact.
pub fn strip_json_comments(input: &str) -> String {
    // Pass 1: drop `//` line comments, keeping string literals.
    let without_comments = comment_regex().replace_all(input, |caps: &regex::Captures| {
        let matched = &caps[0];
        if matched.starts_with('"') {
            matched.to_string()
        } else {
            String::new()
        }
    });

    // Pass 2: drop trailing commas before `}` or `]`, keeping the whitespace
    // and closing bracket, and keeping string literals.
    trailing_comma_regex()
        .replace_all(&without_comments, |caps: &regex::Captures| {
            if let Some(tail) = caps.get(1) {
                tail.as_str().to_string()
            } else {
                let matched = &caps[0];
                if matched.starts_with('"') {
                    matched.to_string()
                } else {
                    String::new()
                }
            }
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_line_comments() {
        let input = "{\n  \"a\": 1 // comment\n}";
        assert_eq!(strip_json_comments(input), "{\n  \"a\": 1 \n}");
    }

    #[test]
    fn strips_trailing_commas_in_objects() {
        assert_eq!(strip_json_comments("{\"a\": 1,}"), "{\"a\": 1}");
    }

    #[test]
    fn strips_trailing_commas_in_arrays() {
        assert_eq!(strip_json_comments("[1, 2, 3,]"), "[1, 2, 3]");
    }

    #[test]
    fn strips_trailing_comma_with_whitespace() {
        assert_eq!(strip_json_comments("[1, 2,\n]"), "[1, 2\n]");
    }

    #[test]
    fn preserves_double_slash_inside_string_literal() {
        let input = "{\"url\": \"https://example.com\"}";
        assert_eq!(strip_json_comments(input), input);
    }

    #[test]
    fn preserves_comment_like_text_and_commas_inside_strings() {
        let input = "{\"note\": \"a, b, // c\"}";
        assert_eq!(strip_json_comments(input), input);
    }

    #[test]
    fn preserves_escaped_quotes_in_strings() {
        let input = "{\"quote\": \"say \\\"hi\\\" // now\"}";
        assert_eq!(strip_json_comments(input), input);
    }

    #[test]
    fn leaves_clean_json_unchanged() {
        let input = "{\"a\": 1, \"b\": [2, 3]}";
        assert_eq!(strip_json_comments(input), input);
    }
}
