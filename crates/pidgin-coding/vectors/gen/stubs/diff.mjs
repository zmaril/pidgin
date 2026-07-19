// straitjacket-allow-file:duplication — inert test-harness stub.
// Inert stand-in for the `diff` package. Imported (as a namespace) by pi's tool
// stack but never called on the fallback render paths these vectors exercise.
export const structuredPatch = () => ({ hunks: [] });
export const createTwoFilesPatch = () => "";
export const diffLines = () => [];
export const applyPatch = () => false;
export default { structuredPatch, createTwoFilesPatch, diffLines, applyPatch };
