// A minimal self-contained extension fixture for the deno-gated CLI test.
//
// It registers one command (`task`) and one tool (`list_tasks`), mirroring the
// shape of PR 188's `examples/extensions/task-list/index.ts` but kept inline so
// this PR is independent of PR 188 merging. Same runtime constraints as the other
// deno fixtures: a default-export factory, a plain-object tool `parameters`
// schema (the embedded runtime has no bare-specifier module loader for
// typebox), and no runtime imports.
export default function (pi) {
  pi.registerCommand("task", {
    description: "Manage the task list",
    handler: async () => {},
  });
  pi.registerTool({
    name: "list_tasks",
    description: "List the current tasks",
    parameters: {},
    execute: async () => ({ tasks: [] }),
  });
}
