// straitjacket-allow-file:duplication — this generator's dump()/path boilerplate
// intentionally mirrors the pidgin-tui vector generators (generate_widgets.mjs
// etc.); each generator is a standalone script.
//
// Vector generator for the byte-exact Rust port of pi's interactive-mode
// MESSAGE-RENDER components (PR-4A): AssistantMessageComponent,
// UserMessageComponent, ToolExecutionComponent. Runs pi's OWN component classes
// from vendor/pi/.../modes/interactive/components/*.ts and dumps
// input -> rendered string[] JSON that the Rust replay asserts byte-identical
// (crates/pidgin-coding/tests/interactive_message_vectors.rs).
//
// The pi packages + npm deps are resolved by ./loader.mjs (see its header for
// the one-time `npm i` setup). Run from this directory (FORCE_COLOR=3 makes
// chalk emit level-3 SGR for bold/italic/underline, matching the Rust runtime
// Theme; without it a piped run yields level-0 identity styles):
//
//   FORCE_COLOR=3 GEN_NPM=<node_modules_dir> \
//     node --import ./loader-register.mjs generate_interactive_messages.mjs
//
// pi upstream pin: vendor/pi submodule @ 3da591a (pi v0.80.10). Theme baked at
// 256-color with images/hyperlinks off, matching the Rust replay's ColorMode.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { setCapabilities } from "@earendil-works/pi-tui";

import { generateDiffString } from "../../../../vendor/pi/packages/coding-agent/src/core/tools/edit-diff.ts";
import { AssistantMessageComponent } from "../../../../vendor/pi/packages/coding-agent/src/modes/interactive/components/assistant-message.ts";
import { ToolExecutionComponent } from "../../../../vendor/pi/packages/coding-agent/src/modes/interactive/components/tool-execution.ts";
import { UserMessageComponent } from "../../../../vendor/pi/packages/coding-agent/src/modes/interactive/components/user-message.ts";
import {
    getMarkdownTheme,
    loadThemeFromPath,
    setThemeInstance,
} from "../../../../vendor/pi/packages/coding-agent/src/modes/interactive/theme/theme.ts";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..", "..", "..");
const darkThemePath = join(
    repoRoot,
    "vendor",
    "pi",
    "packages",
    "coding-agent",
    "src",
    "modes",
    "interactive",
    "theme",
    "dark.json",
);
const outDir = join(here, "..", "..", "tests", "vectors");
mkdirSync(outDir, { recursive: true });

// Deterministic terminal capabilities: no images, no hyperlinks, 256-color.
setCapabilities({ images: null, trueColor: false, hyperlinks: false });
// Bake the built-in dark theme at 256-color (matches the Rust ColorMode::Color256).
setThemeInstance(loadThemeFromPath(darkThemePath, "256color"));

let total = 0;
function dump(name, vectors) {
    const path = join(outDir, `${name}.json`);
    writeFileSync(path, `${JSON.stringify(vectors, null, "\t")}\n`);
    total += vectors.length;
    console.log(`  ${name}.json: ${vectors.length}`);
}

const WIDTHS = [40, 80];

// --- content-block + message helpers ---------------------------------------

const textBlock = (text) => ({ type: "text", text });
const thinkingBlock = (thinking) => ({ type: "thinking", thinking });
const toolCallBlock = (id, name, args) => ({ type: "toolCall", id, name, arguments: args });

function assistantMessage(content, stopReason = "stop", errorMessage) {
    const msg = {
        role: "assistant",
        content,
        api: "test",
        provider: "test",
        model: "test",
        usage: {
            input: 0,
            output: 0,
            cacheRead: 0,
            cacheWrite: 0,
            totalTokens: 0,
            cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
        },
        stopReason,
        timestamp: 0,
    };
    if (errorMessage !== undefined) msg.errorMessage = errorMessage;
    return msg;
}

// ---------------------------------------------------------------------------
// AssistantMessage
// ---------------------------------------------------------------------------

const MARKDOWN_SAMPLE =
    "# Heading\n\nSome **bold** and *italic* and ~~struck~~ text with `inline code`.\n\n" +
    "- one\n- two\n\n> a quote\n\n[a link](https://example.com)";

const assistantCases = [
    { label: "plain-text", content: [textBlock("Hello world")] },
    { label: "markdown", content: [textBlock(MARKDOWN_SAMPLE)] },
    { label: "empty-text-skipped", content: [textBlock("   ")] },
    { label: "empty-content", content: [] },
    { label: "thinking-visible", content: [thinkingBlock("Let me think about this carefully")] },
    {
        label: "thinking-then-text",
        content: [thinkingBlock("Consider the options"), textBlock("Here is the answer")],
    },
    {
        label: "multi-thinking-run",
        content: [thinkingBlock("First thought"), thinkingBlock("Second thought")],
    },
    {
        label: "text-thinking-text",
        content: [textBlock("Intro"), thinkingBlock("Middle reasoning"), textBlock("Conclusion")],
    },
    {
        label: "with-tool-call",
        content: [textBlock("Running a tool"), toolCallBlock("call_1", "read", { path: "a.txt" })],
    },
    { label: "stop-length", content: [textBlock("partial answer")], stopReason: "length" },
    {
        label: "stop-aborted-default",
        content: [textBlock("partial")],
        stopReason: "aborted",
    },
    {
        label: "stop-aborted-custom",
        content: [textBlock("partial")],
        stopReason: "aborted",
        errorMessage: "User cancelled",
    },
    {
        label: "stop-aborted-request-was-aborted",
        content: [textBlock("partial")],
        stopReason: "aborted",
        errorMessage: "Request was aborted",
    },
    { label: "stop-error-default", content: [textBlock("oops")], stopReason: "error" },
    {
        label: "stop-error-custom",
        content: [textBlock("oops")],
        stopReason: "error",
        errorMessage: "Boom",
    },
    {
        label: "tool-call-with-error-stop",
        content: [textBlock("x"), toolCallBlock("c1", "bash", { command: "ls" })],
        stopReason: "error",
        errorMessage: "ignored because tool call present",
    },
];

const assistantVariants = [
    { hideThinkingBlock: false, hiddenThinkingLabel: "Thinking...", outputPad: 1 },
    { hideThinkingBlock: true, hiddenThinkingLabel: "Thinking...", outputPad: 1 },
    { hideThinkingBlock: false, hiddenThinkingLabel: "Thinking...", outputPad: 0 },
    { hideThinkingBlock: true, hiddenThinkingLabel: "Reasoning", outputPad: 2 },
];

function genAssistant() {
    const vectors = [];
    for (const c of assistantCases) {
        const message = assistantMessage(c.content, c.stopReason ?? "stop", c.errorMessage);
        for (const v of assistantVariants) {
            const component = new AssistantMessageComponent(
                message,
                v.hideThinkingBlock,
                getMarkdownTheme(),
                v.hiddenThinkingLabel,
                v.outputPad,
            );
            for (const width of WIDTHS) {
                vectors.push({
                    label: `${c.label}/hide=${v.hideThinkingBlock}/pad=${v.outputPad}`,
                    message,
                    hideThinkingBlock: v.hideThinkingBlock,
                    hiddenThinkingLabel: v.hiddenThinkingLabel,
                    outputPad: v.outputPad,
                    width,
                    expected: component.render(width),
                });
            }
        }
    }
    dump("interactive_assistant_message", vectors);
}

// ---------------------------------------------------------------------------
// UserMessage
// ---------------------------------------------------------------------------

const userCases = [
    { label: "plain", text: "Hello there" },
    { label: "markdown", text: MARKDOWN_SAMPLE },
    { label: "ordered-list", text: "1. first\n2. second\n3. third" },
    { label: "backslash-escapes", text: "escaped \\* not italic \\_ and \\#" },
    { label: "multiline", text: "line one\n\nline two" },
    { label: "empty", text: "" },
];

const userPads = [1, 0, 2];

function genUser() {
    const vectors = [];
    for (const c of userCases) {
        for (const outputPad of userPads) {
            const component = new UserMessageComponent(c.text, getMarkdownTheme(), outputPad);
            for (const width of WIDTHS) {
                vectors.push({
                    label: `${c.label}/pad=${outputPad}`,
                    text: c.text,
                    outputPad,
                    width,
                    expected: component.render(width),
                });
            }
        }
    }
    dump("interactive_user_message", vectors);
}

// ---------------------------------------------------------------------------
// ToolExecution
//
// Two byte-exact oracles, both emitting pi's REAL renderer output and both
// matching the Rust `ToolExecution::render`:
//
//  1. FALLBACK cases — a tool with no built-in definition (unknown name → the
//     `formatToolExecution` text path) or an extension definition WITHOUT
//     renderers (`{ renderShell }` → the call/result fallbacks). These run pi's
//     own `ToolExecutionComponent`, whose fallback composition the Rust port
//     mirrors exactly. (Every real pi tool ships renderCall/renderResult, so a
//     real name can't exercise the fallback; the unknown-name / bare-shell
//     definition is the only way to vector it.)
//
//  2. EDIT cases — the first tool whose renderers are ported to Rust. These are
//     driven through pi's OWN `ToolExecutionComponent` (constructed for the real
//     "edit" tool, then fed `setExpanded`/`updateResult`), so its stateful
//     `updateDisplay` threads the mutable renderer `state` across
//     renderCall→renderResult exactly as pi does at runtime: on settle the edit
//     `renderResult` folds the diff INTO the recolored call box (pending→
//     success/error) via `state.callComponent` and returns an empty result slot,
//     yielding pi's TRUE single recolored `Box`. The Rust port reproduces those
//     bytes statelessly (its full-box `edit_render_result` + the `ToolExecution`
//     self-shell rendering only the result component once settled), so this
//     oracle is pi's real single-box output, not the earlier stateless split.
//     Diffs deliberately avoid single-removed/single-added hunks, whose pi
//     intra-line `diffWords` inverse-highlighting is still deferred in Rust.
// ---------------------------------------------------------------------------

const stubUi = { requestRender() {} };
const TOOL_CWD = "/tmp/tool-cwd";
const UNKNOWN = "myUnknownTool"; // not in createAllToolDefinitions -> no built-in def

// The edit cases are driven through pi's own `ToolExecutionComponent` (below in
// `genToolExecution`), which resolves the real built-in "edit" renderers by name
// and threads renderer state across renderCall→renderResult on settle.

const textResult = (text, isError = false) => ({ content: [textBlock(text)], details: null, isError });
const multiTextResult = (texts, isError = false) => ({
    content: texts.map(textBlock),
    details: null,
    isError,
});

// diffs are picked so every changed hunk has != 1 removed or != 1 added line,
// keeping the port byte-exact (single-line intra-line highlighting is deferred).
const MULTILINE_DIFF = generateDiffString("alpha\nbeta\ngamma\ndelta\n", "alpha\nBETA\nGAMMA\ndelta\n");
const ADDITION_DIFF = generateDiffString("line 1\nline 2\n", "line 1\nline 2\nline 3\nline 4\n");
const TAB_DIFF = generateDiffString("\tfirst\n\tsecond\nlast\n", "\tFIRST\n\tSECOND\nlast\n");

const editResult = (details, text = "Successfully replaced 1 block(s) in foo.txt.") => ({
    content: [textBlock(text)],
    details,
    isError: false,
});
const editArgs = { edits: [{ newText: "y", oldText: "x" }], path: "foo.txt" };

// Fallback cases (oracle 1). Each: toolDefinition is null (no definition ->
// formatToolExecution text path) or { renderShell } (definition without
// renderers -> call/result fallbacks). Keep args keys alphabetical so
// JSON.stringify order == the Rust serde_json pretty order regardless of map
// backing.
const fallbackCases = [
    {
        label: "nodef-pending",
        toolDefinition: null,
        args: { command: "ls -la", path: "/tmp" },
        result: null,
        isPartial: true,
    },
    {
        label: "nodef-success",
        toolDefinition: null,
        args: { command: "cat file" },
        result: textResult("line 1\nline 2\nline 3"),
        isPartial: false,
    },
    {
        label: "nodef-error",
        toolDefinition: null,
        args: { command: "bad" },
        result: textResult("command failed: not found", true),
        isPartial: false,
    },
    {
        label: "nodef-partial",
        toolDefinition: null,
        args: { command: "stream" },
        result: textResult("partial output so far"),
        isPartial: true,
    },
    {
        label: "nodef-nested-args",
        toolDefinition: null,
        args: { config: { depth: 2, enabled: true }, name: "run" },
        result: null,
        isPartial: true,
    },
    {
        label: "nodef-empty-result",
        toolDefinition: null,
        args: { command: "quiet" },
        result: multiTextResult([""], false),
        isPartial: false,
    },
    {
        label: "def-default-pending",
        toolDefinition: { renderShell: "default" },
        args: { path: "notes.md" },
        result: null,
        isPartial: true,
    },
    {
        label: "def-default-success",
        toolDefinition: { renderShell: "default" },
        args: { path: "notes.md" },
        result: textResult("file contents here"),
        isPartial: false,
    },
    {
        label: "def-default-error",
        toolDefinition: { renderShell: "default" },
        args: { path: "missing.md" },
        result: textResult("ENOENT: no such file", true),
        isPartial: false,
    },
    {
        label: "def-self-pending",
        toolDefinition: { renderShell: "self" },
        args: { query: "needle" },
        result: null,
        isPartial: true,
    },
    {
        label: "def-self-success",
        toolDefinition: { renderShell: "self" },
        args: { query: "needle" },
        result: textResult("match at line 5"),
        isPartial: false,
    },
];

// Edit cases (oracle 2) — real renderCall / renderResult / renderDiff coverage.
const editCases = [
    {
        label: "edit-call-pending",
        args: editArgs,
        result: null,
        isPartial: true,
        expanded: false,
    },
    {
        label: "edit-result-multiline",
        args: editArgs,
        result: editResult(MULTILINE_DIFF),
        isPartial: false,
        expanded: false,
    },
    {
        label: "edit-result-multiline-expanded",
        args: editArgs,
        result: editResult(MULTILINE_DIFF),
        isPartial: false,
        expanded: true,
    },
    {
        label: "edit-result-addition",
        args: editArgs,
        result: editResult(ADDITION_DIFF),
        isPartial: false,
        expanded: false,
    },
    {
        label: "edit-result-tabs",
        args: editArgs,
        result: editResult(TAB_DIFF),
        isPartial: false,
        expanded: false,
    },
    // held: error-frame speculative-preview deferred. On settle-error pi keeps
    // the recolored (error-bg) header box AND a separate result slot holding the
    // error text; the Rust port's stateless full-box `edit_render_result` folds a
    // (missing) diff into a single error-bg box instead, so it is not yet
    // pi-exact. Re-enable this case once the error-frame decoration is ported.
    // {
    //     label: "edit-result-error",
    //     args: editArgs,
    //     result: {
    //         content: [textBlock("Error: could not find text to replace in foo.txt")],
    //         details: {},
    //         isError: true,
    //     },
    //     isPartial: false,
    //     expanded: false,
    // },
];

// Coding-tool cases (oracle 2, cont.) — the read/write/ls/grep/find renderers,
// driven through pi's own ToolExecutionComponent. Inputs deliberately avoid
// deferred sub-features so every case is byte-exact:
//   * read/write use unmapped extensions (.txt) so getLanguageFromPath returns
//     undefined and no syntax highlighting runs (the hljs engine is a deno seam
//     and is stubbed in this harness);
//   * outputs stay within each renderer's display window (read 10, ls/find 20,
//     grep 15) so the "(N more lines, <keyhint>)" branch never fires;
//   * result `details` is null so no `[Truncated: …]` footer renders;
//   * read paths are plain files (not SKILL.md/CLAUDE.md/AGENTS.md) so the
//     compact-call classification returns undefined.
const READ_BODY = "line one\nline two\nline three";
const codingToolCases = [
    // read
    { tool: "read", label: "read-call-pending", args: { path: "notes.txt" }, result: null, isPartial: true, expanded: false },
    { tool: "read", label: "read-call-range", args: { limit: 10, offset: 5, path: "notes.txt" }, result: null, isPartial: true, expanded: false },
    { tool: "read", label: "read-result-collapsed", args: { path: "notes.txt" }, result: textResult(READ_BODY), isPartial: false, expanded: false },
    { tool: "read", label: "read-result-expanded", args: { path: "notes.txt" }, result: textResult(READ_BODY), isPartial: false, expanded: true },
    { tool: "read", label: "read-result-error", args: { path: "missing.txt" }, result: textResult("ENOENT: no such file or directory", true), isPartial: false, expanded: false },
    // write
    { tool: "write", label: "write-call-pending", args: { content: "hello\nworld", path: "out.txt" }, result: null, isPartial: true, expanded: false },
    { tool: "write", label: "write-call-empty-content", args: { content: "", path: "out.txt" }, result: null, isPartial: true, expanded: false },
    { tool: "write", label: "write-result-success", args: { content: "hello\nworld", path: "out.txt" }, result: textResult("Successfully wrote 11 bytes to out.txt"), isPartial: false, expanded: false },
    { tool: "write", label: "write-result-error", args: { content: "data", path: "out.txt" }, result: textResult("EACCES: permission denied", true), isPartial: false, expanded: false },
    // ls
    { tool: "ls", label: "ls-call-pending", args: { path: "src" }, result: null, isPartial: true, expanded: false },
    { tool: "ls", label: "ls-call-limit", args: { limit: 100, path: "src" }, result: null, isPartial: true, expanded: false },
    { tool: "ls", label: "ls-result", args: { path: "src" }, result: textResult("file1.txt\nfile2.txt\nsub/"), isPartial: false, expanded: false },
    { tool: "ls", label: "ls-result-expanded", args: { path: "src" }, result: textResult("file1.txt\nfile2.txt\nsub/"), isPartial: false, expanded: true },
    // grep
    { tool: "grep", label: "grep-call-default-path", args: { pattern: "needle" }, result: null, isPartial: true, expanded: false },
    { tool: "grep", label: "grep-call-glob-limit", args: { glob: "*.rs", limit: 50, path: "src", pattern: "foo" }, result: null, isPartial: true, expanded: false },
    { tool: "grep", label: "grep-result", args: { path: "src", pattern: "needle" }, result: textResult("src/a.rs:1: let needle = 1;\nsrc/b.rs:5: fn needle() {}"), isPartial: false, expanded: false },
    // find
    { tool: "find", label: "find-call-default-path", args: { pattern: "*.md" }, result: null, isPartial: true, expanded: false },
    { tool: "find", label: "find-call-limit", args: { limit: 1000, path: "src", pattern: "*.rs" }, result: null, isPartial: true, expanded: false },
    { tool: "find", label: "find-result", args: { path: "src", pattern: "*.rs" }, result: textResult("a.rs\nb.rs\nsub/c.rs"), isPartial: false, expanded: false },
];

function genToolExecution() {
    const vectors = [];
    // Oracle 1: fallback paths via pi's own ToolExecutionComponent.
    for (const c of fallbackCases) {
        for (const width of WIDTHS) {
            const component = new ToolExecutionComponent(
                UNKNOWN,
                "tool_call_id_1",
                c.args,
                { showImages: true, imageWidthCells: 60 },
                c.toolDefinition ?? undefined,
                stubUi,
                TOOL_CWD,
            );
            if (c.result) {
                component.updateResult(c.result, c.isPartial);
            }
            vectors.push({
                label: c.label,
                toolName: UNKNOWN,
                args: c.args,
                toolDefinition: c.toolDefinition,
                cwd: TOOL_CWD,
                result: c.result,
                isPartial: c.isPartial,
                expanded: false,
                width,
                expected: component.render(width),
            });
        }
    }
    // Oracle 2: real edit renderer, driven through pi's own
    // ToolExecutionComponent so its stateful updateDisplay folds the settled diff
    // into the recolored call box (pi's TRUE single-box output).
    for (const c of editCases) {
        for (const width of WIDTHS) {
            const component = new ToolExecutionComponent(
                "edit",
                "tool_call_id_1",
                c.args,
                { showImages: true, imageWidthCells: 60 },
                undefined,
                stubUi,
                TOOL_CWD,
            );
            if (c.expanded) {
                component.setExpanded(true);
            }
            if (c.result) {
                component.updateResult(c.result, c.isPartial);
            }
            vectors.push({
                label: c.label,
                toolName: "edit",
                args: c.args,
                toolDefinition: null,
                cwd: TOOL_CWD,
                result: c.result,
                isPartial: c.isPartial,
                expanded: c.expanded,
                width,
                expected: component.render(width),
            });
        }
    }
    // Oracle 2 (cont.): read/write/ls/grep/find renderers, driven the same way.
    for (const c of codingToolCases) {
        for (const width of WIDTHS) {
            const component = new ToolExecutionComponent(
                c.tool,
                "tool_call_id_1",
                c.args,
                { showImages: true, imageWidthCells: 60 },
                undefined,
                stubUi,
                TOOL_CWD,
            );
            if (c.expanded) {
                component.setExpanded(true);
            }
            if (c.result) {
                component.updateResult(c.result, c.isPartial);
            }
            vectors.push({
                label: c.label,
                toolName: c.tool,
                args: c.args,
                toolDefinition: null,
                cwd: TOOL_CWD,
                result: c.result,
                isPartial: c.isPartial,
                expanded: c.expanded,
                width,
                expected: component.render(width),
            });
        }
    }
    dump("interactive_tool_execution", vectors);
}

genAssistant();
genUser();
genToolExecution();
console.log(`total: ${total} vectors`);
