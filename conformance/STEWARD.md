# Conformance flip ledger (shim maintainer)

Tracks which pi modules are backed by the Rust engine (`atilla-napi`) vs still
running pi's own TypeScript. "Native" = a hand-written shim in
`conformance/shims/` overlays pi's source and delegates to the addon; the
manifest row's `status` is `native` and codegen preserves pi's original beside
the shim as `*.__pi_original__.ts`.

## Determinism of the baseline

The baseline in `conformance.json` must be reproducible: an auto-refresh workflow
regenerates it and diffs the result, so any run-to-run flap shows up as noise.

The runner used to flap by +/-1 because the environment injects AWS credentials
and an `ANTHROPIC_BASE_URL`. With those visible, packages/ai's provider tests take
the live path instead of the offline/skip path the baseline documents, and that
drift also perturbed one coding-agent test
(`test/session-manager/file-operations.test.ts`). `scripts/conformance.sh` now
strips `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` /
`ANTHROPIC_BASE_URL` from the vitest invocation (setup steps keep the full
environment), which pins every suite offline. Two back-to-back full runs now
produce byte-identical `conformance.json`.

Deterministic per-package baseline (offline): agent 180/0/0, ai 556/0/738,
coding-agent 1491/87/47, tui 678/0, orchestrator 0/0/0 (passing/failing/skipped);
total 2905 passing, 87 failing, 785 skipped.

A second hardening idea -- running the vitest step as an unprivileged user so the
coding-agent chmod/EACCES asserts (which the root harness silently bypasses)
would exercise real permission errors -- was evaluated and deferred. In this tree
many suites create working dirs inside the package tree and transpile extensions
at runtime, which is fragile under a dropped uid: it made the baseline both worse
and less stable, so it is a follow-up rather than part of this change.

## Baseline auto-refresh

The committed `conformance.json` baseline is refreshed automatically by
`.github/workflows/conformance-baseline.yml` — option (c): **per-PR, on the PR's
own branch.** On every PR (opened/synchronize/reopened) from a same-repo branch
the workflow regenerates the full five-package baseline (`scripts/conformance.sh
--setup`) and, if the result drifted, commits `conformance.json` back to the
PR's HEAD branch under the `github-actions[bot]` identity.

Two design points to keep in mind when touching this:

- **Per-PR, on the PR branch — no protected-main bypass.** The refreshed
  baseline is committed to the PR's own head branch, never to `main`. Because
  PR branches are not the protected `main`, the default `GITHUB_TOKEN`'s
  `contents: write` is enough and no branch-protection bypass is needed. Each PR
  thus carries a baseline already reflecting its own changes, so the sticky
  per-PR delta reads against a current file (its passing/failing deltas settle
  to ±0; the absolute rust-backed numbers and the `conformance.json` git diff
  show the real change). Fork PRs get a read-only token and cannot push, so the
  same-repo guard skips them.
- **Determinism is what bounds the loop.** Creds-stripping is baked into
  `scripts/conformance.sh` itself (#85) — `AWS_ACCESS_KEY_ID`,
  `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, and `ANTHROPIC_BASE_URL` are
  unset for the vitest run so packages/ai stays on the offline profile
  (ai 556/0/738) and the baseline is byte-deterministic. Do not re-add a manual
  `env -u` in the workflow.

Loop termination: committing to the PR branch pushes a new HEAD and **does**
re-trigger the PR workflows — intentionally, so the required checks re-run on the
new HEAD and the PR stays mergeable (hence **no** `[skip ci]`). Determinism
bounds this to exactly one extra run: the re-triggered regen produces an
identical `conformance.json`, the `git diff` is empty, no second commit is made,
and the loop stops.

## Merge queue

Current native count on main: **13** (ai anthropic-messages + ai faux, tui keys
+ tui utils, coding-agent utils {ansi, mime, changelog, version-check, git} +
export-html ansi-to-html + tools {truncate, edit-diff, path-utils}). Batch 3
(this PR) takes it to **16**.

The human merges in this order (rebasing each onto the prior as needed):

1. **#44** — faux provider native (foundations) — native 3 -> 4 — **MERGED**
2. **#58** — coding-agent utils + export-html ansi-to-html — native 4 -> 10 —
   **MERGED**
3. **batch 2** — coding-agent tools truncate + edit-diff + path-utils — native
   10 -> 13 — **MERGED**
4. **THIS PR (batch 3)** — coding-agent core config + keybindings
   (resolve-config-value + trust-manager + keybindings) — native 13 -> 16
5. **#50** — retry (ai) — queued behind this PR

## Flip table

| module | package | status | pi test | native? | notes |
| --- | --- | --- | --- | --- | --- |
| utils/ansi | coding-agent | flipped | test/ansi-utils.test.ts (5) | yes | `stripAnsi`; shim keeps pi's non-string `TypeError` guard |
| utils/mime | coding-agent | flipped | test/image-process.test.ts (3) | yes | `detectSupportedImageMimeType`; `...FromFile` stays original |
| utils/changelog | coding-agent | flipped | test/changelog.test.ts (2) | yes | `normalizeChangelogLinks`; version crosses as JSON (string or ChangelogEntry); parse/getPath stay original |
| utils/version-check | coding-agent | flipped | test/version-check.test.ts (6) | yes | `comparePackageVersions` (null->undefined), `isNewerPackageVersion`; fetch fns stay original |
| utils/git | coding-agent | flipped | test/git-ssh-url.test.ts (10) | yes | `parseGitUrl`; returns pi's `GitSource` JSON shape; type stays original |
| core/export-html/ansi-to-html | coding-agent | flipped | test/export-html-whitespace.test.ts (3) | yes | `ansiToHtml` + `ansiLinesToHtml`; index.ts/tool-renderer.ts stay original; xss/skill-block asserts are inert source-text greps on template assets |
| utils/frontmatter | coding-agent | held | — | no | yaml block-scalar trailing-newline delta vs pi's parser |
| utils/pi-user-agent | coding-agent | excluded | — | no | test asserts `node/<ver>` + node arch, which the Rust port deliberately replaces |
| core/tools/truncate | coding-agent | flipped | test/tools.test.ts (read + bash blocks; no deep import) | yes | `formatSize`, `truncateHead`, `truncateTail`, `truncateLine`; shim re-adds pi's dropped JS default args (`options = {}`, `maxChars`) and consts; `TruncatedBy` enum + `Option` marshal to pi's `"lines" \| "bytes" \| null`; result crosses as JSON |
| core/tools/edit-diff | coding-agent | flipped | test/tools.test.ts (edit block, jsdiff `applyPatch` round-trip) + edit-tool-no-full-redraw | yes | 10 sync pure fns (`detectLineEnding`, `normalizeToLF`, `restoreLineEndings`, `normalizeForFuzzyMatch`, `fuzzyFindText`, `stripBom`, `applyReplacementsPreservingUnchangedLines`, `applyEditsToNormalizedContent`, `generateUnifiedPatch`, `generateDiffString`); shim re-adds `contextLines = 4`; `computeEditsDiff`/`computeEditDiff` stay original |
| core/tools/path-utils | coding-agent | flipped | test/path-utils.test.ts (13) | yes (hybrid) | `expandPath`, `resolveToCwd` (Rust `Result` → throw); `resolveReadPath` rebuilt in shim with real `accessSync` probe over native macOS filename transforms; `pathExists`/`resolveReadPathAsync` stay original |
| core/resolve-config-value | coding-agent | flipped | test/resolve-config-value.test.ts (10) | yes | all ported symbols; `env?` override crosses as JSON, process env read by Rust (`std::env::var`); `!command` cache + subprocess (default `sh -c`) in Rust; `None`→`undefined`; Windows configured-shell/stdin path not ported (that pi test passes via `sh -c` anyway) |
| core/trust-manager | coding-agent | flipped | test/trust-manager.test.ts (2) | yes | `getProjectTrustParentPath`, `getProjectTrustOptions`, `hasTrustRequiringProjectResources` (explicit `$HOME` injected by shim); `ProjectTrustStore` stays a JS class over the agent dir delegating to stateless native get-entry/set-many; `proper-lockfile` advisory lock not ported |
| core/keybindings | coding-agent | flipped | test/keybindings-migration.test.ts (3) | yes | native `KEYBINDINGS` default table (`keybindingsFor`, IndexMap-ordered) + `migrateKeybindingsConfig` (`{config, migrated}`); class keeps extending pi-tui's still-original base so `matches()`/conflict/`instanceof` stay pi-tui; `toKeybindingsConfig`/`loadRawConfig` glue rebuilt in shim |
| core/tools/read | coding-agent | pending | — | no | hybrid port (later batch) |
| core/tools/edit | coding-agent | pending | — | no | hybrid port (later batch) |
| agent session modules | coding-agent | pending | — | no | later batch |
