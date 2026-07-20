// Faithful shim of the one extension-facing VALUE export of
// @earendil-works/pi-coding-agent that tool extensions use: `defineTool`.
// Upstream `defineTool` is a pure identity function that only exists to preserve
// TypeScript parameter inference (pi packages/coding-agent/src/core/extensions/
// types.ts:497 — `export function defineTool(tool) { return tool; }`). At runtime
// it returns its argument unchanged, so this one-liner is behavior-faithful.
export const defineTool = (tool) => tool;
