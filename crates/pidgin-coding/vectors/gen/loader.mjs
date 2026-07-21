//
// Node ESM resolve hook for `generate_interactive_messages.mjs`. pi's
// coding-agent components import the pi packages by name
// (`@earendil-works/pi-tui`, `@earendil-works/pi-ai`) and a few npm packages
// (`chalk`, `typebox`, `get-east-asian-width`, plus `highlight.js` / `diff`
// which the message components never execute). This hook:
//   * maps the `@earendil-works/*` package specifiers to the vendored pi SOURCE
//     (`vendor/pi/packages/*/src/index.ts`), so no built `dist/` is needed;
//   * resolves the real npm deps from a node_modules the dev installs once
//     (path via $GEN_NPM, default `./npm`), pinned to pi's versions — the dir
//     must also contain an empty `anchor.js` used to anchor the lookup:
//         cd <dir> && printf '{"type":"module"}' > package.json && : > anchor.js
//         npm i chalk@5.6.2 typebox@1.1.38 get-east-asian-width@1.6.0 \
//               marked@18.0.5 cross-spawn@7.0.6 diff@8.0.4 semver yaml \
//               hosted-git-info
//   * stubs `highlight.js` (imported by the transitive tool stack but never
//     called on these render paths) and the heavy/dynamic-only deps
//     photon-node / jiti / hosted-git-info. `diff` is NOT stubbed: the real
//     `edit` renderer's `renderDiff` calls `diffWords`/`diffLines`.
//
// Run (chalk MUST be at level 3 so bold/italic emit SGR, matching the Rust
// runtime Theme):
//   FORCE_COLOR=3 GEN_NPM=<node_modules_dir> \
//     node --import ./loader-register.mjs generate_interactive_messages.mjs

import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..", "..", "..");
const piPackages = join(repoRoot, "vendor", "pi", "packages");

const npmRoot = process.env.GEN_NPM ?? join(here, "npm");
const anchorUrl = pathToFileURL(join(npmRoot, "anchor.js")).href;

const PI_SRC = {
    "@earendil-works/pi-tui": join(piPackages, "tui", "src", "index.ts"),
    "@earendil-works/pi-ai": join(piPackages, "ai", "src", "index.ts"),
};

// No import stubs remain: the interactive tool-execution vectors now run the
// REAL `edit` renderer, whose `renderDiff` calls the `diff` package
// (`diffWords`/`diffLines`) — so `diff` must resolve to the pinned npm build,
// not an inert stub. (`highlight.js` is still short-circuited below.)
const STUBS = {};

// Heavy/dynamic-only deps the message-render paths never execute — stubbed.
const STUB_BASES = new Set(["@silvia-odwyer/photon-node", "jiti", "hosted-git-info"]);

function fileUrl(path) {
    return pathToFileURL(path).href;
}

export async function resolve(specifier, context, nextResolve) {
    if (Object.hasOwn(PI_SRC, specifier)) {
        return { url: fileUrl(PI_SRC[specifier]), shortCircuit: true };
    }
    if (specifier.startsWith("highlight.js")) {
        return {
            url: fileUrl(join(here, "stubs", "highlightjs.mjs")),
            shortCircuit: true,
        };
    }
    if (Object.hasOwn(STUBS, specifier)) {
        return { url: fileUrl(STUBS[specifier]), shortCircuit: true };
    }
    const scoped = specifier.startsWith("@")
        ? specifier.split("/").slice(0, 2).join("/")
        : specifier.split("/")[0];
    if (STUB_BASES.has(scoped)) {
        return { url: fileUrl(join(here, "stubs", "empty.mjs")), shortCircuit: true };
    }
    const isBuiltin = context.parentURL === undefined;
    const isRelative =
        specifier.startsWith(".") || specifier.startsWith("/") || specifier.startsWith("file:");
    const isNode = specifier.startsWith("node:");
    const isInternal = specifier.startsWith("#");
    if (!isRelative && !isNode && !isInternal) {
        // Any remaining bare specifier is an npm package: resolve it from the
        // dev-installed node_modules by anchoring the lookup there, so Node's own
        // resolver handles subpath exports (e.g. `typebox/compile`).
        try {
            return await nextResolve(specifier, { ...context, parentURL: anchorUrl });
        } catch (err) {
            if (isBuiltin) throw err;
            return nextResolve(specifier, context);
        }
    }
    return nextResolve(specifier, context);
}
