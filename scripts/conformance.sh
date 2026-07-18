#!/usr/bin/env bash
#
# Conformance runner for atilla.
#
# Runs the vendored pi (vendor/pi) test suites against the codegen-materialized
# module tree and records an honest baseline in conformance.json at the repo
# root. The all-original manifest means the generated tree is pi itself, so this
# measures pi's own suites in this environment — the number to beat as modules
# migrate to the native (Rust) addon.
#
# Usage:
#   scripts/conformance.sh [--setup] [PACKAGES]
#
#   --setup      Install OS libs (canvas) + pi npm deps + provider-data codegen.
#                Skip it when deps are already present (CI can cache them).
#   PACKAGES     Space-separated subset to run (also honored via the
#                CONFORMANCE_PACKAGES env var). Default: all five pi packages.
#                Example (smoke):  CONFORMANCE_PACKAGES="agent" scripts/conformance.sh
#                Example:          scripts/conformance.sh "agent tui"
#
# Environment knobs:
#   CONFORMANCE_OUT          Where reporter output + logs land.
#                            Default: <repo>/conformance/.out (gitignored).
#   CONFORMANCE_PKG_TIMEOUT  Per-vitest-package timeout, seconds. Default 1800.
#   CONFORMANCE_FILE_TIMEOUT Per-tui-file timeout, seconds. Default 120.
#
# The run is non-interactive and re-runnable. Provider API keys are intentionally
# NOT supplied: the ai suite skips its live-provider tests, which is expected and
# correct for the baseline. A package whose suite cannot load in this environment
# is recorded as env-blocked (0 passing) rather than faked.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PI_ROOT="$REPO_ROOT/vendor/pi"
OUT="${CONFORMANCE_OUT:-$REPO_ROOT/conformance/.out}"
PKG_TIMEOUT="${CONFORMANCE_PKG_TIMEOUT:-1800}"
FILE_TIMEOUT="${CONFORMANCE_FILE_TIMEOUT:-120}"
export CONFORMANCE_OUT="$OUT"

# canvas (packages/ai devDependency) needs these system libraries to build.
# libcairo2-dev and libpixman-1-dev are typically preinstalled; the four below
# are the ones this environment was missing.
CANVAS_LIBS="libpango1.0-dev libjpeg-dev libgif-dev librsvg2-dev"

DO_SETUP=0
PKG_ARG=""
for arg in "$@"; do
  case "$arg" in
    --setup) DO_SETUP=1 ;;
    *) PKG_ARG="$arg" ;;
  esac
done

PACKAGES="${PKG_ARG:-${CONFORMANCE_PACKAGES:-ai agent coding-agent tui orchestrator}}"

mkdir -p "$OUT" "$OUT/pkgmeta"
# Start clean so a stale reporter from a prior run is never mis-parsed.
rm -f "$OUT"/*.vitest.json "$OUT"/*.tap "$OUT"/pkgmeta/*.json

log() { printf '[conformance] %s\n' "$*"; }

# --- optional setup ---------------------------------------------------------
if [ "$DO_SETUP" -eq 1 ]; then
  log "setup: installing canvas system libs ($CANVAS_LIBS)"
  if command -v apt-get >/dev/null 2>&1; then
    apt-get update >"$OUT/apt-update.log" 2>&1 || log "apt-get update failed (see apt-update.log)"
    # shellcheck disable=SC2086
    apt-get install -y $CANVAS_LIBS >"$OUT/apt-install.log" 2>&1 || log "apt-get install failed (see apt-install.log)"
  else
    log "apt-get not available; assuming canvas libs already present"
  fi

  log "setup: npm install in $PI_ROOT (large; logging to npm-install.log)"
  (cd "$PI_ROOT" && npm install) >"$OUT/npm-install.log" 2>&1 || log "npm install reported errors (see npm-install.log)"

  log "setup: generating ai provider model data"
  (cd "$PI_ROOT/packages/ai" && npm run generate-models && npm run generate-image-models) \
    >"$OUT/generate-models.log" 2>&1 || log "generate-models failed (see generate-models.log)"
fi

# --- napi addon (best effort) ----------------------------------------------
# The all-original baseline does not import the addon yet, so a build failure
# here is non-fatal — it is logged and the run continues.
log "verifying atilla-napi addon (best effort)"
if ls "$REPO_ROOT"/crates/atilla-napi/*.node >/dev/null 2>&1; then
  log "napi addon already built; skipping rebuild"
elif (cd "$REPO_ROOT/crates/atilla-napi" && npm install && npx napi build --platform) \
  >"$OUT/napi-build.log" 2>&1; then
  log "napi addon built"
else
  log "napi addon build failed (non-fatal; see napi-build.log)"
fi

# --- codegen: materialize the module tree ----------------------------------
log "running codegen (materialize module tree)"
node "$REPO_ROOT/conformance/codegen.mjs" | tee "$OUT/codegen.log"
if ! grep -q '"missing": 0' "$OUT/codegen.log"; then
  log "ERROR: codegen reports manifest drift (missing != 0); aborting"
  exit 1
fi

ENV_NOTES=()
add_note() { ENV_NOTES+=("$1"); }
add_note "canvas system libs required: $CANVAS_LIBS (plus preinstalled libcairo2-dev, libpixman-1-dev)"
add_note "packages/ai provider data (src/providers/data/*.json) is generated via 'npm run generate-models', not committed in pi"
add_note "no provider API keys supplied: ai skips its live-provider tests by design"

# Write a per-package meta record consumed by parse-results.mjs.
# args: pkg status note reporter format total
write_pkgmeta() {
  local pkg="$1" status="$2" note="$3" reporter="$4" format="$5" total="$6"
  pkg="$pkg" status="$status" note="$note" reporter="$reporter" format="$format" total="$total" \
    node -e '
      const o = {
        status: process.env.status,
        note: process.env.note,
        total: Number(process.env.total || 0),
      };
      if (process.env.reporter) o.reporter = process.env.reporter;
      if (process.env.format) o.format = process.env.format;
      require("node:fs").writeFileSync(
        `${process.env.CONFORMANCE_OUT}/pkgmeta/${process.env.pkg}.json`,
        JSON.stringify(o),
      );
    '
}

want() { case " $PACKAGES " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

# --- vitest packages: ai, agent, coding-agent ------------------------------
run_vitest_pkg() {
  local pkg="$1"
  local reporter="$pkg.vitest.json"
  local human="$OUT/$pkg.human.log"
  log "vitest: $pkg (timeout ${PKG_TIMEOUT}s)"
  local start end rc
  start="$(date +%s)"
  set +e
  (cd "$PI_ROOT/packages/$pkg" && timeout "$PKG_TIMEOUT" npx vitest run \
    --reporter=json --outputFile="$OUT/$reporter") >"$human" 2>&1
  rc=$?
  set -e
  end="$(date +%s)"
  log "vitest: $pkg exit=$rc elapsed=$((end - start))s"

  # Decide ok vs env-blocked. A produced reporter with tests collected is ok
  # even when some tests fail (exit 1). A timeout or a suite that never loaded
  # is env-blocked.
  if [ "$rc" -eq 124 ]; then
    write_pkgmeta "$pkg" "env-blocked" "suite timed out after ${PKG_TIMEOUT}s" "" "" 0
    add_note "$pkg: timed out after ${PKG_TIMEOUT}s"
    return
  fi
  if [ ! -s "$OUT/$reporter" ]; then
    local tail_msg
    tail_msg="$(tail -n 3 "$human" | tr '\n' ' ' | tr -d '"')"
    write_pkgmeta "$pkg" "env-blocked" "no reporter produced: ${tail_msg}" "" "" 0
    add_note "$pkg: no reporter file produced (env-blocked)"
    return
  fi
  local ntotal
  ntotal="$(node -e 'const j=require(process.argv[1]);process.stdout.write(String(j.numTotalTests??0))' "$OUT/$reporter")"
  if [ "$ntotal" = "0" ] && [ "$rc" -ne 0 ]; then
    local tail_msg
    tail_msg="$(tail -n 3 "$human" | tr '\n' ' ' | tr -d '"')"
    write_pkgmeta "$pkg" "env-blocked" "suite collected 0 tests, exit ${rc}: ${tail_msg}" "$reporter" "" 0
    add_note "$pkg: suite failed to collect tests (env-blocked)"
    return
  fi
  write_pkgmeta "$pkg" "ok" "vitest run, exit ${rc}" "$reporter" "" "$ntotal"
}

for pkg in ai agent coding-agent; do
  want "$pkg" && run_vitest_pkg "$pkg"
done

# --- tui: node:test, per file, concatenated TAP ----------------------------
if want tui; then
  log "tui: node --test per file (timeout ${FILE_TIMEOUT}s/file)"
  TUI_TAP="$OUT/tui.tap"
  : >"$TUI_TAP"
  tui_start="$(date +%s)"
  tui_files=$(cd "$PI_ROOT/packages/tui" && ls test/*.test.ts 2>/dev/null || true)
  tui_blocked=0
  if [ -z "$tui_files" ]; then
    write_pkgmeta "tui" "env-blocked" "no tui test files found" "" "" 0
    add_note "tui: no test files discovered (env-blocked)"
  else
    for f in $tui_files; do
      printf '# ATILLA-FILE: %s\n' "$f" >>"$TUI_TAP"
      set +e
      (cd "$PI_ROOT/packages/tui" && timeout "$FILE_TIMEOUT" node --test --test-reporter=tap "$f") \
        >>"$TUI_TAP" 2>>"$OUT/tui.human.log"
      frc=$?
      set -e
      if [ "$frc" -eq 124 ]; then
        printf '# tests 0\n# pass 0\n# fail 0\n# skipped 0\n# ATILLA-TIMEOUT %s\n' "$f" >>"$TUI_TAP"
        tui_blocked=$((tui_blocked + 1))
      fi
    done
    tui_end="$(date +%s)"
    log "tui: elapsed=$((tui_end - tui_start))s (timeouts: ${tui_blocked})"
    if [ "$tui_blocked" -gt 0 ]; then
      add_note "tui: ${tui_blocked} test file(s) timed out after ${FILE_TIMEOUT}s each"
    fi
    write_pkgmeta "tui" "ok" "node:test per file, ${tui_blocked} file timeout(s)" "tui.tap" "tap" 0
  fi
fi

# --- orchestrator: no tests ------------------------------------------------
if want orchestrator; then
  write_pkgmeta "orchestrator" "ok" "no tests" "" "" 0
fi

# --- assemble run-meta.json + parse ----------------------------------------
log "assembling run-meta.json"
ENV_NOTES_JSON="$(printf '%s\n' "${ENV_NOTES[@]}" | node -e 'const l=require("node:fs").readFileSync(0,"utf8").split("\n").filter(Boolean);process.stdout.write(JSON.stringify(l))')"
export ENV_NOTES_JSON
CONFORMANCE_OUT="$OUT" node -e '
  const fs = require("node:fs");
  const out = process.env.CONFORMANCE_OUT;
  const dir = `${out}/pkgmeta`;
  const packages = {};
  for (const f of fs.readdirSync(dir)) {
    if (!f.endsWith(".json")) continue;
    packages[f.replace(/\.json$/, "")] = JSON.parse(fs.readFileSync(`${dir}/${f}`, "utf8"));
  }
  const notes = JSON.parse(process.env.ENV_NOTES_JSON || "[]");
  const meta = {
    pi_sha: "3da591ab74ab9ab407e72ed882600b2c851fae21",
    environment_notes: notes,
    packages,
  };
  fs.writeFileSync(`${out}/run-meta.json`, JSON.stringify(meta, null, 2));
'

log "parsing results into conformance.json"
node "$REPO_ROOT/conformance/parse-results.mjs"
log "done"
