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
`.github/workflows/conformance-baseline.yml`. On every push/merge to `main` the
workflow regenerates the full five-package baseline (`scripts/conformance.sh
--setup`) and, if the result drifted, commits `conformance.json` back to `main`
under the `github-actions[bot]` identity with a `[skip ci]` message.

Three design points to keep in mind when touching this:

- **Main only, never per-PR.** The baseline moves only on `main`. PR runs stay
  read-only, so a PR's conformance delta always compares against the last merged
  baseline rather than a moving target. Do not add per-PR baseline writes.
- **Offline profile is enforced inside `conformance.sh`.** The regen runs
  `scripts/conformance.sh --setup` plainly; the script strips `AWS_ACCESS_KEY_ID`,
  `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, and `ANTHROPIC_BASE_URL` from the
  vitest invocation itself (#85), so packages/ai stays on the offline profile (ai
  556/0/738). With ambient creds present, ai flips to the live-failing profile
  (562/24/708) and the baseline would be non-deterministic. No manual `env -u`
  wrapper is needed in the workflow.
- **Pushing to protected `main` needs the bot ruleset bypass.** The commit-back
  pushes `conformance.json` straight to protected `main`, so `main`'s branch
  ruleset must grant `github-actions[bot]` a one-time bypass for the default
  `GITHUB_TOKEN` push to be accepted. Without it, branch protection rejects the
  push.

Loop prevention: the commit-back is a no-op when the baseline is unchanged (the
steady state), and the `[skip ci]` message stops the commit-back push from
re-triggering CI.

## Merge queue

Current native count on main: **38** (agent 5, ai 7, coding-agent 15, tui 11).

Wave settled state (this session): native **16 → 38** — the full tui surface is
now native; the agent-loop flip is an honest-0 hybrid (single-text-turn via
bridge); the ext-tests flip is parked as a deno-gated follow-up. The earlier
tui-pure batch that took native 16 → 21 is folded into this total.

The human merges in this order (rebasing each onto the prior as needed):

1. **#44** — faux provider native (foundations) — native 3 -> 4 — **MERGED**
2. **#58** — coding-agent utils + export-html ansi-to-html — native 4 -> 10 —
   **MERGED**
3. **batch 2** — coding-agent tools truncate + edit-diff + path-utils — native
   10 -> 13 — **MERGED**
4. **#102 (batch 3)** — coding-agent core config + keybindings
   (resolve-config-value + trust-manager + keybindings) — native 13 -> 16 —
   **MERGED**
5. **THIS PR (tui-pure batch)** — tui fuzzy + word-navigation + truncated-text +
   markdown + keybindings — native 16 -> 21
6. **#50** — retry (ai) — queued behind this PR

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
| fuzzy | tui | flipped | test/fuzzy.test.ts (14) | yes | `fuzzyMatch` native; `fuzzyFilter` re-implemented in JS over native `fuzzyMatch` (its `getText` callback can't cross the boundary) |
| word-navigation | tui | flipped | test/word-navigation.test.ts (19) | yes | `findWordBackward`/`findWordForward` on the default-segmenter path; shim delegates to original when `options.segment`/`isAtomicSegment` callbacks are supplied |
| components/truncated-text | tui | flipped | test/truncated-text.test.ts (9) | yes | `TruncatedText` class re-implemented; `render(width)` → native `truncatedTextRender` |
| components/markdown | tui | flipped | test/markdown.test.ts (66) | yes (gated) | `markdownRender` on the default-theme / no-padding / no-defaultTextStyle / no-options path (theme probed against exact chalk-l3 output); delegates to pi's original class for custom theme/padding/style/options and when `getCapabilities().hyperlinks` (OSC 8 seam) is on |
| keybindings | tui | flipped | test/keybindings.test.ts (4) | yes | `KeybindingsManagerCore` (napi class) backs resolution — `matches`/`getKeys`/`getConflicts`/`getResolvedBindings`; shim keeps `definitions`/`userBindings`/`getDefinition`/`getUserBindings` as JS; defs + user bindings cross as ordered JSON arrays (preserve insertion order); `setKeybindings`/`getKeybindings`/`TUI_KEYBINDINGS` stay original |
| core/tools/read | coding-agent | pending | — | no | hybrid port (later batch) |
| core/tools/edit | coding-agent | pending | — | no | hybrid port (later batch) |
| agent session modules | coding-agent | pending | — | no | later batch |
