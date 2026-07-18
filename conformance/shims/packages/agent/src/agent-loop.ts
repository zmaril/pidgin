// Native shim for packages/agent/src/agent-loop.ts, backed by the atilla Rust
// addon (`atilla-napi`) — bridge slice 1. When conformance/codegen.mjs marks
// this module `native` in conformance/manifest.json, the original pi file is
// preserved alongside as `agent-loop.__pi_original__.ts` and this shim takes its
// place, so pi's tests (which import `../agent-loop.ts` unchanged) drive the Rust
// `run_agent_loop` while their injected JS closures still fire mid-run.
//
// Scope of the native flip (slice 1): the four public entry points route through
// the Rust loop over a blocking Rust→JS callback bridge (one
// `ThreadsafeFunction` dispatcher + a per-request resolve channel; see
// crates/atilla-napi/src/agent_bridge.rs and ./_bridge/dispatcher.ts). Only the
// `streamFn` and `convertToLlm` seams are wired in this slice, covering the
// simplest single-text-turn cases; tool.execute, the remaining loop hooks,
// agent.ts, and the harness are later slices. Everything else pi's agent-loop
// module exports (the `AgentEventSink` type) is re-exported from the original
// unchanged.
//
// IMPORTANT — this shim is scaffolding until a steward-owned codegen change
// lands: codegen.mjs only overlays manifest-listed `src` files into the vendored
// pi tree, so the net-new `./_bridge/*` helpers this shim imports are not yet
// copied next to the overlay. The manifest row for agent-loop.ts therefore stays
// `original` in this slice; the primitive is proven directly against the built
// addon in crates/atilla-napi/__tests__/. See the PR description for detail.

export * from "./agent-loop.__pi_original__.ts";

import { EventStream } from "@earendil-works/pi-ai/compat";
import { runAgentLoopNative } from "./_bridge/dispatcher.ts";
import type {
	AgentContext,
	AgentEvent,
	AgentLoopConfig,
	AgentMessage,
	StreamFn,
} from "./types.ts";

function createAgentStream(): EventStream<AgentEvent, AgentMessage[]> {
	return new EventStream<AgentEvent, AgentMessage[]>(
		(event: AgentEvent) => event.type === "agent_end",
		(event: AgentEvent) => (event.type === "agent_end" ? event.messages : []),
	);
}

/** Native `runAgentLoop`: drive the Rust loop, emitting through `emit`. */
export async function runAgentLoop(
	prompts: AgentMessage[],
	context: AgentContext,
	config: AgentLoopConfig,
	emit: (event: AgentEvent) => Promise<void> | void,
	signal?: AbortSignal,
	streamFn?: StreamFn,
): Promise<AgentMessage[]> {
	return runAgentLoopNative(
		prompts,
		context,
		config,
		emit,
		signal,
		streamFn ?? (config as { streamFn?: StreamFn }).streamFn!,
	);
}

/** Native `agentLoop`: same as pi — wraps `runAgentLoop` in an `EventStream`. */
export function agentLoop(
	prompts: AgentMessage[],
	context: AgentContext,
	config: AgentLoopConfig,
	signal?: AbortSignal,
	streamFn?: StreamFn,
): EventStream<AgentEvent, AgentMessage[]> {
	const stream = createAgentStream();
	void runAgentLoop(
		prompts,
		context,
		config,
		async (event) => {
			stream.push(event);
		},
		signal,
		streamFn,
	).then((messages) => {
		stream.end(messages);
	});
	return stream;
}

/** Native `runAgentLoopContinue`: continue from the current context. The
 * continue-guards match pi; the loop itself is driven in Rust. */
export async function runAgentLoopContinue(
	context: AgentContext,
	config: AgentLoopConfig,
	emit: (event: AgentEvent) => Promise<void> | void,
	signal?: AbortSignal,
	streamFn?: StreamFn,
): Promise<AgentMessage[]> {
	if (context.messages.length === 0) {
		throw new Error("Cannot continue: no messages in context");
	}
	if (context.messages[context.messages.length - 1].role === "assistant") {
		throw new Error("Cannot continue from message role: assistant");
	}
	// The Rust `run` entry appends the prompts; continue passes none and keeps the
	// existing context, matching pi's `runAgentLoopContinue`.
	return runAgentLoopNative(
		[],
		context,
		config,
		emit,
		signal,
		streamFn ?? (config as { streamFn?: StreamFn }).streamFn!,
	);
}

/** Native `agentLoopContinue`: wraps `runAgentLoopContinue` in an `EventStream`. */
export function agentLoopContinue(
	context: AgentContext,
	config: AgentLoopConfig,
	signal?: AbortSignal,
	streamFn?: StreamFn,
): EventStream<AgentEvent, AgentMessage[]> {
	if (context.messages.length === 0) {
		throw new Error("Cannot continue: no messages in context");
	}
	if (context.messages[context.messages.length - 1].role === "assistant") {
		throw new Error("Cannot continue from message role: assistant");
	}
	const stream = createAgentStream();
	void runAgentLoopContinue(
		context,
		config,
		async (event) => {
			stream.push(event);
		},
		signal,
		streamFn,
	).then((messages) => {
		stream.end(messages);
	});
	return stream;
}
