# Upstream tracking

atilla is a continually updating mirror of [pi](https://github.com/earendil-works/pi).
This note describes how the repository tracks upstream, so a fast-moving upstream
turns into tracked porting work instead of silent drift. It implements the
mirror strategy in `design.md` and section 7 of `notes/startup/PLAN.md`.

The system has three parts: a pinned commit, a correspondence map, and a weekly
drift job.

## The pin: `UPSTREAM_COMMIT`

`UPSTREAM_COMMIT` at the repository root records the exact upstream commit the
mirror currently targets. It is the machine-readable source of truth for the
tracking automation. The file holds comment lines starting with `#` and one
40-character commit SHA on its own line, so any tool can read the pin with a
single grep.

The `vendor/pi` submodule is pinned to the same commit. The submodule is the
vendored copy used by the conformance harness (codegen and the pi test suites);
`UPSTREAM_COMMIT` is the value the drift job reads. The two must stay identical.
When they disagree, `UPSTREAM_COMMIT` is authoritative for the drift job, and the
disagreement is itself a bug to fix by realigning the submodule pin. Both are
`3da591ab74ab9ab407e72ed882600b2c851fae21` (pi v0.80.10).

Every port PR that advances the mirror bumps both the `UPSTREAM_COMMIT` file and
the `vendor/pi` submodule pin together.

## The correspondence map

`scripts/upstream-correspondence.json` maps upstream pi source directories to
atilla crates and modules at directory granularity. It is what makes an upstream
diff portable: given the set of files a range of upstream commits touched, the
map says which atilla crate and module owns each one.

Each mapping has an `upstream` path prefix, the `crate` and `module` that mirror
it, the `target` path in this repository, and a `mirrored` flag. A mapping
matches an upstream path when the path equals the prefix or begins with the
prefix followed by a slash; when several prefixes match, the longest wins. The
`mirrored` flag is true when an atilla crate for that path exists today, and
false for a package that is planned but has no crate yet.

The map covers all 397 source modules listed in `conformance/manifest.json`. The
table below is the directory-level view; the JSON file is the authoritative,
machine-usable form, and the two are kept consistent by hand.

| Upstream directory | atilla crate | Module | Mirrored |
|---|---|---|---|
| `packages/ai/src/api` | `atilla-ai` | `api` | yes |
| `packages/ai/src/auth` | `atilla-ai` | `auth` | yes |
| `packages/ai/src/providers` | `atilla-ai` | `providers` | yes |
| `packages/ai/src/utils` | `atilla-ai` | `utils` | yes |
| `packages/ai/src/compat` | `atilla-ai` | `compat` | yes |
| `packages/ai/src` (root files) | `atilla-ai` | crate root | yes |
| `packages/agent/src/harness` | `atilla-agent` | `harness` | yes |
| `packages/agent/src/agent-loop.ts` | `atilla-agent` | `agent_loop` | yes |
| `packages/agent/src/agent.ts` | `atilla-agent` | `agent` | yes |
| `packages/agent/src/node.ts` | `atilla-agent` | `node` | yes |
| `packages/agent/src/proxy.ts` | `atilla-agent` | `proxy` | yes |
| `packages/agent/src/types.ts` | `atilla-agent` | `types` | yes |
| `packages/agent/src` (root files) | `atilla-agent` | crate root | yes |
| `packages/coding-agent/src/bun` | `atilla-coding` | `bun` | yes |
| `packages/coding-agent/src/cli` | `atilla-coding` | `cli` | yes |
| `packages/coding-agent/src/core` | `atilla-coding` | `core` | yes |
| `packages/coding-agent/src/extensions` | `atilla-coding` | `extensions` | yes |
| `packages/coding-agent/src/modes` | `atilla-coding` | `modes` | yes |
| `packages/coding-agent/src/utils` | `atilla-coding` | `utils` | yes |
| `packages/coding-agent/src` (root files) | `atilla-coding` | crate root | yes |
| `packages/tui/src` | none (`atilla-tui` planned) | — | no |
| `packages/orchestrator/src` | none (`atilla-orchestrator` planned) | — | no |

The `crate root` rows cover the package-root `.ts` files whose Rust module
placement is not yet decided in the current scaffold. The `tui` and
`orchestrator` packages have no crate yet, so their paths are reported as
planned-package drift rather than mirrored drift.

## The drift job

`.github/workflows/upstream-drift.yml` runs weekly (Monday 07:00 UTC) and on
manual dispatch. It calls `scripts/upstream-drift.sh`, which:

1. Reads the pinned SHA from `UPSTREAM_COMMIT`.
2. Clones upstream pi metadata and resolves upstream HEAD.
3. Counts how many commits upstream is ahead of the pin.
4. Runs `git diff <pin>..upstream/main` over `packages/`, then filters the
   touched paths through the correspondence map to find which mirrored crate
   modules changed and which planned packages changed.
5. Diffs the upstream test-file set against the pin to list new pi test files,
   which are new conformance work.
6. Diffs upstream source modules against `conformance/manifest.json` to list new
   modules that need manifest entries.
7. Opens or updates a single tracking issue titled
   `upstream drift: N commits, M mirrored paths touched`, labeled
   `upstream-drift`. It searches for the existing open issue by label first and
   edits it in place, so the job never files duplicate issues.

The job is non-gating. It only reports and never fails a PR or the build. The
script always exits with status 0, and the workflow step also carries
`continue-on-error` as a second guard.

Run the report locally without touching any issue:

```
scripts/upstream-drift.sh
```

Add `--emit-issue` to open or update the tracking issue (this needs the `gh`
CLI and a token, as provided in Actions).

## Re-syncing to a newer upstream

1. Run `scripts/upstream-drift.sh` to see the drift report and the tracking
   issue.
2. Port the touched modules the report lists, updating the manifest status rows
   and the porting ledger in `notes/startup/porting-map.md` as work lands.
3. Bump `UPSTREAM_COMMIT` and the `vendor/pi` submodule pin to the new commit in
   the same PR, keeping them identical.
4. Update the correspondence map if upstream added or moved a source directory,
   and update the manifest if upstream added or removed modules.
