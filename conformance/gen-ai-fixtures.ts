// straitjacket-allow-file:duplication — each fixture literal below is a distinct
// boundary-type sample; their object shapes necessarily rhyme (they share the pi
// content-block/message field vocabulary) but every one is load-bearing.
//
// Fixture generator for crates/atilla-ai boundary-type round-trip tests.
//
// Run with:  bun run conformance/gen-ai-fixtures.ts
//
// Authoritative JSON is produced two ways, both from pi's own source at the
// pinned submodule (vendor/pi, commit 3da591ab):
//   1. Executed runtime: pi's faux builders (providers/faux.ts) and its real
//      streaming loop (createFauxCore().stream) emit content blocks, messages,
//      and the AssistantMessageEvent union; pi's calculateCost (models.ts) emits
//      the cost math. These are pi's actual code paths, not transcriptions.
//   2. Type-checked literals: the values pi has no runtime constructor for
//      (ImageContent, UserMessage, ToolResultMessage, Model) are written as
//      literals annotated with pi's own exported types from vendor/pi's
//      src/types.ts, so `bun`/`tsc` validate their field names and shapes.
//
// Output lands in crates/atilla-ai/tests/fixtures/ and is committed.
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { calculateCost } from "../vendor/pi/packages/ai/src/models.ts";
import {
	createFauxCore,
	fauxAssistantMessage,
	fauxText,
	fauxThinking,
	fauxToolCall,
} from "../vendor/pi/packages/ai/src/providers/faux.ts";
import type {
	AssistantMessageEvent,
	ImageContent,
	Model,
	ModelCost,
	TextContent,
	ThinkingContent,
	ToolCall,
	ToolResultMessage,
	UserMessage,
	Usage,
} from "../vendor/pi/packages/ai/src/types.ts";

const here = fileURLToPath(new URL(".", import.meta.url));
const outDir = join(here, "..", "crates", "atilla-ai", "tests", "fixtures");

function write(name: string, value: unknown): void {
	const path = join(outDir, name);
	mkdirSync(dirname(path), { recursive: true });
	writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

// --- Content blocks -------------------------------------------------------
// Executed pi runtime builders (providers/faux.ts:49-64).
const text: TextContent = fauxText("hello world");
const textWithSignature: TextContent = { type: "text", text: "final", textSignature: "sig-123" };
const thinking: ThinkingContent = fauxThinking("let me think");
const thinkingRedacted: ThinkingContent = {
	type: "thinking",
	thinking: "",
	thinkingSignature: "encrypted-payload",
	redacted: true,
};
const image: ImageContent = { type: "image", data: "aGVsbG8=", mimeType: "image/png" };
const toolCall: ToolCall = fauxToolCall("echo", { text: "hi" }, { id: "tool-fixed-1" });
const toolCallWithThought: ToolCall = {
	type: "toolCall",
	id: "tool-fixed-2",
	name: "search",
	arguments: { query: "rust serde", limit: 5 },
	thoughtSignature: "google-thought-sig",
};

write("content_text.json", text);
write("content_text_signature.json", textWithSignature);
write("content_thinking.json", thinking);
write("content_thinking_redacted.json", thinkingRedacted);
write("content_image.json", image);
write("content_toolcall.json", toolCall);
write("content_toolcall_thought.json", toolCallWithThought);
// Provider-boundary forward-compat: an unknown block type must survive as the
// serde `Unknown` catch-all variant.
write("content_unknown.json", { type: "video", url: "https://example.com/v.mp4", frames: 3 });

// --- Messages -------------------------------------------------------------
const userString: UserMessage = { role: "user", content: "what is 2+2?", timestamp: 1700000000000 };
const userBlocks: UserMessage = {
	role: "user",
	content: [
		{ type: "text", text: "describe this" },
		{ type: "image", data: "aW1n", mimeType: "image/jpeg" },
	],
	timestamp: 1700000000001,
};
// Executed pi runtime builder (providers/faux.ts:73-95).
const assistant = fauxAssistantMessage(
	[fauxThinking("plan"), fauxToolCall("run", { cmd: "ls" }, { id: "tool-fixed-3" }), fauxText("done")],
	{ stopReason: "toolUse", responseId: "resp-1", timestamp: 1700000000002 },
);
const toolResult: ToolResultMessage = {
	role: "toolResult",
	toolCallId: "tool-fixed-3",
	toolName: "run",
	content: [{ type: "text", text: "file-a\nfile-b" }],
	isError: false,
	timestamp: 1700000000003,
};

write("message_user_string.json", userString);
write("message_user_blocks.json", userBlocks);
write("message_assistant.json", assistant);
write("message_tool_result.json", toolResult);

// --- Usage ----------------------------------------------------------------
const usage: Usage = {
	input: 1000,
	output: 500,
	cacheRead: 200,
	cacheWrite: 100,
	cacheWrite1h: 40,
	reasoning: 120,
	totalTokens: 1800,
	cost: { input: 0.003, output: 0.0075, cacheRead: 0.00006, cacheWrite: 0.000465, total: 0.011025 },
};
write("usage.json", usage);

// --- Streaming event union ------------------------------------------------
// Drive pi's real streaming loop so the emitted AssistantMessageEvent sequence
// is authoritative. Fixed token sizes make each block emit a single delta; no
// tokensPerSecond means no timer delay.
async function collectEvents(): Promise<AssistantMessageEvent[]> {
	const core = createFauxCore({ api: "faux", provider: "faux", tokenSize: { min: 4096, max: 4096 } });
	core.setResponses([
		fauxAssistantMessage(
			[fauxThinking("reasoning"), fauxToolCall("lookup", { id: 7 }, { id: "tool-fixed-4" }), fauxText("answer")],
			{ stopReason: "toolUse", timestamp: 1700000000004 },
		),
	]);
	const events: AssistantMessageEvent[] = [];
	for await (const event of core.stream(core.getModel(), {
		messages: [{ role: "user", content: "go", timestamp: 1700000000000 }],
	})) {
		events.push(event);
	}
	return events;
}

const events = await collectEvents();
write("events.json", events);

// A hand-built error event (terminal error variant) for round-trip coverage.
const errorEvent: AssistantMessageEvent = {
	type: "error",
	reason: "error",
	error: fauxAssistantMessage("boom", { stopReason: "error", errorMessage: "kaboom", timestamp: 1700000000005 }),
};
write("event_error.json", errorEvent);

// --- Model with cost + per-provider compat map ----------------------------
const cost: ModelCost = {
	input: 3,
	output: 15,
	cacheRead: 0.3,
	cacheWrite: 3.75,
	tiers: [{ inputTokensAbove: 200000, input: 6, output: 22.5, cacheRead: 0.6, cacheWrite: 7.5 }],
};
const model: Model<"anthropic-messages"> = {
	id: "claude-opus-4-8",
	name: "Claude Opus 4.8",
	api: "anthropic-messages",
	provider: "anthropic",
	baseUrl: "https://api.anthropic.com",
	reasoning: true,
	thinkingLevelMap: { off: null, low: "low", high: "high" },
	input: ["text", "image"],
	cost,
	contextWindow: 200000,
	maxTokens: 64000,
	headers: { "anthropic-beta": "context-1m" },
	compat: {
		supportsEagerToolInputStreaming: true,
		supportsLongCacheRetention: true,
		supportsTemperature: false,
	},
};
write("model.json", model);

// --- calculateCost cases (executed against pi's models.ts) ----------------
type CostCase = { name: string; model: Model<"anthropic-messages">; usage: Usage; cost: Usage["cost"] };

function costCase(name: string, m: Model<"anthropic-messages">, u: Usage): CostCase {
	// calculateCost mutates u.cost in place and returns it.
	const computed = calculateCost(m, structuredClone(u));
	return { name, model: m, usage: { ...u, cost: computed }, cost: computed };
}

const zeroCost: Usage["cost"] = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 };
const cases: CostCase[] = [
	costCase("base_rates_with_1h_cache", model, {
		input: 1000,
		output: 500,
		cacheRead: 200,
		cacheWrite: 100,
		cacheWrite1h: 40,
		totalTokens: 1800,
		cost: { ...zeroCost },
	}),
	costCase("tiered_above_threshold", model, {
		input: 300000,
		output: 1000,
		cacheRead: 0,
		cacheWrite: 0,
		totalTokens: 301000,
		cost: { ...zeroCost },
	}),
	costCase("no_cache", model, {
		input: 500,
		output: 250,
		cacheRead: 0,
		cacheWrite: 0,
		totalTokens: 750,
		cost: { ...zeroCost },
	}),
];
write("cost_cases.json", cases);

console.log(`wrote fixtures to ${outDir}`);
console.log(`events: ${events.map((e) => e.type).join(", ")}`);
