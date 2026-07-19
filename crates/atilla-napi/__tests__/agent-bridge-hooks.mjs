// straitjacket-allow-file:duplication
// Bridge slice 3 — the eight loop hooks through the bridge.
//
// Each hook is a BLOCKING Rust→JS→Rust round-trip over the same NonBlocking-TSFN
// + resolve-channel primitive slices 1–2 use. This harness registers the test's
// JS hook closures into a dispatcher (the _bridge/dispatcher.ts shape) and drives
// the Rust agent loop, proving a representative round-trip per hook:
//  - (a) transformContext mutates the context: it prunes the transcript and the
//        pruned messages are what convertToLlm (and thus the LLM turn) sees;
//  - (b) shouldStopAfterTurn returning true stops the loop after the current turn
//        (a single LLM turn; the second would-be turn never runs);
//  - (c) beforeToolCall + afterToolCall are observed for a tool call, and
//        afterToolCall returning { terminate: true } stops the batch;
//  - (d) beforeToolCall returning { block: true } prevents execution (error
//        result, tool never runs);
//  - (e) getSteeringMessages injects a queued message that reaches the next turn;
//  - (f) abort DURING a hook settles cleanly (no deadlock): a beforeToolCall that
//        aborts and never resolves is drained, and the loop ends with an
//        aborted tool result.
// The process must still exit 0 on its own — condition (C).
//
// Run: node __tests__/agent-bridge-hooks.mjs   (after `npx napi build --platform`)

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

function assistantToolCall(id, name, args) {
  return {
    role: "assistant",
    content: [{ type: "toolCall", id, name, arguments: args }],
    api: "faux",
    provider: "faux",
    model: "faux-model",
    usage: zeroUsage,
    stopReason: "toolUse",
    timestamp: 0,
  };
}

const userMessage = (content) => ({ role: "user", content, timestamp: 0 });

function fakeStream(events, message) {
  return {
    async *[Symbol.asyncIterator]() {
      for (const ev of events) {
        await sleep(1);
        yield ev;
      }
    },
    async result() {
      await sleep(1);
      return message;
    },
  };
}

function doneStream(message) {
  return fakeStream(
    [{ type: "done", reason: message.stopReason, message }],
    message,
  );
}

function toolMeta(tool) {
  return {
    name: tool.name,
    label: tool.label ?? tool.name,
    description: tool.description ?? "",
    parameters: tool.parameters ?? {},
    executionMode: tool.executionMode ?? null,
    hasPrepareArguments: typeof tool.prepareArguments === "function",
  };
}

// --- the JS side of the envelope protocol (the _bridge/dispatcher.ts shape) --
// Routes bridge kinds to the injected closures, including (slice 3) the eight
// loop hooks. Sends the `hooks` presence flags in the run payload so Rust wires
// a round-trip only for the hooks `config` actually defines.
function runLoop(bridge, { streamFn, config, tools, onEvent }) {
  const toolList = tools ?? [];
  const toolsByName = new Map(toolList.map((t) => [t.name, t]));
  const mkSignal = (aborted) => ({ aborted: !!aborted });
  const reviveContext = (wire) => {
    const wireTools = Array.isArray(wire?.tools) ? wire.tools : [];
    return {
      systemPrompt: wire?.systemPrompt,
      messages: wire?.messages ?? [],
      tools: wireTools.map((m) => toolsByName.get(m.name) ?? m),
    };
  };
  const reviveTurnCtx = (p) => ({
    message: p.message,
    toolResults: p.toolResults,
    context: reviveContext(p.context),
    newMessages: p.newMessages,
  });
  const serializeUpdate = (update) => {
    if (!update || typeof update !== "object") return update ?? null;
    if (!update.context) return update;
    const ctx = update.context;
    return {
      ...update,
      context: {
        systemPrompt: ctx.systemPrompt,
        messages: ctx.messages ?? [],
        tools: Array.isArray(ctx.tools) ? ctx.tools.map(toolMeta) : [],
      },
    };
  };

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
        return;
      }
      const handle = async () => {
        switch (kind) {
          case "streamFn": {
            const stream = await streamFn(p.model, p.context, p.options);
            const events = [];
            for await (const ev of stream) events.push(ev);
            const message = await stream.result();
            return { events, message };
          }
          case "convertToLlm":
            return await config.convertToLlm(p.messages);
          case "toolExecute": {
            const tool = toolsByName.get(p.toolName);
            if (!tool) throw new Error(`no tool ${p.toolName}`);
            const onUpdate = (partial) =>
              bridge.emitToolUpdate(p.toolCallId, JSON.stringify(partial));
            return await tool.execute(p.toolCallId, p.args, mkSignal(p.aborted), onUpdate);
          }
          case "transformContext":
            return await config.transformContext(p.messages, mkSignal(p.aborted));
          case "getApiKey":
            return (await config.getApiKey(p.provider)) ?? null;
          case "getSteeringMessages":
            return (await config.getSteeringMessages()) ?? [];
          case "getFollowUpMessages":
            return (await config.getFollowUpMessages()) ?? [];
          case "shouldStopAfterTurn":
            return await config.shouldStopAfterTurn(reviveTurnCtx(p));
          case "prepareNextTurn":
            return serializeUpdate(await config.prepareNextTurn(reviveTurnCtx(p)));
          case "beforeToolCall":
            return (
              (await config.beforeToolCall(
                {
                  assistantMessage: p.assistantMessage,
                  toolCall: p.toolCall,
                  args: p.args,
                  context: reviveContext(p.context),
                },
                mkSignal(p.aborted),
              )) ?? null
            );
          case "afterToolCall":
            return (
              (await config.afterToolCall(
                {
                  assistantMessage: p.assistantMessage,
                  toolCall: p.toolCall,
                  args: p.args,
                  result: p.result,
                  isError: p.isError,
                  context: reviveContext(p.context),
                },
                mkSignal(p.aborted),
              )) ?? null
            );
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
    bridge.run(
      dispatcher,
      JSON.stringify({
        prompts: [userMessage("go")],
        context: {
          systemPrompt: config.systemPrompt ?? "",
          messages: config.messages ?? [],
        },
        model: MODEL,
        tools: toolList.map(toolMeta),
        toolExecution: config.toolExecution ?? null,
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

const identityConverter = (messages) =>
  messages.filter(
    (m) => m.role === "user" || m.role === "assistant" || m.role === "toolResult",
  );

function toolThenDoneStreamFn(toolCallId, toolName, args) {
  let callIndex = 0;
  return async () => {
    const i = callIndex++;
    if (i === 0) return doneStream(assistantToolCall(toolCallId, toolName, args));
    return doneStream(assistantText("done"));
  };
}

const echoTool = (executed) => ({
  name: "echo",
  label: "Echo",
  description: "Echo tool",
  parameters: {},
  async execute(_id, params) {
    if (executed) executed.push(params.value);
    return {
      content: [{ type: "text", text: `echoed: ${params.value}` }],
      details: { value: params.value },
    };
  },
});

// --- tests ----------------------------------------------------------------
async function testTransformContext() {
  console.log("# (a) transformContext prunes the context before convertToLlm");
  const bridge = new AgentBridge();
  let transformedLen = -1;
  let convertedLen = -1;
  await runLoop(bridge, {
    streamFn: async () => doneStream(assistantText("Response")),
    config: {
      messages: [
        userMessage("old 1"),
        assistantText("resp 1"),
        userMessage("old 2"),
        assistantText("resp 2"),
      ],
      transformContext: async (messages) => {
        const pruned = messages.slice(-2);
        transformedLen = pruned.length;
        return pruned;
      },
      convertToLlm: (messages) => {
        const kept = identityConverter(messages);
        convertedLen = kept.length;
        return kept;
      },
    },
  });
  assert(transformedLen === 2, `transformContext ran and pruned to 2 (got ${transformedLen})`);
  assert(convertedLen === 2, `convertToLlm received the pruned transcript (got ${convertedLen})`);
}

async function testShouldStopAfterTurn() {
  console.log("# (b) shouldStopAfterTurn=true stops the loop after the current turn");
  const bridge = new AgentBridge();
  const executed = [];
  let llmCalls = 0;
  let steeringPolls = 0;
  let followUpPolls = 0;
  let stopCtxRoles = [];
  let stopToolResultIds = [];
  const out = await runLoop(bridge, {
    streamFn: async () => {
      const i = llmCalls++;
      if (i === 0) return doneStream(assistantToolCall("tool-1", "echo", { value: "hello" }));
      return doneStream(assistantText("should not run"));
    },
    config: {
      convertToLlm: identityConverter,
      getSteeringMessages: async () => {
        steeringPolls++;
        return [];
      },
      getFollowUpMessages: async () => {
        followUpPolls++;
        return [userMessage("follow up should stay queued")];
      },
      shouldStopAfterTurn: async ({ message, toolResults, context }) => {
        stopCtxRoles = context.messages.map((m) => m.role);
        stopToolResultIds = toolResults.map((tr) => tr.toolCallId);
        return message.role === "assistant";
      },
    },
    tools: [echoTool(executed)],
  });
  assert(llmCalls === 1, `only one LLM turn ran (got ${llmCalls})`);
  assert(JSON.stringify(executed) === JSON.stringify(["hello"]), `tool ran once (got ${JSON.stringify(executed)})`);
  assert(steeringPolls === 1, `getSteeringMessages polled once, at start (got ${steeringPolls})`);
  assert(followUpPolls === 0, `getFollowUpMessages not polled (stopped first) (got ${followUpPolls})`);
  assert(JSON.stringify(stopToolResultIds) === JSON.stringify(["tool-1"]), `hook saw the toolResult id (got ${JSON.stringify(stopToolResultIds)})`);
  assert(
    JSON.stringify(stopCtxRoles) === JSON.stringify(["user", "assistant", "toolResult"]),
    `hook saw the post-turn context messages (got ${JSON.stringify(stopCtxRoles)})`,
  );
  assert(
    JSON.stringify(out.messages?.map((m) => m.role)) === JSON.stringify(["user", "assistant", "toolResult"]),
    `run returned exactly the stopped transcript (got ${JSON.stringify(out.messages?.map((m) => m.role))})`,
  );
}

async function testBeforeAfterToolCall() {
  console.log("# (c) beforeToolCall + afterToolCall are observed; afterToolCall terminate stops the batch");
  const bridge = new AgentBridge();
  const executed = [];
  let beforeArgs = null;
  let afterResultText = null;
  let llmCalls = 0;
  const out = await runLoop(bridge, {
    streamFn: async () => {
      const i = llmCalls++;
      if (i === 0) return doneStream(assistantToolCall("tool-1", "echo", { value: "hello" }));
      return doneStream(assistantText("should not run"));
    },
    config: {
      convertToLlm: identityConverter,
      beforeToolCall: async ({ args }) => {
        beforeArgs = args;
        return undefined; // do not block
      },
      afterToolCall: async ({ result }) => {
        afterResultText = result?.content?.find((c) => c.type === "text")?.text ?? null;
        return { terminate: true };
      },
    },
    tools: [echoTool(executed)],
  });
  assert(JSON.stringify(beforeArgs) === JSON.stringify({ value: "hello" }), `beforeToolCall observed validated args (got ${JSON.stringify(beforeArgs)})`);
  assert(JSON.stringify(executed) === JSON.stringify(["hello"]), `tool executed (not blocked) (got ${JSON.stringify(executed)})`);
  assert(afterResultText === "echoed: hello", `afterToolCall observed the executed result (got ${JSON.stringify(afterResultText)})`);
  assert(llmCalls === 1, `afterToolCall terminate:true stopped after the batch (got ${llmCalls} turns)`);
  assert(
    JSON.stringify(out.messages?.map((m) => m.role)) === JSON.stringify(["user", "assistant", "toolResult"]),
    `transcript ends at the terminated batch (got ${JSON.stringify(out.messages?.map((m) => m.role))})`,
  );
}

async function testBeforeToolCallBlock() {
  console.log("# (d) beforeToolCall block:true prevents execution");
  const bridge = new AgentBridge();
  const executed = [];
  const out = await runLoop(bridge, {
    streamFn: toolThenDoneStreamFn("tool-1", "echo", { value: "hello" }),
    config: {
      convertToLlm: identityConverter,
      beforeToolCall: async () => ({ block: true, reason: "nope" }),
    },
    tools: [echoTool(executed)],
  });
  assert(executed.length === 0, `blocked tool never executed (got ${JSON.stringify(executed)})`);
  const toolResult = out.messages?.find((m) => m.role === "toolResult");
  const text = toolResult?.content?.find((c) => c.type === "text")?.text;
  assert(text === "nope", `block reason surfaced as the error result (got ${JSON.stringify(text)})`);
}

async function testGetSteeringInjection() {
  console.log("# (e) getSteeringMessages injects a queued message into the next turn");
  const bridge = new AgentBridge();
  const executed = [];
  let queuedDelivered = false;
  let sawInterrupt = false;
  let callIndex = 0;
  await runLoop(bridge, {
    streamFn: async (_model, ctx) => {
      if (callIndex === 1) {
        sawInterrupt = ctx.messages.some(
          (m) => m.role === "user" && m.content === "interrupt",
        );
      }
      const i = callIndex++;
      if (i === 0) return doneStream(assistantToolCall("tool-1", "echo", { value: "first" }));
      return doneStream(assistantText("done"));
    },
    config: {
      convertToLlm: identityConverter,
      toolExecution: "sequential",
      getSteeringMessages: async () => {
        if (executed.length >= 1 && !queuedDelivered) {
          queuedDelivered = true;
          return [userMessage("interrupt")];
        }
        return [];
      },
    },
    tools: [echoTool(executed)],
  });
  assert(JSON.stringify(executed) === JSON.stringify(["first"]), `tool ran before steering injection (got ${JSON.stringify(executed)})`);
  assert(sawInterrupt === true, "injected steering message reached the next LLM turn's context");
}

async function testAbortDuringHook() {
  console.log("# (f) abort DURING a hook settles cleanly (no deadlock)");
  const bridge = new AgentBridge();
  const executed = [];
  const out = await runLoop(bridge, {
    streamFn: toolThenDoneStreamFn("tool-1", "echo", { value: "hello" }),
    config: {
      convertToLlm: identityConverter,
      // Abort while parked inside beforeToolCall and never resolve; abort() must
      // drain the parked id so the loop thread is released (no hang), and the
      // loop's post-hook abort re-check yields an aborted tool result.
      beforeToolCall: () =>
        new Promise(() => {
          bridge.abort();
        }),
    },
    tools: [echoTool(executed)],
  });
  assert(!!out.messages, "run settled after abort mid-hook (no deadlock)");
  assert(executed.length === 0, `aborted tool did not execute (got ${JSON.stringify(executed)})`);
  const toolResult = out.messages?.find((m) => m.role === "toolResult");
  const text = toolResult?.content?.find((c) => c.type === "text")?.text ?? "";
  assert(!toolResult || /abort/i.test(text), `aborted hook surfaces an aborted result if any (got ${JSON.stringify(text)})`);
}

async function main() {
  await testTransformContext();
  await testShouldStopAfterTurn();
  await testBeforeAfterToolCall();
  await testBeforeToolCallBlock();
  await testGetSteeringInjection();
  await testAbortDuringHook();

  console.log("");
  if (failures === 0) console.log("SLICE 3: ALL HOOK-BRIDGE CHECKS PASSED");
  else {
    console.log(`SLICE 3: ${failures} CHECK(S) FAILED`);
    process.exitCode = 1;
  }
  // Clean exit with no explicit process.exit(): condition (C).
}

main();
