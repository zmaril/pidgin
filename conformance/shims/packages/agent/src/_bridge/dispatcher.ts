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
// toolCallId), and `prepareArguments` rewrites raw args before validation.
// Slice 3 adds the eight loop hooks (`transformContext`, `getApiKey`,
// `shouldStopAfterTurn`, `prepareNextTurn`, `getSteeringMessages`,
// `getFollowUpMessages`, `beforeToolCall`, `afterToolCall`): each case just
// invokes the test's closure off `config` and returns its value; the revive
// helpers reconstruct the JS hook-context, substituting `context.tools` with the
// live JS tool objects from `toolsByName` (and a returned `prepareNextTurn`
// context re-serializes its tools to `ToolMeta[]` for Rust to rebuild). agent.ts
// and the harness are later slices. This module is the seam the `agent-loop.ts`
// native shim delegates to.
//
// NOTE (codegen): this helper is NOT a pi source module and has no manifest row.
// `conformance/codegen.mjs::overlayHelpers` copies these row-less `_bridge/*`
// helpers into the vendored pi tree next to the overlaid `agent-loop.ts` (and
// `restoreHelpers` removes them after a run), so the shim resolves at conformance
// time. The manifest row's `tests[]` still stays empty until agent-loop crosses
// majority-native; the tool seams are also proven directly against the built
// addon in crates/pidgin-napi/__tests__/agent-bridge-tools.mjs.

import { AgentBridge } from "pidgin-napi";
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
	AfterToolCallPayload,
	BeforeToolCallPayload,
	BridgeEnvelope,
	CtxJson,
	GetApiKeyPayload,
	PrepareArgumentsPayload,
	ToolExecutePayload,
	TransformContextPayload,
	TurnHookPayload,
} from "./envelope.ts";

/** A JS AbortSignal-like: only `.aborted` and `addEventListener` are used. */
interface AbortSignalLike {
	readonly aborted: boolean;
	addEventListener(type: "abort", listener: () => void): void;
}

/** A minimal AbortSignal carrying the dispatch-time `aborted` snapshot. pi's
 * loop-hook signatures take an `AbortSignal`; the hooks that read one (transform,
 * before/afterToolCall) only ever inspect `.aborted`. */
function mkSignal(aborted: boolean | undefined): AbortSignal {
	return { aborted: !!aborted } as AbortSignal;
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

	// Revive a wire `CtxJson` into the JS `AgentContext` the hooks expect,
	// substituting each `ToolMeta` with its live JS tool by name so a case that
	// reads `context.tools` (or passes it through — 970) round-trips real tools.
	const reviveContext = (wire: CtxJson): AgentContext => {
		const wireTools = Array.isArray(wire?.tools) ? wire.tools : [];
		const liveTools = wireTools.map(
			(m) => toolsByName.get((m as { name: string }).name) ?? (m as AgentTool<any>),
		);
		return {
			systemPrompt: wire?.systemPrompt,
			messages: (wire?.messages ?? []) as AgentMessage[],
			tools: liveTools,
		} as AgentContext;
	};

	// Revive a shouldStopAfterTurn / prepareNextTurn context (they share pi's
	// `ShouldStopAfterTurnContext` shape).
	const reviveTurnCtx = (p: TurnHookPayload) => ({
		message: p.message as never,
		toolResults: p.toolResults as never,
		context: reviveContext(p.context),
		newMessages: p.newMessages as never,
	});

	// A `prepareNextTurn` snapshot's context carries live JS tools; re-serialize
	// them to `ToolMeta[]` so Rust rebuilds bridge tools (same path as `run`).
	const serializeUpdate = (update: unknown): unknown => {
		if (!update || typeof update !== "object") return update ?? null;
		const u = update as { context?: { systemPrompt?: string; messages?: unknown[]; tools?: unknown[] } };
		if (!u.context) return update;
		const ctx = u.context;
		return {
			...u,
			context: {
				systemPrompt: ctx.systemPrompt,
				messages: ctx.messages ?? [],
				tools: Array.isArray(ctx.tools) ? ctx.tools.map((t) => toolMeta(t as AgentTool<any>)) : [],
			},
		};
	};

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
					case "transformContext": {
						const p = payload as TransformContextPayload;
						return await config.transformContext!(
							p.messages as AgentMessage[],
							mkSignal(p.aborted),
						);
					}
					case "getApiKey": {
						const p = payload as GetApiKeyPayload;
						return (await config.getApiKey!(p.provider)) ?? null;
					}
					case "getSteeringMessages":
						return (await config.getSteeringMessages!()) ?? [];
					case "getFollowUpMessages":
						return (await config.getFollowUpMessages!()) ?? [];
					case "shouldStopAfterTurn":
						return await config.shouldStopAfterTurn!(
							reviveTurnCtx(payload as TurnHookPayload),
						);
					case "prepareNextTurn":
						return serializeUpdate(
							await config.prepareNextTurn!(reviveTurnCtx(payload as TurnHookPayload)),
						);
					case "beforeToolCall": {
						const p = payload as BeforeToolCallPayload;
						// Faithful to pi: the hook may mutate `args` in place and pi
						// reuses that reference for `execute`. `p.args` is the object
						// handed to the hook, so echo it back (alongside any
						// block/reason) — the Rust bridge adopts it for execution.
						const result = await config.beforeToolCall!(
							{
								assistantMessage: p.assistantMessage as never,
								toolCall: p.toolCall as never,
								args: p.args as never,
								context: reviveContext(p.context),
							},
							mkSignal(p.aborted),
						);
						return { ...(result ?? {}), args: p.args };
					}
					case "afterToolCall": {
						const p = payload as AfterToolCallPayload;
						return (
							(await config.afterToolCall!(
								{
									assistantMessage: p.assistantMessage as never,
									toolCall: p.toolCall as never,
									args: p.args as never,
									result: p.result as never,
									isError: p.isError,
									context: reviveContext(p.context),
								},
								mkSignal(p.aborted),
							)) ?? null
						);
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
				// Report which loop hooks this case defined so Rust wires a bridge
				// round-trip only for those (closures cannot cross the wire).
				hooks: {
					transformContext: typeof config.transformContext === "function",
					getApiKey: typeof config.getApiKey === "function",
					shouldStopAfterTurn: typeof config.shouldStopAfterTurn === "function",
					prepareNextTurn: typeof config.prepareNextTurn === "function",
					getSteeringMessages: typeof config.getSteeringMessages === "function",
					getFollowUpMessages: typeof config.getFollowUpMessages === "function",
					beforeToolCall: typeof config.beforeToolCall === "function",
					afterToolCall: typeof config.afterToolCall === "function",
				},
			}),
		);
	});
}
