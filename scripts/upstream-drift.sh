#!/usr/bin/env bash
# upstream-drift.sh -- report how far the atilla mirror has drifted from pi.
#
# It reads the pinned upstream commit from the repo-root UPSTREAM_COMMIT file,
# fetches upstream pi, and compares the pinned commit against upstream HEAD:
#
#   1. how many commits upstream is ahead,
#   2. which touched paths map to atilla crates (via the correspondence map),
#   3. which upstream test files are new since the pin (new conformance work),
#   4. which upstream source modules are missing from conformance/manifest.json.
#
# It prints a Markdown report. With --emit-issue it opens or updates a single
# tracking issue (label upstream-drift) through the gh CLI, editing the existing
# open issue in place rather than filing a new one.
#
# This script is a REPORTER. It always exits 0 so it can never fail a build.
#
# Usage:
#   scripts/upstream-drift.sh                 # print the report to stdout
#   scripts/upstream-drift.sh --out FILE      # also write the report to FILE
#   scripts/upstream-drift.sh --emit-issue    # open or update the tracking issue
#
# Environment overrides:
#   PI_REMOTE     upstream git URL (default https://github.com/earendil-works/pi)
#   PI_REF        upstream ref to compare against (default HEAD of the clone)
#   ISSUE_LABEL   tracking-issue label (default upstream-drift)
#   GH_REPO       owner/repo for gh (default: current repo detected by gh)
#
# Dependencies: git, python3 (both present on GitHub ubuntu runners); gh only
# when --emit-issue is passed.

set -uo pipefail

PI_REMOTE="${PI_REMOTE:-https://github.com/earendil-works/pi}"
ISSUE_LABEL="${ISSUE_LABEL:-upstream-drift}"

EMIT_ISSUE=0
OUT_FILE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --emit-issue) EMIT_ISSUE=1 ;;
    --out) shift; OUT_FILE="${1:-}" ;;
    --out=*) OUT_FILE="${1#--out=}" ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "upstream-drift: ignoring unknown argument: $1" >&2 ;;
  esac
  shift || true
done

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
COMMIT_FILE="$REPO_ROOT/UPSTREAM_COMMIT"
CORRESPONDENCE="$REPO_ROOT/scripts/upstream-correspondence.json"
MANIFEST="$REPO_ROOT/conformance/manifest.json"

fail_soft() {
  # Emit a minimal report and exit 0 so the job stays green.
  echo "## upstream drift: report could not be produced"
  echo
  echo "$1"
  exit 0
}

[ -f "$COMMIT_FILE" ] || fail_soft "UPSTREAM_COMMIT not found at $COMMIT_FILE"
[ -f "$CORRESPONDENCE" ] || fail_soft "correspondence map not found at $CORRESPONDENCE"
[ -f "$MANIFEST" ] || fail_soft "manifest not found at $MANIFEST"

# Pinned SHA: first 40-hex token in UPSTREAM_COMMIT, ignoring comments/keys.
PINNED="$(grep -oE '[0-9a-f]{40}' "$COMMIT_FILE" | head -n1)"
[ -n "$PINNED" ] || fail_soft "no 40-char commit SHA found in UPSTREAM_COMMIT"

WORK="$(mktemp -d)"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

echo "upstream-drift: cloning $PI_REMOTE (metadata only) ..." >&2
if ! git clone --filter=blob:none --no-checkout --quiet "$PI_REMOTE" "$WORK/pi" 2>"$WORK/clone.err"; then
  fail_soft "could not clone upstream ($PI_REMOTE):
$(cat "$WORK/clone.err")"
fi

PI="$WORK/pi"
UPSTREAM_HEAD="$(git -C "$PI" rev-parse "${PI_REF:-HEAD}" 2>/dev/null)"
[ -n "$UPSTREAM_HEAD" ] || fail_soft "could not resolve upstream ref ${PI_REF:-HEAD}"

if ! git -C "$PI" cat-file -e "${PINNED}^{commit}" 2>/dev/null; then
  # The pinned commit is not reachable in the default clone; try to fetch it.
  git -C "$PI" fetch --quiet --filter=blob:none origin "$PINNED" 2>/dev/null || true
fi
if ! git -C "$PI" cat-file -e "${PINNED}^{commit}" 2>/dev/null; then
  fail_soft "pinned commit $PINNED is not present in upstream history"
fi

COMMITS_AHEAD="$(git -C "$PI" rev-list --count "${PINNED}..${UPSTREAM_HEAD}" 2>/dev/null || echo 0)"
COMMITS_AHEAD="${COMMITS_AHEAD:-0}"

# Touched paths under packages/ between the pin and upstream head.
git -C "$PI" diff --name-only "${PINNED}..${UPSTREAM_HEAD}" -- 'packages' > "$WORK/touched.txt" 2>/dev/null || : > "$WORK/touched.txt"
git -C "$PI" diff --stat "${PINNED}..${UPSTREAM_HEAD}" -- 'packages' > "$WORK/stat.txt" 2>/dev/null || : > "$WORK/stat.txt"

# File-set snapshots at each end, for test and module diffs.
git -C "$PI" ls-tree -r --name-only "$PINNED" -- 'packages' > "$WORK/files_old.txt" 2>/dev/null || : > "$WORK/files_old.txt"
git -C "$PI" ls-tree -r --name-only "$UPSTREAM_HEAD" -- 'packages' > "$WORK/files_new.txt" 2>/dev/null || : > "$WORK/files_new.txt"

# Everything else is analysis; hand off to python for the correspondence filter,
# test-set diff, and manifest diff. Python writes the Markdown report to stdout.
REPORT_BODY="$(
PINNED="$PINNED" UPSTREAM_HEAD="$UPSTREAM_HEAD" COMMITS_AHEAD="$COMMITS_AHEAD" \
PI_REMOTE="$PI_REMOTE" WORK="$WORK" CORRESPONDENCE="$CORRESPONDENCE" MANIFEST="$MANIFEST" \
python3 - <<'PY'
import json, os

work = os.environ["WORK"]
pinned = os.environ["PINNED"]
head = os.environ["UPSTREAM_HEAD"]
ahead = os.environ["COMMITS_AHEAD"]
remote = os.environ["PI_REMOTE"]

def readlines(name):
    p = os.path.join(work, name)
    try:
        with open(p) as f:
            return [l.rstrip("\n") for l in f if l.strip()]
    except FileNotFoundError:
        return []

touched = readlines("touched.txt")
files_old = set(readlines("files_old.txt"))
files_new = set(readlines("files_new.txt"))

corr = json.load(open(os.environ["CORRESPONDENCE"]))
mappings = corr["mappings"]

def resolve(path):
    best = None
    for m in mappings:
        p = m["upstream"]
        if path == p or path.startswith(p + "/"):
            if best is None or len(p) > len(best["upstream"]):
                best = m
    return best

def is_test(path):
    return "/test/" in path or "/tests/" in path or ".test." in path or ".spec." in path

def is_source(path):
    if "/src/" not in path:
        return False
    if is_test(path):
        return False
    if not (path.endswith(".ts") or path.endswith(".tsx")):
        return False
    if path.endswith(".d.ts"):
        return False
    return True

# 1. Mirrored source paths touched, grouped by crate::module.
mirrored_hits = {}   # "crate::module" -> count
planned_hits = {}    # package -> count (tui/orchestrator, no crate yet)
mirrored_total = 0
for path in touched:
    if is_test(path):
        continue
    if "/src/" not in path:
        continue
    m = resolve(path)
    if m is None:
        continue
    if m.get("mirrored"):
        mod = m["module"] or "(crate root)"
        key = "{}::{}".format(m["crate"], mod)
        mirrored_hits[key] = mirrored_hits.get(key, 0) + 1
        mirrored_total += 1
    else:
        pkg = path.split("/")[1] if "/" in path else path
        planned_hits[pkg] = planned_hits.get(pkg, 0) + 1

# 2. New / removed upstream test files since the pin.
old_tests = {p for p in files_old if is_test(p)}
new_tests = {p for p in files_new if is_test(p)}
added_tests = sorted(new_tests - old_tests)
removed_tests = sorted(old_tests - new_tests)

# 3. Upstream source modules missing from the manifest.
manifest = json.load(open(os.environ["MANIFEST"]))
manifest_srcs = {x["src"] for x in manifest["modules"]}
upstream_srcs = {p for p in files_new if is_source(p)}
new_modules = sorted(upstream_srcs - manifest_srcs)
dropped_modules = sorted(manifest_srcs - upstream_srcs)

# ---- render markdown ----
title = "upstream drift: {} commits, {} mirrored paths touched".format(ahead, mirrored_total)
out = []
out.append("<!-- upstream-drift-report -->")
out.append("## " + title)
out.append("")
out.append("- pinned commit: [`{}`]({}/commit/{})".format(pinned[:12], remote, pinned))
out.append("- upstream head: [`{}`]({}/commit/{})".format(head[:12], remote, head))
out.append("- commits ahead: **{}**".format(ahead))
out.append("- compare: {}/compare/{}...{}".format(remote, pinned, head))
out.append("")

if int(ahead) == 0:
    out.append("Upstream is level with the pin. No drift to report.")
    out.append("")

out.append("### Mirrored paths touched ({} source files)".format(mirrored_total))
out.append("")
if mirrored_hits:
    out.append("| atilla crate::module | files touched |")
    out.append("|---|---|")
    for key in sorted(mirrored_hits):
        out.append("| `{}` | {} |".format(key, mirrored_hits[key]))
else:
    out.append("None. No mirrored source paths changed upstream.")
out.append("")

if planned_hits:
    out.append("### Planned packages touched (no crate yet)")
    out.append("")
    out.append("| pi package | source files touched |")
    out.append("|---|---|")
    for pkg in sorted(planned_hits):
        out.append("| `{}` | {} |".format(pkg, planned_hits[pkg]))
    out.append("")

def listing(header, items, cap=40):
    out.append(header)
    out.append("")
    if not items:
        out.append("None.")
        out.append("")
        return
    for it in items[:cap]:
        out.append("- `{}`".format(it))
    if len(items) > cap:
        out.append("- ... and {} more".format(len(items) - cap))
    out.append("")

listing("### New upstream test files ({}) -- new conformance work".format(len(added_tests)), added_tests)
if removed_tests:
    listing("### Removed upstream test files ({})".format(len(removed_tests)), removed_tests)
listing("### New source modules ({}) -- need conformance/manifest.json entries".format(len(new_modules)), new_modules)
if dropped_modules:
    listing("### Source modules in manifest but gone upstream ({})".format(len(dropped_modules)), dropped_modules)

# Diffstat, capped so the issue body stays readable.
stat = readlines("stat.txt")
if stat:
    out.append("<details><summary>diff --stat (packages/, capped)</summary>")
    out.append("")
    out.append("```")
    for line in stat[:60]:
        out.append(line)
    if len(stat) > 60:
        out.append("... {} more lines".format(len(stat) - 60))
    out.append("```")
    out.append("</details>")
    out.append("")

out.append("---")
out.append("Generated by `scripts/upstream-drift.sh`. This is an automated, non-gating report.")

# Emit the title on the first line (for the workflow to read), a separator, then body.
print("TITLE:" + title)
print("---BODY---")
print("\n".join(out))
PY
)"

# Split the python output into title and body.
DRIFT_TITLE="$(printf '%s\n' "$REPORT_BODY" | sed -n 's/^TITLE://p' | head -n1)"
BODY="$(printf '%s\n' "$REPORT_BODY" | sed '1,/^---BODY---$/d')"

if [ -z "$DRIFT_TITLE" ]; then
  DRIFT_TITLE="upstream drift: report"
fi

# Always surface the report on stdout and, in Actions, in the step summary.
printf '%s\n' "$BODY"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  printf '%s\n' "$BODY" >> "$GITHUB_STEP_SUMMARY" || true
fi
if [ -n "$OUT_FILE" ]; then
  printf '%s\n' "$BODY" > "$OUT_FILE" || true
fi

if [ "$EMIT_ISSUE" -eq 1 ]; then
  if ! command -v gh >/dev/null 2>&1; then
    echo "upstream-drift: gh CLI not available; skipping issue update" >&2
    exit 0
  fi

  REPO_ARG=()
  [ -n "${GH_REPO:-}" ] && REPO_ARG=(--repo "$GH_REPO")

  # Ensure the label exists (idempotent; ignore failure).
  gh label create "$ISSUE_LABEL" "${REPO_ARG[@]}" \
    --color BFD4F2 --description "Automated upstream (pi) drift tracking" 2>/dev/null || true

  BODY_FILE="$WORK/issue-body.md"
  printf '%s\n' "$BODY" > "$BODY_FILE"

  # Find an existing open tracking issue by label.
  EXISTING="$(gh issue list "${REPO_ARG[@]}" --state open --label "$ISSUE_LABEL" \
    --json number --jq '.[0].number' 2>/dev/null || true)"

  if [ -n "$EXISTING" ] && [ "$EXISTING" != "null" ]; then
    echo "upstream-drift: updating existing issue #$EXISTING" >&2
    gh issue edit "$EXISTING" "${REPO_ARG[@]}" \
      --title "$DRIFT_TITLE" --body-file "$BODY_FILE" >/dev/null 2>&1 || \
      echo "upstream-drift: failed to edit issue #$EXISTING" >&2
    echo "upstream-drift: issue #$EXISTING updated" >&2
  else
    echo "upstream-drift: creating new tracking issue" >&2
    gh issue create "${REPO_ARG[@]}" \
      --title "$DRIFT_TITLE" --body-file "$BODY_FILE" --label "$ISSUE_LABEL" \
      >/dev/null 2>&1 || echo "upstream-drift: failed to create issue" >&2
  fi
fi

exit 0
