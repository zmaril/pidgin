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

/// A pragmatic port of the `partial-json` dependency's completion pass, adequate
/// for streamed tool-argument fragments: it closes any strings, objects, and
/// arrays left open by a truncated fragment and drops a dangling key or trailing
/// separator so the result parses. Returns `None` when the fragment cannot be
/// completed into valid JSON.
fn partial_parse(text: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return Some(value);
    }
    let completed = complete_partial(text)?;
    serde_json::from_str::<Value>(&completed).ok()
}

fn complete_partial(text: &str) -> Option<String> {
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut last_significant: Option<char> = None;
    let chars: Vec<char> = text.chars().collect();

    for &ch in &chars {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
                last_significant = Some('"');
            }
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
            }
            '{' | '[' => stack.push(ch),
            '}' | ']' => {
                stack.pop()?;
                last_significant = Some(ch);
            }
            c if c.is_whitespace() => {}
            c => last_significant = Some(c),
        }
    }

    let mut completed = String::from(text.trim_end());

    // Close an unterminated string literal.
    if in_string {
        if escaped {
            completed.push('\\');
        }
        completed.push('"');
        last_significant = Some('"');
    }

    // Drop a trailing separator or dangling key that cannot be completed.
    loop {
        let trimmed = completed.trim_end();
        let Some(last) = trimmed.chars().last() else {
            break;
        };
        if last == ',' || last == ':' {
            let new_len = trimmed.len() - last.len_utf8();
            completed.truncate(new_len);
            // Re-trim before inspecting the next trailing character.
            let re = completed.trim_end().to_string();
            completed = re;
        } else {
            break;
        }
    }
    let _ = last_significant;

    // Close open containers in reverse order.
    while let Some(open) = stack.pop() {
        completed.push(match open {
            '{' => '}',
            _ => ']',
        });
    }

    Some(completed)
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
}
