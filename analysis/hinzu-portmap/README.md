# hinzu port-diff flip-frontier extract

Compact, join-ready extract from a hinzu port-diff run over the 5 pi packages
(ai, agent, coding-agent, orchestrator, tui). Generated 2026-07-20.

## Join key
Everything is keyed on the **pi source path in manifest format**:
`packages/<pkg>/src/<...>.ts` — identical to `conformance/manifest.json`'s
`modules[].src`. Join any file here directly to manifest on that string.

## Files
- `ported-modules.json` — array, one record per pi source module (all 344, every
  band). Fields: `src, package, band (DONE|PORTED|STARTED|NOT-STARTED), coverage,
  matched_symbols, total_symbols, mapped_target, manifest_status
  (native|original|MISSING)`. band/coverage/symbols/mapped_target come from
  `portdiff-<pkg>.json` per-file records; `manifest_status` from a join to manifest.
- `module-deps.json` — adjacency `{ "<src>": ["<dep-src>", ...] }`; key depends on
  each value. Derived from `src-<pkg>.graph.json` `.file_edges` (file-level edges),
  keeping only intra-repo `src/`→`src/` edges and normalizing both endpoints to
  manifest `src` format. Test/example importers and external deps are excluded (no
  source→source dependency is lost). See its `_meta`.
- `blocker-labels.json` — `{ "<src>": "<label>" }` heuristic blocker class
  (already-native, not-started, flippable-now, dep-not-native,
  structural-only-low-coverage). flippable-now = band PORTED and all intra-repo
  deps native (or none). Precedence + label defs in its `_meta`.
- `summary.json` — copied verbatim from the run (per-package bands, frontier lists).

## Provenance
pidgin main 7756e93, pi_sha 3da591ab, hinzu main ae2ebd7, generated 2026-07-20.

## Caveats
`blocker-labels.json` is a best-effort heuristic. Only `DONE` / manifest `native`
is a test-verified band (DONE aligns exactly with the 39 native modules here); all
other bands and every blocker label are structural inferences, not test-verified.
