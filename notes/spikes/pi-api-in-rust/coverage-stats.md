# "pi API in Rust" generator — feasibility spike coverage

**Verdict: YES, it works end-to-end and produces compilable Rust — with honest, measurable lossiness.**

Pipeline proven:
`pi orchestrator TS` → `hinzu api` → ApiReport JSON → **`hinzu api-fluessig`** (the converter built here)
→ fluessig `api.json` + `catalog.json` → **`fluessig-gen --rust-core`** (the backend added here)
→ `generated_pi_api.rs` → **`rustc --crate-type=lib` exits 0**.

Source package: `@earendil-works/pi-orchestrator` (72 public items).

## Item-level coverage (72 source items)

| Source kind | Count | Outcome |
|---|---:|---|
| `interface` | 21 | → 21 DTO structs (`models[]`) |
| `function` | 22 | → 22 trait fns (free-function interface `PiOrchestrator`) |
| `method` | 17 | → 17 trait fns (grouped under their class's interface) |
| `class` | 2 | → 2 op-bearing interfaces (`RpcProcessInstanceCore`, `OrchestratorSupervisorCore`) |
| `typeAlias` | 7 | 1 lifted to a catalog **enum** (`InstanceStatus`); 6 dropped (complex/conditional unions, not enums) |
| `const` | 3 | dropped (no op/DTO home) |

**Emitted Rust artifacts:** 21 structs + 1 enum + 3 traits (with 39 trait fns total).
**Dropped entirely:** 9 / 72 = **12.5%** (3 `const` + 6 non-enum `typeAlias`).
**Produced a typed Rust artifact:** 63 / 72 = **87.5%**.

## Type-fidelity within what was emitted

**Ops (39 trait fns):**
- **24 / 39 = 62% fully & faithfully typed** — every param and the return mapped to a real fluessig type.
- 15 / 39 = 38% **compile but carry a `Json`-degraded param or return** (the fn exists in the trait, but a type rode as `String`/`Json` instead of its real shape).

**Struct fields:** 76 / 78 = **97%** cleanly typed (2 degraded to `Json`).

**Params:** 16 / 31 clean, 15 degraded. **Returns:** 28 / 39 clean, 11 degraded.

## Why things degraded (every `Json` fallback is counted, not hidden)

| Cause | Count | Example |
|---|---:|---|
| unresolved type reference | 14 | `RpcCommand`, `RpcResponse`, `ChildProcess` — types referenced but never re-exported into the extracted public surface (cross-package / class handles) |
| function type | 9 | callback params like `(event: AgentSessionEvent) => void` — no fluessig home; rode as `String` |
| inline object literal | 4 | anonymous `{ ... }` param/return shapes |
| unparsable type expression | 1 | a rendered type the parser could not decompose |

Non-fatal ambiguity (a real typed mapping, just noted): `number → float64` (×2) — TS does not distinguish int/float.

## Honest reading

- The **DTO layer round-trips almost perfectly** (97% field fidelity, 21/21 interfaces → structs). Data shapes are the easy, high-value win.
- The **op layer is ~62% faithfully typed.** The 38% that degrade are dominated by two *inherent* TS→Rust gaps, not converter bugs:
  1. **Callbacks** (`=> void`): pi's RPC surface is event-driven; those params genuinely have no value-type. A real generator would model them as streams/handles (out of spike scope).
  2. **Unresolved refs** (14): types the ApiReport names but whose definitions are outside the single analyzed package (`ChildProcess` from node, cross-module RPC types). With a whole-program report these would resolve — the converter already resolves every ref that *is* present.
- **Named string-literal unions lift to enums cleanly** (`InstanceStatus`), but only 1 of 7 aliases was actually such a union; the other 6 are model-unions / conditional types that are correctly *not* forced into enums.

## Reproduce

```bash
# 1. converter (hinzu)
hinzu api-fluessig sample-apireport.json \
  --out-api api.json --out-catalog catalog.json --out-stats coverage-stats.json

# 2. Rust skeleton (fluessig)
fluessig-gen catalog.json schema_out.rs --api api.json --rust-core generated_pi_api.rs

# 3. compile proof (throwaway crate provides `anyhow`)
cd compile-proof && cargo build            # exit 0
rustc --edition 2021 --crate-type=lib \
  --extern anyhow=<libanyhow.rlib> -L compile-proof/target/debug/deps \
  generated_pi_api.rs                       # exit 0
```
