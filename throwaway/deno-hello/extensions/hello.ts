// Sample pi-style extension, authored in real TypeScript.
//
// The type annotations below exist on purpose: the Rust host transpiles this
// file with deno_ast (strip-types) before it ever reaches the JS runtime, so
// the annotations exercise the transpile path. A real pi extension would also
// `import` from `@earendil-works/pi-*` and `typebox`; this sample has NO
// imports, because bare deno_core has no module resolver for those specifiers
// (see the README "Findings" section).

type ToolArgs = { name: string };
type ToolResult = { content: string };

interface ToolCallEvent {
  input?: Record<string, unknown>;
}

type HookOutcome =
  | { block: true; reason: string }
  | { input: Record<string, unknown> }
  | undefined;

export default (pi: any) => {
  pi.registerTool({
    name: "greet",
    description: "Greets a person asynchronously",
    execute: async (args: ToolArgs): Promise<ToolResult> => {
      // A genuine macrotask: setTimeout schedules on the deno_core timer
      // queue, so the Rust host has to keep pumping run_event_loop to see
      // this promise resolve. That is the async round-trip proof.
      await new Promise<void>((resolve) => setTimeout(resolve, 10));
      return { content: `Hello, ${args.name}!` };
    },
  });

  pi.on("tool_call", (ev: ToolCallEvent): HookOutcome => {
    if (ev.input && ev.input.danger === true) {
      return { block: true, reason: "blocked dangerous call" };
    }
    if (ev.input) {
      ev.input.audited = true; // modify in place
    }
    return undefined; // allow
  });
};
