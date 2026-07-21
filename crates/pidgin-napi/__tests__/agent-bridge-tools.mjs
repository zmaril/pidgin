// Bridge slice 2 — tool.execute + onUpdate through the bridge.
//
// Registers a JS tool whose `execute` (and, for one case, `prepareArguments`) is
// driven live by the Rust agent loop via the slice-2 `toolExecute` round-trip,
// and proves the three steward conditions for tool support:
//  - (a) a toolExecute blocking round-trip returns an AgentToolResult that the
//        loop threads into a toolResult message + tool_execution_end (isError
//        false);
//  - (b) emit_tool_update pushes an interim update mid-execute (the JS tool calls
//        onUpdate before it resolves) WITHOUT deadlocking the parked loop thread,
//        and every tool_execution_update lands before that tool's
//        tool_execution_end;
//  - (c) a tool that aborts mid-execute settles cleanly (no hang), surfacing an
//        error toolResult.
// The process must still exit 0 on its own — condition (C).
//
// Run: node __tests__/agent-bridge-tools.mjs   (after `npm run build:debug`)

import {
  AgentBridge,
  assert,
  getFailures,
  MODEL,
  runLoopBridge,
  sleep,
  toolMeta,
  toolThenDoneStreamFn,
} from "./_harness.mjs";

const userPrompt = { role: "user", content: "run tool", timestamp: 0 };

// Slice 2 adds `toolExecute` (→ tool.execute with an onUpdate that pushes via
// emitToolUpdate) and `prepareArguments` on top of the shared base kinds. A
// name→tool map is built from the run payload's tools.
function runLoop(bridge, { streamFn, convertToLlm, tools, onEvent }, payload) {
  const toolMap = new Map((tools ?? []).map((t) => [t.name, t]));
  return runLoopBridge(bridge, payload, {
    streamFn,
    convertToLlm,
    onEvent,
    handle: (kind, p) => {
      switch (kind) {
        case "toolExecute": {
          const tool = toolMap.get(p.toolName);
          if (!tool) throw new Error(`no tool ${p.toolName}`);
          // Route onUpdate back to Rust by closing over this toolCallId; the
          // push is fire-and-forget (no resolve, no round-trip).
          const onUpdate = (partial) =>
            bridge.emitToolUpdate(p.toolCallId, JSON.stringify(partial));
          const signal = { aborted: !!p.aborted };
          return tool.execute(p.toolCallId, p.args, signal, onUpdate);
        }
        case "prepareArguments": {
          const tool = toolMap.get(p.toolName);
          if (!tool || typeof tool.prepareArguments !== "function") return p.args;
          return tool.prepareArguments(p.args);
        }
        default:
          throw new Error(`unhandled kind: ${kind}`);
      }
    },
  });
}

// --- tests ----------------------------------------------------------------
async function testToolExecuteRoundTrip() {
  console.log("# (a) toolExecute blocking round-trip returns an AgentToolResult");
  const bridge = new AgentBridge();
  const executed = [];
  const echo = {
    name: "echo",
    label: "Echo",
    description: "Echo tool",
    parameters: {},
    async execute(_id, params) {
      executed.push(params.value);
      return {
        content: [{ type: "text", text: `echoed: ${params.value}` }],
        details: { value: params.value },
      };
    },
  };
  const events = [];
  const out = await runLoop(
    bridge,
    {
      streamFn: toolThenDoneStreamFn("tool-1", "echo", { value: "hello" }),
      convertToLlm: async (messages) => messages,
      tools: [echo],
      onEvent: (e) => events.push(e),
    },
    {
      prompts: [userPrompt],
      context: { systemPrompt: "", messages: [] },
      model: MODEL,
      tools: [toolMeta(echo)],
    },
  );

  assert(executed.length === 1 && executed[0] === "hello", `tool executed with JS args (got ${JSON.stringify(executed)})`);
  const toolResult = out.messages?.find((m) => m.role === "toolResult");
  assert(!!toolResult, "a toolResult message was assembled");
  const text = toolResult?.content?.find((c) => c.type === "text");
  assert(text?.text === "echoed: hello", `toolResult carries the JS-produced content (got ${JSON.stringify(text)})`);
  const end = events.find((e) => e.type === "tool_execution_end");
  assert(!!end, "tool_execution_end emitted");
  assert(end?.isError === false, `tool_execution_end isError is false (got ${end?.isError})`);
  assert(
    out.messages?.map((m) => m.role).join(",") === "user,assistant,toolResult,assistant",
    `full transcript roles (got ${out.messages?.map((m) => m.role).join(",")})`,
  );
}

async function testEmitToolUpdateNoDeadlock() {
  console.log("# (b) onUpdate mid-execute pushes interim updates without deadlock");
  const bridge = new AgentBridge();
  const streamedTool = {
    name: "stream",
    label: "Stream",
    description: "Streaming tool",
    parameters: {},
    async execute(_id, params, _signal, onUpdate) {
      // Fire two interim updates while the loop thread is parked on this
      // tool's toolExecute id, then settle. If emit_tool_update deadlocked the
      // parked thread this would hang and the run would never complete.
      onUpdate({ content: [{ type: "text", text: "partial 1" }], details: { step: 1 } });
      await sleep(2);
      onUpdate({ content: [{ type: "text", text: "partial 2" }], details: { step: 2 } });
      await sleep(2);
      return { content: [{ type: "text", text: `final: ${params.value}` }], details: { value: params.value } };
    },
  };
  const events = [];
  const out = await runLoop(
    bridge,
    {
      streamFn: toolThenDoneStreamFn("tool-1", "stream", { value: "go" }),
      convertToLlm: async (messages) => messages,
      tools: [streamedTool],
      onEvent: (e) => events.push(e),
    },
    {
      prompts: [userPrompt],
      context: { systemPrompt: "", messages: [] },
      model: MODEL,
      tools: [toolMeta(streamedTool)],
    },
  );

  const updateIdxs = events.flatMap((e, i) => (e.type === "tool_execution_update" ? [i] : []));
  const endIdx = events.findIndex((e) => e.type === "tool_execution_end");
  assert(!!out.messages, "run settled (no deadlock while onUpdate fired mid-execute)");
  assert(updateIdxs.length === 2, `two tool_execution_update events delivered (got ${updateIdxs.length})`);
  assert(endIdx >= 0, "tool_execution_end emitted");
  assert(
    updateIdxs.every((i) => i < endIdx),
    `every tool_execution_update precedes tool_execution_end (updates=${JSON.stringify(updateIdxs)}, end=${endIdx})`,
  );
  const firstUpdate = events.find((e) => e.type === "tool_execution_update");
  assert(
    firstUpdate?.partialResult?.content?.[0]?.text === "partial 1",
    `interim update carries the JS partial result (got ${JSON.stringify(firstUpdate?.partialResult)})`,
  );
  const finalResult = out.messages?.find((m) => m.role === "toolResult");
  const finalText = finalResult?.content?.find((c) => c.type === "text");
  assert(finalText?.text === "final: go", `final toolResult is the resolved value (got ${JSON.stringify(finalText)})`);
}

async function testToolAbortSettlesCleanly() {
  console.log("# (c) a tool that aborts mid-execute settles cleanly (no hang)");
  const bridge = new AgentBridge();
  const abortingTool = {
    name: "hang",
    label: "Hang",
    description: "Never resolves on its own",
    parameters: {},
    // Never returns; the abort must be what unblocks the parked Rust thread.
    execute: () =>
      new Promise(() => {
        bridge.abort();
      }),
  };
  const events = [];
  const out = await runLoop(
    bridge,
    {
      streamFn: toolThenDoneStreamFn("tool-1", "hang", { value: "x" }),
      convertToLlm: async (messages) => messages,
      tools: [abortingTool],
      onEvent: (e) => events.push(e),
    },
    {
      prompts: [userPrompt],
      context: { systemPrompt: "", messages: [] },
      model: MODEL,
      tools: [toolMeta(abortingTool)],
    },
  );

  assert(!!out.messages, "run settled after abort mid-execute (no deadlock)");
  const toolResult = out.messages?.find((m) => m.role === "toolResult");
  const text = toolResult?.content?.find((c) => c.type === "text");
  assert(
    !toolResult || /abort/i.test(text?.text ?? ""),
    `aborted tool surfaces an error/aborted result if any (got ${JSON.stringify(text)})`,
  );
  // A late emitToolUpdate for the now-unregistered id must be a harmless no-op.
  bridge.emitToolUpdate("tool-1", JSON.stringify({ content: [], details: {} }));
  assert(true, "late emitToolUpdate after abort is a no-op (did not throw)");
}

async function testPrepareArgumentsSeam() {
  console.log("# (d) prepareArguments rewrites raw args before execute");
  const bridge = new AgentBridge();
  const executed = [];
  const editTool = {
    name: "edit",
    label: "Edit",
    description: "Edit tool",
    parameters: {},
    prepareArguments(args) {
      if (!args || typeof args !== "object") return args;
      const input = args;
      if (typeof input.oldText !== "string" || typeof input.newText !== "string") return args;
      return { edits: [...(input.edits ?? []), { oldText: input.oldText, newText: input.newText }] };
    },
    async execute(_id, params) {
      executed.push(params.edits);
      return { content: [{ type: "text", text: `edited ${params.edits.length}` }], details: { count: params.edits.length } };
    },
  };
  await runLoop(
    bridge,
    {
      streamFn: toolThenDoneStreamFn("tool-1", "edit", { oldText: "before", newText: "after" }),
      convertToLlm: async (messages) => messages,
      tools: [editTool],
    },
    {
      prompts: [userPrompt],
      context: { systemPrompt: "", messages: [] },
      model: MODEL,
      tools: [toolMeta(editTool)],
    },
  );
  assert(
    JSON.stringify(executed) === JSON.stringify([[{ oldText: "before", newText: "after" }]]),
    `execute saw the prepared args (got ${JSON.stringify(executed)})`,
  );
}

async function main() {
  await testToolExecuteRoundTrip();
  await testEmitToolUpdateNoDeadlock();
  await testToolAbortSettlesCleanly();
  await testPrepareArgumentsSeam();

  console.log("");
  if (getFailures() === 0) console.log("SLICE 2: ALL TOOL-BRIDGE CHECKS PASSED");
  else {
    console.log(`SLICE 2: ${getFailures()} CHECK(S) FAILED`);
    process.exitCode = 1;
  }
  // Clean exit with no explicit process.exit(): condition (C).
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
