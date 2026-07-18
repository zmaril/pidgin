// straitjacket-allow-file:duplication — this generator's dump()/paths boilerplate
// intentionally mirrors the other generate_*.mjs scripts; each is standalone.
//
// Vector generator for the byte-exact Rust port of pi's autocomplete provider
// (PR C5). Runs pi's OWN `CombinedAutocompleteProvider` from
// vendor/pi/packages/tui/src/autocomplete.ts (Node 22 strips TS types natively)
// and dumps query/state -> expected {suggestions, prefix, applied-completion}
// JSON that the Rust test suite asserts byte-identical.
//
// The scenarios replay pi's own test/autocomplete.test.ts cases. Because the
// provider performs host I/O (readdirSync/statSync and shelling out to the `fd`
// binary), each vector also records the exact host-call results at the seam:
//   - readdir[dir]   -> the directory listing pi/Rust reads
//   - stat[path]     -> statSync(path).isDirectory() (null = threw)
//   - fdCalls[]      -> the raw stdout/exit-code of each `fd` invocation
// The Rust replay injects a deterministic FileProvider answering from these
// recordings, so it needs neither a real filesystem nor the fd binary.
//
// The fd `@`-file suggestion cases are gated in pi on the fd binary being
// installed; this generator records real fd output (via fd_wrapper.mjs) so they
// become deterministic vectors. If fd is absent at generation time, those
// scenarios are skipped and noted in the printed summary.
//
// Run from this directory:  node generate_autocomplete.mjs
// Output is written to ../../tests/vectors/autocomplete_scenarios.json
//
// pi upstream pin: vendor/pi submodule @ 3da591a (pi v0.80.10).

import { CombinedAutocompleteProvider } from "../../../../vendor/pi/packages/tui/src/autocomplete.ts";
import {
	mkdirSync,
	mkdtempSync,
	readdirSync,
	readFileSync,
	rmSync,
	statSync,
	symlinkSync,
	writeFileSync,
} from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const outDir = join(here, "..", "..", "tests", "vectors");
mkdirSync(outDir, { recursive: true });

const fdWrapper = join(here, "fd_wrapper.mjs");
const fdLog = join(here, ".fd_log.tmp");

// Detect fd exactly as pi's own test does (`which fd`), but fall back to the
// Debian `fdfind` name; the wrapper execs whatever REAL_FD points at.
function resolveRealFd() {
	for (const name of ["fd", "fdfind"]) {
		const r = spawnSync("which", [name], { encoding: "utf-8" });
		if (r.status === 0 && r.stdout) {
			const first = r.stdout.split(/\r?\n/).find(Boolean);
			if (first) return first.trim();
		}
	}
	return null;
}
const realFd = resolveRealFd();
const isFdInstalled = Boolean(realFd);

function setupFolder(baseDir, structure = {}) {
	(structure.dirs ?? []).forEach((dir) => {
		mkdirSync(join(baseDir, dir), { recursive: true });
	});
	Object.entries(structure.files ?? {}).forEach(([filePath, contents]) => {
		const fullPath = join(baseDir, filePath);
		mkdirSync(dirname(fullPath), { recursive: true });
		writeFileSync(fullPath, contents);
	});
}

// Record readdir + follow-stat for every path reachable under `root` (real
// directories only; symlinks are not descended into, matching how pi's
// readdir-based logic never follows them — fd handles symlinks separately).
function recordTree(root, readdir, stat) {
	let entries;
	try {
		entries = readdirSync(root, { withFileTypes: true });
	} catch {
		return;
	}
	readdir[root] = entries.map((e) => ({
		name: e.name,
		dir: e.isDirectory(),
		link: e.isSymbolicLink(),
	}));
	try {
		stat[root] = statSync(root).isDirectory();
	} catch {
		stat[root] = null;
	}
	for (const e of entries) {
		const full = join(root, e.name);
		try {
			stat[full] = statSync(full).isDirectory();
		} catch {
			stat[full] = null;
		}
		if (e.isDirectory()) {
			recordTree(full, readdir, stat);
		}
	}
}

// One-level record for a system directory (e.g. "/"), including follow-stat of
// its symlink entries so getFileSuggestions' symlink resolution replays.
function recordDirLevel(dir, readdir, stat) {
	let entries;
	try {
		entries = readdirSync(dir, { withFileTypes: true });
	} catch {
		return;
	}
	readdir[dir] = entries.map((e) => ({
		name: e.name,
		dir: e.isDirectory(),
		link: e.isSymbolicLink(),
	}));
	for (const e of entries) {
		if (!e.isDirectory() && e.isSymbolicLink()) {
			const full = join(dir, e.name);
			try {
				stat[full] = statSync(full).isDirectory();
			} catch {
				stat[full] = null;
			}
		}
	}
}

function itemToJson(item) {
	return {
		value: item.value,
		label: item.label,
		description: item.description ?? null,
	};
}

function readFdCalls() {
	let text = "";
	try {
		text = readFileSync(fdLog, "utf-8");
	} catch {
		return [];
	}
	return text
		.split("\n")
		.filter(Boolean)
		.map((line) => JSON.parse(line));
}

const getSuggestions = (provider, lines, cursorLine, cursorCol, force = false) =>
	provider.getSuggestions(lines, cursorLine, cursorCol, {
		signal: new AbortController().signal,
		force,
	});

const scenarios = [];

// Clears the fd log; returns an env-scoped run. We set FD_LOG so the wrapper
// appends; the wrapper is only used when a provider is built with it.
function clearFdLog() {
	try {
		rmSync(fdLog, { force: true });
	} catch {
		/* ignore */
	}
}

// -------------------------------------------------------------------------
// Block A: extractPathPrefix (basePath "/tmp", forced) — asserts prefix + the
// value multiset (system-root listing order is not part of pi's contract).
// -------------------------------------------------------------------------
async function blockA() {
	const cases = [
		{ name: "extractPathPrefix hey-slash forced", lines: ["hey /"], col: 5 },
		{ name: "extractPathPrefix /A forced", lines: ["/A"], col: 2 },
		{ name: "extractPathPrefix /model not-triggered", lines: ["/model"], col: 6 },
		{ name: "extractPathPrefix /command /-forced", lines: ["/command /"], col: 10 },
	];
	for (const c of cases) {
		const readdir = {};
		const stat = {};
		recordDirLevel("/", readdir, stat);
		const provider = new CombinedAutocompleteProvider([], "/tmp");
		const result = await getSuggestions(provider, c.lines, 0, c.col, true);
		scenarios.push({
			name: c.name,
			basePath: "/tmp",
			hasFd: false,
			lines: c.lines,
			cursorLine: 0,
			cursorCol: c.col,
			force: true,
			readdir,
			stat,
			homedir: homedir(),
			fdCalls: [],
			expected: result
				? { prefix: result.prefix, items: result.items.map(itemToJson) }
				: null,
			assertMode: "prefixAndValueSet",
		});
	}
}

// -------------------------------------------------------------------------
// Block C: dot-slash path completion (temp fixture, no fd, forced).
// Block D: quoted path completion (temp fixture, no fd, forced).
// Both assert full byte-exact items (and applied completion where applicable).
// -------------------------------------------------------------------------
async function blockReaddir(name, structure, lines, cursorCol, opts = {}) {
	const base = mkdtempSync(join(tmpdir(), "pi-autocomplete-"));
	try {
		setupFolder(base, structure);
		const readdir = {};
		const stat = {};
		recordTree(base, readdir, stat);
		const provider = new CombinedAutocompleteProvider([], base);
		const result = await getSuggestions(provider, lines, 0, cursorCol, opts.force ?? true);
		const vec = {
			name,
			basePath: base,
			hasFd: false,
			lines,
			cursorLine: 0,
			cursorCol,
			force: opts.force ?? true,
			readdir,
			stat,
			homedir: homedir(),
			fdCalls: [],
			expected: result
				? { prefix: result.prefix, items: result.items.map(itemToJson) }
				: null,
			assertMode: "items",
		};
		if (opts.applyValue !== undefined && result) {
			const item = result.items.find((i) => i.value === opts.applyValue);
			if (!item) throw new Error(`${name}: apply item ${opts.applyValue} not found`);
			const applied = provider.applyCompletion(lines, 0, cursorCol, item, result.prefix);
			vec.apply = { itemValue: opts.applyValue, expectedLines: applied.lines };
		}
		scenarios.push(vec);
	} finally {
		rmSync(base, { recursive: true, force: true });
	}
}

// -------------------------------------------------------------------------
// Block B: fd @ file suggestions (gated on fd). Records real fd output.
// -------------------------------------------------------------------------
async function blockFd(name, build) {
	if (!isFdInstalled) return false;
	const rootDir = mkdtempSync(join(tmpdir(), "pi-autocomplete-root-"));
	try {
		const baseDir = join(rootDir, "cwd");
		const outsideDir = join(rootDir, "outside");
		mkdirSync(baseDir, { recursive: true });
		mkdirSync(outsideDir, { recursive: true });
		const spec = build({ rootDir, baseDir, outsideDir });
		const readdir = {};
		const stat = {};
		recordTree(rootDir, readdir, stat);
		clearFdLog();
		process.env.FD_LOG = fdLog;
		process.env.REAL_FD = realFd;
		const provider = new CombinedAutocompleteProvider([], spec.baseDir ?? baseDir, fdWrapper);
		const result = await getSuggestions(
			provider,
			spec.lines,
			spec.cursorLine ?? 0,
			spec.cursorCol,
		);
		const fdCalls = readFdCalls();
		const vec = {
			name,
			basePath: spec.baseDir ?? baseDir,
			hasFd: true,
			lines: spec.lines,
			cursorLine: spec.cursorLine ?? 0,
			cursorCol: spec.cursorCol,
			force: false,
			readdir,
			stat,
			homedir: homedir(),
			fdCalls,
			expected: result
				? { prefix: result.prefix, items: result.items.map(itemToJson) }
				: null,
			assertMode: "items",
		};
		if (spec.applyValue !== undefined && result) {
			const item = result.items.find((i) => i.value === spec.applyValue);
			if (!item) throw new Error(`${name}: apply item ${spec.applyValue} not found`);
			const applied = provider.applyCompletion(
				spec.lines,
				spec.cursorLine ?? 0,
				spec.cursorCol,
				item,
				result.prefix,
			);
			vec.apply = { itemValue: spec.applyValue, expectedLines: applied.lines };
		}
		scenarios.push(vec);
		return true;
	} finally {
		rmSync(rootDir, { recursive: true, force: true });
		delete process.env.FD_LOG;
	}
}

async function main() {
	await blockA();

	// Block C — dot-slash
	await blockReaddir(
		"dot-slash preserves ./ file",
		{ files: { "update.sh": "#!/bin/bash", "utils.ts": "export {};" } },
		["./up"],
		4,
	);
	await blockReaddir(
		"dot-slash preserves ./ directory",
		{ dirs: ["src"], files: { "src/index.ts": "export {};" } },
		["./sr"],
		4,
	);

	// Block D — quoted path completion (no fd)
	await blockReaddir(
		"quoted paths with spaces direct",
		{ dirs: ["my folder"], files: { "my folder/test.txt": "content" } },
		["my"],
		2,
	);
	await blockReaddir(
		"quoted continues inside quoted paths",
		{ files: { "my folder/test.txt": "content", "my folder/other.txt": "content" } },
		['"my folder/"'],
		'"my folder/"'.length - 1,
	);
	await blockReaddir(
		"quoted applies without duplicating quote",
		{ files: { "my folder/test.txt": "content" } },
		['"my folder/te"'],  // codespell:ignore te
		'"my folder/te"'.length - 1,  // codespell:ignore te
		{ applyValue: '"my folder/test.txt"' },
	);

	// Block B — fd @ suggestions
	let fdCount = 0;
	const fdRan = async (name, build) => {
		if (await blockFd(name, build)) fdCount += 1;
	};

	await fdRan("fd empty @ query", ({ baseDir }) => {
		setupFolder(baseDir, { dirs: ["src"], files: { "README.md": "readme" } });
		return { baseDir, lines: ["@"], cursorCol: 1 };
	});
	await fdRan("fd matches file with extension", ({ baseDir }) => {
		setupFolder(baseDir, { files: { "file.txt": "content" } });
		return { baseDir, lines: ["@file.txt"], cursorCol: "@file.txt".length };
	});
	await fdRan("fd case insensitive filter", ({ baseDir }) => {
		setupFolder(baseDir, { dirs: ["src"], files: { "README.md": "readme" } });
		return { baseDir, lines: ["@re"], cursorCol: 3 };
	});
	await fdRan("fd ranks directories before files", ({ baseDir }) => {
		setupFolder(baseDir, { dirs: ["src"], files: { "src.txt": "text" } });
		return { baseDir, lines: ["@src"], cursorCol: 4 };
	});
	await fdRan("fd returns nested file paths", ({ baseDir }) => {
		setupFolder(baseDir, { files: { "src/index.ts": "export {};\n" } });
		return { baseDir, lines: ["@index"], cursorCol: 6 };
	});
	await fdRan("fd matches deeply nested paths", ({ baseDir }) => {
		setupFolder(baseDir, {
			files: {
				"packages/tui/src/autocomplete.ts": "export {};",
				"packages/ai/src/autocomplete.ts": "export {};",
			},
		});
		return { baseDir, lines: ["@tui/src/auto"], cursorCol: "@tui/src/auto".length };
	});
	await fdRan("fd matches directory in middle with full-path", ({ baseDir }) => {
		setupFolder(baseDir, {
			files: {
				"src/components/Button.tsx": "export {};",
				"src/utils/helpers.ts": "export {};",
			},
		});
		return { baseDir, lines: ["@components/"], cursorCol: "@components/".length };
	});
	await fdRan("fd scopes to relative directories recursively", ({ baseDir, outsideDir }) => {
		setupFolder(outsideDir, {
			files: {
				"nested/alpha.ts": "export {};",
				"nested/deeper/also-alpha.ts": "export {};",
				"nested/deeper/zzz.ts": "export {};",
			},
		});
		return { baseDir, lines: ["@../outside/a"], cursorCol: "@../outside/a".length };
	});
	await fdRan("fd quotes paths with spaces", ({ baseDir }) => {
		setupFolder(baseDir, { dirs: ["my folder"], files: { "my folder/test.txt": "content" } });
		return { baseDir, lines: ["@my"], cursorCol: 3 };
	});
	await fdRan("fd includes hidden excludes .git", ({ baseDir }) => {
		setupFolder(baseDir, {
			dirs: [".pi", ".github", ".git"],
			files: {
				".pi/config.json": "{}",
				".github/workflows/ci.yml": "name: ci",
				".git/config": "[core]",
			},
		});
		return { baseDir, lines: ["@"], cursorCol: 1 };
	});
	await fdRan("fd follows symlinked directories", ({ baseDir, outsideDir }) => {
		setupFolder(baseDir, { files: { "dir/some_file.txt": "real" } });
		setupFolder(outsideDir, { files: { "some_file.txt": "symlinked" } });
		symlinkSync("../outside", join(baseDir, "symlinked_dir"));
		return { baseDir, lines: ["@some"], cursorCol: 5 };
	});
	await fdRan("fd returns symlinked directory by name", ({ baseDir, outsideDir }) => {
		setupFolder(outsideDir, { files: { "nested/file.txt": "symlinked" } });
		symlinkSync("../outside", join(baseDir, "symlinked_dir"));
		return { baseDir, lines: ["@symlinked"], cursorCol: 10 };
	});
	await fdRan("fd returns symlinked files without type l", ({ baseDir }) => {
		setupFolder(baseDir, { files: { "original.txt": "content" } });
		symlinkSync("original.txt", join(baseDir, "link.txt"));
		return { baseDir, lines: ["@link"], cursorCol: 5 };
	});
	await fdRan("fd continues inside quoted @ paths", ({ baseDir }) => {
		setupFolder(baseDir, {
			files: { "my folder/test.txt": "content", "my folder/other.txt": "content" },
		});
		const line = '@"my folder/"';
		return { baseDir, lines: [line], cursorCol: line.length - 1 };
	});
	await fdRan("fd applies quoted @ completion without dup quote", ({ baseDir }) => {
		setupFolder(baseDir, { files: { "my folder/test.txt": "content" } });
		const line = '@"my folder/te"';  // codespell:ignore te
		return {
			baseDir,
			lines: [line],
			cursorCol: line.length - 1,
			applyValue: '@"my folder/test.txt"',
		};
	});

	clearFdLog();

	const path = join(outDir, "autocomplete_scenarios.json");
	writeFileSync(path, JSON.stringify(scenarios, null, "\t") + "\n");

	const fdScenarios = scenarios.filter((s) => s.hasFd).length;
	const nonFd = scenarios.length - fdScenarios;
	console.log(`autocomplete_scenarios.json: ${scenarios.length} scenarios`);
	console.log(`  non-fd (Block A/C/D): ${nonFd}`);
	console.log(`  fd-recorded (Block B): ${fdScenarios}${isFdInstalled ? "" : " (fd NOT installed -> Block B SKIPPED)"}`);
	if (!isFdInstalled) {
		console.log("  WARNING: fd binary not found; fd-gated scenarios were excluded.");
	}
}

await main();
