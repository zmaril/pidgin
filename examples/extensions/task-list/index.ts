/**
 * Task List Extension
 *
 * A small, self-contained pi extension that keeps an in-memory task list for
 * the current session. It demonstrates the three core extension surfaces in one
 * cohesive module:
 *
 *   - a slash command    `/task <text>`  — append a task (user-facing)
 *   - a custom tool       `list_tasks`   — let the LLM read the tasks back
 *   - two lifecycle hooks `session_start` + `tool_call` — a load notice and a
 *     demonstration of the blocking contract (mirrors protected-paths.ts).
 *     The block check is illustrative only — see the note on Hook 2 below.
 *
 * The tasks live in a module-scoped array, so they persist for the life of the
 * process (a real extension would reconstruct state from session entries, as
 * pi's upstream stateful-list examples do, but an in-memory list keeps this
 * focused).
 *
 * How to load it:
 *   - Quick test:   pi -e ./examples/extensions/task-list/index.ts
 *   - Auto-discover: drop this directory into `.pi/extensions/` (project-local)
 *     or `~/.pi/agent/extensions/` (global); pi loads `<dir>/index.ts`.
 *
 * In pidgin, the same file is loaded by the deno-backed extension runtime
 * (build with `--features deno`); see ../README.md.
 */

// Type-only import: erased by the transpiler, so it never triggers module
// resolution at load time. Do NOT add a *value* import here (e.g.
// `import { Type } from "typebox"`) — pidgin's current deno runtime has no
// bare-specifier module loader, so a value import would fail to load.
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

interface Task {
	id: number;
	text: string;
}

export default function taskListExtension(pi: ExtensionAPI) {
	// In-memory task list, scoped to this loaded module instance.
	const tasks: Task[] = [];
	let nextId = 1;

	// A user command: `/task buy milk` appends a task and notifies.
	pi.registerCommand("task", {
		description: "Add a task to the in-memory task list: /task <text>",
		handler: async (args, ctx) => {
			const text = args.trim();
			if (!text) {
				ctx.ui.notify("Usage: /task <text>", "warning");
				return;
			}
			const task: Task = { id: nextId++, text };
			tasks.push(task);
			ctx.ui.notify(`Added task #${task.id}: ${task.text}`, "info");
		},
	});

	// A custom tool the LLM can call to read the current tasks back.
	//
	// `parameters` is a plain JSON-schema object literal. In a full pi install
	// the idiomatic choice is typebox — `Type.Object({ text: Type.String() })` —
	// but that needs a value import, which pidgin's deno runtime cannot resolve
	// yet, so we use the equivalent plain schema here.
	pi.registerTool({
		name: "list_tasks",
		label: "List Tasks",
		description: "List all tasks currently in the in-memory task list.",
		parameters: {
			type: "object",
			properties: {
				filter: {
					type: "string",
					description: "Optional case-insensitive substring to filter tasks by.",
				},
			},
			required: [],
		},
		async execute(_toolCallId, params, _signal, _onUpdate, _ctx) {
			const filter = typeof params.filter === "string" ? params.filter.toLowerCase() : "";
			const visible = filter ? tasks.filter((t) => t.text.toLowerCase().includes(filter)) : tasks;

			const text = visible.length
				? visible.map((t) => `#${t.id}: ${t.text}`).join("\n")
				: "No tasks yet. Add one with /task <text>.";

			return {
				content: [{ type: "text", text }],
				details: { count: visible.length, total: tasks.length },
			};
		},
	});

	// Hook 1: announce how many tasks are loaded when a session starts.
	pi.on("session_start", async (_event, ctx) => {
		ctx.ui.notify(`Task list ready (${tasks.length} task(s) loaded).`, "info");
	});

	// Hook 2: demonstrates the blocking contract used by protected-paths.ts —
	// returning `{ block: true, reason }` refuses the tool call.
	//
	// This is NOT a usable guardrail. It matches one literal spelling, so
	// `rm -fr`, `rm -Rf`, `rm -r -f`, `rm --recursive --force` and `rm -rfv`
	// all pass through. It is here to show the hook shape, not to protect
	// anything; a real check would inspect the parsed command.
	pi.on("tool_call", async (event, ctx) => {
		if (event.toolName !== "bash") {
			return undefined;
		}

		const command = (event.input.command as string | undefined) ?? "";
		if (/\brm\s+-rf\b/.test(command)) {
			if (ctx.hasUI) {
				ctx.ui.notify("Blocked a destructive `rm -rf` command", "warning");
			}
			return { block: true, reason: "Blocked destructive `rm -rf` command by task-list guardrail" };
		}

		return undefined;
	});
}
