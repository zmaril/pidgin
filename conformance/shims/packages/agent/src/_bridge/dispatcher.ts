// Bridge slice 1 — the JS side of the Rust→JS callback bridge.
//
// Owns the envelope protocol: it installs one dispatcher on a native
// `AgentBridge`, routes each `{ id, kind, payload }` to the injected JS closure,
// and resolves the parked Rust request through `resolveBridge` (success) or
// `resolveBridgeError` (throw / rejection). The dedicated Rust loop thread blocks
// on each id's channel; because that thread is off the Node event loop, the
// closures' async work (stream drains, hooks) settles normally while it waits.
//
// Slice 1 wired `streamFn` and `convertToLlm` (plus the fire-and-forget event
// forward). Slice 2 adds the tool seams: `toolExecute` runs the registered JS
// tool's `execute(id, args, signal, onUpdate)`, wiring `onUpdate` back to Rust
// via the fire-and-forget `emitToolUpdate` method (routed by closing over the
// toolCallId), and `prepareArguments` rewrites raw args before validation. The
// remaining loop hooks, agent.ts, and the harness are later slices. This module
// is the seam the `agent-loop.ts` native shim delegates to.
//
// NOTE (codegen): this helper is NOT a pi source module and has no manifest row.
// `conformance/codegen.mjs::overlayHelpers` copies these row-less `_bridge/*`
// helpers into the vendored pi tree next to the overlaid `agent-loop.ts` (and
// `restoreHelpers` removes them after a run), so the shim resolves at conformance
// time. The manifest row's `tests[]` still stays empty until agent-loop crosses
// majority-native; the tool seams are also proven directly against the built
// addon in crates/atilla-napi/__tests__/agent-bridge-tools.mjs.

import { AgentBridge } from "atilla-napi";
import type {
	AgentContext,
	AgentEvent,
	AgentLoopConfig,
	AgentMessage,
	AgentTool,
	AgentToolResult,
	StreamFn,
} from "../types.ts";
import type {
	BridgeEnvelope,
	PrepareArgumentsPayload,
	ToolExecutePayload,
} from "./envelope.ts";

/** A JS AbortSignal-like: only `.aborted` and `addEventListener` are used. */
interface AbortSignalLike {
	readonly aborted: boolean;
	addEventListener(type: "abort", listener: () => void): void;
}

/** Fully drain a pi `AssistantMessageEventStream` into the eager StreamResult
 * JSON the Rust loop consumes: collect every event, then its final message. */
async function drainStream(
	streamFn: StreamFn,
	payload: { model: unknown; context: unknown; options: unknown },
): Promise<{ events: unknown[]; message: unknown }> {
	// The Rust loop passes the LLM-ready context and options; the injected
	// streamFn returns pi's async event stream.
	const stream = await (streamFn as unknown as (
		model: unknown,
		context: unknown,
		options: unknown,
	) => AsyncIterable<unknown> & { result(): Promise<unknown> })(
		payload.model,
		payload.context,
		payload.options,
	);
	const events: unknown[] = [];
	for await (const event of stream) events.push(event);
	const message = await stream.result();
	return { events, message };
}

/** Wire metadata for one tool. Closures cannot cross the bridge, so only these
 * fields are serialized into the `run` payload; the Rust side reconstructs the
 * `AgentTool` with `execute` / `prepareArguments` bridge seams keyed by name. */
interface ToolMetaJson {
	name: string;
	label: string;
	description: string;
	parameters: unknown;
	executionMode: string | null;
	hasPrepareArguments: boolean;
}

/** Serialize a JS `AgentTool` to its wire metadata. */
function toolMeta(tool: AgentTool<any>): ToolMetaJson {
	return {
		name: tool.name,
		label: tool.label,
		description: tool.description,
		parameters: tool.parameters,
		executionMode: tool.executionMode ?? null,
		hasPrepareArguments: typeof tool.prepareArguments === "function",
	};
}

/**
 * Run the Rust agent loop for a new prompt batch, wiring `streamFn`,
 * `convertToLlm`, and (slice 2) the registered tools' `execute` / `onUpdate` /
 * `prepareArguments` through the bridge. Resolves with the run's
 * `AgentMessage[]`.
 */
export function runAgentLoopNative(
	prompts: AgentMessage[],
	context: AgentContext,
	config: AgentLoopConfig,
	emit: (event: AgentEvent) => Promise<void> | void,
	signal: AbortSignalLike | undefined,
	streamFn: StreamFn,
): Promise<AgentMessage[]> {
	const bridge = new AgentBridge();

	// name → tool, so `toolExecute` / `prepareArguments` envelopes resolve against
	// the right JS closure. Only meaningful when the case carries tools.
	const tools = Array.isArray(context.tools) ? context.tools : [];
	const toolsByName = new Map<string, AgentTool<any>>(tools.map((t) => [t.name, t]));

	// The cooperative abort signal is Rust-owned; a JS abort just trips it, which
	// also unblocks any request currently parked on the loop thread.
	if (signal) {
		if (signal.aborted) bridge.abort();
		else signal.addEventListener("abort", () => bridge.abort());
	}

	return new Promise<AgentMessage[]>((resolve, reject) => {
		const dispatcher = (envelopeJson: string): void => {
			let env: BridgeEnvelope;
			try {
				env = JSON.parse(envelopeJson) as BridgeEnvelope;
			} catch (error) {
				reject(error);
				return;
			}
			const { id, kind, payload } = env;

			if (kind === "__complete__") {
				// Reap the loop thread + release the TSFN before settling so the
				// process can exit cleanly.
				bridge.join();
				resolve((payload as { messages: AgentMessage[] }).messages);
				return;
			}
			if (kind === "event") {
				// Fire-and-forget: forward to the caller's sink, never resolve.
				void emit(payload as AgentEvent);
				return;
			}

			// Every seam round-trip: run the closure, then release the parked id.
			// Any throw / rejection is delivered via resolveBridgeError so the Rust
			// thread surfaces a clean error instead of hanging forever.
			const handle = async (): Promise<unknown> => {
				switch (kind) {
					case "streamFn":
						return await drainStream(streamFn, payload as never);
					case "convertToLlm":
						return await config.convertToLlm(
							(payload as { messages: AgentMessage[] }).messages,
						);
					case "toolExecute": {
						const p = payload as ToolExecutePayload;
						const tool = toolsByName.get(p.toolName);
						if (!tool) throw new Error(`bridge: no tool "${p.toolName}"`);
						// onUpdate forwards straight to Rust via emitToolUpdate; the
						// toolCallId is closed over so no JS-side lookup is needed. The
						// push is fire-and-forget (no resolve, no round-trip, no id).
						const onUpdate = (partial: AgentToolResult<unknown>): void => {
							bridge.emitToolUpdate(p.toolCallId, JSON.stringify(partial));
						};
						const toolSignal = { aborted: !!p.aborted } as AbortSignal;
						return await tool.execute(
							p.toolCallId,
							p.args as never,
							toolSignal,
							onUpdate,
						);
					}
					case "prepareArguments": {
						const p = payload as PrepareArgumentsPayload;
						const tool = toolsByName.get(p.toolName);
						if (!tool || typeof tool.prepareArguments !== "function") {
							return p.args;
						}
						return tool.prepareArguments(p.args);
					}
					default:
						throw new Error(`bridge: unhandled kind "${kind}"`);
				}
			};
			handle()
				.then((result) => bridge.resolveBridge(id, JSON.stringify(result ?? null)))
				.catch((error: unknown) =>
					bridge.resolveBridgeError(
						id,
						JSON.stringify({
							__bridge_error: String(
								(error as { message?: string })?.message ?? error,
							),
						}),
					),
				);
		};

		bridge.run(
			dispatcher,
			JSON.stringify({
				prompts,
				context: {
					systemPrompt: context.systemPrompt,
					messages: context.messages,
				},
				model: config.model,
				streamOptions: config.sessionId ? { sessionId: config.sessionId } : null,
				reasoning: config.reasoning ?? null,
				tools: tools.map(toolMeta),
				toolExecution: config.toolExecution ?? null,
			}),
		);
	});
}
