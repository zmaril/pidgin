# reload-runtime extension

A canonical pi example, vendored verbatim from the upstream pi project
(`earendil-works/pi`,
`packages/coding-agent/examples/extensions/reload-runtime.ts`, MIT License, see
[`../NOTICE`](../NOTICE)).

## What it does

- Registers a `/reload-runtime` slash command whose handler calls `ctx.reload()`
  to reload extensions, skills, prompts, themes, and context files.
- Registers an LLM-callable tool `reload_runtime` that queues `/reload-runtime`
  as a follow-up user command (tools get an `ExtensionContext` and cannot call
  `ctx.reload()` directly). Its parameter schema is built with TypeBox
  (`Type.Object({})`).

## Why it needs the module loader

Unlike the type-only import in the pirate example, this extension has a **value**
import:

```ts
import { Type } from "typebox";
```

`Type.Object(...)` runs at load time to build the tool's parameter schema, so the
`typebox` import is not erased by the TypeScript transpile — it must resolve to a
real module. pidgin's extension plane serves it through the deno module loader
(`crates/pidgin-extensions/src/module_loader.rs`), which maps the bare `typebox`
specifier to a vendored, pinned TypeBox 1.1.38 bundle, mirroring pi's jiti
`virtualModules` alias. Before that loader existed, this extension failed to load
on the plane.

## How to load it

```
pidgin -e ./examples/extensions/reload-runtime/index.ts --features deno
```

The `--features deno` build wires the real JS extension runtime (deno_core / V8).

## Verified

`crates/pidgin-extensions/tests/deno_typebox_module_loader.rs` (deno-gated) loads
this exact file through the plane and asserts it loads with no errors (proving
`import { Type } from "typebox"` resolved through the module loader), that it
registered the `reload-runtime` command and `reload_runtime` tool (proving
`Type.Object(...)` evaluated and the tool schema built), and that the tool
dispatches through the plane. That test runs in CI's `deno runtime (V8)` job.
