// Bridge slices 1–2 — shared envelope types for the Rust→JS callback bridge.
//
// Every seam the Rust agent loop calls back into JS multiplexes through a single
// dispatcher function via a tagged JSON envelope. The Rust side
// (crates/atilla-napi/src/agent_bridge.rs) owns the id allocation and the
// blocking resolve channel; this file names the wire shapes the two sides agree
// on. JSON is the boundary — nothing rich crosses.

/** The kinds the Rust loop dispatches. Slice 1 wired `streamFn` / `convertToLlm`;
 * slice 2 adds the tool seams (`toolExecute`, `prepareArguments`). Later slices
 * add the remaining hooks and the agent.ts / harness seams. The interim
 * tool-update push (`emitToolUpdate`) is NOT a dispatched kind — it is a JS→Rust
 * method call, so it never rides the envelope. */
export type BridgeKind =
	| "streamFn" // drain the async stream → eager StreamResult
	| "convertToLlm" // AgentMessage[] → Message[]
	| "toolExecute" // run the registered tool's execute(id, args, signal, onUpdate)
	| "prepareArguments" // rewrite raw tool args before schema validation
	| "event" // fire-and-forget forward of an AgentEvent (no resolve)
	| "__complete__"; // terminal: the run's AgentMessage[] (resolve the promise)

/** The tagged envelope the dispatcher receives (as a JSON string). */
export interface BridgeEnvelope {
	/** Per-request id; the Rust loop thread is parked on this id's channel.
	 * `0` for `event` / `__complete__`, which never round-trip. */
	readonly id: number;
	readonly kind: BridgeKind | string;
	readonly payload: unknown;
	/** Snapshot of the cooperative abort signal at dispatch time. */
	readonly aborted?: boolean;
}

/** The reserved error shape a seam resolves with when its JS closure throws or
 * rejects, delivered via `AgentBridge.resolveBridgeError`. */
export interface BridgeError {
	readonly __bridge_error: string;
}

/** streamFn request payload. */
export interface StreamFnPayload {
	readonly model: unknown;
	readonly context: unknown;
	readonly options: unknown | null;
	readonly aborted: boolean;
}

/** convertToLlm request payload. */
export interface ConvertToLlmPayload {
	readonly messages: unknown[];
}

/** toolExecute request payload. `toolCallId` both routes the interim
 * `emitToolUpdate` push and resolves the parked round-trip; `toolName` selects
 * the registered JS tool. */
export interface ToolExecutePayload {
	readonly toolCallId: string;
	readonly toolName: string;
	readonly args: unknown;
	readonly aborted: boolean;
}

/** prepareArguments request payload — a pure sync transform of the raw args. */
export interface PrepareArgumentsPayload {
	readonly toolName: string;
	readonly args: unknown;
}

/** The eager StreamResult a drained JS stream is re-presented as. */
export interface StreamResultJson {
	readonly events: unknown[];
	readonly message: unknown;
}

/** A serialized `AgentToolResult` — the toolExecute round-trip's resolve value
 * and the `emitToolUpdate` partial-result shape. `details` is required Rust-side
 * (defaults to `{}`), so a tool that omits it should still send an object. */
export interface AgentToolResultJson {
	readonly content: unknown[];
	readonly details: unknown;
	readonly addedToolNames?: string[];
	readonly terminate?: boolean;
}
