// straitjacket-allow-file:duplication — generator scaffolding; the parallel
// per-entry driver blocks are intentionally uniform, not shared logic.
//
// REAL v3 golden-session generator for the bit-exact Rust port of pi's session
// JSONL writers. This drives pi's OWN canonical code paths (Node 22 strips TS
// types natively, so pi source `.ts` is imported directly) and dumps the exact
// bytes those writers emit:
//
//   1. agent-core   packages/agent/src/harness/session/{session,jsonl-storage}.ts
//                   -> the `Session` class + `JsonlSessionStorage` (11-variant
//                      SessionTreeEntry union, including the persisted `leaf`).
//   2. coding-agent packages/coding-agent/src/core/session-manager.ts
//                   -> the `SessionManager` (9-variant SessionEntry union; NO
//                      persisted leaf line; custom / custom_message put their
//                      type-specific fields BEFORE id/parentId/timestamp).
//
// Determinism: pi's writers stamp `new Date().toISOString()` on every entry and
// derive ids from `uuidv7()` (which reads `Date.now()` and `crypto.getRandom-
// Values`). We install a fake clock (bumped 1s between top-level appends, so
// each entry gets a distinct, stable timestamp) and a seeded PRNG so the golden
// is byte-reproducible across runs. Everything else is 100% pi's real code.
//
// pi upstream pin: vendor/pi submodule @ 3da591a (pi v0.80.10).
//
// Run from anywhere:  node crates/atilla-agent/tests/gen/generate_sessions.mjs
//
// Outputs:
//   crates/atilla-agent/tests/fixtures/v3-pi-generated.jsonl
//   crates/atilla-coding/tests/fixtures/v3-pi-coding-generated.jsonl
// and prints a structural DIFF against the hand-authored
//   crates/atilla-agent/tests/fixtures/v3-all-line-types.jsonl

import { mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..", "..", ".."); // crates/atilla-agent/tests/gen -> repo root
const piRoot = join(repoRoot, "vendor", "pi");

// ---------------------------------------------------------------------------
// Deterministic clock + RNG. Must be installed BEFORE importing pi modules so
// uuidv7's module-level state and every `new Date()` see the fake environment.
// ---------------------------------------------------------------------------
const BASE = Date.UTC(2026, 6, 18, 0, 0, 0, 0); // 2026-07-18T00:00:00.000Z
const clock = { ms: BASE };
const RealDate = Date;
// biome-ignore lint: intentional global override for deterministic generation
globalThis.Date = class extends RealDate {
	constructor(...args) {
		if (args.length === 0) super(clock.ms);
		else super(...args);
	}
	static now() {
		return clock.ms;
	}
};
let seed = 0x1234abcd;
const nextByte = () => {
	// mulberry32-ish LCG; only needs to be deterministic, not cryptographic.
	seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
	return (seed >>> 24) & 0xff;
};
Object.defineProperty(globalThis, "crypto", {
	configurable: true,
	writable: true,
	value: {
		getRandomValues: (arr) => {
			for (let i = 0; i < arr.length; i++) arr[i] = nextByte();
			return arr;
		},
	},
});
/** Advance the fake clock by one second and return `this` for chaining. */
function tick() {
	clock.ms += 1000;
}

// ---------------------------------------------------------------------------
// pi imports (after fake env is installed).
// ---------------------------------------------------------------------------
const { Session } = await import(join(piRoot, "packages/agent/src/harness/session/session.ts"));
const { JsonlSessionStorage } = await import(join(piRoot, "packages/agent/src/harness/session/jsonl-storage.ts"));
const { SessionManager } = await import(join(piRoot, "packages/coding-agent/src/core/session-manager.ts"));

// ---------------------------------------------------------------------------
// In-memory FileSystem satisfying the Pick<FileSystem,...> that
// JsonlSessionStorage consumes (readTextFile / readTextLines / writeFile /
// appendFile). Returns pi's Result<T, FileError> shape.
// ---------------------------------------------------------------------------
class MemFS {
	constructor() {
		this.files = new Map();
	}
	async readTextFile(path) {
		const c = this.files.get(path);
		return c === undefined ? { ok: false, error: { code: "not_found", message: `not found: ${path}` } } : { ok: true, value: c };
	}
	async readTextLines(path, options) {
		const c = this.files.get(path) ?? "";
		let lines = c.split("\n");
		if (options?.maxLines) lines = lines.slice(0, options.maxLines);
		return { ok: true, value: lines };
	}
	async writeFile(path, content) {
		this.files.set(path, typeof content === "string" ? content : Buffer.from(content).toString("utf8"));
		return { ok: true, value: undefined };
	}
	async appendFile(path, content) {
		const s = typeof content === "string" ? content : Buffer.from(content).toString("utf8");
		this.files.set(path, (this.files.get(path) ?? "") + s);
		return { ok: true, value: undefined };
	}
}

// ---------------------------------------------------------------------------
// (1) agent-core corpus — drive the real Session + JsonlSessionStorage.
// ---------------------------------------------------------------------------
async function generateAgentCore() {
	const fs = new MemFS();
	const path = "/session.jsonl";
	clock.ms = BASE;
	const storage = await JsonlSessionStorage.create(fs, path, {
		cwd: "/workspace/proj",
		sessionId: "019de8c2-de29-73e9-ae0c-e134db34c447",
		parentSessionPath: "/home/u/.pi/agent/sessions/--workspace-proj--/2026-07-18T00-00-00-000Z_parent.jsonl",
		metadata: { profile: "reviewer", tags: ["a", "b"] },
	});
	const session = new Session(storage);

	// user message
	tick();
	const userId = await session.appendMessage({ role: "user", content: [{ type: "text", text: "hello" }], timestamp: 1 });
	// thinking level + model + active tools changes
	tick();
	await session.appendThinkingLevelChange("high");
	tick();
	await session.appendModelChange("anthropic", "claude-opus-4-5");
	tick();
	await session.appendActiveToolsChange(["read", "bash"]);
	// assistant message carrying a toolCall content block (pi's real assistant shape)
	tick();
	await session.appendMessage({
		role: "assistant",
		content: [
			{ type: "text", text: "let me look" },
			{ type: "toolCall", id: "call_1", name: "read", arguments: { path: "/a.txt" } },
		],
		api: "anthropic-messages",
		provider: "anthropic",
		model: "claude-opus-4-5",
		usage: { input: 10, output: 5, cacheRead: 0, cacheWrite: 0, totalTokens: 15, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
		stopReason: "toolUse",
		timestamp: 5,
	});
	// tool RESULT is a separate message in pi's model (role:"toolResult"), NOT a
	// content block inside the assistant message.
	tick();
	await session.appendMessage({ role: "toolResult", toolCallId: "call_1", toolName: "read", content: [{ type: "text", text: "file body" }], isError: false, timestamp: 6 });
	// compaction — full detail (details + fromHook present)
	tick();
	await session.appendCompaction("did stuff", userId, 4096, { reason: "auto" }, true);
	// compaction — minimal (details + fromHook omitted)
	tick();
	await session.appendCompaction("min", userId, 10);
	// custom — with data
	tick();
	await session.appendCustomEntry("chat_message", { text: "note" });
	// custom — without data (data omitted)
	tick();
	await session.appendCustomEntry("marker");
	// custom_message — display true, details present
	tick();
	await session.appendCustomMessageEntry("banner", "hello world", true, { ok: true });
	// label set
	tick();
	await session.appendLabel(userId, "checkpoint");
	// label cleared (label omitted -> undefined dropped by JSON.stringify)
	tick();
	await session.appendLabel(userId, undefined);
	// session_info with name
	tick();
	await session.appendSessionName("my session");
	// branch_summary via moveTo(entryId, summary). NOTE: moveTo FIRST calls
	// storage.setLeafId(entryId) which persists a `leaf` line, THEN appends the
	// branch_summary — so this single call emits TWO lines (leaf, then
	// branch_summary). This is the only public agent-core path to a
	// branch_summary entry.
	tick();
	await session.moveTo(userId, { summary: "branch summary", details: { k: 1 }, fromHook: false });
	// explicit persisted leaf line: target a string id, then cleared/null.
	tick();
	await storage.setLeafId(userId);
	tick();
	await storage.setLeafId(null);

	return fs.files.get(path);
}

// ---------------------------------------------------------------------------
// (2) coding-agent corpus — drive the real SessionManager (persisted).
// ---------------------------------------------------------------------------
function generateCodingAgent() {
	clock.ms = BASE;
	const dir = mkdtempSync(join(tmpdir(), "pi-coding-"));
	const mgr = SessionManager.create("/workspace/proj", dir, { id: "019de8c2-de29-73e9-ae0c-e134db34c447" });

	// SessionManager buffers entries until the first assistant message, then
	// flushes header + all buffered entries. Emit user + assistant first.
	tick();
	const userId = mgr.appendMessage({ role: "user", content: [{ type: "text", text: "hello" }], timestamp: 1 });
	tick();
	mgr.appendMessage({
		role: "assistant",
		content: [
			{ type: "text", text: "let me look" },
			{ type: "toolCall", id: "call_1", name: "read", arguments: { path: "/a.txt" } },
		],
		api: "anthropic-messages",
		provider: "anthropic",
		model: "claude-opus-4-5",
		usage: { input: 10, output: 5, cacheRead: 0, cacheWrite: 0, totalTokens: 15, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
		stopReason: "toolUse",
		timestamp: 2,
	});
	tick();
	mgr.appendMessage({ role: "toolResult", toolCallId: "call_1", toolName: "read", content: [{ type: "text", text: "file body" }], isError: false, timestamp: 3 });
	tick();
	mgr.appendThinkingLevelChange("high");
	tick();
	mgr.appendModelChange("anthropic", "claude-opus-4-5");
	tick();
	mgr.appendCompaction("did stuff", userId, 4096, { reason: "auto" }, true);
	tick();
	mgr.appendCompaction("min", userId, 10);
	tick();
	mgr.appendCustomEntry("chat_message", { text: "note" }); // custom WITH data
	tick();
	mgr.appendCustomEntry("marker"); // custom WITHOUT data
	tick();
	mgr.appendCustomMessageEntry("banner", "hello world", true, { ok: true });
	tick();
	mgr.appendLabelChange(userId, "checkpoint");
	tick();
	mgr.appendLabelChange(userId, undefined); // cleared
	tick();
	mgr.appendSessionInfo("my session");
	tick();
	mgr.branchWithSummary(userId, "branch summary", { k: 1 }, false); // standalone branch_summary, no leaf line

	const file = mgr.getSessionFile();
	const bytes = readFileSync(file, "utf8");
	// clean up temp dir contents
	for (const f of readdirSync(dir)) {
		try {
			require("node:fs").unlinkSync(join(dir, f));
		} catch {}
	}
	return bytes;
}

// ---------------------------------------------------------------------------
// Structural DIFF helper. The session WRITER's parity surface is the top-level
// entry ENVELOPE: which keys appear, in what order, and which are omitted vs
// null vs present. Payload values the writer stores verbatim (id references,
// timestamps, and the opaque `message`/`data`/`details`/`content`/`metadata`
// blobs) are NOT the writer's concern, so we mask their VALUES to a stable
// placeholder while KEEPING the key present. Scalar discriminants that the
// writer itself sets (thinkingLevel, provider, tokensBefore, display, fromHook,
// label, ...) are left intact so null-vs-omitted differences still surface.
// After masking we compare the SET of distinct envelope shapes per entry type,
// so corpus ordering / multiplicity never creates false positives — only a
// shape present on one side and absent on the other is reported.
// ---------------------------------------------------------------------------
const VALUE_NOISE = new Set([
	"id",
	"parentId",
	"timestamp",
	"targetId", // leaf/label reference (null preserved below)
	"firstKeptEntryId", // compaction id reference
	"fromId", // branch_summary id reference
	"message", // opaque AgentMessage payload (stored verbatim)
	"data", // opaque custom payload
	"details", // opaque extension payload
	"content", // opaque custom_message payload
	"metadata", // opaque header payload
]);
function maskLine(line) {
	const obj = JSON.parse(line);
	const out = {};
	for (const [k, v] of Object.entries(obj)) {
		if (VALUE_NOISE.has(k) && v !== null) out[k] = `<${k}>`;
		else out[k] = v; // null stays null; scalar discriminants stay intact
	}
	return JSON.stringify(out);
}

function shapesByType(text) {
	const byType = new Map();
	for (const l of text.trim().split("\n")) {
		const t = JSON.parse(l).type;
		if (!byType.has(t)) byType.set(t, new Set());
		byType.get(t).add(maskLine(l));
	}
	return byType;
}

function structuralDiff(generated, handAuthored) {
	const gen = shapesByType(generated);
	const hand = shapesByType(handAuthored);
	const findings = [];
	const types = new Set([...gen.keys(), ...hand.keys()]);
	for (const t of types) {
		const g = gen.get(t) ?? new Set();
		const h = hand.get(t) ?? new Set();
		for (const shape of h) {
			if (!g.has(shape)) {
				findings.push(`  [${t}] envelope shape in HAND fixture with no matching pi shape:\n      hand: ${shape}`);
			}
		}
		for (const shape of g) {
			if (!h.has(shape)) {
				findings.push(`  [${t}] envelope shape in PI golden with no matching hand shape:\n      pi:   ${shape}`);
			}
		}
	}
	return findings;
}

// ---------------------------------------------------------------------------
// Run.
// ---------------------------------------------------------------------------
const agentOut = join(repoRoot, "crates/atilla-agent/tests/fixtures/v3-pi-generated.jsonl");
const codingOut = join(repoRoot, "crates/atilla-coding/tests/fixtures/v3-pi-coding-generated.jsonl");
mkdirSync(dirname(agentOut), { recursive: true });
mkdirSync(dirname(codingOut), { recursive: true });

const agentBytes = await generateAgentCore();
writeFileSync(agentOut, agentBytes);
console.log(`wrote ${agentOut} (${agentBytes.trim().split("\n").length} lines)`);

const codingBytes = generateCodingAgent();
writeFileSync(codingOut, codingBytes);
console.log(`wrote ${codingOut} (${codingBytes.trim().split("\n").length} lines)`);

const handAuthored = readFileSync(join(repoRoot, "crates/atilla-agent/tests/fixtures/v3-all-line-types.jsonl"), "utf8");
console.log("\n=== structural DIFF: pi agent-core golden vs hand-authored v3-all-line-types.jsonl ===");
const findings = structuralDiff(agentBytes, handAuthored);
if (findings.length === 0) {
	console.log("  byte-identical for overlapping types (after masking id/parentId/timestamp/targetId).");
} else {
	console.log(`  ${findings.length} structural finding(s):`);
	for (const f of findings) console.log(f);
}
