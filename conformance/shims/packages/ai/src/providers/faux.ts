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
				const aborted = streamOptions?.signal?.aborted ?? false;
				const parsed = JSON.parse(
					core.streamResolved(modelJson, contextJson, optionsJson, JSON.stringify(resolved), aborted),
				) as { events: AssistantMessageEvent[]; message: AssistantMessage };
				for (const event of parsed.events) outer.push(event);
				outer.end(parsed.message);
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

	function getModel(): Model<string>;
	function getModel(requestedModelId: string): Model<string> | undefined;
	function getModel(requestedModelId?: string): Model<string> | undefined {
		const json = core.getModelJson(requestedModelId ?? undefined);
		return json ? (JSON.parse(json) as Model<string>) : undefined;
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
