# behavior-hooks (Python)

[`index.py`](./index.py) is a small pidgin **Python** extension (build with the
non-default `python` Cargo feature) that demonstrates the three
*behavior-modifying* hooks the turn dispatch already applies. A Python extension
default-exports its factory as `def extension(pi):` and registers hooks through
`pi.on(event, handler)`, in snake_case.

| Hook | What it does | Return shape |
| --- | --- | --- |
| `before_agent_start` | Appends `Always respond like a pirate.` to the system prompt for the turn | `{"systemPrompt": <str>}` (and/or `{"message": <dict>}`) |
| `input` | Redacts a leaked `password is <secret>` and appends a steering note to the user's text | `{"action": "transform", "text": <str>}` |
| `message_end` | Appends a signature line to the finalized assistant message's first text block | `{"message": <dict>}` (same `role`) |

## These hooks change real behavior today

The Rust turn dispatch calls each of these emitters **and applies the return**:

- the `before_agent_start` handler's returned `systemPrompt` becomes the system
  prompt in the model context for that turn;
- the `input` handler's `transform` replaces the text the agent processes;
- the `message_end` handler's returned `message` replaces the finalized
  assistant message.

This is different from a `tool_call` guardrail (see
[`../task-list-py`](../task-list-py)), which is **decision-only**: the runner
computes a block decision, but that block is not wired end-to-end through the
turn until the AgentSession `_installAgentToolHooks` slice lands. The three hooks
here need no such follow-up — the dispatch that consumes them is already live.

## Return shapes match a real pi handler

The return values are the exact shapes a JavaScript pi handler returns (and the
exact shapes the `--features python` engine deserializes):

- `before_agent_start` → `BeforeAgentStartEventResult`: `{"systemPrompt": <str>}`
  and/or `{"message": <custom message>}` (camelCase `systemPrompt`; the system
  prompt chains across handlers, so a later handler sees the running value in
  `event["systemPrompt"]`);
- `input` → `InputEventResult`, a discriminated union on `action`:
  `{"action": "transform", "text": <str>, "images"?: [...]}` to rewrite,
  `{"action": "handled"}` to fully consume the input, `{"action": "continue"}` or
  `None` to leave it unchanged;
- `message_end` → `MessageEndEventResult`: `{"message": <replacement>}`, where the
  replacement must keep the original `role` (a role change is isolated as an error
  and skipped).

## Running the offline test

The engine and this example are exercised together — no network, no API key — by
the loader test
[`behavior_hooks.rs`](../../crates/pidgin-extensions/tests/behavior_hooks.rs). It
loads *this* `index.py` through the real Python extension loader, builds the
runner, and asserts each emitter carries the behavior change (pirate system
prompt, redacted+steered input, signed assistant message):

```bash
cargo test -p pidgin-extensions --features python
```

libpython is embedded through PyO3, so the test builds and runs in-sandbox.

## Authoring notes specific to pidgin's Python runtime

1. **Stdlib-only imports.** The engine runs each module in a stdlib-only
   namespace with no external-dependency resolution, so the example imports only
   `re` from the standard library.
2. **Plain-dict events.** Each hook `handler(event, ctx)` receives the event as a
   plain dict with camelCase multi-word keys (`systemPrompt`,
   `systemPromptOptions`, `streamingBehavior`), mirroring what a JS handler sees.
3. **`ctx` is `None` offline.** The current offline engine passes `ctx=None`; the
   hooks here read only `event`, so they run unchanged.
