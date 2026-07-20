// straitjacket-allow-file:duplication — this generator's dump()/path/theme
// boilerplate intentionally mirrors generate_interactive_messages.mjs; each
// generator is a standalone script.
//
// Vector generator for the byte-exact Rust port of pi's interactive-mode FOOTER
// + STATUS chrome (PR-4C): FooterComponent, IdleStatus, WorkingStatusIndicator.
// Runs pi's OWN component classes from
// vendor/pi/.../modes/interactive/components/{footer,status-indicator}.ts and
// dumps input -> rendered string[] JSON that the Rust replay asserts
// byte-identical (crates/pidgin-coding/tests/interactive_footer_vectors.rs).
//
// The pi packages + npm deps are resolved by ./loader.mjs (see its header for
// the one-time `npm i` setup). Run from this directory (FORCE_COLOR=3 makes
// chalk emit level-3 SGR, matching the Rust runtime Theme):
//
//   FORCE_COLOR=3 GEN_NPM=<node_modules_dir> \
//     node --import ./loader-register.mjs generate_interactive_footer.mjs
//
// pi upstream pin: vendor/pi submodule @ 3da591a (pi v0.80.10). Theme baked at
// 256-color with images/hyperlinks off, matching the Rust replay's ColorMode.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { setCapabilities } from "@earendil-works/pi-tui";

import { FooterComponent } from "../../../../vendor/pi/packages/coding-agent/src/modes/interactive/components/footer.ts";
import {
    IdleStatus,
    WorkingStatusIndicator,
} from "../../../../vendor/pi/packages/coding-agent/src/modes/interactive/components/status-indicator.ts";
import {
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

// Fixed HOME so formatCwdForFooter is deterministic; the Rust FooterData carries
// the same value explicitly.
const HOME = "/home/tester";
process.env.HOME = HOME;
delete process.env.USERPROFILE;

let total = 0;
function dump(name, vectors) {
    const path = join(outDir, `${name}.json`);
    writeFileSync(path, `${JSON.stringify(vectors, null, "\t")}\n`);
    total += vectors.length;
    console.log(`  ${name}.json: ${vectors.length}`);
}

const WIDTHS = [40, 80];

// --- stub AgentSession + ReadonlyFooterDataProvider -------------------------

const stubUi = { requestRender() {} };

// A single assistant entry carrying the given usage; with one entry the footer's
// summed totals equal the entry, and the latest-cache-hit-rate is this entry's.
function assistantEntry(usage) {
    return {
        type: "message",
        message: {
            role: "assistant",
            usage: {
                input: usage.input,
                output: usage.output,
                cacheRead: usage.cacheRead,
                cacheWrite: usage.cacheWrite,
                cost: { total: usage.cost },
            },
        },
    };
}

function makeSession(c) {
    const entries = c.usage ? [assistantEntry(c.usage)] : [];
    return {
        state: { model: c.model ?? undefined, thinkingLevel: c.thinkingLevel ?? "off" },
        sessionManager: {
            getEntries: () => entries,
            getCwd: () => c.cwd,
            getSessionName: () => c.sessionName ?? undefined,
        },
        getContextUsage: () => c.contextUsage,
        modelRuntime: { isUsingOAuth: () => c.isUsingOAuth ?? false },
    };
}

function makeProvider(c) {
    const map = new Map(Object.entries(c.extensionStatuses ?? {}));
    return {
        getGitBranch: () => c.gitBranch ?? null,
        getExtensionStatuses: () => map,
        getAvailableProviderCount: () => c.providerCount ?? 1,
    };
}

// Derive the aggregated FooterData fields the Rust replay needs from the case.
function deriveFooterData(c) {
    const u = c.usage ?? { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, cost: 0 };
    const prompt = u.input + u.cacheRead + u.cacheWrite;
    const latestCacheHitRate = prompt > 0 ? (u.cacheRead / prompt) * 100 : null;
    const usingSubscription = c.model
        ? c.model.provider === "kimi-coding" || (c.isUsingOAuth ?? false)
        : false;
    const thinking =
        c.model && c.model.reasoning ? (c.thinkingLevel ?? "") : null;
    return {
        cwd: c.cwd,
        home: HOME,
        gitBranch: c.gitBranch ?? null,
        sessionName: c.sessionName ?? null,
        totalInput: u.input,
        totalOutput: u.output,
        totalCacheRead: u.cacheRead,
        totalCacheWrite: u.cacheWrite,
        latestCacheHitRate,
        totalCost: u.cost,
        usingSubscription,
        contextPercent: c.contextUsage ? c.contextUsage.percent : null,
        contextWindow: c.contextUsage ? c.contextUsage.contextWindow : 0,
        autoCompact: c.autoCompact ?? true,
        experimental: c.experimental ?? false,
        modelId: c.model ? c.model.id : null,
        provider: c.model ? c.model.provider : "",
        thinking,
        providerCount: c.providerCount ?? 1,
        extensionStatuses: c.extensionStatuses ?? {},
    };
}

// --- footer case matrix -----------------------------------------------------

const MODEL = { id: "claude-sonnet", provider: "anthropic", contextWindow: 200000, reasoning: false };
const REASONING_MODEL = { ...MODEL, reasoning: true };
const PROJECT_CWD = "/home/tester/project";

const footerCases = [
    {
        label: "zero-stats",
        model: MODEL,
        cwd: PROJECT_CWD,
        contextUsage: { contextWindow: 200000, percent: 0 },
    },
    {
        label: "tokens-basic",
        model: MODEL,
        cwd: PROJECT_CWD,
        usage: { input: 5000, output: 2500, cacheRead: 0, cacheWrite: 0, cost: 0.05 },
        contextUsage: { contextWindow: 200000, percent: 45.0 },
    },
    {
        label: "tokens-large-M-warning",
        model: MODEL,
        cwd: PROJECT_CWD,
        usage: { input: 1500000, output: 12000000, cacheRead: 50000, cacheWrite: 50000, cost: 123.456 },
        contextUsage: { contextWindow: 1000000, percent: 88.0 },
    },
    {
        label: "context-error-band",
        model: MODEL,
        cwd: PROJECT_CWD,
        usage: { input: 100, output: 10, cacheRead: 0, cacheWrite: 0, cost: 0.001 },
        contextUsage: { contextWindow: 200000, percent: 95.5 },
    },
    {
        label: "compaction-unknown",
        model: MODEL,
        cwd: PROJECT_CWD,
        usage: { input: 100, output: 10, cacheRead: 0, cacheWrite: 0, cost: 0.001 },
        contextUsage: { contextWindow: 200000, percent: null },
    },
    {
        label: "no-model",
        model: null,
        cwd: PROJECT_CWD,
        usage: { input: 100, output: 0, cacheRead: 0, cacheWrite: 0, cost: 0 },
        contextUsage: { contextWindow: 200000, percent: 5.0 },
    },
    {
        label: "branch-and-session",
        model: MODEL,
        cwd: PROJECT_CWD,
        gitBranch: "main",
        sessionName: "my-session",
        contextUsage: { contextWindow: 200000, percent: 5.0 },
    },
    {
        label: "cache-hit-rate",
        model: MODEL,
        cwd: PROJECT_CWD,
        usage: { input: 0, output: 100, cacheRead: 50000, cacheWrite: 50000, cost: 0.5 },
        contextUsage: { contextWindow: 200000, percent: 30.0 },
    },
    {
        label: "provider-multi",
        model: MODEL,
        cwd: PROJECT_CWD,
        providerCount: 2,
        contextUsage: { contextWindow: 200000, percent: 10.0 },
    },
    {
        label: "thinking-high",
        model: REASONING_MODEL,
        thinkingLevel: "high",
        cwd: PROJECT_CWD,
        contextUsage: { contextWindow: 200000, percent: 10.0 },
    },
    {
        label: "thinking-off",
        model: REASONING_MODEL,
        thinkingLevel: "off",
        cwd: PROJECT_CWD,
        contextUsage: { contextWindow: 200000, percent: 10.0 },
    },
    {
        label: "subscription-kimi",
        model: { ...MODEL, provider: "kimi-coding" },
        cwd: PROJECT_CWD,
        contextUsage: { contextWindow: 200000, percent: 10.0 },
    },
    {
        label: "subscription-oauth",
        model: MODEL,
        isUsingOAuth: true,
        cwd: PROJECT_CWD,
        usage: { input: 100, output: 10, cacheRead: 0, cacheWrite: 0, cost: 0.5 },
        contextUsage: { contextWindow: 200000, percent: 10.0 },
    },
    {
        label: "experimental",
        model: MODEL,
        experimental: true,
        cwd: PROJECT_CWD,
        contextUsage: { contextWindow: 200000, percent: 10.0 },
    },
    {
        label: "auto-compact-off",
        model: MODEL,
        autoCompact: false,
        cwd: PROJECT_CWD,
        contextUsage: { contextWindow: 200000, percent: 10.0 },
    },
    {
        label: "extension-statuses",
        model: MODEL,
        cwd: PROJECT_CWD,
        extensionStatuses: { zeta: "build ok", alpha: "lint\trunning  now" },
        contextUsage: { contextWindow: 200000, percent: 10.0 },
    },
    {
        label: "cwd-outside-home",
        model: MODEL,
        cwd: "/var/data/repo",
        contextUsage: { contextWindow: 200000, percent: 10.0 },
    },
    {
        label: "wide-model-and-provider-truncation",
        model: {
            id: "claude-sonnet-4-20250514-extra-super-long-model-identifier",
            provider: "anthropic",
            contextWindow: 200000,
            reasoning: true,
        },
        thinkingLevel: "high",
        providerCount: 2,
        cwd: PROJECT_CWD,
        usage: { input: 123456, output: 67890, cacheRead: 200000, cacheWrite: 5000, cost: 12.345 },
        contextUsage: { contextWindow: 200000, percent: 76.0 },
    },
    {
        label: "long-pwd-truncation",
        model: MODEL,
        cwd: "/home/tester/project/very/deeply/nested/directory/structure/that/exceeds/the/available/width",
        gitBranch: "feature/some-really-long-branch-name-here",
        sessionName: "an-equally-long-session-name-for-good-measure",
        contextUsage: { contextWindow: 200000, percent: 10.0 },
    },
];

function genFooter() {
    const vectors = [];
    for (const c of footerCases) {
        // areExperimentalFeaturesEnabled() reads process.env.PI_EXPERIMENTAL; set
        // it so pi's render matches the derived FooterData.experimental.
        if (c.experimental) {
            process.env.PI_EXPERIMENTAL = "1";
        } else {
            delete process.env.PI_EXPERIMENTAL;
        }
        const data = deriveFooterData(c);
        for (const width of WIDTHS) {
            const component = new FooterComponent(makeSession(c), makeProvider(c));
            component.setAutoCompactEnabled(c.autoCompact ?? true);
            vectors.push({ label: c.label, ...data, width, expected: component.render(width) });
        }
    }
    delete process.env.PI_EXPERIMENTAL;
    dump("interactive_footer", vectors);
}

// --- status indicators ------------------------------------------------------

function genStatus() {
    const vectors = [];

    // IdleStatus: two blank full-width lines.
    for (const width of WIDTHS) {
        const idle = new IdleStatus();
        vectors.push({ kind: "idle", message: null, ticks: 0, width, expected: idle.render(width) });
    }

    // WorkingStatusIndicator: accent spinner + muted message, frame pinned by
    // capturing pi's setInterval callback (as generate_widgets.mjs does).
    const workingScenarios = [
        { message: "Thinking...", ticks: 0 },
        { message: "Thinking...", ticks: 3 },
        { message: "Working on it", ticks: 1 },
    ];
    const realSetInterval = global.setInterval;
    const realClearInterval = global.clearInterval;
    for (const sc of workingScenarios) {
        for (const width of WIDTHS) {
            let captured = null;
            global.setInterval = (fn) => {
                captured = fn;
                return 0;
            };
            global.clearInterval = () => {};
            try {
                const working = new WorkingStatusIndicator(stubUi, sc.message);
                for (let i = 0; i < sc.ticks; i++) {
                    if (captured) captured();
                }
                vectors.push({
                    kind: "working",
                    message: sc.message,
                    ticks: sc.ticks,
                    width,
                    expected: working.render(width),
                });
            } finally {
                global.setInterval = realSetInterval;
                global.clearInterval = realClearInterval;
            }
        }
    }

    dump("interactive_status", vectors);
}

genFooter();
genStatus();
console.log(`total: ${total} vectors`);
