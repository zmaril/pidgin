// straitjacket-allow-file:duplication — the primitive-coercion match arms and
// the parallel passing/failing case tables in the tests are a byte-faithful
// transcription of pi's `validation.ts` coercion rules and `validation.test.ts`
// cases; the repeated per-type arms and per-case assertions read as duplicates
// but are the port's load-bearing fidelity surface.
//! Tool-argument validation and JSON-Schema coercion, ported from pi-ai's
//! `packages/ai/src/utils/validation.ts` at pinned commit `3da591ab`.
//!
//! Given a tool's parameter schema and an LLM's raw tool-call arguments,
//! [`validate_tool_arguments`] coerces the arguments toward the schema (numbers
//! from strings, booleans from `"true"`/`"false"`, etc.) and then validates
//! them, returning the coerced value or a formatted error. [`validate_tool_call`]
//! resolves the tool by name first.
//!
//! # Schema representation
//!
//! pi's schemas are TypeBox `TSchema` objects — JSON-Schema objects carrying a
//! hidden `[TypeBox.Kind]` symbol. This crate models them as
//! [`serde_json::Value`], which is exactly the JSON-Schema projection minus that
//! symbol. That is the only representation available in Rust and it is
//! sufficient: every schema feature these utils inspect (`type`, `properties`,
//! `required`, `items`, `additionalProperties`, `enum`, `allOf`/`anyOf`/`oneOf`,
//! type-arrays) lives in the JSON projection.
//!
//! # Parity notes
//!
//! - pi's coercion path has two branches gated on the TypeBox Kind symbol:
//!   TypeBox schemas run only `Value.Convert`; plain serialized JSON schemas
//!   additionally run the hand-rolled `coerceWithJsonSchema`. Rust `Value`s carry
//!   no Kind, so this port always runs the hand-rolled coercion. For the shapes
//!   these utils see, `coerce_with_json_schema` reproduces the same observable
//!   result `Value.Convert` would have produced (e.g. `{count:"42"}` →
//!   `{count:42}` for a `Type.Number()` field), so the branch collapse is
//!   behaviour-preserving.
//! - pi validates with TypeBox's compiled `Value.Check`; this port hand-rolls
//!   [`check`] over the same JSON-Schema subset. The pass/fail decision matches;
//!   see the divergence note below on error-message text.
//! - **Error-message divergence (intentional):** pi's per-error strings (e.g.
//!   `"Expected number"`) come from TypeBox's localized error generator, which is
//!   not reproduced here. The port matches pi's *envelope* byte-for-byte —
//!   `Validation failed for tool "<name>":\n<lines>\n\nReceived arguments:\n<json>`
//!   with each line `  - <path>: <message>` — and produces its own descriptive
//!   per-error `<message>`. pi's own tests assert only the substring
//!   `"Validation failed"` (never a per-error body), so this divergence is
//!   invisible to the ported contract. Paths are formatted exactly as pi's
//!   `formatValidationPath` (dotted instance path, `root` at the top, and
//!   `<base>.<prop>` for a missing required property).
//! - JS `Number(string)`/`String(number)` semantics are approximated with Rust
//!   `f64` parsing/formatting. They agree for the decimal payloads tool calls
//!   carry; exotic forms (hex literals, `1e21`-style exponentials) are not
//!   reproduced and are not exercised by the ported tests.

use std::collections::HashSet;

use serde_json::{Map, Value};

/// A tool definition (pi's `Tool`, `types.ts:444`). Only the fields
/// [`validate_tool_arguments`] reads are modelled; `description` is retained for
/// shape fidelity. `parameters` is the TypeBox/JSON-Schema object as a [`Value`].
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    /// The tool name matched against a [`ToolCall::name`].
    pub name: String,
    /// Human-readable description (unused by validation; kept for fidelity).
    pub description: String,
    /// The parameter schema (a JSON-Schema object).
    pub parameters: Value,
}

/// A tool invocation from the model (pi's `ToolCall`, `types.ts:349`). Only the
/// fields validation reads are modelled.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// The invoked tool's name.
    pub name: String,
    /// The raw arguments object emitted by the model.
    pub arguments: Value,
}

/// pi's `getSchemaTypes` (`validation.ts:19`): the `type` keyword normalized to a
/// list — `[t]` for a string, the string members for an array, else empty.
fn get_schema_types(schema: &Value) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|t| t.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// pi's `matchesJsonType` (`validation.ts:29`): does `value` satisfy JSON-Schema
/// primitive `type`? Mirrors pi's `typeof`/`Number.isInteger`/`Array.isArray`
/// checks.
fn matches_json_type(value: &Value, type_name: &str) -> bool {
    match type_name {
        "number" => value.is_number(),
        "integer" => is_integer(value),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

/// JS `typeof value === "number" && Number.isInteger(value)`.
fn is_integer(value: &Value) -> bool {
    match value {
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                true
            } else {
                n.as_f64().map(is_whole).unwrap_or(false)
            }
        }
        _ => false,
    }
}

// JS parity requires exact float comparison against integral / 0 / 1 sentinels,
// mirroring `Number.isInteger` and `value === 0/1`; approximate comparison would
// change the coercion decisions.
#[allow(clippy::float_cmp)]
fn is_whole(n: f64) -> bool {
    n.is_finite() && n.fract() == 0.0
}

#[allow(clippy::float_cmp)]
fn f64_eq(n: f64, target: f64) -> bool {
    n == target
}

/// JS `Number(string)`: parse a trimmed decimal to `f64`, `None` when the parse
/// fails (JS `NaN`). See the module parity note on unsupported exotic forms.
fn js_string_to_number(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

/// A parsed number as a [`Value`], preferring an integer representation when the
/// value is integral so results compare equal to JSON integer literals.
fn number_value(n: f64) -> Value {
    if is_whole(n) && n >= i64::MIN as f64 && n <= i64::MAX as f64 {
        Value::from(n as i64)
    } else {
        serde_json::Number::from_f64(n)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

/// JS `String(number)` for the number→string coercion. Best-effort; see the
/// module parity note on exotic forms.
fn js_number_to_string(value: &Value) -> String {
    match value {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                n.as_f64().map(|f| f.to_string()).unwrap_or_default()
            }
        }
        _ => String::new(),
    }
}

/// pi's `coercePrimitiveByType` (`validation.ts:58`): AJV-compatible primitive
/// coercion. Returns a *new* value when it changes, else the input unchanged.
fn coerce_primitive_by_type(value: &Value, type_name: &str) -> Value {
    match type_name {
        "number" => {
            if value.is_null() {
                return Value::from(0);
            }
            if let Some(s) = value.as_str() {
                if !s.trim().is_empty() {
                    if let Some(parsed) = js_string_to_number(s) {
                        if parsed.is_finite() {
                            return number_value(parsed);
                        }
                    }
                }
            }
            if let Some(b) = value.as_bool() {
                return Value::from(if b { 1 } else { 0 });
            }
            value.clone()
        }
        "integer" => {
            if value.is_null() {
                return Value::from(0);
            }
            if let Some(s) = value.as_str() {
                if !s.trim().is_empty() {
                    if let Some(parsed) = js_string_to_number(s) {
                        if is_whole(parsed) {
                            return number_value(parsed);
                        }
                    }
                }
            }
            if let Some(b) = value.as_bool() {
                return Value::from(if b { 1 } else { 0 });
            }
            value.clone()
        }
        "boolean" => {
            if value.is_null() {
                return Value::from(false);
            }
            if let Some(s) = value.as_str() {
                if s == "true" {
                    return Value::from(true);
                }
                if s == "false" {
                    return Value::from(false);
                }
            }
            if value.is_number() {
                if let Some(n) = value.as_f64() {
                    if f64_eq(n, 1.0) {
                        return Value::from(true);
                    }
                    if f64_eq(n, 0.0) {
                        return Value::from(false);
                    }
                }
            }
            value.clone()
        }
        "string" => {
            if value.is_null() {
                return Value::from("");
            }
            match value {
                Value::Number(_) => Value::String(js_number_to_string(value)),
                Value::Bool(b) => Value::String(b.to_string()),
                _ => value.clone(),
            }
        }
        "null" => {
            let is_empty_string = value.as_str() == Some("");
            let is_zero =
                value.is_number() && value.as_f64().map(|n| f64_eq(n, 0.0)).unwrap_or(false);
            let is_false = value.as_bool() == Some(false);
            if is_empty_string || is_zero || is_false {
                Value::Null
            } else {
                value.clone()
            }
        }
        _ => value.clone(),
    }
}

/// pi's `applySchemaObjectCoercion` (`validation.ts:132`): recurse into declared
/// `properties` present on the value, then into extra keys when
/// `additionalProperties` is itself a schema.
fn apply_schema_object_coercion(mut map: Map<String, Value>, schema: &Value) -> Map<String, Value> {
    let properties = schema.get("properties").and_then(Value::as_object);
    let defined_keys: HashSet<String> = properties
        .map(|p| p.keys().cloned().collect())
        .unwrap_or_default();

    if let Some(properties) = properties {
        for (key, property_schema) in properties {
            if let Some(current) = map.get(key) {
                let coerced = coerce_with_json_schema(current, property_schema);
                map.insert(key.clone(), coerced);
            }
        }
    }

    if let Some(additional) = schema.get("additionalProperties") {
        if additional.is_object() {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if defined_keys.contains(&key) {
                    continue;
                }
                if let Some(current) = map.get(&key) {
                    let coerced = coerce_with_json_schema(current, additional);
                    map.insert(key, coerced);
                }
            }
        }
    }

    map
}

/// pi's `applySchemaArrayCoercion` (`validation.ts:155`): tuple `items` coerce
/// positionally; a single `items` schema coerces every element.
fn apply_schema_array_coercion(mut items: Vec<Value>, schema: &Value) -> Vec<Value> {
    match schema.get("items") {
        Some(Value::Array(item_schemas)) => {
            for (index, slot) in items.iter_mut().enumerate() {
                if let Some(item_schema) = item_schemas.get(index) {
                    *slot = coerce_with_json_schema(slot, item_schema);
                }
            }
        }
        Some(item_schema) if item_schema.is_object() => {
            for slot in items.iter_mut() {
                *slot = coerce_with_json_schema(slot, item_schema);
            }
        }
        _ => {}
    }
    items
}

/// pi's `coerceWithUnionSchema` (`validation.ts:174`): coerce a clone of the value
/// against each union member and return the first coercion that validates, else
/// the original value.
fn coerce_with_union_schema(value: &Value, schemas: &[Value]) -> Value {
    for schema in schemas {
        let coerced = coerce_with_json_schema(value, schema);
        if check(&coerced, schema) {
            return coerced;
        }
    }
    value.clone()
}

/// pi's `coerceWithJsonSchema` (`validation.ts:186`): the recursive coercion
/// driver over `allOf`/`anyOf`/`oneOf`, primitive types, objects, and arrays.
fn coerce_with_json_schema(value: &Value, schema: &Value) -> Value {
    let mut next = value.clone();

    if let Some(all_of) = schema.get("allOf").and_then(Value::as_array) {
        for nested in all_of {
            next = coerce_with_json_schema(&next, nested);
        }
    }
    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
        next = coerce_with_union_schema(&next, any_of);
    }
    if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array) {
        next = coerce_with_union_schema(&next, one_of);
    }

    let schema_types = get_schema_types(schema);
    let matches_union_member = schema_types.len() > 1
        && schema_types
            .iter()
            .any(|schema_type| matches_json_type(&next, schema_type));
    if !schema_types.is_empty() && !matches_union_member {
        for schema_type in &schema_types {
            let candidate = coerce_primitive_by_type(&next, schema_type);
            if candidate != next {
                next = candidate;
                break;
            }
        }
    }

    if schema_types.iter().any(|t| t == "object") {
        if let Value::Object(map) = next {
            next = Value::Object(apply_schema_object_coercion(map, schema));
        } else {
            return next;
        }
    }

    if schema_types.iter().any(|t| t == "array") {
        if let Value::Array(items) = next {
            next = Value::Array(apply_schema_array_coercion(items, schema));
        }
    }

    next
}

/// Hand-rolled replacement for pi's compiled TypeBox `Value.Check` over the
/// JSON-Schema subset these utils use: `type` (string or array), `enum`,
/// `const`, `properties`, `required`, `additionalProperties`, `items` (tuple or
/// single), and `allOf`/`anyOf`/`oneOf`.
fn check(value: &Value, schema: &Value) -> bool {
    if let Some(all_of) = schema.get("allOf").and_then(Value::as_array) {
        if !all_of.iter().all(|s| check(value, s)) {
            return false;
        }
    }
    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
        if !any_of.iter().any(|s| check(value, s)) {
            return false;
        }
    }
    if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array) {
        if one_of.iter().filter(|s| check(value, s)).count() != 1 {
            return false;
        }
    }

    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array) {
        if !enum_values.iter().any(|e| e == value) {
            return false;
        }
    }
    if let Some(constant) = schema.get("const") {
        if constant != value {
            return false;
        }
    }

    let schema_types = get_schema_types(schema);
    if !schema_types.is_empty() && !schema_types.iter().any(|t| matches_json_type(value, t)) {
        return false;
    }

    if schema_types.iter().any(|t| t == "object") {
        if let Value::Object(map) = value {
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for entry in required {
                    if let Some(key) = entry.as_str() {
                        if !map.contains_key(key) {
                            return false;
                        }
                    }
                }
            }
            let properties = schema.get("properties").and_then(Value::as_object);
            if let Some(properties) = properties {
                for (key, property_schema) in properties {
                    if let Some(current) = map.get(key) {
                        if !check(current, property_schema) {
                            return false;
                        }
                    }
                }
            }
            match schema.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    for key in map.keys() {
                        let known = properties.map(|p| p.contains_key(key)).unwrap_or(false);
                        if !known {
                            return false;
                        }
                    }
                }
                Some(additional) if additional.is_object() => {
                    for (key, current) in map {
                        let known = properties.map(|p| p.contains_key(key)).unwrap_or(false);
                        if !known && !check(current, additional) {
                            return false;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if schema_types.iter().any(|t| t == "array") {
        if let Value::Array(items) = value {
            match schema.get("items") {
                Some(Value::Array(item_schemas)) => {
                    for (index, item) in items.iter().enumerate() {
                        if let Some(item_schema) = item_schemas.get(index) {
                            if !check(item, item_schema) {
                                return false;
                            }
                        }
                    }
                }
                Some(item_schema) if item_schema.is_object() => {
                    for item in items {
                        if !check(item, item_schema) {
                            return false;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    true
}

/// A single validation error: a formatted path and a human-readable message.
struct ValidationError {
    path: String,
    message: String,
}

/// `root` for the top level, else the dotted path (pi's `formatValidationPath`
/// tail, `validation.ts:243`).
fn normalize_path(path: &str) -> String {
    if path.is_empty() {
        "root".to_string()
    } else {
        path.to_string()
    }
}

fn join_path(base: &str, segment: &str) -> String {
    if base.is_empty() {
        segment.to_string()
    } else {
        format!("{base}.{segment}")
    }
}

/// Collect validation errors mirroring [`check`]'s decisions, with pi-compatible
/// paths. Messages are this port's own descriptive text (see the module parity
/// note on error-message divergence).
fn collect_errors(value: &Value, schema: &Value, path: &str, out: &mut Vec<ValidationError>) {
    let schema_types = get_schema_types(schema);
    if !schema_types.is_empty() && !schema_types.iter().any(|t| matches_json_type(value, t)) {
        out.push(ValidationError {
            path: normalize_path(path),
            message: format!("Expected {}", schema_types.join(" | ")),
        });
        return;
    }

    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array) {
        if !enum_values.iter().any(|e| e == value) {
            out.push(ValidationError {
                path: normalize_path(path),
                message: "Expected value to match one of the enum values".to_string(),
            });
        }
    }

    if schema_types.iter().any(|t| t == "object") {
        if let Value::Object(map) = value {
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for entry in required {
                    if let Some(key) = entry.as_str() {
                        if !map.contains_key(key) {
                            out.push(ValidationError {
                                path: join_path(path, key),
                                message: "Expected required property".to_string(),
                            });
                        }
                    }
                }
            }
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (key, property_schema) in properties {
                    if let Some(current) = map.get(key) {
                        collect_errors(current, property_schema, &join_path(path, key), out);
                    }
                }
            }
            if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                let properties = schema.get("properties").and_then(Value::as_object);
                for key in map.keys() {
                    let known = properties.map(|p| p.contains_key(key)).unwrap_or(false);
                    if !known {
                        out.push(ValidationError {
                            path: join_path(path, key),
                            message: "Unexpected property".to_string(),
                        });
                    }
                }
            }
        }
    }

    if schema_types.iter().any(|t| t == "array") {
        if let Value::Array(items) = value {
            match schema.get("items") {
                Some(Value::Array(item_schemas)) => {
                    for (index, item) in items.iter().enumerate() {
                        if let Some(item_schema) = item_schemas.get(index) {
                            collect_errors(
                                item,
                                item_schema,
                                &join_path(path, &index.to_string()),
                                out,
                            );
                        }
                    }
                }
                Some(item_schema) if item_schema.is_object() => {
                    for (index, item) in items.iter().enumerate() {
                        collect_errors(
                            item,
                            item_schema,
                            &join_path(path, &index.to_string()),
                            out,
                        );
                    }
                }
                _ => {}
            }
        }
    }
}

/// Resolve a tool by name, then validate the call's arguments against it
/// (`validation.ts:263`, `validateToolCall`).
///
/// Returns the validated (and possibly coerced) arguments, or an error message.
/// `Err` carries pi's thrown `Error` message verbatim: `Tool "<name>" not found`
/// when unresolved, else [`validate_tool_arguments`]'s envelope.
pub fn validate_tool_call(tools: &[Tool], tool_call: &ToolCall) -> Result<Value, String> {
    match tools.iter().find(|t| t.name == tool_call.name) {
        Some(tool) => validate_tool_arguments(tool, tool_call),
        None => Err(format!("Tool \"{}\" not found", tool_call.name)),
    }
}

/// Validate a tool call's arguments against the tool's schema
/// (`validation.ts:278`, `validateToolArguments`).
///
/// Coerces the arguments toward the schema and returns them on success. On
/// failure returns pi's error envelope:
/// `Validation failed for tool "<name>":\n<lines>\n\nReceived arguments:\n<json>`.
pub fn validate_tool_arguments(tool: &Tool, tool_call: &ToolCall) -> Result<Value, String> {
    let args = tool_call.arguments.clone();
    let coerced = coerce_with_json_schema(&args, &tool.parameters);

    // Mirror pi's merge/short-circuit block (`validation.ts:284`): objects adopt
    // the coerced result and fall through to the final check; a differing
    // non-object result short-circuits (returning the original args when the
    // coercion itself does not validate — pi never throws on that path).
    let final_args = if coerced != args {
        if args.is_object() && coerced.is_object() {
            coerced
        } else if check(&coerced, &tool.parameters) {
            return Ok(coerced);
        } else {
            return Ok(args);
        }
    } else {
        args
    };

    if check(&final_args, &tool.parameters) {
        return Ok(final_args);
    }

    let mut errors = Vec::new();
    collect_errors(&final_args, &tool.parameters, "", &mut errors);
    let body = if errors.is_empty() {
        "Unknown validation error".to_string()
    } else {
        errors
            .iter()
            .map(|e| format!("  - {}: {}", e.path, e.message))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let received = serde_json::to_string_pretty(&tool_call.arguments).unwrap_or_default();

    Err(format!(
        "Validation failed for tool \"{}\":\n{body}\n\nReceived arguments:\n{received}",
        tool_call.name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn echo_tool(value_schema: Value) -> Tool {
        Tool {
            name: "echo".to_string(),
            description: "Echo tool".to_string(),
            parameters: json!({
                "type": "object",
                "properties": { "value": value_schema },
                "required": ["value"],
            }),
        }
    }

    fn echo_call(value: Value) -> ToolCall {
        ToolCall {
            name: "echo".to_string(),
            arguments: json!({ "value": value }),
        }
    }

    // Port of validation.test.ts "still validates when Function constructor is
    // unavailable". The JS test disables `globalThis.Function` to force TypeBox's
    // non-eval validation path; that runtime concern does not exist in Rust, so
    // the port asserts the observable behaviour it guards: a string coerces to a
    // number for a `Type.Number()` field.
    #[test]
    fn coerces_string_number_field() {
        let tool = Tool {
            name: "echo".to_string(),
            description: "Echo tool".to_string(),
            parameters: json!({
                "type": "object",
                "properties": { "count": { "type": "number" } },
                "required": ["count"],
            }),
        };
        let call = ToolCall {
            name: "echo".to_string(),
            arguments: json!({ "count": "42" }),
        };
        assert_eq!(
            validate_tool_arguments(&tool, &call).unwrap(),
            json!({ "count": 42 })
        );
    }

    // Port of validation.test.ts "coerces serialized plain JSON schemas with
    // AJV-compatible primitive rules".
    #[test]
    fn coerces_plain_json_schemas() {
        let passing: Vec<(Value, Value, Value)> = vec![
            (json!({ "type": "number" }), json!("42"), json!(42)),
            (json!({ "type": "number" }), json!(true), json!(1)),
            (json!({ "type": "number" }), json!(null), json!(0)),
            (json!({ "type": "integer" }), json!("42"), json!(42)),
            (json!({ "type": "boolean" }), json!("true"), json!(true)),
            (json!({ "type": "boolean" }), json!("false"), json!(false)),
            (json!({ "type": "boolean" }), json!(1), json!(true)),
            (json!({ "type": "boolean" }), json!(0), json!(false)),
            (json!({ "type": "string" }), json!(null), json!("")),
            (json!({ "type": "string" }), json!(true), json!("true")),
            (json!({ "type": "null" }), json!(""), json!(null)),
            (json!({ "type": "null" }), json!(0), json!(null)),
            (json!({ "type": "null" }), json!(false), json!(null)),
            (
                json!({ "type": ["number", "string"] }),
                json!("1"),
                json!("1"),
            ),
            (
                json!({ "type": ["boolean", "number"] }),
                json!("1"),
                json!(1),
            ),
        ];

        for (schema, input, expected) in passing {
            let tool = echo_tool(schema.clone());
            let call = echo_call(input.clone());
            assert_eq!(
                validate_tool_arguments(&tool, &call).unwrap(),
                json!({ "value": expected }),
                "schema {schema}, input {input}",
            );
        }
    }

    // Port of validation.test.ts "rejects invalid coercions for serialized plain
    // JSON schemas".
    #[test]
    fn rejects_invalid_coercions() {
        let failing: Vec<(Value, Value)> = vec![
            (json!({ "type": "boolean" }), json!("1")),
            (json!({ "type": "boolean" }), json!("0")),
            (json!({ "type": "null" }), json!("null")),
            (json!({ "type": "integer" }), json!("42.1")),
        ];

        for (schema, input) in failing {
            let tool = echo_tool(schema.clone());
            let call = echo_call(input.clone());
            let err = validate_tool_arguments(&tool, &call).unwrap_err();
            assert!(
                err.contains("Validation failed"),
                "schema {schema}, input {input}: {err}",
            );
        }
    }

    // Additive (no pi test): the error envelope and formatted path/received body.
    #[test]
    fn error_envelope_matches_pi_shape() {
        let tool = echo_tool(json!({ "type": "boolean" }));
        let call = echo_call(json!("1"));
        let err = validate_tool_arguments(&tool, &call).unwrap_err();
        assert_eq!(
            err,
            "Validation failed for tool \"echo\":\n  - value: Expected boolean\n\nReceived arguments:\n{\n  \"value\": \"1\"\n}"
        );
    }

    // Additive (no pi test): missing required property path formatting.
    #[test]
    fn missing_required_property_path() {
        let tool = echo_tool(json!({ "type": "string" }));
        let call = ToolCall {
            name: "echo".to_string(),
            arguments: json!({}),
        };
        let err = validate_tool_arguments(&tool, &call).unwrap_err();
        assert!(
            err.contains("  - value: Expected required property"),
            "{err}"
        );
    }

    // Additive (no pi test): validate_tool_call resolves by name and throws pi's
    // not-found message.
    #[test]
    fn tool_call_not_found() {
        let tools = vec![echo_tool(json!({ "type": "string" }))];
        let call = ToolCall {
            name: "missing".to_string(),
            arguments: json!({ "value": "x" }),
        };
        assert_eq!(
            validate_tool_call(&tools, &call).unwrap_err(),
            "Tool \"missing\" not found"
        );
    }

    // Additive (no pi test): validate_tool_call delegates to argument validation
    // on a resolved tool.
    #[test]
    fn tool_call_resolves_and_validates() {
        let tools = vec![echo_tool(json!({ "type": "number" }))];
        let call = echo_call(json!("42"));
        assert_eq!(
            validate_tool_call(&tools, &call).unwrap(),
            json!({ "value": 42 })
        );
    }

    // Additive (no pi test): a StringEnum-produced schema validates members and
    // rejects non-members, exercising the `enum` keyword in `check`.
    #[test]
    fn string_enum_schema_validation() {
        let schema = crate::utils::typebox_helpers::string_enum(&["add", "subtract"], None);
        let tool = echo_tool(schema);

        assert_eq!(
            validate_tool_arguments(&tool, &echo_call(json!("add"))).unwrap(),
            json!({ "value": "add" })
        );
        assert!(validate_tool_arguments(&tool, &echo_call(json!("divide")))
            .unwrap_err()
            .contains("Validation failed"));
    }

    // Additive (no pi test): nested object + array coercion recurses through
    // `properties` and `items`.
    #[test]
    fn nested_object_and_array_coercion() {
        let tool = Tool {
            name: "echo".to_string(),
            description: "Echo tool".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "nested": {
                        "type": "object",
                        "properties": { "count": { "type": "number" } },
                    },
                    "list": {
                        "type": "array",
                        "items": { "type": "boolean" },
                    },
                },
                "required": ["nested", "list"],
            }),
        };
        let call = ToolCall {
            name: "echo".to_string(),
            arguments: json!({ "nested": { "count": "7" }, "list": ["true", "false"] }),
        };
        assert_eq!(
            validate_tool_arguments(&tool, &call).unwrap(),
            json!({ "nested": { "count": 7 }, "list": [true, false] })
        );
    }
}
