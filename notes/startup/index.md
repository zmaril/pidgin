# notes/startup

The July 2026 research and planning phase, one file per workstream. These are the source material for `../design.md`, which is authoritative where they disagree.

- `PLAN.md` — the original end-to-end plan: goals, workspace layout, binding architecture, conformance, upstream tracking, and milestones.
- `porting-map.md` — inventory of pi at pinned commit `3da591ab`: per-package LOC, the dependency DAG, recommended porting order, and the porting ledger.
- `testing-strategy.md` — how pi's own suite runs against the Rust core: napi shim packages, the src-module swap, injection seams, the coverage audit, and the conformance dashboard.
- `communications.md` — everything in pi that crosses a process or network boundary, and the chosen Rust stack per surface.
- `extensibility.md` — the cross-language extension API: one Rust registry, with host-language bindings exposing pi's ExtensionAPI shape.
- `deep-hooks.md` — dispatch mechanics for host-language hook closures: trampoline versus rendezvous, thread affinity, timeouts, and ordering.
- `tui-ratatui.md` — assessment of rebuilding pi's TUI on ratatui; conclusion: port pi's renderer and width contract faithfully instead.
- `ts-to-rust.md` — why there is no transpiler path: tool survey, pi codebase characterization, and the hand-rewrite decision.
- `bun-in-rust-takeaways.md` — lessons from Bun's Zig-to-Rust rewrite applied to pi: tests as spec, big-bang porting, adversarial review.
- `prior-art.md` — survey of the roughly ten existing pi-in-Rust ports and the adjacent Rust building blocks worth studying.
