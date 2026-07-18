// Native shim for packages/ai/src/api/anthropic-messages.ts, backed by the
// atilla Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs when
// the module is marked `native` in conformance/manifest.json: the original pi
// file is preserved alongside as `anthropic-messages.__pi_original__.ts` and
// this shim takes its place, so pi's tests import `../src/api/anthropic-messages.ts`
// unchanged and hit Rust.
//
// Scope of the native flip (Stage 2): the SSE decode + event dispatch of pi's
// `stream()` is served by the Rust parser (`atilla_ai::api::anthropic`). That is
// exactly the path exercised through an injected transport (`options.client`),
// which is how pi's `anthropic-sse-parsing.test.ts` drives the parser. The
// request-shaping + auth + real HTTP path (no injected client) is not yet ported,
// so it delegates to pi's original implementation, and every other export
// (`streamSimple`, option/effort types) is re-exported from the original
// unchanged. The public runtime surface therefore stays byte-for-byte pi's.

export * from "./anthropic-messages.__pi_original__.ts";

import { anthropicParseSseStream } from "atilla-napi";
import type {
	AssistantMessage,
	AssistantMessageEvent,
	Context,
	Model,
	StopReason,
} from "../types.ts";
import { AssistantMessageEventStream } from "../utils/event-stream.ts";
import { stream as originalStream } from "./anthropic-messages.__pi_original__.ts";

// The option bag pi passes to `stream`; typed loosely here because the shim only
// reads the injected transport and the pass-through fields. The full type is
// re-exported from the original module above.
type StreamOptions = {
	client?: {
		messages: {
			create: (
				params: unknown,
				options?: unknown,
			) => { asResponse: () => Promise<Response> };
		};
	};
	signal?: AbortSignal;
	timeoutMs?: number;
	maxRetries?: number;
	[key: string]: unknown;
};

function emptyMessage(model: Model<"anthropic-messages">, errorMessage: string): AssistantMessage {
	return {
		role: "assistant",
		content: [],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "error" as StopReason,
		errorMessage,
		timestamp: Date.now(),
	};
}

/**
 * Anthropic `stream`, with the SSE decode + dispatch served by Rust when a
 * transport is injected (`options.client`). Without an injected client the call
 * needs pi's unported request-shaping + auth + HTTP, so it delegates to the
 * original implementation.
 */
export const stream = (
	model: Model<"anthropic-messages">,
	context: Context,
	options?: StreamOptions,
): AssistantMessageEventStream => {
	if (!options?.client) {
		return originalStream(model, context, options as never);
	}

	const client = options.client;
	const out = new AssistantMessageEventStream();

	(async () => {
		try {
			const requestOptions = {
				...(options.signal ? { signal: options.signal } : {}),
				...(options.timeoutMs !== undefined ? { timeout: options.timeoutMs } : {}),
				maxRetries: options.maxRetries ?? 0,
			};
			const response = await client.messages
				.create({ stream: true }, requestOptions)
				.asResponse();
			const body = await response.text();
			const parsed = JSON.parse(
				anthropicParseSseStream(body, JSON.stringify(model), false, Date.now()),
			) as { events: AssistantMessageEvent[]; message: AssistantMessage };
			for (const event of parsed.events) {
				out.push(event);
			}
			out.end(parsed.message);
		} catch (error) {
			const message = emptyMessage(
				model,
				error instanceof Error ? error.message : JSON.stringify(error),
			);
			out.push({ type: "error", reason: "error", error: message });
			out.end(message);
		}
	})();

	return out;
};
