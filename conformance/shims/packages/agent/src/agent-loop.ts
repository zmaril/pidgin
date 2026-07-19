// Native shim for packages/agent/src/agent-loop.ts, backed by the atilla Rust
// addon (`atilla-napi`) via the slice-1 callback bridge. Installed by
// conformance/codegen.mjs when the module is marked `native` in
// conformance/manifest.json: the original pi file is preserved alongside as
// `agent-loop.__pi_original__.ts` and this shim takes its place, so pi's tests
// import `../src/agent-loop.ts` unchanged.
//
// Scope of the native flip (bridge slices 1–2): the single-text-turn shape AND
// tool-using runs the eager Rust loop reproduces faithfully. Slice 1 served the
// run whose config carries just `model` + `convertToLlm` + a `streamFn` with no
// tools and no hooks. Slice 2 adds tool support — the loop drives the test's
// live JS `tool.execute` (and `prepareArguments`) through the bridge, forwarding
// each `onUpdate` back via `emitToolUpdate` (see crates/atilla-napi/src/
// agent_bridge.rs and _bridge/dispatcher). For those shapes the loop runs on a
// dedicated Rust thread and calls the test's live JS closures back mid-run; the
// assembled `AgentMessage[]` and the `agent_start … agent_end` event sequence
// are Rust-produced.
//
// The ONE thing the eager loop cannot reproduce is *observable* concurrent tool
// execution: it runs deferred tool calls serially in source order (see the
// agent_loop.rs module docs). That intent is always signalled statically in pi's
// tests, so `canRouteNative` still rejects it — a `config.toolExecution:
// "parallel"` opt-in or any tool `executionMode:"parallel"` delegates to the
// original (cases 525/896/1192). Default- and sequential-mode tool runs, which
// the loop reproduces exactly, are admitted.
//
// Everything else falls through to pi's original, unchanged:
//   - `agentLoop` calls that carry an unsupported loop hook, or opt into parallel
//     execution (the conservative predicate below rejects them — see
//     canRouteNative), and
//   - every other export (`agentLoopContinue`, `runAgentLoop`,
//     `runAgentLoopContinue`, the `AgentEventSink` type) is re-exported from the
//     preserved original untouched.
// Conservative by construction: if the call is not provably a supported shape,
// it uses the original. It is impossible to route an unsupported case to native.
//
// Native after slice 2 = 8/20 (2 carried from slice 1 + 6 new: the tool cases
// 239, 310, 445, 726, 809, 1140). 8/20 is below majority, so the manifest row's
// `tests[]` stays empty (the metric under-reports) until slice 3 lands the
// remaining loop hooks and crosses majority.

import { EventStream } from "@earendil-works/pi-ai/compat";
import { agentLoop as originalAgentLoop } from "./agent-loop.__pi_original__.ts";
import { runAgentLoopNative } from "./_bridge/dispatcher.ts";
import type {
	AgentContext,
	AgentEvent,
	AgentLoopConfig,
	AgentMessage,
	AgentTool,
	StreamFn,
} from "./types.ts";

export * from "./agent-loop.__pi_original__.ts";

// The loop-hook config keys the bridge does NOT support yet. Any one of them
// present (defined) forces the original path — the Rust `run` config wires none
// of these (agent_bridge.rs sets transform_context / get_api_key /
// should_stop_after_turn / prepare_next_turn / get_steering_messages /
// get_follow_up_messages / before_tool_call / after_tool_call to None). Note:
// `toolExecution` is NOT in this list as of slice 2 — it is supported except for
// the `"parallel"` opt-in, which the parallelism guard below rejects.
const UNSUPPORTED_CONFIG_KEYS = [
	"transformContext",
	"getApiKey",
	"shouldStopAfterTurn",
	"prepareNextTurn",
	"getSteeringMessages",
	"getFollowUpMessages",
	"beforeToolCall",
	"afterToolCall",
] as const;

/**
 * Whether this `agentLoop` call is a shape the slice-1/2 bridge reproduces
 * faithfully. Purely a static inspection of the call's config/context — it never
 * inspects what `streamFn` will produce — so it is conservative:
 *
 *  - a real `streamFn` and a real `convertToLlm` must be present (the two seams
 *    the bridge always wires),
 *  - at least one prompt to run,
 *  - NONE of the still-unsupported loop-hook config keys are defined, and
 *  - "no observable parallelism": the eager loop runs deferred tool calls
 *    serially in source order, so any case that asserts real concurrency must be
 *    delegated. That intent is always statically signalled, so we reject:
 *      · `config.toolExecution === "parallel"` (an explicit parallel opt-in), and
 *      · any registered tool with `executionMode === "parallel"`.
 *    Default- and sequential-mode tool runs (which the loop reproduces exactly)
 *    are admitted; the `prepareArguments` seam is shipped, so tools that declare
 *    it are supported.
 *
 * When any condition fails the call is delegated to pi's original loop. There is
 * no path by which an unsupported-hook or parallel-observing case reaches native.
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
	for (const key of UNSUPPORTED_CONFIG_KEYS) {
		if ((config as Record<string, unknown>)[key] !== undefined) return false;
	}
	// No observable parallelism: reject explicit parallel opt-in or any tool that
	// asks to run concurrently — the eager loop cannot reproduce real concurrency.
	if ((config as { toolExecution?: unknown }).toolExecution === "parallel") {
		return false;
	}
	const tools: AgentTool<any>[] = Array.isArray(context.tools) ? context.tools : [];
	if (tools.some((tool) => tool.executionMode === "parallel")) return false;
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
