// Bridge slice 1 — shared envelope types for the Rust→JS callback bridge.
//
// Every seam the Rust agent loop calls back into JS multiplexes through a single
// dispatcher function via a tagged JSON envelope. The Rust side
// (crates/atilla-napi/src/agent_bridge.rs) owns the id allocation and the
// blocking resolve channel; this file names the wire shapes the two sides agree
// on. JSON is the boundary — nothing rich crosses.

/** The kinds the Rust loop dispatches in slice 1. Later slices add tool.execute,
 * the remaining hooks, and the agent.ts / harness seams. */
export type BridgeKind =
	| "streamFn" // drain the async stream → eager StreamResult
	| "convertToLlm" // AgentMessage[] → Message[]
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

/** The eager StreamResult a drained JS stream is re-presented as. */
export interface StreamResultJson {
	readonly events: unknown[];
	readonly message: unknown;
}
