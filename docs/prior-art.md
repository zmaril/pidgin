# Prior Art: Rewriting `pi` in Rust

**Survey date:** 2026-07-18 · **Target:** [`earendil-works/pi`](https://github.com/earendil-works/pi) — the "Pi Agent Harness," a MIT-licensed TypeScript monorepo whose flagship is a self-extensible coding-agent CLI named `pi` (npm `@earendil-works/pi-coding-agent`), by Mario Zechner (`badlogic`/`badlogicgames`) and Armin Ronacher (`mitsuhiko`).

## TL;DR

**A clean-slate "no prior art" assumption is wrong.** pi has already been ported to Rust *many times over* — at least ~10 genuine standalone Rust ports/rewrites exist, plus a broad ring of Rust tooling that wraps the TS CLI. The standout is **[`Dicklesworthstone/pi_agent_rust`](https://github.com/Dicklesworthstone/pi_agent_rust)** (1,342★), a production-ready, **author-blessed** from-scratch port. Separately, the pi maintainers have **no plans for an official Rust rewrite** — lead maintainer `badlogic` filed a "Rewrite pi in Rust" issue as an explicit joke and their stance for non-TS users is "use pi's RPC mode."

Implication: our value isn't proving feasibility (done) — it's picking the right architecture. Read the existing ports before writing code; the best adjacent Rust building blocks (codex-rs, rust-genai, ratatui, rig) are mature and permissively licensed.

---

## 1. Actual pi → Rust ports

No fork of upstream diverged into Rust — every port below is a **standalone repo**. Ordered by maturity.

### Tier 1 — substantial / working

| Repo | Author | Stars | License | Last activity | What it is |
|---|---|---|---|---|---|
| [Dicklesworthstone/pi_agent_rust](https://github.com/Dicklesworthstone/pi_agent_rust) | Jeff Emanuel (`doodlestein`) | **1,342** | MIT + "Rider" (non-standard — verify) | Active (2026-07) | **The headline.** Author-blessed, from-scratch port. |
| [c4pt0r/pie](https://github.com/c4pt0r/pie) | Ed Huang (`dxhuang`, PingCAP/TiDB CTO) | **131** | MIT | Active (2026-07) | "Rust rewrite of pi-coding-agent," aimed beyond a general coding agent toward multi-agent orchestration. |

**`pi_agent_rust` in detail** — the reference to study:
- **Endorsement is real.** README: *"an authorized Rust port of Pi Agent by Mario Zechner, created with his blessing."* Confirmed in the author's [start tweet](https://x.com/doodlestein/status/2018416579449425991) and [finish tweet](https://x.com/doodlestein/status/2024526138102435934). Mario [publicly acknowledged it](https://x.com/badlogicgames/status/2024528220906017129): *"the mad man! it's not quite pi, but it's a neat experiment!"* — so: blessed, but framed as an experiment, not an official successor.
- **Structure:** single static binary, `<100ms` startup (vs 500ms–2s Node), `#![forbid(unsafe_code)]`, Rust 2024. 8 built-in tools, tree-branching sessions, 10+ providers, RPC/print/interactive modes. Documented Rust SDK. On crates.io (`pi_agent_rust`, ~2,378 downloads — the most-used of any pi port).
- **Notable engineering choices** (things to learn from):
  - Rolls its **own async runtime** `asupersync` (replaces Node's event loop) and its **own TUI lib** `rich_rust`/`charmed_rust` (a Rust port of Python Rich) instead of reusing ecosystem crates. This is a *lot* of surface area to own — a caution as much as a model.
  - **TS/JS pi extensions run unchanged in embedded QuickJS** with Node-API shims — no Node/Bun required. Claims 224/224 pi extensions pass a conformance suite. This is the single most important design idea if we want extension compatibility.
- **Caveat:** the "MIT + Rider" license is non-standard (crates.io lists it as `non-standard`) — **not safe to vendor** without legal review. Read it, don't copy it. Early skeptical HN reception noted a very-early version was largely AI-generated and buggy; the current version is far more built-out.

### Tier 2 — real multi-crate ports, smaller / partial

| Repo | Stars | License | Notes |
|---|---|---|---|
| [qhkm/pi-rs](https://github.com/qhkm/pi-rs) | 3 | MIT | Broadest scope: pi-ai (17+ providers), agent-core, tui, CLI, + Slack bot, web UI. 461+ tests. "Vibecoded." |
| [nktkt/pi](https://github.com/nktkt/pi) | 3 | MIT | Ports pi-ai + pi-agent + CLI; 3 crates on crates.io (`pi-coding-agent`/`pi-ai`/`pi-agent`, v1.0.0). Note: crate `repository` metadata points at the upstream TS repo despite not being maintainer-published — misleading, flag it. |
| [OdradekAI/opi](https://github.com/OdradekAI/opi) | 4 | MIT | Reimplementation (not API-compatible), 4 crates, v0.7.0, working terminal agent. |
| [metaphorics/pi-rust](https://github.com/metaphorics/pi-rust) | 3 | unstated | Greenfield rewrite keeping `~/.pi` config/session/auth drop-in compatible; unmodified TS extensions via an on-demand **Bun sidecar** (different bet than QuickJS). |
| [xmonader/pirs](https://github.com/xmonader/pirs) | 0 | MIT | 6+ crates; extensibility via **`.rhai` scripts** instead of TS. Alpha, 150+ tests. |
| [grainbook/grain-agent](https://github.com/grainbook/grain-agent) | — | MIT | `grain-agent-core`/`grain-agent-harness` on crates.io — early-beta port of pi's agent-runtime layer. |
| [rcarmo/rs-ai](https://github.com/rcarmo/rs-ai) | 1 | MIT | Ports the `pi-ai` LLM layer only; tracks upstream. |
| [y0usaf/pi-rs](https://github.com/y0usaf/pi-rs) | 0 | — | Rust mechanism + **Lua 5.4** policy. Skeleton/rebuild. |
| [ben1009/pi-rs](https://github.com/ben1009/pi-rs) | 0 | Apache-2.0 | pi-inspired multi-LLM agent; doesn't explicitly claim to be a port. |
| [Zeppelinpp/pi-rust](https://github.com/Zeppelinpp/pi-rust) | 0 | — | Early: pi-ai done, agent-core/tui placeholders. |
| [dbareautopi/mezzala](https://github.com/dbareautopi/mezzala) | 0 | MIT/Apache | *"pi, pero en Rust"* — scaffold only, impl dirs largely empty. |

**Extension-compatibility strategies observed** (the key design axis — how each keeps pi's TS extensions working): embedded **QuickJS** (`pi_agent_rust`), **Bun sidecar** (`metaphorics`), or *abandon* TS compat and adopt a native scripting layer — **Rhai** (`xmonader`), **Lua** (`y0usaf`), or none. This is the decision our rewrite most needs to make deliberately.

Non-Rust ports also exist (Zig: `DaviRain-Su/pi-mono-zig`, `DeanoC/ZiggyPiAi`; Kotlin: `multimail-dev/pi-droid`; Python: `jimmyzhouj/mini-agent`), showing this is a broadly re-implemented project.

## 2. Maintainer stance on a Rust rewrite

The pi maintainers are **not** planning an official Rust port:
- [Issue #4609 "Rewrite pi in Rust"](https://github.com/earendil-works/pi/issues/4609) was opened **by `badlogic` himself and closed as "completed" in ~20 seconds** — his only comment is literally **"joke"**; the thread is comedic (Rickroll, "why not brainfuck").
- [Issue #6185](https://github.com/earendil-works/pi/issues/6185) (a user asking for a Rust version citing a "performance bottleneck") was auto-closed `not_planned` with a `no-action` label.
- Stated alternative for non-TS users ([tweet](https://x.com/badlogicgames/status/2003074290242195545)): *"you can achieve pretty much the same via RPC mode, if TypeScript is not your language of choice."*
- No RFC (rfc.earendil.com) or GitHub Discussion argues TS-vs-Rust. The maintainers' only Rust involvement is (a) endorsing Emanuel's external port and (b) shipping a small Rust native-clipboard dependency (`@mariozechner/clipboard`).

**Read:** a Rust rewrite is a third-party affair. Upstream won't adopt it, but the lead author is friendly to the idea as an experiment — useful for goodwill, don't expect official support or coordination.

## 3. Adjacent Rust prior art (building blocks to reuse/vendor)

Not ports of pi, but the mature Rust pieces a rewrite should lean on. All permissive unless noted.

| Need | Best pick | Stars | License | Why |
|---|---|---|---|---|
| **Architectural reference + exec sandbox** | [openai/codex `codex-rs`](https://github.com/openai/codex) | 99k | Apache-2.0 | Closest production analogue to pi-agent-core + pi-coding-agent. `codex-linux-sandbox` (Landlock+seccomp) is a vendorable exec sandbox for the bash tool. |
| **Multi-provider LLM client** (the `pi-ai` layer) | [jeremychone/rust-genai](https://github.com/jeremychone/rust-genai) | 839 | MIT/Apache | The only Rust lib natively covering **all** of pi's providers: OpenAI+Anthropic+Gemini+Bedrock+Mistral. Thin client like pi-ai. |
| **Terminal UI w/ differential rendering** (the `pi-tui` layer) | [ratatui](https://github.com/ratatui/ratatui) + [crossterm](https://github.com/crossterm-rs/crossterm) | 22k | MIT | Immediate-mode + cell diffing = exactly pi-tui's "differential rendering." What codex-tui and forge use. **Prefer this over porting pi-tui's hand-rolled renderer** (and over `pi_agent_rust`'s custom `rich_rust`). |
| **Typed tool-calling / agent loop** | [0xPlaygrounds/rig](https://github.com/0xPlaygrounds/rig) | 8k | MIT | Compile-time-typed tools (serde), multi-turn streaming loop. Maps onto pi's `AgentTool`/`AgentToolCall`/`AgentState`. |
| **Mature shipping agent to study** | [block/goose](https://github.com/block/goose) | 51k | Apache-2.0 | Best real-world Rust agent runtime + extension/provider design; MCP-native, 15+ providers. |
| **Feature-closest Rust coding-agent CLI** | [antinomyhq/forge](https://github.com/antinomyhq/forge) | 7.5k | Apache-2.0 | Interactive TUI + one-shot CLI + custom skills + MCP — the closest existing Rust CLI to pi's shape. |

Also: `64bit/async-openai` (MIT, gold-standard OpenAI adapter reference); `bosun-ai/swiftide` (MIT, hook-driven agent harness whose hooks parallel pi's before/after-tool-call contexts); sandbox building blocks `landlock` + `seccompiler`/`extrasafe` (all permissive) if hand-rolling; `~alip/syd` (GPL — shell-out only, don't link).

## 4. Implications for our rewrite

**Reuse:**
- Study `codex-rs` for the agent-core architecture and take its sandbox crate for the exec tool.
- Use `rust-genai` for the provider layer, `ratatui`+`crossterm` for the TUI — do **not** hand-roll a Rich port the way `pi_agent_rust` did (`asupersync`/`rich_rust` is a huge maintenance surface it chose to own).
- Read `pi_agent_rust` and `c4pt0r/pie` closely as the two most complete same-target ports — but treat `pi_agent_rust`'s "MIT + Rider" license as non-vendorable until reviewed.
- Because our goal is a *continually-updating Rust mirror that tracks upstream pi* (not a one-shot rewrite), ports structured to track a specific upstream version are the most relevant prior art — `c4pt0r/pie` and `nktkt/pi` both explicitly reference tracking pinned upstream versions, which favors an architecture with a thin, mechanically-regenerable provider/tool layer over a hand-crafted divergent fork.

**Decide early — extension compatibility.** The ports split three ways: embedded QuickJS (keep TS extensions, `pi_agent_rust`), Bun sidecar (`metaphorics`), or native scripting/no-TS (Rhai/Lua). This is the load-bearing architectural choice and it's where the ports most disagree.

**Mistakes to avoid:**
- Owning a custom async runtime + TUI framework (`pi_agent_rust`) unless that's explicitly the point — huge surface for marginal gain over tokio+ratatui.
- Publishing crates whose `repository` metadata points at upstream (`nktkt/pi`) — misleading and a supply-chain/trust smell.
- Expecting upstream adoption or coordination — they've declined an official rewrite; position this as an independent effort.

**Feasibility is settled.** Multiple people have shipped working Rust ports of pi in months, several AI-assisted. The open question is architecture and extension strategy, not "can it be done."

---

*Search coverage (for negative credibility): GitHub repo/code search + fork network + `network/members`; crates.io API (`pi coding agent`, `earendil`, `agent harness` + direct crate pulls); upstream issues/PRs/Discussions + rfc.earendil.com; HN (Algolia), Reddit, lobste.rs, and maintainer socials/blogs (mariozechner.at, lucumr.pocoo.org, @badlogicgames, @mitsuhiko). Reddit/lobste.rs returned nothing on-topic — treat as not-found rather than proven-absent.*
