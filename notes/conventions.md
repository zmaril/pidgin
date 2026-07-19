# atilla — repo conventions

Hard-won rules that new sessions keep rediscovering. Read this before touching
CI-sensitive files, the conformance baseline, or any file governed by
`STEWARD.md`.

## Straitjacket (custom lint, pinned `v0.2.3`)

Straitjacket runs as its own CI check (`zmaril/straitjacket@v0.2.3`,
`straitjacket --format text` from the repo root). Only **errors** fail CI;
warnings do not. CI checks out **without** submodules, so `vendor/pi` is empty
and only atilla source is scanned. CI scans the **PR merge commit**
(`refs/pull/N/merge`), not the branch tip — so an error in a file that changed
only on newer `main` is a merge artifact, not something you introduced.

- **File-size ceiling = 1500 lines.** Fix by splitting the file into a directory
  module; keep under ~1400 for margin. Do **not** suppress the file-size rule.
- **Duplication between faithful-mirror pairs.** Faithful ports that mirror pi
  (including its duplication) need a **file-level** marker, placed after the
  module doc comment:

  ```rust
  // straitjacket-allow-file:duplication
  ```

  Use the file-level form, not line-level markers (rustfmt moves comments and
  the finding reappears on shifted lines). The marker is read only from the
  **alphabetically-first** of the two cloned files (cpd keeps that one as
  `fragment_a`). If the first-sorting file is off-limits, **rename your new file
  so it sorts first**.
- **Emoji in source.** Replace literal emoji glyphs with Rust unicode escapes
  that produce the byte-identical string — e.g. a smiley becomes `"\u{1F642}"`,
  a ZWJ sequence becomes `"\u{1F469}\u{200D}\u{1F4BB}"`. Do **not** rely on an
  allow marker for emoji.

Verify locally by building Straitjacket from source at tag `v0.2.3` (not
`main`, which carries extra rules), scoped to changed paths and excluding
`vendor/`.

## codespell

A **separate** CI check, independent of Straitjacket. It flags real
dictionary-word typos in code, strings, and comments. Fix by **renaming the
offending source token** — do not reach for `.codespellrc` ignores (the action's
ignore input has proven unreliable).

## vale (prose linter)

Runs `errata-ai/vale-action` on `*.md` with `fail_on_error: true`. `.vale.ini`
disables `Vale.Spelling` (codespell owns spelling) and keeps the terminology and
proselint checks.

- House style: write **"façade" with the cedilla (ç)**, consistently, in prose
  and docs. This is the form used across the codebase.
- No literal emoji in Markdown, and keep tokens ordinary so codespell stays
  quiet.

## Conformance regen

The committed `conformance.json` baseline **must** be regenerated with provider
credentials unset, or the offline CI runner reports phantom deltas. The remote
environment injects `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and
`ANTHROPIC_BASE_URL`; strip them for the run:

```sh
env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u ANTHROPIC_BASE_URL \
  bash scripts/conformance.sh
```

Two back-to-back creds-stripped runs produce a byte-identical `conformance.json`.
A baseline captured with creds present shows a phantom pass/fail delta on every
PR. Trust a fresh creds-stripped regen, not a remembered per-package number.

## Reading CI / merge-safety

atilla is an Actions-only repo, so CI posts **check-runs**, not legacy commit
statuses. The combined-status endpoint always returns `state: pending,
total_count: 0` — that does **not** mean CI failed or is absent. Gate on the
**check-runs API for the exact head SHA**: require a non-zero check-run count
**and** all of them `success`. (`mergeable_state: unknown` means not yet
recomputed, not a conflict.)

## GITHUB_TOKEN pushes do not trigger CI

Pushes authenticated with the default `GITHUB_TOKEN` do **not** spawn new
workflow runs — this is GitHub's anti-recursion behavior. If you push with that
token and see no checks appear, that is expected; a maintainer push or a manual
re-trigger is needed to get checks to run.

## Baseline ownership (`STEWARD.md`)

A single designated writer — the `STEWARD.md` role — is the **only** writer for
the conformance baseline and ledger:

- `conformance/manifest.json`
- root `conformance.json`
- `conformance/STEWARD.md` (and root `STEWARD.md`)

**Flip-crew PRs may touch only** additions under `crates/atilla-napi`, shim
files, and manifest **row additions** — never the `conformance.json` baseline
and never `STEWARD.md`. Baseline regens (creds-stripped, see above) are done by
that single writer at merge-sequence time. Attribution stays honest: list a test
file under a native row only when a genuine majority of its cases run native via
the addon; when in doubt, under-report rather than over-claim.

## atilla-napi lib.rs: keep it a thin module list

Every napi addition (a flip's #[napi] class/functions) goes in its OWN module file crates/atilla-napi/src/<name>.rs with only a `mod <name>;` line added to lib.rs — never inline code in lib.rs. Rationale: Straitjacket enforces a 1500-line file-size ceiling; lib.rs hit 1621 lines and had to be split (autocomplete.rs, #149). Keeping lib.rs a thin mod list keeps it under the ceiling and keeps cross-PR merges to single additive mod lines (trivial keep-both resolution).
