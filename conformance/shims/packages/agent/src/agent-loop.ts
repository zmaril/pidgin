// Native shim for packages/agent/src/agent-loop.ts, backed by the atilla Rust
// addon (`atilla-napi`) via the slice-1 callback bridge. Installed by
// conformance/codegen.mjs when the module is marked `native` in
// conformance/manifest.json: the original pi file is preserved alongside as
// `agent-loop.__pi_original__.ts` and this shim takes its place, so pi's tests
// import `../src/agent-loop.ts` unchanged.
//
// Scope of the native flip (bridge slices 1–3): the single-text-turn shape, the
// tool-using runs, AND the loop-hook shapes the eager Rust loop reproduces
// faithfully. Slice 1 served the run whose config carries just `model` +
// `convertToLlm` + a `streamFn` with no tools and no hooks. Slice 2 adds tool
// support — the loop drives the test's live JS `tool.execute` (and
// `prepareArguments`) through the bridge, forwarding each `onUpdate` back via
// `emitToolUpdate`. Slice 3 wires the eight loop hooks (`transformContext`,
// `getApiKey`, `shouldStopAfterTurn`, `prepareNextTurn`, `getSteeringMessages`,
// `getFollowUpMessages`, `beforeToolCall`, `afterToolCall`) as blocking bridge
// round-trips (see crates/atilla-napi/src/agent_bridge.rs and _bridge/
// dispatcher). For those shapes the loop runs on a dedicated Rust thread and
// calls the test's live JS closures back mid-run; the assembled `AgentMessage[]`
// and the `agent_start … agent_end` event sequence are Rust-produced.
//
// Two shapes the eager loop cannot reproduce still delegate, and both are always
// signalled statically so `canRouteNative` rejects them:
//   1. *observable* concurrent tool execution — the loop runs deferred tool calls
//      serially in source order (see the agent_loop.rs module docs), so a
//      `config.toolExecution:"parallel"` opt-in or any tool
//      `executionMode:"parallel"` delegates to the original (cases 525/896/1192).
//   2. `beforeToolCall` in-place argument mutation — case 383's hook mutates
//      `args.value` in place and returns `undefined`, and pi reuses that same
//      reference for `execute`. The eager loop deliberately does NOT cross that
//      boundary (beforeToolCall receives an immutable context and returns only
//      `{block,reason}`), so routing 383 native would execute the un-mutated args
//      and fail conformance. `beforeToolCall` is used ONLY by 383, so rejecting
//      its presence keeps exactly that case delegated. Adopting the post-hook
//      args is a deferred agent_loop.rs spike (out of bridge scope).
// Default- and sequential-mode tool runs, which the loop reproduces exactly, are
// admitted.
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
// Native after slice 3 = 13/20 (8 carried from slices 1–2 + 5 new hook cases:
// 186 transformContext, 620 getSteeringMessages, 970 prepareNextTurn, 1043
// shouldStopAfterTurn, 1257 afterToolCall). 13/20 is a MAJORITY, so the manifest
// row now qualifies for `tests: ["test/agent-loop.test.ts"]` — left to the
// steward's attribution follow-up, not flipped in the bridge PR. The remaining 7
// stay delegated by construction: 383 (beforeToolCall in-place mutation), the
// three parallel cases (525/896/1192), and the three `agentLoopContinue` cases
// (1307/1322/1364, which this shim does not override).

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

// Slice 3 wires all eight loop hooks through the bridge, so the former
// UNSUPPORTED_CONFIG_KEYS reject-list is empty — none of the hooks force the
// original path any more. The one exception is `beforeToolCall`: it is kept as a
// dedicated reject below (not because the seam is missing — it is wired and
// proven — but because pi's only beforeToolCall case, 383, relies on in-place arg
// mutation the eager loop deliberately does not reproduce; see the header).
const UNSUPPORTED_CONFIG_KEYS = [] as const;

/**
 * Whether this `agentLoop` call is a shape the slice-1/2/3 bridge reproduces
 * faithfully. Purely a static inspection of the call's config/context — it never
 * inspects what `streamFn` will produce — so it is conservative:
 *
 *  - a real `streamFn` and a real `convertToLlm` must be present (the two seams
 *    the bridge always wires),
 *  - at least one prompt to run,
 *  - `beforeToolCall` is NOT defined — case 383 mutates the validated args in
 *    place and pi executes the mutated reference; the eager loop does not cross
 *    that boundary, so it delegates (the only beforeToolCall case), and
 *  - "no observable parallelism": the eager loop runs deferred tool calls
 *    serially in source order, so any case that asserts real concurrency must be
 *    delegated. That intent is always statically signalled, so we reject:
 *      · `config.toolExecution === "parallel"` (an explicit parallel opt-in), and
 *      · any registered tool with `executionMode === "parallel"`.
 *    Default- and sequential-mode tool runs (which the loop reproduces exactly)
 *    are admitted; the `prepareArguments` seam is shipped, so tools that declare
 *    it are supported. transformContext / getApiKey / shouldStopAfterTurn /
 *    prepareNextTurn / getSteeringMessages / getFollowUpMessages / afterToolCall
 *    are all wired (slice 3) and admitted.
 *
 * When any condition fails the call is delegated to pi's original loop. There is
 * no path by which a beforeToolCall-mutating or parallel-observing case reaches
 * native.
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
	// beforeToolCall (case 383): the hook mutates the validated args in place and
	// pi reuses that reference for execute; the eager loop passes an immutable
	// context, so native would run the un-mutated args. Delegate — adopting the
	// post-hook args is a deferred agent_loop.rs spike.
	if ((config as { beforeToolCall?: unknown }).beforeToolCall !== undefined) {
		return false;
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
