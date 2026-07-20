# task-list (Python twin)

[`index.py`](./index.py) is the Python port of the JavaScript
[`task-list/index.ts`](../task-list/index.ts) example (#188). It is the same
in-memory task-list extension, written for pidgin's **Python** extension engine
(build with the non-default `python` Cargo feature) instead of the deno/JS one.

A Python extension default-exports its factory as `def extension(pi):` and
registers behavior through the same `pi` surface the JS example uses, in
snake_case: `pi.register_command`, `pi.register_tool`, `pi.on`.

| Surface | What it registers |
| --- | --- |
| Slash command | `/task <text>` — appends a task to an in-memory list |
| Custom tool | `list_tasks` — lets the LLM read the current tasks back |
| Hook: `session_start` | notifies how many tasks are loaded |
| Hook: `tool_call` | blocks destructive `rm -rf` bash commands (a guardrail) |

The tasks live in a closure-scoped list for the life of the process, exactly as
the JS twin keeps them in a module-scoped array.

## Running the offline test

The engine and this example are exercised together, with no network and no API
key, by the loader test
[`task_list_example.rs`](../../crates/pidgin-extensions/tests/task_list_example.rs).
It loads *this* `index.py` through the real Python extension loader and asserts
the command, tool, and hooks register, that `emit_tool_call` blocks `rm -rf` and
lets a benign command through, and that invoking `/task` adds a task:

```bash
cargo test -p pidgin-extensions --features python
```

libpython is embedded through PyO3, so the test builds and runs in-sandbox.

## Authoring notes specific to pidgin's Python runtime

1. **Stdlib-only imports.** The engine runs each module in a stdlib-only
   namespace with no external-dependency resolution, so the example imports only
   `re` (for the `rm -rf` match). This mirrors the JS twin's constraint that a
   value import would fail on pidgin's deno runtime.
2. **Plain-dict tool parameters.** `list_tasks` declares its `parameters` as a
   plain JSON-schema dict — the Python analog of the JS example's plain schema
   literal.
3. **`ctx` may be absent.** The current offline engine passes `ctx=None`, so
   every UI touch goes through the module's `notify` helper, which degrades to a
   no-op when no live context is bound rather than crashing. The JS twin calls
   `ctx.ui.notify` directly because its runtime always supplies a context.

## Scope (honest)

This example demonstrates **registration parity** with the JS twin plus **three
live handlers** wired end to end by the `--features python` engine: the `/task`
command, the `session_start` hook, and the `tool_call` `rm -rf` guardrail. The
remaining dispatch emitters and full turn-time dispatch are on the **same gap
ladder as the JS engine** — not yet wired — so the tool `execute` and the other
lifecycle events are registered for parity but not driven by the offline test.
