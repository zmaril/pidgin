//! JSON repair + streaming-JSON parsing, ported from pi-ai's
//! `packages/ai/src/utils/json-parse.ts` at pinned commit `3da591ab`.
//!
//! Anthropic (and other providers) occasionally emit JSON with raw control
//! characters or invalid escape sequences inside string literals, and tool-call
//! arguments arrive as a growing stream of partial JSON. These helpers mirror
//! pi's behaviour exactly:
//!
//! - [`repair_json`] escapes raw control characters and doubles backslashes
//!   before invalid escapes, matching pi's `repairJson` character-for-character.
//! - [`parse_json_with_repair`] parses, and on failure retries once against the
//!   repaired text (`parseJsonWithRepair`).
//! - [`parse_streaming_json`] always yields a value for a possibly-incomplete
//!   fragment (`parseStreamingJson`); it layers a partial-JSON completion pass
//!   over the repair path for truncated fragments.
//!
//! pi iterates strings by UTF-16 code unit; this port iterates by Unicode scalar
//! value. The two agree for the BMP text these helpers actually see (JSON
//! escapes, ASCII control characters, tool-argument payloads).

use serde_json::Value;

/// The set of characters that form a valid JSON escape after a backslash.
const VALID_JSON_ESCAPES: [char; 9] = ['"', '\\', '/', 'b', 'f', 'n', 'r', 't', 'u'];

fn is_control_character(ch: char) -> bool {
    let code = ch as u32;
    code <= 0x1f
}

fn escape_control_character(ch: char) -> String {
    match ch {
        '\u{08}' => "\\b".to_string(),
        '\u{0c}' => "\\f".to_string(),
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        _ => format!("\\u{:04x}", ch as u32),
    }
}

/// Repairs malformed JSON string literals by escaping raw control characters and
/// doubling backslashes before invalid escape characters (pi's `repairJson`).
pub fn repair_json(json: &str) -> String {
    let chars: Vec<char> = json.chars().collect();
    let mut repaired = String::with_capacity(json.len());
    let mut in_string = false;
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];

        if !in_string {
            repaired.push(ch);
            if ch == '"' {
                in_string = true;
            }
            index += 1;
            continue;
        }

        if ch == '"' {
            repaired.push(ch);
            in_string = false;
            index += 1;
            continue;
        }

        if ch == '\\' {
            let next_char = chars.get(index + 1).copied();
            match next_char {
                None => {
                    repaired.push_str("\\\\");
                    index += 1;
                    continue;
                }
                Some('u') => {
                    let unicode_digits: String = chars.iter().skip(index + 2).take(4).collect();
                    if unicode_digits.len() == 4
                        && unicode_digits.chars().all(|c| c.is_ascii_hexdigit())
                    {
                        repaired.push_str(&format!("\\u{unicode_digits}"));
                        index += 6;
                        continue;
                    }
                    // Fall through to the invalid-escape branch below.
                }
                Some(nc) if VALID_JSON_ESCAPES.contains(&nc) => {
                    repaired.push('\\');
                    repaired.push(nc);
                    index += 2;
                    continue;
                }
                _ => {}
            }

            // Invalid escape: double the backslash and re-process the next char.
            repaired.push_str("\\\\");
            index += 1;
            continue;
        }

        if is_control_character(ch) {
            repaired.push_str(&escape_control_character(ch));
        } else {
            repaired.push(ch);
        }
        index += 1;
    }

    repaired
}

/// Parses `json`, retrying once against [`repair_json`] output on failure
/// (pi's `parseJsonWithRepair`).
pub fn parse_json_with_repair(json: &str) -> Result<Value, serde_json::Error> {
    match serde_json::from_str::<Value>(json) {
        Ok(value) => Ok(value),
        Err(error) => {
            let repaired = repair_json(json);
            if repaired != json {
                serde_json::from_str::<Value>(&repaired)
            } else {
                Err(error)
            }
        }
    }
}

/// Attempts to parse potentially incomplete JSON during streaming, always
/// returning a value (pi's `parseStreamingJson`). Falls back to `{}` when even
/// the partial-completion attempts fail.
pub fn parse_streaming_json(partial_json: Option<&str>) -> Value {
    let Some(text) = partial_json else {
        return Value::Object(Default::default());
    };
    if text.trim().is_empty() {
        return Value::Object(Default::default());
    }

    if let Ok(value) = parse_json_with_repair(text) {
        return value;
    }
    if let Some(value) = partial_parse(text) {
        return value;
    }
    if let Some(value) = partial_parse(&repair_json(text)) {
        return value;
    }
    Value::Object(Default::default())
}

/// A faithful port of the `partial-json@0.1.7` dependency's parser (promplate's
/// `parseJSON`) with its default `Allow.ALL`, adequate for streamed
/// tool-argument fragments.
///
/// Unlike a close-the-open-containers heuristic, this recursive-descent parser
/// reproduces partial-json's actual behaviour: it keeps every completed token,
/// **drops the incomplete trailing token** (a truncated number/literal/value —
/// and, because a value is dropped, the key it belonged to as well), completes a
/// partial string to its prefix and a partial `tr`/`nu`/`fa` literal to
/// `true`/`null`/`false`, and closes the still-open containers. Returns `None`
/// when even partial-json would throw.
///
/// Known divergences from `partial-json@0.1.7`, both irrelevant to the
/// tool-argument JSON these helpers see:
///
/// - The non-standard JS literals `Infinity`/`-Infinity`/`NaN` are not
///   recognised; such input is treated as malformed (as strict JSON would). They
///   are also unrepresentable as [`serde_json::Value`].
/// - Like [`repair_json`], strings are walked by Unicode scalar value rather than
///   pi's/JS's UTF-16 code units; the two agree for the BMP text these helpers
///   see.
fn partial_parse(text: &str) -> Option<Value> {
    // partial-json's `parseJSON` trims first and throws on an empty string; the
    // caller then falls through to the next strategy.
    let chars: Vec<char> = text.trim().chars().collect();
    if chars.is_empty() {
        return None;
    }
    PartialParser {
        chars: &chars,
        index: 0,
    }
    .parse_any()
    .ok()
}

/// A terminal signal that a fragment could not be parsed even partially. Fuses
/// partial-json's `PartialJSON` and `MalformedJSON` errors: with `Allow.ALL`
/// both are caught identically by the container parsers, so a single marker
/// suffices.
struct Incomplete;

/// A recursive-descent parser mirroring partial-json's `_parseJSON` closure set.
struct PartialParser<'a> {
    chars: &'a [char],
    index: usize,
}

impl PartialParser<'_> {
    fn len(&self) -> usize {
        self.chars.len()
    }

    /// JS `jsonString[index]`: the char at `index`, or `None` past the end.
    fn char_at(&self, index: usize) -> Option<char> {
        self.chars.get(index).copied()
    }

    /// JS `String.prototype.substring(start, end)`: clamps each bound to
    /// `[0, len]` and swaps them when `start > end`.
    fn js_substring(&self, start: i64, end: i64) -> String {
        let len = self.len() as i64;
        let mut a = start.clamp(0, len);
        let mut b = end.clamp(0, len);
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        self.chars[a as usize..b as usize].iter().collect()
    }

    /// JS `jsonString.lastIndexOf(needle)` over the whole input: the last index
    /// of `needle`, or `-1` when absent.
    fn js_last_index_of(&self, needle: char) -> i64 {
        self.chars
            .iter()
            .rposition(|&c| c == needle)
            .map(|i| i as i64)
            .unwrap_or(-1)
    }

    /// The remaining substring `jsonString.substring(index)`.
    fn remaining(&self) -> String {
        self.chars[self.index.min(self.len())..].iter().collect()
    }

    fn skip_blank(&mut self) {
        while let Some(c) = self.char_at(self.index) {
            if c == ' ' || c == '\n' || c == '\r' || c == '\t' {
                self.index += 1;
            } else {
                break;
            }
        }
    }

    fn parse_any(&mut self) -> Result<Value, Incomplete> {
        self.skip_blank();
        if self.index >= self.len() {
            return Err(Incomplete); // markPartialJSON("Unexpected end of input")
        }
        match self.char_at(self.index) {
            Some('"') => return self.parse_str(),
            Some('{') => return self.parse_obj(),
            Some('[') => return self.parse_arr(),
            _ => {}
        }
        let remaining_len = self.len() - self.index;
        // `null` (complete or a partial prefix like `nu`).
        if self.js_substring(self.index as i64, (self.index + 4) as i64) == "null"
            || (remaining_len < 4 && "null".starts_with(&self.remaining()))
        {
            self.index += 4;
            return Ok(Value::Null);
        }
        // `true` / `tr`.
        if self.js_substring(self.index as i64, (self.index + 4) as i64) == "true"
            || (remaining_len < 4 && "true".starts_with(&self.remaining()))
        {
            self.index += 4;
            return Ok(Value::Bool(true));
        }
        // `false` / `fa`.
        if self.js_substring(self.index as i64, (self.index + 5) as i64) == "false"
            || (remaining_len < 5 && "false".starts_with(&self.remaining()))
        {
            self.index += 5;
            return Ok(Value::Bool(false));
        }
        // partial-json also recognises Infinity/-Infinity/NaN here; see the
        // `partial_parse` doc comment for why this port omits them.
        self.parse_num()
    }

    fn parse_str(&mut self) -> Result<Value, Incomplete> {
        let start = self.index;
        let mut escape = false;
        self.index += 1; // skip initial quote
        while self.index < self.len()
            && (self.chars[self.index] != '"' || (escape && self.chars[self.index - 1] == '\\'))
        {
            escape = if self.chars[self.index] == '\\' {
                !escape
            } else {
                false
            };
            self.index += 1;
        }

        if self.char_at(self.index) == Some('"') {
            self.index += 1; // JS `++index`
            let end = self.index as i64 - escape as i64;
            let slice = self.js_substring(start as i64, end);
            return serde_json::from_str::<Value>(&slice).map_err(|_| Incomplete);
        }
        // Allow.STR: complete the truncated string to its prefix.
        let attempt = self.js_substring(start as i64, self.index as i64 - escape as i64) + "\"";
        if let Ok(value) = serde_json::from_str::<Value>(&attempt) {
            return Ok(value);
        }
        // A trailing invalid escape: drop back to the last backslash and close.
        let fallback = self.js_substring(start as i64, self.js_last_index_of('\\')) + "\"";
        serde_json::from_str::<Value>(&fallback).map_err(|_| Incomplete)
    }

    fn parse_obj(&mut self) -> Result<Value, Incomplete> {
        self.index += 1; // skip initial brace
        self.skip_blank();
        let mut obj = serde_json::Map::new();
        // partial-json wraps the whole body in `try { ... } catch { return obj }`
        // (Allow.OBJ), so every `Incomplete` below yields the object accumulated
        // so far — WITHOUT the trailing `index++` that a normally-closed object
        // performs. Both control-flow paths are reproduced explicitly.
        loop {
            if self.char_at(self.index) == Some('}') {
                self.index += 1; // skip final brace
                return Ok(Value::Object(obj));
            }
            self.skip_blank();
            if self.index >= self.len() {
                return Ok(Value::Object(obj)); // index >= length && Allow.OBJ
            }
            let key = match self.parse_str() {
                Ok(Value::String(key)) => key,
                // A throwing key parse (or a non-string key) is caught by the
                // outer Allow.OBJ handler.
                _ => return Ok(Value::Object(obj)),
            };
            self.skip_blank();
            self.index += 1; // skip colon
            match self.parse_any() {
                Ok(value) => {
                    obj.insert(key, value);
                }
                // Inner Allow.OBJ catch: drop the incomplete value (and its key).
                Err(Incomplete) => return Ok(Value::Object(obj)),
            }
            self.skip_blank();
            if self.char_at(self.index) == Some(',') {
                self.index += 1; // skip comma
            }
        }
    }

    fn parse_arr(&mut self) -> Result<Value, Incomplete> {
        self.index += 1; // skip initial bracket
        let mut arr = Vec::new();
        loop {
            if self.char_at(self.index) == Some(']') {
                self.index += 1; // skip final bracket
                return Ok(Value::Array(arr));
            }
            match self.parse_any() {
                Ok(value) => arr.push(value),
                // Allow.ARR: a truncated trailing element is dropped.
                Err(Incomplete) => return Ok(Value::Array(arr)),
            }
            self.skip_blank();
            if self.char_at(self.index) == Some(',') {
                self.index += 1; // skip comma
            }
        }
    }

    fn parse_num(&mut self) -> Result<Value, Incomplete> {
        if self.index == 0 {
            // Whole input is a bare number (possibly truncated).
            let whole: String = self.chars.iter().collect();
            if whole == "-" {
                return Err(Incomplete);
            }
            if let Ok(value) = serde_json::from_str::<Value>(&whole) {
                return Ok(value);
            }
            let sub = self.js_substring(0, self.js_last_index_of('e'));
            return serde_json::from_str::<Value>(&sub).map_err(|_| Incomplete);
        }

        let start = self.index;
        if self.char_at(self.index) == Some('-') {
            self.index += 1;
        }
        while let Some(c) = self.char_at(self.index) {
            if c == ',' || c == ']' || c == '}' {
                break;
            }
            self.index += 1;
        }

        let num_str = self.js_substring(start as i64, self.index as i64);
        if let Ok(value) = serde_json::from_str::<Value>(&num_str) {
            return Ok(value);
        }
        if num_str == "-" {
            return Err(Incomplete);
        }
        // Drop a truncated exponent/fraction: retry at the last `e`. When there
        // is no `e`, `lastIndexOf` yields `-1` and the swap-clamped substring
        // becomes a non-numeric prefix that fails to parse, dropping the value.
        let sub = self.js_substring(start as i64, self.js_last_index_of('e'));
        serde_json::from_str::<Value>(&sub).map_err(|_| Incomplete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repairs_invalid_escape_and_raw_control_char() {
        // Mirrors the malformed tool JSON from pi's anthropic-sse-parsing test:
        // an invalid `\H` escape and a raw tab inside a string literal.
        let raw = "{\"path\":\"A\\H\",\"text\":\"col1\tcol2\"}";
        let parsed = parse_json_with_repair(raw).expect("repairs and parses");
        assert_eq!(parsed, json!({ "path": "A\\H", "text": "col1\tcol2" }));
    }

    #[test]
    fn repair_is_identity_for_valid_json() {
        let valid = r#"{"a":"b\n","c":1}"#;
        assert_eq!(repair_json(valid), valid);
    }

    #[test]
    fn preserves_valid_unicode_escape() {
        let valid = r#"{"a":"ç"}"#;
        assert_eq!(repair_json(valid), valid);
        let parsed = parse_json_with_repair(valid).unwrap();
        assert_eq!(parsed, json!({ "a": "ç" }));
    }

    #[test]
    fn streaming_json_empty_is_object() {
        assert_eq!(parse_streaming_json(None), json!({}));
        assert_eq!(parse_streaming_json(Some("   ")), json!({}));
    }

    #[test]
    fn streaming_json_complete_object() {
        assert_eq!(
            parse_streaming_json(Some(r#"{"path":"a","text":"b"}"#)),
            json!({ "path": "a", "text": "b" }),
        );
    }

    #[test]
    fn streaming_json_completes_truncated_object() {
        // A tool-argument fragment cut off mid-value should still yield the
        // parsed prefix rather than throwing.
        assert_eq!(
            parse_streaming_json(Some(r#"{"path":"abc","text":"he"#)),
            json!({ "path": "abc", "text": "he" }),
        );
    }

    #[test]
    fn streaming_json_completes_open_object_after_key() {
        assert_eq!(
            parse_streaming_json(Some(r#"{"path":"abc","#)),
            json!({ "path": "abc" }),
        );
    }

    #[test]
    fn streaming_json_dangling_key_dropped() {
        assert_eq!(parse_streaming_json(Some(r#"{"path":"#)), json!({}));
    }

    // --- repair_json escape handling ---------------------------------------

    #[test]
    fn repair_escapes_raw_control_characters() {
        // `\b \f \n \r \t` use their short escapes; the other control
        // character (`0x01`) uses `\uXXXX` (pi's `escapeControlCharacter`).
        let raw = "{\"a\":\"x\u{08}\u{0c}\n\r\t\u{01}\"}";
        assert_eq!(repair_json(raw), r#"{"a":"x\b\f\n\r\t\u0001"}"#);
    }

    #[test]
    fn repair_preserves_valid_unicode_escape_but_doubles_invalid_one() {
        // A well-formed `\uXXXX` is passed through untouched...
        assert_eq!(repair_json(r#"{"a":"ç"}"#), r#"{"a":"ç"}"#);
        // ...while a `\u` with fewer than four hex digits is an invalid escape,
        // so its backslash is doubled.
        assert_eq!(repair_json(r#"{"a":"\u12"}"#), r#"{"a":"\\u12"}"#);
    }

    #[test]
    fn repair_doubles_backslash_before_invalid_escape() {
        // `\H` is not a valid JSON escape, so the backslash is doubled.
        assert_eq!(repair_json(r#"{"a":"\H"}"#), r#"{"a":"\\H"}"#);
    }

    #[test]
    fn repair_doubles_trailing_backslash_in_open_string() {
        // A dangling backslash at end-of-input inside a string is doubled
        // (pi's `nextChar === undefined` branch).
        assert_eq!(repair_json("{\"a\":\"x\\"), "{\"a\":\"x\\\\");
    }

    #[test]
    fn repair_leaves_backslashes_outside_strings_untouched() {
        // Backslash handling only applies inside string literals.
        assert_eq!(repair_json(r#"[1, 2]"#), r#"[1, 2]"#);
    }

    // --- parse_json_with_repair --------------------------------------------

    #[test]
    fn parse_json_with_repair_only_retries_when_repair_changed_text() {
        // Malformed-but-unrepairable JSON (structure, not string escapes) returns
        // the ORIGINAL parse error: repair leaves it unchanged, so no retry.
        let err = parse_json_with_repair("{not json").unwrap_err();
        let direct = serde_json::from_str::<Value>("{not json").unwrap_err();
        assert_eq!(err.to_string(), direct.to_string());
    }

    #[test]
    fn parse_json_with_repair_recovers_repairable_string() {
        // Repair changes the text (doubles the invalid escape) and the retry
        // succeeds.
        let parsed = parse_json_with_repair(r#"{"a":"\H"}"#).unwrap();
        assert_eq!(parsed, json!({ "a": "\\H" }));
    }

    // --- parse_streaming_json truncation cases -----------------------------
    //
    // Expected values are the ground-truth outputs of `partial-json@0.1.7`
    // (promplate's parser) with its default `Allow.ALL`.

    #[test]
    fn streaming_json_single_key_tool_arg_object() {
        // The conformance single-key tool-argument case must still round-trip.
        assert_eq!(
            parse_streaming_json(Some(r#"{"path":"/tmp/x","content":"hello wor"#)),
            json!({ "path": "/tmp/x", "content": "hello wor" }),
        );
    }

    #[test]
    fn streaming_json_partial_number_drops_incomplete_pair() {
        // `1.` is an incomplete number; partial-json drops the trailing value
        // AND the key it belonged to.
        assert_eq!(parse_streaming_json(Some(r#"{"a":1."#)), json!({}));
        assert_eq!(parse_streaming_json(Some(r#"{"a":-"#)), json!({}));
    }

    #[test]
    fn streaming_json_partial_exponent_drops_to_valid_prefix() {
        // A truncated exponent falls back to the valid numeric prefix at `e`.
        assert_eq!(parse_streaming_json(Some(r#"{"a":1e"#)), json!({ "a": 1 }));
        assert_eq!(
            parse_streaming_json(Some(r#"{"a":1.5e"#)),
            json!({ "a": 1.5 }),
        );
    }

    #[test]
    fn streaming_json_complete_numbers_are_kept() {
        assert_eq!(
            parse_streaming_json(Some(r#"{"a":123.45"#)),
            json!({ "a": 123.45 }),
        );
        assert_eq!(
            parse_streaming_json(Some(r#"{"a":-12"#)),
            json!({ "a": -12 })
        );
    }

    #[test]
    fn streaming_json_partial_literals_complete() {
        // `tr`/`nu`/`fa` prefixes complete to their literals.
        assert_eq!(
            parse_streaming_json(Some(r#"{"a":tr"#)),
            json!({ "a": true })
        );
        assert_eq!(
            parse_streaming_json(Some(r#"{"a":nu"#)),
            json!({ "a": null }),
        );
        assert_eq!(
            parse_streaming_json(Some(r#"{"a":fa"#)),
            json!({ "a": false }),
        );
        assert_eq!(
            parse_streaming_json(Some(r#"{"a":true,"b":fal"#)),
            json!({ "a": true, "b": false }),
        );
    }

    #[test]
    fn streaming_json_truncated_array_element_dropped() {
        assert_eq!(parse_streaming_json(Some("[1,2,3")), json!([1, 2, 3]));
        assert_eq!(parse_streaming_json(Some("[1,2,tr")), json!([1, 2, true]));
        assert_eq!(
            parse_streaming_json(Some(r#"{"items":["x","y"#)),
            json!({ "items": ["x", "y"] }),
        );
    }

    #[test]
    fn streaming_json_dangling_value_after_key_dropped() {
        // A `key:` with no value drops that pair but keeps prior ones.
        assert_eq!(
            parse_streaming_json(Some(r#"{"a":1,"b":"#)),
            json!({ "a": 1 }),
        );
    }

    #[test]
    fn streaming_json_unterminated_string_returns_prefix() {
        assert_eq!(
            parse_streaming_json(Some(r#"{"key":"unterminated"#)),
            json!({ "key": "unterminated" }),
        );
    }

    #[test]
    fn streaming_json_nested_truncation() {
        assert_eq!(
            parse_streaming_json(Some(r#"{"a":{"b":1"#)),
            json!({ "a": { "b": 1 } }),
        );
        assert_eq!(
            parse_streaming_json(Some(r#"{"a":"x","b":[1,2,"#)),
            json!({ "a": "x", "b": [1, 2] }),
        );
        assert_eq!(
            parse_streaming_json(Some(r#"{"nested":{"arr":[1,{"k":"v"#)),
            json!({ "nested": { "arr": [1, { "k": "v" }] } }),
        );
    }

    #[test]
    fn streaming_json_trailing_backslash_in_string_dropped() {
        // The dangling escape is dropped, leaving the string prefix.
        assert_eq!(
            parse_streaming_json(Some("{\"path\":\"abc\",\"text\":\"he\\")),
            json!({ "path": "abc", "text": "he" }),
        );
    }
}
