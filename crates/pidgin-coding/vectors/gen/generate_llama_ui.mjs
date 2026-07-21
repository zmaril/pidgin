//
// Vector generator for the byte-exact Rust port of pi's llama-extension TUI
// (PR: port llama ui.ts). ui.ts's internals (frame, HuggingFaceSearch,
// LlamaView, contextLabel/modelDescription/compactCount/selectTheme) are NOT
// exported, so — as sanctioned for headless-only components — this reconstructs
// each render surface from pi's REAL primitives: the exported DynamicBorder and
// keyHint (coding-agent), and Container/Text/Spacer/SelectList/Input/visibleWidth
// /truncateToWidth (pi-tui). The tiny helper logic (frame composition, the
// progress-bar string, the model-list description, the search result lines) is
// inlined verbatim from ui.ts. The byte-exactness that matters — Text wrapping,
// SelectList column layout, the border rule, keyHint colouring, truncation —
// comes from pi's own code; the Rust replay
// (crates/pidgin-coding/tests/llama_ui_vectors.rs) asserts byte-identical.
//
// The prereq components (DynamicBorder, keyHint) are ALSO vectored directly from
// their real exported functions — those are truly independent.
//
// The pi packages + npm deps are resolved by ./loader.mjs. Run from this
// directory (FORCE_COLOR=3 makes chalk emit level-3 SGR, matching the Rust
// runtime Theme):
//
//   FORCE_COLOR=3 GEN_NPM=<node_modules_dir> \
//     node --import ./loader-register.mjs generate_llama_ui.mjs
//
// pi upstream pin: vendor/pi submodule @ 3da591a (pi v0.80.10). Theme baked at
// 256-color with images/hyperlinks off, matching the Rust replay's ColorMode.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
    Container,
    fuzzyFilter,
    Input,
    SelectList,
    setCapabilities,
    Spacer,
    Text,
    truncateToWidth,
    visibleWidth,
} from "@earendil-works/pi-tui";

import { DynamicBorder } from "../../../../vendor/pi/packages/coding-agent/src/modes/interactive/components/dynamic-border.ts";
import {
    formatKeyText,
    keyHint,
    keyText,
    rawKeyHint,
} from "../../../../vendor/pi/packages/coding-agent/src/modes/interactive/components/keybinding-hints.ts";
import {
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

setCapabilities({ images: null, trueColor: false, hyperlinks: false });
setThemeInstance(loadThemeFromPath(darkThemePath, "256color"));

let total = 0;
function dump(name, vectors) {
    const path = join(outDir, `${name}.json`);
    writeFileSync(path, `${JSON.stringify(vectors, null, "\t")}\n`);
    total += vectors.length;
    console.log(`  ${name}.json: ${vectors.length}`);
}

const WIDTHS = [40, 80];
const DOWNLOAD_VALUE = "\0download";

// --- ui.ts helpers reconstructed verbatim -----------------------------------

function selectTheme() {
    return {
        selectedPrefix: (t) => theme.fg("accent", t),
        selectedText: (t) => theme.fg("accent", t),
        description: (t) => theme.fg("muted", t),
        scrollInfo: (t) => theme.fg("dim", t),
        noMatch: (t) => theme.fg("warning", t),
    };
}

function frame(title, body, footer) {
    const container = new Container();
    container.addChild(new DynamicBorder((t) => theme.fg("accent", t)));
    container.addChild(new Text(theme.fg("accent", theme.bold(title)), 1, 0));
    for (const child of body) container.addChild(child);
    if (footer) {
        container.addChild(new Spacer(1));
        container.addChild(new Text(theme.fg("dim", footer), 1, 0));
    }
    container.addChild(new DynamicBorder((t) => theme.fg("accent", t)));
    return container;
}

function contextLabel(model) {
    const context = model.meta?.n_ctx ?? model.meta?.n_ctx_train;
    if (context) return context >= 1000 ? `${Math.round(context / 1000)}k` : String(context);
    const args = model.status.args ?? [];
    for (let index = 0; index < args.length - 1; index++) {
        if (args[index] !== "--ctx-size" && args[index] !== "-c" && args[index] !== "-ctx") continue;
        const value = Number(args[index + 1]);
        if (Number.isFinite(value) && value > 0) return value >= 1000 ? `${Math.round(value / 1000)}k` : String(value);
    }
    return undefined;
}

function modelDescription(model) {
    const details = [];
    const loaded = model.status.value === "loaded" || model.status.value === "sleeping";
    if (loaded) details.push("loaded");
    else if (model.status.value !== "unloaded") details.push(model.status.value);
    const context = loaded ? contextLabel(model) : undefined;
    if (context) details.push(`${context} context`);
    return details.join(" · ");
}

function compactCount(value) {
    if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(value >= 10_000_000 ? 0 : 1)}M`;
    if (value >= 1_000) return `${(value / 1_000).toFixed(value >= 100_000 ? 0 : 1)}k`;
    return String(value);
}

// Render a Component through the LlamaView.render wrapper (truncate over-wide
// lines to width, empty ellipsis).
function renderView(content, width) {
    return content.render(width).map((line) => (visibleWidth(line) > width ? truncateToWidth(line, width, "") : line));
}

// --- prereq: DynamicBorder --------------------------------------------------

function genDynamicBorder() {
    const vectors = [];
    const widths = [1, 5, 40, 80];
    for (const width of widths) {
        const border = new DynamicBorder((t) => theme.fg("accent", t));
        vectors.push({ label: `accent/w=${width}`, width, expected: border.render(width) });
    }
    // Zero width clamps to 1.
    const zero = new DynamicBorder((t) => theme.fg("accent", t));
    vectors.push({ label: "accent/w=0", width: 0, expected: zero.render(0) });
    dump("llama_dynamic_border", vectors);
}

// --- prereq: keybinding hints -----------------------------------------------

function genKeybindingHints() {
    const vectors = [];
    // Avoid `alt` bindings: their macOS relabel (option) would make the Linux-
    // baked JSON platform-dependent. The llama UI uses confirm/cancel only.
    const bindings = [
        ["tui.select.confirm", "load/unload/download"],
        ["tui.select.confirm", "select"],
        ["tui.select.cancel", "close"],
        ["tui.select.cancel", "cancel"],
        ["tui.select.cancel", "back"],
        ["tui.select.cancel", "stop"],
    ];
    for (const [binding, description] of bindings) {
        vectors.push({
            kind: "keyHint",
            binding,
            description,
            expected: keyHint(binding, description),
        });
    }
    for (const binding of ["tui.select.confirm", "tui.select.cancel", "tui.select.up", "tui.select.down"]) {
        vectors.push({ kind: "keyText", binding, description: "", expected: keyText(binding) });
    }
    for (const [key, description] of [
        ["ctrl+c", "copy"],
        ["enter", "submit"],
        ["escape", "back"],
    ]) {
        vectors.push({ kind: "rawKeyHint", key, description, expected: rawKeyHint(key, description) });
    }
    for (const key of ["ctrl+c", "enter/escape", "escape"]) {
        vectors.push({ kind: "formatKeyText", key, description: "", expected: formatKeyText(key) });
    }
    dump("llama_keybinding_hints", vectors);
}

// --- llama UI surfaces -------------------------------------------------------

const MODELS = [
    {
        id: "unsloth/Qwen3-4B-GGUF",
        status: { value: "loaded", args: ["--ctx-size", "8192"] },
        meta: { n_ctx: 8192, n_ctx_train: 32768 },
    },
    {
        id: "bartowski/Llama-3.2-3B-Instruct-GGUF",
        status: { value: "unloaded" },
    },
    {
        id: "TheBloke/Mistral-7B-Instruct-v0.2-GGUF",
        status: { value: "downloading" },
    },
    {
        id: "ggml-org/gemma-3-1b",
        status: { value: "sleeping" },
        meta: { n_ctx_train: 131072 },
    },
];

function showModelsContent(serverUrl, models) {
    const sorted = [...models].sort((left, right) => {
        const loaded = Number(right.status.value === "loaded") - Number(left.status.value === "loaded");
        return loaded || left.id.localeCompare(right.id);
    });
    const items = [
        ...sorted.map((model) => ({ value: model.id, label: model.id, description: modelDescription(model) })),
        { value: DOWNLOAD_VALUE, label: "Download model…", description: "Hugging Face owner/repository[:quant]" },
    ];
    const list = new SelectList(items, Math.min(items.length, 12), selectTheme(), {
        minPrimaryColumnWidth: 36,
        maxPrimaryColumnWidth: 56,
    });
    const footer = `${keyHint("tui.select.confirm", "load/unload/download")} • ${keyHint("tui.select.cancel", "close")}`;
    return frame("llama.cpp models", [new Text(theme.fg("dim", serverUrl), 1, 0), new Spacer(1), list], footer);
}

function selectContent(title, options) {
    const list = new SelectList(
        options.map((option) => ({ value: option, label: option })),
        Math.min(options.length, 12),
        selectTheme(),
    );
    const footer = `${keyHint("tui.select.confirm", "select")} • ${keyHint("tui.select.cancel", "cancel")}`;
    return frame(title, [new Spacer(1), list], footer);
}

function statusContent(title, message) {
    return frame(title, [new Spacer(1), new Text(theme.fg("muted", message), 1, 0)]);
}

function progressContent(state) {
    const body = [
        new Text(theme.fg("text", state.model), 1, 0),
        new Spacer(1),
        new Text(theme.fg("muted", state.message), 1, 0),
    ];
    if (state.ratio !== undefined) {
        const available = 40;
        const filled = Math.round(Math.max(0, Math.min(1, state.ratio)) * available);
        body.push(
            new Text(
                theme.fg(
                    "accent",
                    `${"█".repeat(filled)}${"─".repeat(available - filled)} ${Math.round(state.ratio * 100)}%`,
                ),
                1,
                0,
            ),
        );
    }
    if (state.detail) body.push(new Text(theme.fg("dim", state.detail), 1, 0));
    return frame(state.title, body, keyHint("tui.select.cancel", "stop"));
}

// The HuggingFaceSearch body: dim hint + input + spacer + result lines. Mirrors
// the component's four constructor children + updateResults.
function searchBody(query, results, selectedIndex, status, focused) {
    const container = new Container();
    container.addChild(new Text(theme.fg("dim", "Model name or owner/repository[:quant]"), 1, 0));
    const input = new Input();
    // Type the query character-by-character (not setValue) so the cursor lands at
    // the end, matching how the Rust replay drives the widget via handleInput.
    input.focused = focused;
    for (const ch of query) input.handleInput(ch);
    container.addChild(input);
    container.addChild(new Spacer(1));

    // filterResults
    let filtered;
    if (query) {
        const matches = new Set(fuzzyFilter(results, query, (m) => m.id).map((m) => m.id));
        filtered = results.filter((m) => matches.has(m.id));
    } else {
        filtered = results;
    }
    selectedIndex = Math.min(selectedIndex, Math.max(0, filtered.length - 1));

    const resultsContainer = new Container();
    const maxVisible = 10;
    const start = Math.max(0, Math.min(selectedIndex - Math.floor(maxVisible / 2), filtered.length - maxVisible));
    const end = Math.min(start + maxVisible, filtered.length);
    for (let index = start; index < end; index++) {
        const model = filtered[index];
        if (!model) continue;
        const prefix = index === selectedIndex ? "→ " : "  ";
        const details = `${compactCount(model.downloads)} downloads`;
        resultsContainer.addChild(
            new Text(
                index === selectedIndex
                    ? theme.fg("accent", `${prefix}${model.id}  ${details}`)
                    : `${prefix}${model.id}${theme.fg("muted", `  ${details}`)}`,
                0,
                0,
            ),
        );
    }
    if (start > 0 || end < filtered.length) {
        resultsContainer.addChild(new Text(theme.fg("dim", `  (${selectedIndex + 1}/${filtered.length})`), 0, 0));
    }
    if (filtered.length === 0) {
        resultsContainer.addChild(new Text(theme.fg("dim", `  ${status}`), 0, 0));
    } else if (status === "Searching Hugging Face…") {
        resultsContainer.addChild(new Text(theme.fg("dim", `  ${status}`), 0, 0));
    }
    container.addChild(resultsContainer);
    return container;
}

function searchContent(query, results, selectedIndex, status, focused) {
    const body = searchBody(query, results, selectedIndex, status, focused);
    const footer = `${keyHint("tui.select.confirm", "select")} • ${keyHint("tui.select.cancel", "back")}`;
    return frame("Download model", [new Spacer(1), body], footer);
}

const SEARCH_RESULTS = [
    { id: "unsloth/Qwen3-4B-GGUF", downloads: 1_534_221 },
    { id: "bartowski/Qwen2.5-Coder-7B-GGUF", downloads: 88_400 },
    { id: "TheBloke/CodeLlama-13B-GGUF", downloads: 2_100 },
    { id: "ggml-org/tinyllama", downloads: 512 },
];

// 15 ids all containing the "ml" subsequence (so a "ml" query fuzzy-matches all),
// to exercise the 10-item scroll window + "(n/m)" indicator.
const SCROLL_RESULTS = Array.from({ length: 15 }, (_, i) => ({
    id: `ggml-org/model-${i}-GGUF`,
    downloads: 1000 + i * 111,
}));

function genLlamaUi() {
    const vectors = [];
    const push = (label, content) => {
        for (const width of WIDTHS) {
            vectors.push({ label: `${label}/w=${width}`, width, expected: renderView(content, width) });
        }
    };

    // Loading (initial content).
    push("loading", frame("llama.cpp models", [new Text(theme.fg("muted", "Loading…"), 1, 1)]));

    // Model manager.
    push("models", showModelsContent("http://127.0.0.1:8080", MODELS));
    push("models-empty", showModelsContent("http://127.0.0.1:8080", []));

    // Generic select.
    push("select", selectContent("Choose an action", ["Load", "Unload", "Remove"]));

    // confirm / connectionError compose MULTI-LINE titles (`${title}\n${message}`
    // and `llama.cpp unavailable\n${serverUrl}\n\n${message}`) and drive the same
    // `select` primitive. Their bold+fg title spans a hard newline — now rendered
    // byte-identically to pi thanks to the chalk per-newline re-encasing fix.
    push(
        "confirm",
        selectContent("Stop download?\nCancel the in-progress download and discard the partial weights?", [
            "Yes",
            "No",
        ]),
    );
    push(
        "connection-error",
        selectContent("llama.cpp unavailable\nhttp://127.0.0.1:8080\n\nConnection refused after 3 retries.", [
            "Retry",
            "Close",
        ]),
    );

    // Status.
    push("status", statusContent("Working", "Contacting llama.cpp router…"));

    // Progress at several ratios (+ no-ratio, + detail).
    push("progress-none", progressContent({ title: "Downloading", model: "unsloth/Qwen3-4B-GGUF", message: "Starting…" }));
    for (const ratio of [0, 0.25, 0.5, 0.5125, 1]) {
        push(
            `progress-${ratio}`,
            progressContent({
                title: "Downloading",
                model: "unsloth/Qwen3-4B-GGUF",
                message: "Fetching weights",
                ratio,
                detail: "1.00 GiB / 2.00 GiB",
            }),
        );
    }

    // HuggingFace search states (each reachable through LlamaView's public API,
    // so the Rust replay drives the real widget to the same state).
    push("search-empty", searchContent("", [], 0, "Type at least 2 characters", true));
    push("search-searching", searchContent("qwen", [], 0, "Searching Hugging Face…", true));
    push("search-results", searchContent("qwen", SEARCH_RESULTS, 0, "", true));
    push("search-results-sel1", searchContent("qwen", SEARCH_RESULTS, 1, "", true));
    push("search-no-models", searchContent("zzzznope", [], 0, "No GGUF models found", true));
    push("search-unfocused", searchContent("qwen", SEARCH_RESULTS, 1, "", false));
    push("search-scroll", searchContent("ml", SCROLL_RESULTS, 0, "", true));

    dump("llama_ui", vectors);
}

genDynamicBorder();
genKeybindingHints();
genLlamaUi();
console.log(`total: ${total} vectors`);
