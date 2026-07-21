//
// Regenerates conformance/manifest.json from the vendored pi tree.
// Lists every .ts source module under vendor/pi/packages/*/src/, excluding
// declaration files (.d.ts) and tests. All modules start as "original".
import { readdirSync, statSync, writeFileSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(new URL(".", import.meta.url));
const repoRoot = join(here, "..");
const piRoot = join(repoRoot, "vendor", "pi");
const packagesDir = join(piRoot, "packages");

const PI_SHA = "3da591ab74ab9ab407e72ed882600b2c851fae21";

/** True for a .ts source module we care about (not a .d.ts, not a test). */
export function isSourceModule(relPath) {
  if (!relPath.endsWith(".ts")) return false;
  if (relPath.endsWith(".d.ts")) return false;
  if (relPath.endsWith(".test.ts") || relPath.endsWith(".spec.ts")) return false;
  const parts = relPath.split("/");
  if (parts.includes("__tests__") || parts.includes("test") || parts.includes("tests")) {
    return false;
  }
  return true;
}

function walk(dir, acc) {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) walk(full, acc);
    else acc.push(full);
  }
}

/** Collect source modules for every package, as manifest rows. */
export function collectModules() {
  const rows = [];
  for (const pkg of readdirSync(packagesDir).sort()) {
    const srcDir = join(packagesDir, pkg, "src");
    let isDir = false;
    try {
      isDir = statSync(srcDir).isDirectory();
    } catch {
      isDir = false;
    }
    if (!isDir) continue;
    const files = [];
    walk(srcDir, files);
    for (const full of files) {
      const rel = relative(piRoot, full).split("\\").join("/");
      if (!isSourceModule(rel)) continue;
      rows.push({ package: pkg, src: rel, status: "original" });
    }
  }
  rows.sort((a, b) => a.src.localeCompare(b.src));
  return rows;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const modules = collectModules();
  const manifest = { pi_sha: PI_SHA, modules };
  const out = join(here, "manifest.json");
  writeFileSync(out, JSON.stringify(manifest, null, 2) + "\n");
  console.log(`wrote ${modules.length} modules to ${relative(repoRoot, out)}`);
}
