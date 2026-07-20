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

import {
  AgentBridge,
  assert,
  assistantText,
  fakeStream,
  getFailures,
  MODEL,
  runLoopBridge,
} from "./_harness.mjs";

const userPrompt = { role: "user", content: "hello", timestamp: 0 };

// Slice 1 needs only the two base kinds (streamFn + convertToLlm), so the shared
// harness helper handles the whole dispatch.
function runLoop(bridge, { streamFn, convertToLlm, onEvent }, payload) {
  return runLoopBridge(bridge, payload, { streamFn, convertToLlm, onEvent });
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

// Drive one identity-convert turn with the given streamFn and return the
// assembled assistant message (the terminal error/abort surface both tests probe).
async function runToAssistant(bridge, streamFn) {
  const out = await runLoop(
    bridge,
    { streamFn, convertToLlm: async (messages) => messages },
    { prompts: [userPrompt], context: { systemPrompt: "", messages: [] }, model: MODEL },
  );
  return out.messages?.find((m) => m.role === "assistant");
}

async function testStreamFnThrowsSurfaces() {
  console.log("# (A) a throwing streamFn yields a terminal error message, not a hang");
  const bridge = new AgentBridge();
  const assistant = await runToAssistant(bridge, async () => {
    throw new Error("provider exploded");
  });
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
  // Never resolves on its own; abort must be what unblocks the Rust thread.
  const assistant = await runToAssistant(bridge, () =>
    new Promise(() => {
      bridge.abort();
    }),
  );
  assert(!!assistant, "run settled after abort (no deadlock)");
  assert(assistant?.stopReason === "aborted", `assistant stopReason is 'aborted' (got ${assistant?.stopReason})`);
}

async function main() {
  await testSingleTextTurn();
  await testStreamFnThrowsSurfaces();
  await testAbortMidRequest();

  console.log("");
  if (getFailures() === 0) console.log("STEP B: ALL AGENT-LOOP CHECKS PASSED");
  else {
    console.log(`STEP B: ${getFailures()} CHECK(S) FAILED`);
    process.exitCode = 1;
  }
  // Clean exit with no explicit process.exit(): condition (C).
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
