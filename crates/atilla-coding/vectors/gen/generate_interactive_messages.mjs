// straitjacket-allow-file:duplication — this generator's dump()/path boilerplate
// intentionally mirrors the atilla-tui vector generators (generate_widgets.mjs
// etc.); each generator is a standalone script.
//
// Vector generator for the byte-exact Rust port of pi's interactive-mode
// MESSAGE-RENDER components (PR-4A): AssistantMessageComponent,
// UserMessageComponent, ToolExecutionComponent. Runs pi's OWN component classes
// from vendor/pi/.../modes/interactive/components/*.ts and dumps
// input -> rendered string[] JSON that the Rust replay asserts byte-identical
// (crates/atilla-coding/tests/interactive_message_vectors.rs).
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
// ToolExecution (renderer-less paths: no definition, and a definition without
// renderCall/renderResult — the branches the Rust port implements).
// ---------------------------------------------------------------------------

const stubUi = { requestRender() {} };
const TOOL_CWD = "/tmp/tool-cwd";
const UNKNOWN = "myUnknownTool"; // not in createAllToolDefinitions -> no built-in def

const textResult = (text, isError = false) => ({ content: [textBlock(text)], isError });
const multiTextResult = (texts, isError = false) => ({
    content: texts.map(textBlock),
    isError,
});

// Each case: toolDefinition is null (no definition -> formatToolExecution text
// path) or { renderShell } (definition without renderers -> call/result
// fallbacks). Keep args keys alphabetical so JSON.stringify order == the Rust
// serde_json pretty order regardless of map backing.
const toolCases = [
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

function genToolExecution() {
    const vectors = [];
    for (const c of toolCases) {
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
                label: `${c.label}`,
                toolName: UNKNOWN,
                args: c.args,
                toolDefinition: c.toolDefinition,
                cwd: TOOL_CWD,
                result: c.result,
                isPartial: c.isPartial,
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
