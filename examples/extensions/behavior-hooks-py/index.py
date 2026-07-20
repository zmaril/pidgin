"""Behavior Hooks Extension (Python).

A small, self-contained pi extension that demonstrates the three
*behavior-modifying* extension hooks the pidgin turn dispatch already applies:

  - ``before_agent_start`` -- rewrite the system prompt (here: make the agent
    answer like a pirate) and/or inject custom messages before the agent loop;
  - ``input``              -- rewrite the user's input before the agent sees it
    (here: redact a leaked password and append a steering note);
  - ``message_end``        -- rewrite the finalized assistant message (here:
    append a signature line to its first text block).

Unlike the ``tool_call`` guardrail in ../task-list-py (which is *decision-only*:
it can block a tool but the block is not yet applied end-to-end until the
AgentSession ``_installAgentToolHooks`` slice lands), these three hooks change
real turn behavior *today*: the Rust turn dispatch calls each emitter and applies
its return -- the mutated system prompt goes into the model context, the
transformed input replaces what the agent processes, and the rewritten message
replaces the finalized one.

This file is loaded by pidgin's Python extension engine (build with
``--features python``); see ./README.md. It imports only the standard library
(``re``) because the engine runs each module in a stdlib-only namespace with no
external-dependency resolution.

Handler return shapes match exactly what the ``--features python`` engine
deserializes -- the same shapes a JavaScript pi handler returns:

  - ``before_agent_start`` -> ``{"systemPrompt": <str>}`` and/or
    ``{"message": <custom message dict>}`` (camelCase ``systemPrompt``, mirroring
    pi's ``BeforeAgentStartEventResult``);
  - ``input``              -> ``{"action": "transform", "text": <str>}`` to
    rewrite, ``{"action": "handled"}`` to fully consume the input, or ``None`` to
    leave it unchanged (pi's ``InputEventResult`` discriminated union on
    ``action``);
  - ``message_end``        -> ``{"message": <replacement message dict>}``; the
    replacement must keep the original ``role`` (pi's ``MessageEndEventResult``).

Every hook ``handler(event, ctx)`` receives the event as a plain dict; ``ctx`` is
``None`` in the current offline engine.
"""

# straitjacket-allow-file:duplication -- the module docstring's authoring-notes
# boilerplate (stdlib-only imports, the "loaded by the --features python engine"
# note, the handler-shape table) is a deliberate parallel of ../task-list-py's
# docstring; the shipped Python examples share this framing intentionally.

import re

# The steering note appended to every user turn, and the signature appended to
# every assistant message -- kept as module constants so the offline test can
# assert against the exact strings.
PIRATE_DIRECTIVE = "Always respond like a pirate."
STEERING_NOTE = "\n\n(Please be concise.)"
SIGNATURE = "\n\n-- via behavior-hooks-py"
REDACTED = "[REDACTED]"

# Matches `password is <secret>` / `password: <secret>` (case-insensitive), so a
# leaked credential in the user's input is scrubbed before the agent sees it.
_PASSWORD_RE = re.compile(r"(password\s*(?:is|:)\s*)(\S+)", re.IGNORECASE)


def extension(pi):
    # Hook 1: `before_agent_start` -- append a pirate directive to the system
    # prompt. The engine passes the running system prompt as `event["systemPrompt"]`
    # (chained across handlers), and returning `{"systemPrompt": ...}` replaces it
    # for this turn.
    def on_before_agent_start(event, ctx):
        system_prompt = event.get("systemPrompt", "")
        return {"systemPrompt": system_prompt + "\n\n" + PIRATE_DIRECTIVE}

    pi.on("before_agent_start", on_before_agent_start)

    # Hook 2: `input` -- redact a leaked password and append a steering note to
    # the user's text. Returning `{"action": "transform", "text": ...}` rewrites
    # the input the agent processes; a later `input` handler would see this text.
    def on_input(event, ctx):
        text = event.get("text", "")
        redacted = _PASSWORD_RE.sub(lambda m: m.group(1) + REDACTED, text)
        return {"action": "transform", "text": redacted + STEERING_NOTE}

    pi.on("input", on_input)

    # Hook 3: `message_end` -- append a signature line to the finalized assistant
    # message's first text block. Returning `{"message": ...}` replaces the
    # message; the replacement keeps the original `role` (required by the engine).
    def on_message_end(event, ctx):
        message = event.get("message")
        if not isinstance(message, dict):
            return None

        # Copy so we never mutate the event dict in place.
        updated = dict(message)
        content = updated.get("content")

        if isinstance(content, list):
            new_content = [dict(block) if isinstance(block, dict) else block for block in content]
            appended = False
            for block in new_content:
                if isinstance(block, dict) and block.get("type") == "text":
                    block["text"] = block.get("text", "") + SIGNATURE
                    appended = True
                    break
            if not appended:
                new_content.append({"type": "text", "text": SIGNATURE.strip()})
            updated["content"] = new_content
        elif isinstance(content, str):
            updated["content"] = content + SIGNATURE
        else:
            updated["content"] = [{"type": "text", "text": SIGNATURE.strip()}]

        return {"message": updated}

    pi.on("message_end", on_message_end)
