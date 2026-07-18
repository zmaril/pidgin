#!/usr/bin/env bash
#
# refresh-model-catalog.sh
#
# Regenerates the vendored model-catalog snapshot consumed by the
# atilla-model-catalog crate. This is the mechanism for bumping the upstream pi
# pin: it re-runs pi's generator at the CURRENT vendor/pi submodule commit and
# rewrites crates/atilla-model-catalog/data/ (models.json, providers.json,
# providers/<id>.json, manifest.json).
#
# This step is manual and NON-GATING — CI does not run it. Run it after bumping
# the vendor/pi submodule, then commit the regenerated data/ directory.
#
# Network: pi's generator uses Node's built-in fetch. In the Claude agent
# sandbox that traffic must go through the egress proxy; we honor HTTPS_PROXY
# via NODE_USE_ENV_PROXY=1 (Node >= 22.21) and trust the proxy CA via
# NODE_EXTRA_CA_CERTS when present. On a plain network both are harmless no-ops.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crate_data="$repo_root/crates/atilla-model-catalog/data"
generator="$repo_root/vendor/pi/packages/ai/scripts/generate-models.ts"

if [[ ! -f "$generator" ]]; then
    echo "error: pi generator not found at $generator" >&2
    echo "       run: git submodule update --init vendor/pi" >&2
    exit 1
fi

tmp_out="$(mktemp -d)"
trap 'rm -rf "$tmp_out"' EXIT

# Honor the agent egress proxy when one is configured; harmless otherwise.
export NODE_USE_ENV_PROXY=1
if [[ -f /root/.ccr/ca-bundle.crt && -z "${NODE_EXTRA_CA_CERTS:-}" ]]; then
    export NODE_EXTRA_CA_CERTS=/root/.ccr/ca-bundle.crt
fi

echo "Regenerating model catalog into $tmp_out ..."
(
    cd "$repo_root/vendor/pi/packages/ai"
    node scripts/generate-models.ts --strict --json-only --json-output "$tmp_out" --pretty
)

if [[ ! -f "$tmp_out/models.json" ]]; then
    echo "error: generator did not produce models.json" >&2
    exit 1
fi

echo "Copying snapshot into $crate_data ..."
rm -rf "$crate_data/providers"
mkdir -p "$crate_data/providers"
cp "$tmp_out/models.json" "$crate_data/models.json"
cp "$tmp_out/providers.json" "$crate_data/providers.json"
cp "$tmp_out"/providers/*.json "$crate_data/providers/"

generated_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
pi_pin="$(cd "$repo_root/vendor/pi" && git rev-parse HEAD)"

# Recompute counts and rewrite the manifest with node (always available here).
node - "$tmp_out/models.json" "$tmp_out/providers.json" "$crate_data/manifest.json" \
    "$generated_at" "$pi_pin" <<'NODE'
const fs = require("fs");
const [, , modelsPath, providersPath, manifestPath, generatedAt, piPin] = process.argv;
const models = JSON.parse(fs.readFileSync(modelsPath, "utf8"));
const providers = JSON.parse(fs.readFileSync(providersPath, "utf8"));
let modelCount = 0;
for (const p of Object.keys(models)) modelCount += Object.keys(models[p]).length;
const manifest = {
  upstream_repo: "https://github.com/earendil-works/pi",
  pi_pin: piPin,
  generated_at: generatedAt,
  source: "https://models.dev/api.json + supplemental provider endpoints",
  generator: "vendor/pi/packages/ai/scripts/generate-models.ts --strict --json-only",
  provider_count: providers.length,
  model_count: modelCount,
};
fs.writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + "\n");
console.log(`Wrote manifest: ${providers.length} providers, ${modelCount} models`);
NODE

echo "Done. Review the diff under crates/atilla-model-catalog/data/ and commit."
