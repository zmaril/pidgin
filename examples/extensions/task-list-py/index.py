"""Task List Extension (Python twin of ../task-list/index.ts, #188).

A small, self-contained pi extension that keeps an in-memory task list for the
current session. It is the Python port of the JavaScript ``task-list`` example
and demonstrates the same three extension surfaces in one cohesive module:

  - a slash command    ``/task <text>``  -- append a task (user-facing)
  - a custom tool       ``list_tasks``   -- let the LLM read the tasks back
  - two lifecycle hooks ``session_start`` + ``tool_call`` -- a load notice and a
    ``rm -rf`` bash guardrail (mirrors protected-paths).

The tasks live in a closure-scoped list, so they persist for the life of the
process (a real extension would reconstruct state from session entries, as pi's
upstream stateful-list examples do, but an in-memory list keeps this focused).

This file is loaded by pidgin's Python extension engine (build with
``--features python``); see ./README.md. It imports only the standard library
(``re``) because the engine runs each module in a stdlib-only namespace with no
external-dependency resolution.

Handler shapes match what the ``--features python`` engine marshals:

  - the command ``handler(args, ctx)`` receives the raw argument string;
  - the tool ``execute(params)`` receives the parsed arguments dict;
  - a hook ``handler(event, ctx)`` receives the event as a plain dict
    (``tool_call`` carries camelCase ``toolCallId`` / ``toolName`` / ``input``);
  - ``ctx`` is ``None`` in the current offline engine, so every UI touch is
    routed through :func:`notify`, which degrades to a no-op when there is no
    live context (the JS twin calls ``ctx.ui.notify`` directly).
"""

import re


def notify(ctx, message, level="info"):
    """Best-effort analog of the JS ``ctx.ui.notify(message, level)``.

    The offline Python engine passes ``ctx=None``, so this degrades to a no-op
    (returning the message it would have shown) instead of crashing when no live
    UI context is bound. When a context with a ``ui.notify`` is later wired in,
    the notification is forwarded unchanged.
    """
    ui = getattr(ctx, "ui", None)
    if ui is not None and hasattr(ui, "notify"):
        ui.notify(message, level)
    return message


def extension(pi):
    # In-memory task list, scoped to this loaded module instance.
    tasks = []
    next_id = [1]

    # A user command: `/task buy milk` appends a task and notifies.
    def task_handler(args, ctx):
        text = (args or "").strip()
        if not text:
            notify(ctx, "Usage: /task <text>", "warning")
            return
        task = {"id": next_id[0], "text": text}
        next_id[0] += 1
        tasks.append(task)
        notify(ctx, "Added task #{}: {}".format(task["id"], task["text"]), "info")

    pi.register_command(
        "task",
        description="Add a task to the in-memory task list: /task <text>",
        handler=task_handler,
    )

    # A custom tool the LLM can call to read the current tasks back.
    #
    # `parameters` is a plain JSON-schema dict (the Python engine has no typebox
    # analog and resolves no external imports), matching the JS example's plain
    # schema literal.
    def list_tasks(params):
        raw_filter = params.get("filter") if isinstance(params, dict) else None
        needle = raw_filter.lower() if isinstance(raw_filter, str) else ""
        visible = (
            [t for t in tasks if needle in t["text"].lower()] if needle else tasks
        )

        if visible:
            text = "\n".join("#{}: {}".format(t["id"], t["text"]) for t in visible)
        else:
            text = "No tasks yet. Add one with /task <text>."

        return {
            "content": [{"type": "text", "text": text}],
            "details": {"count": len(visible), "total": len(tasks)},
        }

    pi.register_tool(
        {
            "name": "list_tasks",
            "label": "List Tasks",
            "description": "List all tasks currently in the in-memory task list.",
            "parameters": {
                "type": "object",
                "properties": {
                    "filter": {
                        "type": "string",
                        "description": "Optional case-insensitive substring to filter tasks by.",
                    },
                },
                "required": [],
            },
            "execute": list_tasks,
        }
    )

    # Hook 1: announce how many tasks are loaded when a session starts.
    def on_session_start(event, ctx):
        notify(
            ctx,
            "Task list ready ({} task(s) loaded).".format(len(tasks)),
            "info",
        )

    pi.on("session_start", on_session_start)

    # Hook 2: a guardrail that blocks destructive `rm -rf` bash commands,
    # mirroring the block contract used by protected-paths.
    def on_tool_call(event, ctx):
        if event.get("toolName") != "bash":
            return None

        command = event.get("input", {}).get("command", "")
        if not isinstance(command, str):
            command = ""
        if re.search(r"\brm\s+-rf\b", command):
            notify(ctx, "Blocked a destructive `rm -rf` command", "warning")
            return {
                "block": True,
                "reason": "Blocked destructive `rm -rf` command by task-list guardrail",
            }

        return None

    pi.on("tool_call", on_tool_call)
