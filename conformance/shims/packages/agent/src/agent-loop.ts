// Native shim for packages/agent/src/agent-loop.ts, backed by the atilla Rust
// addon (`atilla-napi`) via the slice-1 callback bridge. Installed by
// conformance/codegen.mjs when the module is marked `native` in
// conformance/manifest.json: the original pi file is preserved alongside as
// `agent-loop.__pi_original__.ts` and this shim takes its place, so pi's tests
// import `../src/agent-loop.ts` unchanged.
//
// Scope of the native flip (bridge slice 1): ONLY the single-text-turn shape of
// `agentLoop` is served by Rust — the run whose config carries just `model` +
// `convertToLlm`, a `streamFn`, no tools, and none of the loop hooks. That is
// the exact surface the slice-1 bridge supports (`streamFn` + `convertToLlm`
// wired through the TSFN; no tool.execute, steering, prepare, or api-key seams
// yet — see crates/atilla-napi/src/agent_bridge.rs::run and _bridge/dispatcher).
// For that shape the loop runs on a dedicated Rust thread and calls the test's
// live JS closures back mid-run; the assembled `AgentMessage[]` and the
// `agent_start … agent_end` event sequence are Rust-produced.
//
// Everything else falls through to pi's original, unchanged:
//   - `agentLoop` calls that carry tools or any loop hook (the conservative
//     routing predicate below rejects them — see canRouteNative), and
//   - every other export (`agentLoopContinue`, `runAgentLoop`,
//     `runAgentLoopContinue`, the `AgentEventSink` type) is re-exported from the
//     preserved original untouched.
// Conservative by construction: if the call is not provably the supported shape,
// it uses the original. It is impossible to route an unsupported case to native.
//
// As slices 2-3 land tool.execute + the remaining hooks and agent-loop crosses
// into majority-native, the predicate widens and the manifest row's `tests[]`
// gains test/agent-loop.test.ts (kept empty here so the metric under-reports).

import { EventStream } from "@earendil-works/pi-ai/compat";
import { agentLoop as originalAgentLoop } from "./agent-loop.__pi_original__.ts";
import { runAgentLoopNative } from "./_bridge/dispatcher.ts";
import type {
	AgentContext,
	AgentEvent,
	AgentLoopConfig,
	AgentMessage,
	StreamFn,
} from "./types.ts";

export * from "./agent-loop.__pi_original__.ts";

// The config keys the slice-1 bridge does NOT support. Any one of them present
// (defined) forces the original path — the Rust `run` config wires none of these
// (agent_bridge.rs sets transform_context / get_api_key / should_stop_after_turn
// / prepare_next_turn / get_steering_messages / get_follow_up_messages /
// tool_execution / before_tool_call / after_tool_call all to None).
const UNSUPPORTED_CONFIG_KEYS = [
	"transformContext",
	"getApiKey",
	"shouldStopAfterTurn",
	"prepareNextTurn",
	"getSteeringMessages",
	"getFollowUpMessages",
	"beforeToolCall",
	"afterToolCall",
	"toolExecution",
] as const;

/**
 * Whether this `agentLoop` call is the exact single-text-turn shape the slice-1
 * bridge supports. Purely a static inspection of the call's config/context —
 * it never inspects what `streamFn` will produce — so it is conservative:
 *
 *  - a real `streamFn` and a real `convertToLlm` must be present (the two seams
 *    the bridge wires),
 *  - at least one prompt to run,
 *  - NO tools available (an empty/absent `context.tools`) — with no tool registry
 *    the run cannot enter tool execution, which the bridge does not support, and
 *  - NONE of the unsupported hook / tool-execution config keys are defined.
 *
 * When any condition fails the call is delegated to pi's original loop. There is
 * no path by which a tools/hooks/steering case reaches the native bridge.
 */
function canRouteNative(
	prompts: AgentMessage[],
	context: AgentContext,
	config: AgentLoopConfig,
	streamFn: StreamFn | undefined,
): streamFn is StreamFn {
	if (typeof streamFn !== "function") return false;
	if (typeof config.convertToLlm !== "function") return false;
	if (!Array.isArray(prompts) || prompts.length === 0) return false;
	if (Array.isArray(context.tools) && context.tools.length > 0) return false;
	for (const key of UNSUPPORTED_CONFIG_KEYS) {
		if ((config as Record<string, unknown>)[key] !== undefined) return false;
	}
	return true;
}

/** Mirror of pi's private `createAgentStream` (agent-loop.__pi_original__.ts):
 * the run completes on `agent_end`, whose `messages` become `stream.result()`. */
function createAgentStream(): EventStream<AgentEvent, AgentMessage[]> {
	return new EventStream<AgentEvent, AgentMessage[]>(
		(event: AgentEvent) => event.type === "agent_end",
		(event: AgentEvent) => (event.type === "agent_end" ? event.messages : []),
	);
}

/**
 * Start an agent loop with a new prompt message. Same signature and return shape
 * as pi's `agentLoop`. For the supported single-text-turn shape the loop runs in
 * Rust through the slice-1 bridge; every other call delegates to the original.
 */
export function agentLoop(
	prompts: AgentMessage[],
	context: AgentContext,
	config: AgentLoopConfig,
	signal?: AbortSignal,
	streamFn?: StreamFn,
): EventStream<AgentEvent, AgentMessage[]> {
	if (!canRouteNative(prompts, context, config, streamFn)) {
		return originalAgentLoop(prompts, context, config, signal, streamFn);
	}

	// Native path: drive run_agent_loop on a dedicated Rust thread, forwarding
	// every loop event into the stream and ending it with the assembled
	// AgentMessage[] — the same push/end lifecycle pi's agentLoop uses.
	const stream = createAgentStream();
	void runAgentLoopNative(
		prompts,
		context,
		config,
		(event) => {
			stream.push(event);
		},
		signal,
		streamFn,
	).then((messages) => {
		stream.end(messages);
	});
	return stream;
}
