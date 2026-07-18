// Native shim for packages/ai/src/providers/faux.ts, backed by the atilla Rust
// addon (`atilla-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is preserved
// alongside as `faux.__pi_original__.ts` and this shim takes its place, so pi's
// tests (and compat.ts's `registerFauxProvider`) import `../providers/faux.ts`
// unchanged and hit Rust.
//
// Scope of the native flip (Stage 3): `createFauxCore` — the deterministic delta
// streaming plus token/cache accounting — is served by the Rust faux provider
// (`atilla_ai::providers::faux`). The response *queue* and JS response *factories*
// stay on the JS side: the shim pops the next step and, when it is a function,
// calls it here, then hands the resolved `AssistantMessage` to Rust. That is why
// no threadsafe callback is needed — the JS-closure case is resolved in JS and
// only the streaming math crosses to Rust. Everything else pi's faux module
// exports (the `fauxText`/`fauxThinking`/`fauxToolCall`/`fauxAssistantMessage`
// builders, `fauxProvider`, and the option/handle types) is re-exported from the
// original unchanged, so the public runtime surface stays byte-for-byte pi's.
//
// Stage 4 fidelity fixes:
//
//   1. Mid-stream abort. Rust builds the full (non-aborted) event sequence
//      eagerly; the shim replays it across await points, re-checking
//      `signal.aborted` before each delta exactly where pi's `streamWithDeltas`
//      does (`faux.ts:308-401`). A consumer that calls `controller.abort()`
//      between deltas stops the stream and receives pi's `"aborted"` terminal
//      error event, rather than the whole stream ending in `stop`. Rust cannot
//      observe a mid-stream JS abort (it runs synchronously between microtasks),
//      so the abort loop necessarily lives on the JS side; the non-abort output
//      is the exact event list Rust produced, byte-for-byte. `tokensPerSecond`
//      pacing (pi's `scheduleChunk`, `faux.ts:300-306`) is reproduced here too so
//      real-timer abort tests land mid-stream.
//
//   2. `getModel()` reference identity. The models are held as a JS array and
//      `getModel()` returns the same object reference each call (pi returns
//      `models[0]` / `models.find(...)` by reference, `faux.ts:481-488`), so a
//      test that mutates `getModel().thinkingLevelMap` and reads it back through
//      the session observes the mutation.
//
// straitjacket-allow-file[:duplication] — the error-path `AssistantMessage` built
// here is a faithful transcription of pi's own zero-usage/error message shape
// (the same literal `api-messages`/`anthropic-messages` shims emit); the clone
// detector reads it as a duplicate, but it is pi's exact boundary contract.

export * from "./faux.__pi_original__.ts";

import { FauxCore } from "atilla-napi";
import type {
	AssistantMessage,
	AssistantMessageEvent,
	Context,
	Model,
	SimpleStreamOptions,
	StreamFunction,
	StreamOptions,
} from "../types.ts";
import { createAssistantMessageEventStream } from "../utils/event-stream.ts";
import type { FauxResponseStep, RegisterFauxProviderOptions } from "./faux.__pi_original__.ts";

// Only the stream-option fields the Rust faux accounting reads (session-scoped
// prompt caching). The rest of pi's StreamOptions (signal, callbacks, ...) are
// handled on the JS side and must not be JSON-stringified across the boundary.
function pickOptions(options: StreamOptions | undefined): string | null {
	if (!options) return null;
	const picked: { sessionId?: string; cacheRetention?: StreamOptions["cacheRetention"] } = {};
	if (options.sessionId !== undefined) picked.sessionId = options.sessionId;
	if (options.cacheRetention !== undefined) picked.cacheRetention = options.cacheRetention;
	return JSON.stringify(picked);
}

// pi's `estimateTokens` (`faux.ts:140-142`), used only for `scheduleChunk` pacing
// below.
function estimateTokens(text: string): number {
	return Math.ceil(text.length / 4);
}

// pi's `scheduleChunk` (`faux.ts:300-306`): a microtask yield when unpaced, or a
// real-timer delay proportional to the delta size when `tokensPerSecond` is set.
// The yield is what lets a consumer abort between deltas.
function scheduleChunk(delta: string, tokensPerSecond: number | undefined): Promise<void> {
	if (!tokensPerSecond || tokensPerSecond <= 0) {
		return new Promise((resolve) => queueMicrotask(resolve));
	}
	const delayMs = (estimateTokens(delta) / tokensPerSecond) * 1000;
	return new Promise((resolve) => setTimeout(resolve, delayMs));
}

// pi's `createAbortedMessage` (`faux.ts:291-298`): the current partial re-stamped
// as an `aborted` terminal.
function createAbortedMessage(partial: AssistantMessage): AssistantMessage {
	return {
		...partial,
		stopReason: "aborted",
		errorMessage: "Request was aborted",
		timestamp: Date.now(),
	};
}

// Replay Rust's eagerly-built event sequence, honoring a mid-stream abort exactly
// where pi's `streamWithDeltas` checks `signal.aborted`: before `start`, before
// each block `*_start`, and (after a `scheduleChunk` await) before each `*_delta`.
// On abort the stream stops and terminates with pi's `"aborted"` error event,
// carrying the last-emitted partial. With no signal (or none aborted) the exact
// event list Rust produced is pushed, byte-for-byte pi's non-abort output.
async function replayStream(
	outer: ReturnType<typeof createAssistantMessageEventStream>,
	events: AssistantMessageEvent[],
	finalMessage: AssistantMessage,
	signal: AbortSignal | undefined,
	tokensPerSecond: number | undefined,
): Promise<void> {
	const emitAborted = (partial: AssistantMessage) => {
		const aborted = createAbortedMessage(partial);
		outer.push({ type: "error", reason: "aborted", error: aborted });
		outer.end(aborted);
	};

	// pi's pre-stream check (`faux.ts:317-322`): abort before `start`.
	if (signal?.aborted) {
		emitAborted({ ...finalMessage, content: [] });
		return;
	}

	let lastPartial: AssistantMessage = { ...finalMessage, content: [] };
	for (const event of events) {
		if (event.type === "done" || event.type === "error") {
			outer.push(event);
			continue;
		}
		if (event.type === "text_delta" || event.type === "thinking_delta" || event.type === "toolcall_delta") {
			await scheduleChunk(event.delta, tokensPerSecond);
			if (signal?.aborted) {
				emitAborted(lastPartial);
				return;
			}
		} else if (
			(event.type === "text_start" || event.type === "thinking_start" || event.type === "toolcall_start") &&
			signal?.aborted
		) {
			// pi's block-boundary check (`faux.ts:327-332`).
			emitAborted(lastPartial);
			return;
		}
		outer.push(event);
		lastPartial = event.partial;
	}
	outer.end(finalMessage);
}

/**
 * Rust-backed reimplementation of pi's `createFauxCore` (`faux.ts:403-508`). The
 * response queue and factory resolution live here; the streaming and accounting
 * live in Rust behind the `FauxCore` native class.
 */
export function createFauxCore(options: RegisterFauxProviderOptions = {}) {
	const core = new FauxCore(JSON.stringify(options ?? {}));
	const api = core.api();
	const models = JSON.parse(core.modelsJson()) as [Model<string>, ...Model<string>[]];
	const provider = models[0]?.provider ?? "faux";
	const tokensPerSecond = options?.tokensPerSecond;
	let pendingResponses: FauxResponseStep[] = [];
	// `state.callCount` is owned by Rust; expose it as a live getter so response
	// factories and `registration.state` observe pi's post-increment value.
	const state = {
		get callCount() {
			return core.callCount();
		},
	};

	const stream: StreamFunction<string, StreamOptions> = (requestModel, context, streamOptions) => {
		const outer = createAssistantMessageEventStream();
		const step = pendingResponses.shift();
		const callCount = core.bumpCallCount();
		const modelJson = JSON.stringify(requestModel);
		const contextJson = JSON.stringify(context);
		const optionsJson = pickOptions(streamOptions);

		queueMicrotask(async () => {
			try {
				await streamOptions?.onResponse?.({ status: 200, headers: {} }, requestModel);
				if (!step) {
					core.setNowMs(Date.now());
					const parsed = JSON.parse(core.emptyQueueResult(modelJson, contextJson, optionsJson)) as {
						events: AssistantMessageEvent[];
						message: AssistantMessage;
					};
					for (const event of parsed.events) outer.push(event);
					outer.end(parsed.message);
					return;
				}
				const resolved =
					typeof step === "function" ? await step(context, streamOptions, { callCount }, requestModel) : step;
				// Rust builds the full non-aborted event sequence (`aborted: false`);
				// mid-stream abort is honored during replay on the JS side, since Rust
				// runs synchronously and cannot see an abort raised between deltas.
				core.setNowMs(Date.now());
				const parsed = JSON.parse(
					core.streamResolved(modelJson, contextJson, optionsJson, JSON.stringify(resolved), false),
				) as { events: AssistantMessageEvent[]; message: AssistantMessage };
				await replayStream(outer, parsed.events, parsed.message, streamOptions?.signal, tokensPerSecond);
			} catch (error) {
				const message: AssistantMessage = {
					role: "assistant",
					content: [],
					api,
					provider,
					model: requestModel.id,
					usage: {
						input: 0,
						output: 0,
						cacheRead: 0,
						cacheWrite: 0,
						totalTokens: 0,
						cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
					},
					stopReason: "error",
					errorMessage: error instanceof Error ? error.message : String(error),
					timestamp: Date.now(),
				};
				outer.push({ type: "error", reason: "error", error: message });
				outer.end(message);
			}
		});

		return outer;
	};

	const streamSimple: StreamFunction<string, SimpleStreamOptions> = (streamModel, context, streamOptions) =>
		stream(streamModel, context, streamOptions);

	// pi's `getModel` (`faux.ts:481-488`): returns the *same* model object held in
	// `models`, by reference, so mutations (e.g. `getModel().thinkingLevelMap = …`)
	// persist and are observed on later reads through the registration or session.
	function getModel(): Model<string>;
	function getModel(requestedModelId: string): Model<string> | undefined;
	function getModel(requestedModelId?: string): Model<string> | undefined {
		if (requestedModelId === undefined) {
			return models[0];
		}
		return models.find((candidate) => candidate.id === requestedModelId);
	}

	return {
		api,
		provider,
		models,
		stream,
		streamSimple,
		getModel,
		state,
		setResponses(responses: FauxResponseStep[]) {
			pendingResponses = [...responses];
		},
		appendResponses(responses: FauxResponseStep[]) {
			pendingResponses.push(...responses);
		},
		getPendingResponseCount() {
			return pendingResponses.length;
		},
	};
}
