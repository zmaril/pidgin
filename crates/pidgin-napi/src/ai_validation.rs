//! Node-API surface for tool-argument validation and JSON-Schema coercion.
//!
//! This exposes the Rust port of pi's `packages/ai/src/utils/validation.ts`
//! (`validateToolArguments`, `validateToolCall`, ported in
//! [`pidgin_ai::utils::validation`]) to pi's `packages/ai` `validation.test.ts`.
//! The Rust module owns every decision: the AJV-compatible primitive-coercion
//! rules (`coercePrimitiveByType`), the recursive `allOf`/`anyOf`/`oneOf`,
//! object, and array coercion driver, the pass/fail check over the JSON-Schema
//! subset, and the thrown `Validation failed for tool "<name>": …` envelope.
//!
//! # The seam: a `Tool` / `ToolCall` crosses as JSON
//!
//! pi's `validateToolArguments(tool, toolCall)` reads two plain,
//! fully-serializable values — no closures, streams, or live object identity.
//! The tool's `parameters` is a TypeBox `TSchema`, which is a JSON-Schema object
//! carrying a hidden `[TypeBox.Kind]` symbol; `JSON.stringify` drops that symbol,
//! leaving exactly the JSON-Schema projection the port models as
//! [`serde_json::Value`]. So the shim marshals both values honestly with
//! `JSON.stringify` and this layer deserializes them before delegating.
//!
//! The symbol drop is not a loss of fidelity: pi's coercion path has two
//! branches gated on that symbol — TypeBox schemas run only `Value.Convert`,
//! plain serialized schemas additionally run the hand-rolled
//! `coerceWithJsonSchema`. For the shapes these utils see, the hand-rolled
//! coercion reproduces the same observable result `Value.Convert` would have
//! produced (e.g. `{count:"42"}` → `{count:42}` for a `Type.Number()` field), so
//! running it unconditionally is behaviour-preserving. See the parity notes in
//! [`pidgin_ai::utils::validation`].
//!
//! # Marshaling
//!
//! Everything crosses as JSON strings. Each validate fn takes its inputs as JSON
//! and returns a discriminated outcome envelope as JSON:
//! `{"ok":true,"value":<coerced>}` on success, or `{"ok":false,"error":<msg>}`
//! carrying pi's thrown-`Error` message verbatim (`Validation failed for tool
//! "<name>": …`, or `Tool "<name>" not found` for an unresolved call). The shim
//! parses the envelope, returns `value` on success, and re-throws `new
//! Error(error)` on failure so pi's `.toThrow("Validation failed")` contract
//! holds.

use napi_derive::napi;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use pidgin_ai::utils::validation::{validate_tool_arguments, validate_tool_call, Tool, ToolCall};

/// The marshaled `Tool`: `parameters` is the TypeBox schema's JSON-Schema
/// projection (the `[TypeBox.Kind]` symbol is dropped by `JSON.stringify`).
/// `description` defaults to empty for shape tolerance; validation never reads
/// it.
#[derive(Deserialize)]
struct ToolIn {
    name: String,
    #[serde(default)]
    description: String,
    parameters: Value,
}

impl From<ToolIn> for Tool {
    fn from(t: ToolIn) -> Self {
        Tool {
            name: t.name,
            description: t.description,
            parameters: t.parameters,
        }
    }
}

/// The marshaled `ToolCall`. Only `name` and `arguments` are read; pi's extra
/// `type`/`id`/`thoughtSignature` fields are ignored. Absent `arguments`
/// deserializes to JSON `null`.
#[derive(Deserialize)]
struct ToolCallIn {
    name: String,
    #[serde(default)]
    arguments: Value,
}

impl From<ToolCallIn> for ToolCall {
    fn from(c: ToolCallIn) -> Self {
        ToolCall {
            name: c.name,
            arguments: c.arguments,
        }
    }
}

/// The discriminated outcome handed back to the shim. `ok:true` carries the
/// coerced `value`; `ok:false` carries pi's thrown-`Error` message so the shim
/// can re-throw it.
#[derive(Serialize)]
struct Outcome {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Outcome {
    fn from_result(result: Result<Value, String>) -> Self {
        match result {
            Ok(value) => Outcome {
                ok: true,
                value: Some(value),
                error: None,
            },
            Err(error) => Outcome {
                ok: false,
                value: None,
                error: Some(error),
            },
        }
    }

    fn into_json(self) -> napi::Result<String> {
        serde_json::to_string(&self)
            .map_err(|err| napi::Error::from_reason(format!("serialize outcome: {err}")))
    }
}

/// pi's `validateToolArguments`. Takes the JSON-stringified `tool` and
/// `toolCall`, coerces and validates the arguments against the tool's schema in
/// Rust, and returns the discriminated outcome envelope as JSON. On failure the
/// envelope carries pi's `Validation failed for tool "<name>": …` message.
#[napi(js_name = "validateToolArguments")]
pub fn validate_tool_arguments_napi(
    tool_json: String,
    tool_call_json: String,
) -> napi::Result<String> {
    let tool: ToolIn = serde_json::from_str(&tool_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid tool: {err}")))?;
    let tool_call: ToolCallIn = serde_json::from_str(&tool_call_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid tool call: {err}")))?;

    let result = validate_tool_arguments(&tool.into(), &tool_call.into());
    Outcome::from_result(result).into_json()
}

/// pi's `validateToolCall`. Takes a JSON-stringified `tools` array plus the
/// `toolCall`, resolves the tool by name, and delegates to
/// [`validate_tool_arguments_napi`]'s logic in Rust. Returns the discriminated
/// outcome envelope as JSON; an unresolved tool yields pi's
/// `Tool "<name>" not found` message.
#[napi(js_name = "validateToolCall")]
pub fn validate_tool_call_napi(tools_json: String, tool_call_json: String) -> napi::Result<String> {
    let tools: Vec<ToolIn> = serde_json::from_str(&tools_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid tools: {err}")))?;
    let tool_call: ToolCallIn = serde_json::from_str(&tool_call_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid tool call: {err}")))?;

    let tools: Vec<Tool> = tools.into_iter().map(Tool::from).collect();
    let result = validate_tool_call(&tools, &tool_call.into());
    Outcome::from_result(result).into_json()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn echo_tool_json(value_schema: Value) -> String {
        json!({
            "name": "echo",
            "description": "Echo tool",
            "parameters": {
                "type": "object",
                "properties": { "value": value_schema },
                "required": ["value"],
            },
        })
        .to_string()
    }

    fn echo_call_json(value: Value) -> String {
        json!({ "type": "toolCall", "id": "tool-1", "name": "echo", "arguments": { "value": value } })
            .to_string()
    }

    /// Round-trips pi's "coerces serialized plain JSON schemas" passing case:
    /// `"42"` under `{type:"number"}` coerces to `42`, wrapped in `{value}`.
    #[test]
    fn arguments_coerce_across_boundary() {
        let out = validate_tool_arguments_napi(
            echo_tool_json(json!({ "type": "number" })),
            echo_call_json(json!("42")),
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["ok"], json!(true));
        assert_eq!(parsed["value"], json!({ "value": 42 }));
    }

    /// Round-trips pi's TypeBox-schema case: after `JSON.stringify` drops the
    /// `[TypeBox.Kind]` symbol, `{count:"42"}` under a plain `number` field
    /// coerces to `{count:42}`.
    #[test]
    fn typebox_projected_schema_coerces() {
        let tool = json!({
            "name": "echo",
            "description": "Echo tool",
            "parameters": {
                "type": "object",
                "properties": { "count": { "type": "number" } },
                "required": ["count"],
            },
        })
        .to_string();
        let call =
            json!({ "type": "toolCall", "id": "tool-1", "name": "echo", "arguments": { "count": "42" } })
                .to_string();
        let out = validate_tool_arguments_napi(tool, call).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["value"], json!({ "count": 42 }));
    }

    /// Round-trips pi's "rejects invalid coercions" failing case: `"1"` under
    /// `{type:"boolean"}` yields `ok:false` with the `Validation failed` envelope.
    #[test]
    fn invalid_coercion_yields_failure_envelope() {
        let out = validate_tool_arguments_napi(
            echo_tool_json(json!({ "type": "boolean" })),
            echo_call_json(json!("1")),
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["ok"], json!(false));
        assert!(parsed["error"]
            .as_str()
            .unwrap()
            .contains("Validation failed"));
    }

    /// `validateToolCall` resolves by name and carries pi's not-found message
    /// when the tool is absent.
    #[test]
    fn tool_call_not_found_envelope() {
        let tools = format!("[{}]", echo_tool_json(json!({ "type": "string" })));
        let call = json!({ "type": "toolCall", "id": "x", "name": "missing", "arguments": { "value": "x" } })
            .to_string();
        let out = validate_tool_call_napi(tools, call).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["ok"], json!(false));
        assert_eq!(parsed["error"], json!("Tool \"missing\" not found"));
    }

    /// `validateToolCall` delegates to argument validation on a resolved tool.
    #[test]
    fn tool_call_resolves_and_coerces() {
        let tools = format!("[{}]", echo_tool_json(json!({ "type": "number" })));
        let call = echo_call_json(json!("42"));
        let out = validate_tool_call_napi(tools, call).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["value"], json!({ "value": 42 }));
    }
}
