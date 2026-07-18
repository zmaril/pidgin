// straitjacket-allow-file:duplication
// Bridge slice 1 — STEP B: run the Rust agent loop through the bridge.
//
// Registers a JS `streamFn` (returns an eager StreamResult) and a `convertToLlm`
// hook, drives `run_agent_loop` on a dedicated Rust thread through the native
// `AgentBridge`, and asserts the assembled outcome messages for the simplest
// single-text-turn case. Also proves the loop-level steward conditions:
//  - (A) a JS streamFn that throws → the loop returns a terminal error message
//        (clean surface, not a hang);
//  - (B) abort mid-request unblocks the parked loop thread and the run settles.
// The process must still exit 0 on its own — condition (C).
//
// Run: node __tests__/agent-bridge-loop.mjs   (after `npm run build:debug`)

import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const { AgentBridge } = require(join(here, "..", "index.js"));

let failures = 0;
function assert(cond, msg) {
  if (cond) console.log(`  ok - ${msg}`);
  else {
    failures += 1;
    console.log(`  NOT OK - ${msg}`);
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// --- fixtures -------------------------------------------------------------
const MODEL = {
  id: "faux-model",
  name: "Faux Model",
  api: "faux",
  provider: "faux",
  baseUrl: "http://localhost",
  reasoning: false,
  input: ["text"],
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
  contextWindow: 128000,
  maxTokens: 4096,
};

const zeroUsage = {
  input: 0,
  output: 0,
  cacheRead: 0,
  cacheWrite: 0,
  totalTokens: 0,
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
};

function assistantText(text, stopReason = "stop") {
  return {
    role: "assistant",
    content: [{ type: "text", text }],
    api: "faux",
    provider: "faux",
    model: "faux-model",
    usage: zeroUsage,
    stopReason,
    timestamp: 0,
  };
}

const userPrompt = { role: "user", content: "hello", timestamp: 0 };

// --- the JS side of the envelope protocol (the future _bridge/dispatcher.ts) --
// Routes bridge kinds to the injected closures, draining `streamFn`'s async
// stream into the eager StreamResult JSON the Rust loop consumes.
function runLoop(bridge, { streamFn, convertToLlm, onEvent }, payload) {
  return new Promise((resolve, reject) => {
    const dispatcher = (envelopeJson) => {
      let env;
      try {
        env = JSON.parse(envelopeJson);
      } catch (e) {
        reject(e);
        return;
      }
      const { id, kind, payload: p } = env;
      if (kind === "__complete__") {
        bridge.join();
        resolve(p);
        return;
      }
      if (kind === "event") {
        if (onEvent) onEvent(p);
        return; // fire-and-forget: no resolve
      }
      const handle = async () => {
        switch (kind) {
          case "streamFn": {
            // JS-async → eager: fully drain the stream, then re-present it.
            const stream = await streamFn(p.model, p.context, p.options);
            const events = [];
            for await (const ev of stream) events.push(ev);
            const message = await stream.result();
            return { events, message };
          }
          case "convertToLlm":
            return await convertToLlm(p.messages);
          default:
            throw new Error(`unhandled kind: ${kind}`);
        }
      };
      handle()
        .then((result) => bridge.resolveBridge(id, JSON.stringify(result ?? null)))
        .catch((e) =>
          bridge.resolveBridgeError(id, JSON.stringify({ __bridge_error: String(e?.message ?? e) })),
        );
    };
    bridge.run(dispatcher, JSON.stringify(payload));
  });
}

// A tiny async stream (mirrors pi's AssistantMessageEventStream contract):
// async-iterable of events + a `.result()` promise for the final message.
function fakeStream(events, message) {
  return {
    async *[Symbol.asyncIterator]() {
      for (const ev of events) {
        await sleep(1); // force real async scheduling between events
        yield ev;
      }
    },
    async result() {
      await sleep(1);
      return message;
    },
  };
}

// --- tests ----------------------------------------------------------------
async function testSingleTextTurn() {
  console.log("# single text turn: assembled messages come back from the loop");
  const bridge = new AgentBridge();
  const events = [];
  const final = assistantText("hi there");
  const out = await runLoop(
    bridge,
    {
      streamFn: async () =>
        fakeStream(
          [
            { type: "start", partial: assistantText("") },
            { type: "text_delta", contentIndex: 0, delta: "hi there", partial: assistantText("hi there") },
            { type: "done", reason: "stop", message: final },
          ],
          final,
        ),
      convertToLlm: async (messages) => messages, // identity: prompts already valid
      onEvent: (e) => events.push(e.type),
    },
    {
      prompts: [userPrompt],
      context: { systemPrompt: "be brief", messages: [] },
      model: MODEL,
    },
  );

  assert(Array.isArray(out.messages), "run completed with a messages array");
  assert(out.messages.length === 2, `two messages assembled (got ${out.messages?.length})`);
  assert(out.messages[0].role === "user" && out.messages[0].content === "hello", "prompt preserved as message 0");
  assert(out.messages[1].role === "assistant", "assistant message is message 1");
  assert(
    out.messages[1].content?.[0]?.text === "hi there",
    `assistant text is the JS-produced value (got ${JSON.stringify(out.messages[1].content)})`,
  );
  assert(
    events[0] === "agent_start" && events.includes("message_end") && events[events.length - 1] === "agent_end",
    `event stream framed by agent_start..agent_end (got ${JSON.stringify(events)})`,
  );
}

async function testStreamFnThrowsSurfaces() {
  console.log("# (A) a throwing streamFn yields a terminal error message, not a hang");
  const bridge = new AgentBridge();
  const out = await runLoop(
    bridge,
    {
      streamFn: async () => {
        throw new Error("provider exploded");
      },
      convertToLlm: async (messages) => messages,
    },
    { prompts: [userPrompt], context: { systemPrompt: "", messages: [] }, model: MODEL },
  );
  const assistant = out.messages?.find((m) => m.role === "assistant");
  assert(!!assistant, "loop returned an assistant message despite the throw");
  assert(assistant?.stopReason === "error", `assistant stopReason is 'error' (got ${assistant?.stopReason})`);
  assert(
    assistant?.errorMessage === "provider exploded",
    `error message surfaced from JS (got ${assistant?.errorMessage})`,
  );
}

async function testAbortMidRequest() {
  console.log("# (B) abort mid-request unblocks the parked loop thread");
  const bridge = new AgentBridge();
  const out = await runLoop(
    bridge,
    {
      // Never resolves on its own; abort must be what unblocks the Rust thread.
      streamFn: () =>
        new Promise(() => {
          bridge.abort();
        }),
      convertToLlm: async (messages) => messages,
    },
    { prompts: [userPrompt], context: { systemPrompt: "", messages: [] }, model: MODEL },
  );
  const assistant = out.messages?.find((m) => m.role === "assistant");
  assert(!!assistant, "run settled after abort (no deadlock)");
  assert(assistant?.stopReason === "aborted", `assistant stopReason is 'aborted' (got ${assistant?.stopReason})`);
}

async function main() {
  await testSingleTextTurn();
  await testStreamFnThrowsSurfaces();
  await testAbortMidRequest();

  console.log("");
  if (failures === 0) console.log("STEP B: ALL AGENT-LOOP CHECKS PASSED");
  else {
    console.log(`STEP B: ${failures} CHECK(S) FAILED`);
    process.exitCode = 1;
  }
  // Clean exit with no explicit process.exit(): condition (C).
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
