//! Assistant-message diagnostics, ported from pi-ai's
//! `packages/ai/src/utils/diagnostics.ts` at pinned commit `3da591ab`.
//!
//! These helpers capture a thrown value into a structured, serializable
//! diagnostic that can be attached to an assistant message:
//! [`format_thrown_value`] renders a value to a display string,
//! [`extract_diagnostic_error`] normalizes it into a [`DiagnosticErrorInfo`],
//! [`create_assistant_message_diagnostic`] wraps that with a type tag and
//! timestamp, and [`append_assistant_message_diagnostic`] pushes it onto a
//! message's diagnostics list.
//!
//! # Modelling the JS `unknown` throw
//!
//! pi's functions take `value: unknown` and branch on `instanceof Error` /
//! `typeof value`. Rust models the same split explicitly with [`ThrownValue`]:
//! an [`Error`](ThrownValue::Error) case carrying `name`/`message`/`stack`/`code`
//! (the fields pi reads off an `Error`), a [`Str`](ThrownValue::Str) case, and an
//! [`Other`](ThrownValue::Other) case whose payload is the value's `String(...)`
//! rendering. The output strings and JSON shapes match pi exactly.
//!
//! # Timestamps
//!
//! pi stamps `Date.now()` (milliseconds since the Unix epoch).
//! [`create_assistant_message_diagnostic`] reads the same wall clock via
//! [`std::time::SystemTime`]; [`create_assistant_message_diagnostic_at`] takes an
//! explicit timestamp for deterministic callers and tests.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JS `Error.code`, which is `string | number` (`diagnostics.ts:5`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiagnosticCode {
    Str(String),
    Num(f64),
}

/// Structured error info extracted from a thrown value (`diagnostics.ts:1`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticErrorInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<DiagnosticCode>,
}

/// A diagnostic entry attached to an assistant message (`diagnostics.ts:8`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessageDiagnostic {
    #[serde(rename = "type")]
    pub kind: String,
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DiagnosticErrorInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<BTreeMap<String, Value>>,
}

/// A thrown value, modelling JS `unknown` at the diagnostic boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum ThrownValue {
    /// A thrown `Error` (or subclass), with the fields pi reads.
    Error {
        /// `error.name` (the JS default is `"Error"`).
        name: String,
        /// `error.message`.
        message: String,
        /// `error.stack`, when present.
        stack: Option<String>,
        /// `error.code`, when it is a string or number.
        code: Option<DiagnosticCode>,
    },
    /// A thrown string.
    Str(String),
    /// Any other thrown value, carrying its `String(value)` rendering.
    Other(String),
}

/// Render a thrown value to a display string (`diagnostics.ts:15`):
/// an `Error` → `message` (falling back to `name` when empty); a string → the
/// string; anything else → its `String(value)` form.
pub fn format_thrown_value(value: &ThrownValue) -> String {
    match value {
        ThrownValue::Error { name, message, .. } => {
            if message.is_empty() {
                name.clone()
            } else {
                message.clone()
            }
        }
        ThrownValue::Str(s) => s.clone(),
        ThrownValue::Other(s) => s.clone(),
    }
}

/// Normalize a thrown value into structured error info (`diagnostics.ts:21`).
///
/// Non-`Error` values become `{ name: "ThrownValue", message: format_thrown_value(..) }`.
/// `Error` values keep their `name` (dropped when empty), `message` (falling back
/// to `name`), `stack`, and `code` (only when string/number).
pub fn extract_diagnostic_error(error: &ThrownValue) -> DiagnosticErrorInfo {
    match error {
        ThrownValue::Error {
            name,
            message,
            stack,
            code,
        } => DiagnosticErrorInfo {
            name: if name.is_empty() {
                None
            } else {
                Some(name.clone())
            },
            message: if message.is_empty() {
                name.clone()
            } else {
                message.clone()
            },
            stack: stack.clone(),
            code: code.clone(),
        },
        other => DiagnosticErrorInfo {
            name: Some("ThrownValue".to_string()),
            message: format_thrown_value(other),
            stack: None,
            code: None,
        },
    }
}

/// Build a diagnostic tagged `kind`, stamped with the current wall clock
/// (`diagnostics.ts:32`, pi's `Date.now()`).
pub fn create_assistant_message_diagnostic(
    kind: &str,
    error: &ThrownValue,
    details: Option<BTreeMap<String, Value>>,
) -> AssistantMessageDiagnostic {
    create_assistant_message_diagnostic_at(kind, error, details, now_millis())
}

/// [`create_assistant_message_diagnostic`] with an explicit timestamp, for
/// deterministic callers and tests.
pub fn create_assistant_message_diagnostic_at(
    kind: &str,
    error: &ThrownValue,
    details: Option<BTreeMap<String, Value>>,
    timestamp: i64,
) -> AssistantMessageDiagnostic {
    AssistantMessageDiagnostic {
        kind: kind.to_string(),
        timestamp,
        error: Some(extract_diagnostic_error(error)),
        details,
    }
}

/// Append a diagnostic to a message's diagnostics list, creating the list if it
/// was absent (`diagnostics.ts:40`, pi's `message.diagnostics = [...prev, d]`).
pub fn append_assistant_message_diagnostic(
    diagnostics: &mut Option<Vec<AssistantMessageDiagnostic>>,
    diagnostic: AssistantMessageDiagnostic,
) {
    diagnostics.get_or_insert_with(Vec::new).push(diagnostic);
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn error(name: &str, message: &str) -> ThrownValue {
        ThrownValue::Error {
            name: name.to_string(),
            message: message.to_string(),
            stack: None,
            code: None,
        }
    }

    #[test]
    fn format_error_prefers_message_then_name() {
        assert_eq!(format_thrown_value(&error("TypeError", "boom")), "boom");
        assert_eq!(format_thrown_value(&error("TypeError", "")), "TypeError");
    }

    #[test]
    fn format_string_and_other() {
        assert_eq!(
            format_thrown_value(&ThrownValue::Str("oops".into())),
            "oops"
        );
        assert_eq!(format_thrown_value(&ThrownValue::Other("42".into())), "42");
    }

    #[test]
    fn extract_non_error_uses_thrown_value_name() {
        let info = extract_diagnostic_error(&ThrownValue::Other("boom".into()));
        assert_eq!(info.name.as_deref(), Some("ThrownValue"));
        assert_eq!(info.message, "boom");
        assert_eq!(info.stack, None);
        assert_eq!(info.code, None);
    }

    #[test]
    fn extract_error_drops_empty_name_and_falls_back_message() {
        let info = extract_diagnostic_error(&ThrownValue::Error {
            name: String::new(),
            message: String::new(),
            stack: Some("stack trace".into()),
            code: Some(DiagnosticCode::Str("ENOENT".into())),
        });
        assert_eq!(info.name, None);
        assert_eq!(info.message, ""); // both name and message empty → name (empty)
        assert_eq!(info.stack.as_deref(), Some("stack trace"));
        assert_eq!(info.code, Some(DiagnosticCode::Str("ENOENT".into())));
    }

    #[test]
    fn extract_error_message_falls_back_to_name() {
        let info = extract_diagnostic_error(&error("RangeError", ""));
        assert_eq!(info.name.as_deref(), Some("RangeError"));
        assert_eq!(info.message, "RangeError");
    }

    #[test]
    fn extract_error_keeps_numeric_code() {
        let info = extract_diagnostic_error(&ThrownValue::Error {
            name: "HttpError".into(),
            message: "failed".into(),
            stack: None,
            code: Some(DiagnosticCode::Num(500.0)),
        });
        assert_eq!(info.code, Some(DiagnosticCode::Num(500.0)));
    }

    #[test]
    fn create_sets_type_error_and_timestamp() {
        let mut details = BTreeMap::new();
        details.insert("attempt".to_string(), json!(2));
        let diag = create_assistant_message_diagnostic_at(
            "stream-error",
            &error("TypeError", "boom"),
            Some(details),
            1_700_000_000_000,
        );
        assert_eq!(diag.kind, "stream-error");
        assert_eq!(diag.timestamp, 1_700_000_000_000);
        assert_eq!(diag.error.as_ref().unwrap().message, "boom");
        assert_eq!(
            diag.details.as_ref().unwrap().get("attempt"),
            Some(&json!(2))
        );
    }

    #[test]
    fn create_uses_wall_clock_now() {
        let diag = create_assistant_message_diagnostic("t", &error("E", "m"), None);
        assert!(diag.timestamp > 0);
    }

    #[test]
    fn append_creates_and_extends_list() {
        let mut diagnostics: Option<Vec<AssistantMessageDiagnostic>> = None;
        let d1 = create_assistant_message_diagnostic_at("a", &error("E", "one"), None, 1);
        let d2 = create_assistant_message_diagnostic_at("b", &error("E", "two"), None, 2);
        append_assistant_message_diagnostic(&mut diagnostics, d1);
        append_assistant_message_diagnostic(&mut diagnostics, d2);
        let list = diagnostics.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].kind, "a");
        assert_eq!(list[1].kind, "b");
    }

    #[test]
    fn serializes_with_type_key_and_skips_absent_fields() {
        let diag = create_assistant_message_diagnostic_at("net", &error("E", "m"), None, 5);
        let value = serde_json::to_value(&diag).unwrap();
        assert_eq!(value["type"], json!("net"));
        assert_eq!(value["timestamp"], json!(5));
        assert!(value.get("details").is_none());
        // error info is present with a message and no null stack/code keys.
        assert_eq!(value["error"]["message"], json!("m"));
        assert!(value["error"].get("stack").is_none());
    }
}
