# pidgin-model-catalog

A vendored, parsed snapshot of [pi](https://github.com/earendil-works/pi)'s
generated LLM model catalog, exposed as strongly-typed Rust.

pi generates its catalog at build time from
[`https://models.dev/api.json`](https://models.dev/api.json) (filtered to
tool-call-capable models) plus supplemental provider endpoints, and never
commits the result. This crate captures a point-in-time snapshot of that
generated output, embeds the aggregate via `include_str!`, and parses it once
on first use.

## Usage

```rust
let catalog = pidgin_model_catalog::catalog();
for provider in catalog.providers() {
    for (_provider, model) in catalog.all_models() {
        // read model.cost, model.context_window, model.reasoning, ...
    }
}
let claude = catalog.model("anthropic", "claude-fable-5");
```

## Intended pidgin-ai integration

pidgin-ai's provider registry consumes `catalog()` to enumerate providers and
models and to read each model's pricing and capabilities (context window,
modalities, reasoning support, thinking-level map) without issuing any network
calls of its own. This crate owns only the data and its parsing, while pidgin-ai
owns the HTTP clients and per-API request/response handling, so the two can
evolve independently against a stable, embedded data surface.

The `compat` field is intentionally left as a raw `serde_json::Value` and the
`api`/`provider` fields as plain `String`s (they are open unions upstream);
pidgin-ai can strongly-type `compat` per-API when it needs to.

## Data location decision

The snapshot lives in **`crates/pidgin-model-catalog/data/`**, not in
`conformance/data/`, for two reasons:

1. This is the Rust consumption artifact — it is embedded via `include_str!`
   directly into this crate, so it belongs alongside the crate that owns it.
2. The conformance harness has a different requirement. pi's committed
   `*.models.ts` wrappers `import "./data/<id>.json"` relative to the
   `vendor/pi` tree, and its vitest suite loads `models.generated.ts`. So
   `scripts/conformance.sh` must regenerate in-place inside `vendor/pi` and
   cannot consume this crate's copy without a larger refactor. Making the
   harness reuse this snapshot is therefore not a small tweak, so
   `scripts/conformance.sh` is deliberately left untouched.

## Refreshing the snapshot

Run [`scripts/refresh-model-catalog.sh`](../../scripts/refresh-model-catalog.sh)
from the repo root. It re-runs pi's generator at the current `vendor/pi`
submodule pin, copies `models.json` / `providers.json` / `providers/` into
`data/`, and rewrites `data/manifest.json` with a fresh timestamp, the current
pin, and recomputed provider/model counts. This is a manual, non-gating step
tied to upstream pin bumps.
