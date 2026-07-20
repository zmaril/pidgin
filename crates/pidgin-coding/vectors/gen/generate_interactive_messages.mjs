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
import { createAllToolDefinitions } from "../../../../vendor/pi/packages/coding-agent/src/core/tools/index.ts";
import { AssistantMessageComponent } from "../../../../vendor/pi/packages/coding-agent/src/modes/interactive/components/assistant-message.ts";
import { ToolExecutionComponent } from "../../../../vendor/pi/packages/coding-agent/src/modes/interactive/components/tool-execution.ts";
import { UserMessageComponent } from "../../../../vendor/pi/packages/coding-agent/src/modes/interactive/components/user-message.ts";
import {
    getMarkdownTheme,
    loadThemeFromPath,
    setThemeInstance,
    theme,
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
//  2. EDIT cases — the first tool whose renderers are ported to Rust. pi's
//     `ToolExecutionComponent` threads a mutable renderer `state` across
//     renderCall→renderResult: the edit renderResult re-renders the call
//     component with the diff moved inside it and recolors the header
//     (pending→success/error) via `state.callComponent`. The merged Rust
//     `ToolRenderContext` intentionally omits `state`/`lastComponent` (a
//     documented deviation), so the stateless Rust `ToolExecution::render`
//     composes `["", ...renderCall(width), ...renderResult(width)]` with a
//     FRESH context per closure. We reproduce pi's real edit closures under the
//     same stateless composition ([`renderEditSelfShell`]) — this is pi's own
//     `renderCall`/`renderResult`/`renderDiff` output, not a reimplementation,
//     only the cross-render state threading (which the port drops) is elided.
//     Diffs deliberately avoid single-removed/single-added hunks, whose pi
//     intra-line `diffWords` inverse-highlighting is still deferred in Rust.
// ---------------------------------------------------------------------------

const stubUi = { requestRender() {} };
const TOOL_CWD = "/tmp/tool-cwd";
const UNKNOWN = "myUnknownTool"; // not in createAllToolDefinitions -> no built-in def

// Real tool definitions — the edit definition below carries pi's actual
// renderCall/renderResult closures, exactly as the Rust port resolves them from
// its own create_all_tool_definitions.
const toolDefs = createAllToolDefinitions(TOOL_CWD);

const textResult = (text, isError = false) => ({ content: [textBlock(text)], details: null, isError });
const multiTextResult = (texts, isError = false) => ({
    content: texts.map(textBlock),
    details: null,
    isError,
});

// Fresh render context mirroring the stateless `ToolRenderContext` the Rust
// port builds (no state / lastComponent — see the section note).
function editRenderCtx({ args, isPartial, expanded, isError = false }) {
    return {
        args,
        toolCallId: "tool_call_id_1",
        invalidate() {},
        lastComponent: undefined,
        state: {},
        cwd: TOOL_CWD,
        executionStarted: false,
        argsComplete: false,
        isPartial,
        expanded,
        showImages: true,
        isError,
    };
}

// Compose the edit tool's `renderShell: "self"` output exactly as the Rust
// `ToolExecution::render` self-shell path does: the call component, then the
// result component when a result is present, prefixed with a single blank line
// unless everything renders empty.
function renderEditSelfShell(args, result, isPartial, expanded, width) {
    const def = toolDefs.edit;
    const call = def.renderCall(args, theme, editRenderCtx({ args, isPartial, expanded }));
    const lines = [...call.render(width)];
    if (result) {
        const res = def.renderResult(
            { content: result.content, details: result.details },
            { expanded, isPartial },
            theme,
            editRenderCtx({ args, isPartial, expanded, isError: result.isError }),
        );
        lines.push(...res.render(width));
    }
    return lines.length === 0 ? [] : ["", ...lines];
}

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
    {
        label: "edit-result-error",
        args: editArgs,
        result: {
            content: [textBlock("Error: could not find text to replace in foo.txt")],
            details: {},
            isError: true,
        },
        isPartial: false,
        expanded: false,
    },
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
    // Oracle 2: real edit renderer, stateless self-shell composition.
    for (const c of editCases) {
        for (const width of WIDTHS) {
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
                expected: renderEditSelfShell(c.args, c.result, c.isPartial, c.expanded, width),
            });
        }
    }
    dump("interactive_tool_execution", vectors);
}

genAssistant();
genUser();
genToolExecution();
console.log(`total: ${total} vectors`);
