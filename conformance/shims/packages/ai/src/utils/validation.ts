// Native shim for packages/ai/src/utils/validation.ts, backed by the pidgin Rust
// addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is preserved
// alongside as `validation.__pi_original__.ts` and this shim takes its place, so
// pi's tests import `../src/utils/validation.ts` unchanged and hit Rust.
//
// Scope of the native flip: pi's tool-argument validation and JSON-Schema
// coercion is ported in `crates/pidgin-ai` (`utils/validation.rs`). Every
// decision runs in Rust — the AJV-compatible primitive-coercion rules
// (`coercePrimitiveByType`), the recursive `allOf`/`anyOf`/`oneOf`, object, and
// array coercion driver, the pass/fail check over the JSON-Schema subset, and the
// thrown `Validation failed for tool "<name>": …` envelope. Both
// `validateToolArguments` and `validateToolCall` are overridden to delegate.
//
// The flip boundary: pi's `validateToolArguments(tool, toolCall)` reads two
// plain, fully-serializable values — no closures, streams, or live object
// identity. The tool's `parameters` is a TypeBox `TSchema`, a JSON-Schema object
// carrying a hidden `[TypeBox.Kind]` symbol; `JSON.stringify` drops that symbol,
// leaving exactly the JSON-Schema projection the Rust port models. So the shim
// marshals both values honestly with `JSON.stringify` and delegates. The symbol
// drop is not a loss of fidelity: pi runs the hand-rolled `coerceWithJsonSchema`
// for plain serialized schemas and `Value.Convert` for TypeBox schemas, and for
// the shapes these utils see the two agree (e.g. `{count:"42"}` → `{count:42}`
// for a `Type.Number()` field), so the port running the hand-rolled coercion
// unconditionally is behaviour-preserving.
//
// The native layer returns a discriminated outcome envelope
// (`{ok:true,value}` / `{ok:false,error}`); the shim returns `value` on success
// and re-throws `new Error(error)` on failure so pi's thrown-`Error` contract
// (`.toThrow("Validation failed")`, `Tool "<name>" not found`) holds. Deep
// equality (`.toEqual`) means the freshly parsed returned value is fine.

export * from "./validation.__pi_original__.ts";

import {
	validateToolArguments as validateToolArgumentsNative,
	validateToolCall as validateToolCallNative,
} from "pidgin-napi";
import type { Tool, ToolCall } from "../types.ts";

interface Outcome {
	ok: boolean;
	value?: unknown;
	error?: string;
}

function unwrap(outcomeJson: string): any {
	const outcome = JSON.parse(outcomeJson) as Outcome;
	if (!outcome.ok) {
		throw new Error(outcome.error ?? "Unknown validation error");
	}
	return outcome.value;
}

/**
 * Native `validateToolArguments`: coerce the tool call's arguments toward the
 * tool's schema and validate them. The shim `JSON.stringify`s the `tool` (its
 * TypeBox `parameters` serialize to their JSON-Schema projection, the
 * `[TypeBox.Kind]` symbol dropped) and the `toolCall`, and delegates; the whole
 * coercion, validation, and error-envelope logic runs in Rust. Returns the
 * coerced arguments on success, or throws `new Error("Validation failed …")`.
 */
export function validateToolArguments(tool: Tool, toolCall: ToolCall): any {
	return unwrap(validateToolArgumentsNative(JSON.stringify(tool), JSON.stringify(toolCall)));
}

/**
 * Native `validateToolCall`: resolve the tool by name, then validate the call's
 * arguments against it. The shim marshals the `tools` array and `toolCall` as
 * JSON and delegates; an unresolved tool throws `new Error('Tool "<name>" not
 * found')`, matching pi.
 */
export function validateToolCall(tools: Tool[], toolCall: ToolCall): any {
	return unwrap(validateToolCallNative(JSON.stringify(tools), JSON.stringify(toolCall)));
}
