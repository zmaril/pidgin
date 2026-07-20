# pirate extension

A canonical pi example, vendored verbatim from the upstream pi project
(`earendil-works/pi`, `packages/coding-agent/examples/extensions/pirate.ts`,
MIT License, see [`../NOTICE`](../NOTICE)).

## What it does

- Registers a `/pirate` slash command that toggles a module-scoped `pirateMode`
  flag.
- Registers a `before_agent_start` hook that, when `pirateMode` is on, appends
  pirate-speak instructions to the agent's system prompt so the agent answers
  like a stereotypical pirate (while still completing the real task).

When pirate mode is off (the default), the hook returns nothing and the system
prompt is left unchanged.

## Why it loads clean through pidgin

The source uses only a type-only import
(`import type { ExtensionAPI } from "@earendil-works/pi-coding-agent"`), which is
erased at transpile time. It pulls in no runtime/bare-specifier module, so it
loads through pidgin's extension plane without needing a module loader.

## How to load it

```
pidgin -e ./examples/extensions/pirate/index.ts --features deno
```

The `--features deno` build wires the real JS extension runtime (deno_core / V8).
Once loaded, run `/pirate` to toggle pirate mode; the next agent turn picks up
the mutated system prompt through the merged `before_agent_start` dispatch.

## Verified

`crates/pidgin-extensions/tests/deno_pirate_extension.rs` (deno-gated) loads this
exact file through the plane and proves the full command to hook interaction:
the `/pirate` command executes and flips the shared flag, and the next
`before_agent_start` emission returns a system prompt containing pirate's
injected text. That test runs in CI's `deno runtime (V8)` job.
