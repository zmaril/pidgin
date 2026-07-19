#!/usr/bin/env bash
#
# rename-to-pidgin.sh — idempotently rename this project from "atilla" to "pidgin".
#
# The project is a Rust mirror of `pi`; it is being renamed to `pidgin`
# ("pi in many languages"). This performs the whole mechanical rename and is
# safe to re-run (pidgin tokens contain no atilla substrings; moves are guarded).
#
# Case-aware:  atilla -> pidgin,  Atilla -> Pidgin,  ATILLA -> PIDGIN
#
# NOT renamed (guardrails):
#   * vendor/pi/**              upstream submodule (not tracked as files here)
#   * plain "pi" / PI_* vars    the upstream project this repo mirrors
#   * github.com/zmaril/atilla  repo URL (GitHub repo rename handled separately)
#   * this script itself
#
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

SELF="scripts/rename-to-pidgin.sh"

echo ">> content replacement (case-aware) across tracked files"
# git ls-files lists only tracked files: excludes vendor/pi (submodule gitlink),
# target/ and node_modules/ (gitignored). We additionally skip this script.
git ls-files -z -- . ":(exclude)$SELF" | while IFS= read -r -d '' f; do
  case "$f" in vendor/pi/*) continue ;; esac
  [ -f "$f" ] || continue
  perl -i -pe '
    s{(?<!zmaril/)atilla}{pidgin}g;   # protect the github.com/zmaril/atilla URL
    s{Atilla}{Pidgin}g;
    s{ATILLA}{PIDGIN}g;
  ' "$f"
done

echo ">> git mv crate directories crates/atilla-* -> crates/pidgin-*"
for d in crates/atilla-*; do
  [ -d "$d" ] || continue
  git mv "$d" "crates/pidgin-${d#crates/atilla-}"
done

echo ">> git mv workflow files .github/workflows/atilla-* -> pidgin-*"
for f in .github/workflows/atilla-*; do
  [ -e "$f" ] || continue
  git mv "$f" ".github/workflows/pidgin-${f#.github/workflows/atilla-}"
done

echo ">> normalize formatting (rename reorders use-lines rustfmt enforces)"
if command -v cargo >/dev/null 2>&1; then
  cargo fmt --all || true
  ( cd bindings/php && cargo fmt --all || true )
fi

echo ">> make Vale's reviewdog reporter local (its PR-diff fetch 406s past 300 files)"
VALE_YML=".github/workflows/vale.yml"
if [ -f "$VALE_YML" ]; then
  if grep -qE 'reporter:[[:space:]]*github' "$VALE_YML"; then
    perl -i -pe 's/reporter:[[:space:]]*github[\w-]*/reporter: local/g' "$VALE_YML"
  elif ! grep -qE 'reporter:[[:space:]]*local' "$VALE_YML"; then
    perl -0777 -i -pe 's/(uses:[[:space:]]*errata-ai\/vale-action[^\n]*\n(\s+)with:\n)/${1}${2}  reporter: local\n/s' "$VALE_YML"
  fi
fi

echo ">> done. Now regenerate lockfiles: cargo build --workspace (root) and in bindings/php"
