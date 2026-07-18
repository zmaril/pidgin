//! TypeBox schema-builder helpers, ported from pi-ai's
//! `packages/ai/src/utils/typebox-helpers.ts` at pinned commit `3da591ab`.
//!
//! [`string_enum`] builds a string-enum JSON Schema (`{ type: "string", enum:
//! [...] }`) compatible with providers — notably Google — that reject the
//! `anyOf`/`const` union pattern TypeBox emits for `Type.Union([Type.Literal(...)])`.
//!
//! # Parity notes
//!
//! - pi returns `Type.Unsafe<T>({ type: "string", enum, ...description, ...default })`.
//!   `Type.Unsafe` only attaches a non-enumerable `[TypeBox.Kind] = "Unsafe"`
//!   *symbol* to the options object; symbols never survive JSON serialization,
//!   so the observable schema is exactly `{ type, enum, description?, default? }`.
//!   This crate models schemas as [`serde_json::Value`], which carries no
//!   TypeBox Kind, so the produced value reproduces that serialized shape
//!   byte-for-byte.
//! - pi's object spreads `...(options?.description && { description })` and
//!   `...(options?.default && { default })` use JS truthiness: an **empty
//!   string** is falsy and is therefore omitted. This port mirrors that —
//!   `description`/`default` are emitted only when present and non-empty.

use serde_json::{Map, Value};

/// Optional metadata for [`string_enum`] (pi's `{ description?; default? }`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StringEnumOptions {
    /// Human-readable schema description. Omitted from the schema when `None`
    /// or empty (JS falsy-string parity).
    pub description: Option<String>,
    /// Default enum member. Omitted from the schema when `None` or empty.
    pub default: Option<String>,
}

/// Build a string-enum JSON Schema (`typebox-helpers.ts:14`, `StringEnum`).
///
/// Produces `{ "type": "string", "enum": values }`, plus `"description"` and/or
/// `"default"` when supplied and non-empty.
///
/// ```
/// use atilla_ai::utils::typebox_helpers::{string_enum, StringEnumOptions};
/// use serde_json::json;
///
/// let schema = string_enum(&["add", "subtract"], None);
/// assert_eq!(schema, json!({ "type": "string", "enum": ["add", "subtract"] }));
/// ```
pub fn string_enum(values: &[&str], options: Option<&StringEnumOptions>) -> Value {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("string".to_string()));
    map.insert(
        "enum".to_string(),
        Value::Array(
            values
                .iter()
                .map(|v| Value::String((*v).to_string()))
                .collect(),
        ),
    );

    if let Some(options) = options {
        if let Some(description) = &options.description {
            if !description.is_empty() {
                map.insert(
                    "description".to_string(),
                    Value::String(description.clone()),
                );
            }
        }
        if let Some(default) = &options.default {
            if !default.is_empty() {
                map.insert("default".to_string(), Value::String(default.clone()));
            }
        }
    }

    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_bare_string_enum() {
        let schema = string_enum(&["add", "subtract", "multiply", "divide"], None);
        assert_eq!(
            schema,
            json!({ "type": "string", "enum": ["add", "subtract", "multiply", "divide"] })
        );
    }

    #[test]
    fn includes_description_when_present() {
        let options = StringEnumOptions {
            description: Some("The operation to perform".to_string()),
            default: None,
        };
        let schema = string_enum(&["add", "subtract"], Some(&options));
        assert_eq!(
            schema,
            json!({
                "type": "string",
                "enum": ["add", "subtract"],
                "description": "The operation to perform",
            })
        );
    }

    #[test]
    fn includes_default_when_present() {
        let options = StringEnumOptions {
            description: None,
            default: Some("add".to_string()),
        };
        let schema = string_enum(&["add", "subtract"], Some(&options));
        assert_eq!(
            schema,
            json!({ "type": "string", "enum": ["add", "subtract"], "default": "add" })
        );
    }

    #[test]
    fn includes_both_description_and_default() {
        let options = StringEnumOptions {
            description: Some("op".to_string()),
            default: Some("add".to_string()),
        };
        let schema = string_enum(&["add", "subtract"], Some(&options));
        assert_eq!(
            schema,
            json!({
                "type": "string",
                "enum": ["add", "subtract"],
                "description": "op",
                "default": "add",
            })
        );
    }

    #[test]
    fn omits_empty_strings_matching_js_truthiness() {
        // pi spreads `options?.description && {...}`: an empty string is falsy.
        let options = StringEnumOptions {
            description: Some(String::new()),
            default: Some(String::new()),
        };
        let schema = string_enum(&["a"], Some(&options));
        assert_eq!(schema, json!({ "type": "string", "enum": ["a"] }));
    }
}
