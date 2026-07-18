# Conformance flip ledger (shim maintainer)

Tracks which pi modules are backed by the Rust engine (`atilla-napi`) vs still
running pi's own TypeScript. "Native" = a hand-written shim in
`conformance/shims/` overlays pi's source and delegates to the addon; the
manifest row's `status` is `native` and codegen preserves pi's original beside
the shim as `*.__pi_original__.ts`.

## Merge queue

The human merges in this order (rebasing each onto the prior as needed):

1. **#44** — faux provider native (foundations) — native 3 -> 4
2. **THIS PR** — coding-agent utils + export-html ansi-to-html — native 4 -> 10
   after rebasing onto #44; if it lands before #44 it is 3 -> 9 and #44 rebases
   behind it
3. **#50** — retry (ai) — queued next

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
| core/tools/path-utils | coding-agent | pending | — | no | hybrid port (batch 2) |
| core/tools/read | coding-agent | pending | — | no | hybrid port (batch 2) |
| core/tools/edit | coding-agent | pending | — | no | hybrid port (batch 2) |
| agent session modules | coding-agent | pending | — | no | batch 3 |
