# pidgin example extensions

pidgin runs [pi](https://github.com/earendil-works/pi) extensions unchanged: a
pi extension is a TypeScript (or JavaScript) module that default-exports a
factory `(pi) => { ... }`, and pidgin loads it on an embedded `deno_core`
runtime. The extension registers behavior through the `ExtensionAPI` it is
handed — slash commands, custom tools the LLM can call, and lifecycle hooks that
observe or block what pi does.

## `task-list/`

[`task-list/index.ts`](./task-list/index.ts) is a small, complete extension that
exercises all three surfaces:

| Surface | What it registers |
| --- | --- |
| Slash command | `/task <text>` — appends a task to an in-memory list |
| Custom tool | `list_tasks` — lets the LLM read the current tasks back |
| Hook: `session_start` | notifies how many tasks are loaded |
| Hook: `tool_call` | demonstrates the block contract on a literal `rm -rf` |

The tasks live in a module-scoped array for the life of the process. A
production extension would reconstruct state from session entries, as pi's
upstream stateful-list examples do; the in-memory list keeps this example
focused.

> **The `tool_call` hook is a demonstration, not a safety control.** It shows
> the shape of a blocking hook — return `{ block: true, reason }` and the tool
> call is refused — by matching the single literal string `rm -rf`. Equally
> destructive spellings (`rm -fr`, `rm -Rf`, `rm -r -f`, `rm --recursive
> --force`, `rm -rfv`) pass straight through. Do not copy it as a guardrail. A
> real one belongs in pi's `protected-paths.ts` territory: match on the parsed
> command, not a flag substring.

## Loading it

```bash
# Quick, one-off test with the -e/--extension flag:
pi -e ./examples/extensions/task-list/index.ts

# Or auto-discovery: copy the directory into a trusted location and pi will
# load <dir>/index.ts automatically.
cp -r examples/extensions/task-list .pi/extensions/            # project-local
cp -r examples/extensions/task-list ~/.pi/agent/extensions/    # global
```

Once loaded, type `/task write the report`, then ask the model to call
`list_tasks`.

## Two authoring notes specific to pidgin's runtime

The example is deliberately written to load on pidgin's current deno runtime:

1. **Type-only import.** It imports `ExtensionAPI` with
   `import type { ... }`, which the transpiler *erases* — so it never triggers
   module resolution. A **value** import (e.g. `import { Type } from "typebox"`)
   would fail, because pidgin's runtime does not yet have a bare-specifier module
   loader.
2. **Plain JSON-schema tool parameters.** The `list_tasks` tool declares its
   `parameters` as a plain JSON-schema object literal instead of typebox
   `Type.Object(...)`. typebox is the idiomatic choice in a full pi install, but
   it is a value import, so the example uses the equivalent plain schema.

## Running it inside pidgin (deno feature)

pidgin gates the JS/TS extension runtime (`deno_core`, which embeds V8) behind
the non-default `deno` Cargo feature, so the default workspace build stays
V8-free. Build or test the runtime — and the loader test that loads this
example — with:

```bash
cargo build -p pidgin-extensions --features deno
cargo test  -p pidgin-extensions --features deno
```

The loader test [`deno_example_extension.rs`](../../crates/pidgin-extensions/tests/deno_example_extension.rs)
loads `task-list/index.ts` through the real extension loader and asserts that
its command, tool, and hooks **register**. It does not invoke them, so the
command handler, the tool's `execute`, and the `tool_call` hook are unexercised.

It runs in CI's dedicated "deno runtime (V8)" job, and locally wherever
`deno_core` can fetch its V8 blob — some sandboxed environments block that
download, which is the only reason it may not build for you.
