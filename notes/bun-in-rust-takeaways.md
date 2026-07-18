# Bun's Zig→Rust rewrite: takeaways for porting `pi` to Rust

Source: [Rewriting Bun in Rust — Bun Blog (Jarred Sumner, Jul 8 2026)](https://bun.com/blog/bun-in-rust)

## Summary of what the article actually says

Bun is a JavaScript/TypeScript runtime historically written in **Zig** (started April 2021, itself begun as a line-for-line port of esbuild's transpiler from Go). In this article Bun's creator Jarred Sumner describes rewriting the **entire runtime from Zig to Rust** — ~535,496 lines of Zig across 1,448 files — in **11 days** using Claude Code with ~64 parallel agents (the "Fable" model), at a cost of roughly **$165,000** in API tokens. The motivating problem was a recurring class of memory bugs (use-after-free, double-free, leaks in error paths) caused by mixing Zig's manual memory management with JavaScriptCore's garbage-collected values; Rust's borrow checker and `Drop`-based RAII turn many of those into compile-time errors. Note the direction: this is a **like-for-like port of an existing large codebase into Rust**, not a greenfield rewrite and not the reverse — and it was validated against a language-independent test suite that was never rewritten.

> Caveat on reception: the rewrite is contested. The creator of Zig publicly called the AI-generated output ["unreviewed slop"](https://www.theregister.com/devops/2026/07/14/zig-creator-calls-buns-claude-rust-rewrite-unreviewed-slop/5270743), and several write-ups (e.g. [Simon Willison](https://simonwillison.net/2026/Jul/8/rewriting-bun-in-rust/)) treat the passing test suite, not the diff, as the thing to trust. Treat the article as a vendor retrospective ("largely retrospective celebration rather than critique"), not a neutral post-mortem.

## Key claims from the article

**Strategy: big-bang, not incremental.** The article is explicit and unusually opinionated here:

> "Everything all at once is better. An incremental rewrite adds temporary code that you hope gets deleted eventually, and would be painful in the short-medium term."

Their chosen frame was a mechanical, transpile-like port that preserves the original architecture, deferring idiomatic cleanup:

> "Do the rewrite that looks like we transpiled our Zig code to Rust. We can gradually refactor it to reduce `unsafe` usage and look more like idiomatic Rust after Bun v1.4 ships."

The rationale against a slow incremental port was that it would freeze the product: "A rewrite in another language would take a small team of engineers a full year. It would mean freezing bugfixes, security fixes or feature development for that time."

**Why Rust over staying in Zig — memory/ownership.** The driver was safety at the GC/manual-memory boundary. They point to concrete v1.3.14 bugs (heap-use-after-free in zlib and HTTP/2, double-free in UDP sockets, leaks in crypto/TLS/file watchers). Their claim:

> "A large percentage of bugs from that list are use-after-free, double-free, and 'forgot to free' in an error path. In safe Rust, these are compiler errors."

And on why Zig specifically hurt: "Zig does not have constructors/destructors, and most cleanup is expected to be written out explicitly at each call site with `defer`." Rust's `Drop` replaced that: "Compiler errors are a better feedback loop than a style guide."

**Up-front analysis before any code.** They spent ~3 hours with Claude producing `PORTING.md` and a `LIFETIMES.tsv` that serialized lifetime/ownership decisions before porting:

> "I spent about 3 hours talking to Claude about how to map patterns from our Zig codebase closely to Rust."

Const generics were used to replace Zig's `comptime` parameters where feasible.

**Interop / FFI boundaries.** Roughly **20% of the code stayed in C/C++** — the embedded libraries (JavaScriptCore, uWebSockets, BoringSSL, SQLite). Mechanical translation preserved existing C FFI calls, and cross-language LTO let the linker inline across languages. Post-merge, they report ~4% of the Rust is inside `unsafe` (~13,000 `unsafe` keywords), and **78% of those `unsafe` blocks are a single line** — "a pointer from C++ or one call into a C library."

**Module/crate ordering was a real problem.** Splitting Zig's single compilation unit into ~100 Rust crates produced cyclical dependencies. They did not hand-fix; they ran a classification workflow then a refactor workflow:

> "Instead of starting over, I ran another workflow to classify where the code with cyclical dependencies should go and write it all down—and then another workflow to do the refactor."

**The compiler as a work queue.** After generation there were ~16,000 compiler errors, grouped by crate and distributed across the parallel agents; test failures were then looped over file-by-file. Linux went green by May 11; all six platform targets passed by May 14.

**Testing during a live port — the test suite as spec/oracle.** This is the load-bearing lesson. The test suite is in TypeScript, so it is language-independent and survived the rewrite unchanged:

> "Bun's own test suite is written in TypeScript which means it doesn't depend on the runtime's programming language."

They report **0 tests skipped or deleted** and ~1.3–1.4 million `expect()` assertions. The summarizing claim: "A language-independent test suite with a million assertions, adversarial code review and when something does go wrong, fixing the process that generates the code instead of hand-fixing the code." Tests were isolated with `systemd-run`/cgroups because some stress tests exhaust sockets or time out in debug builds.

**Adversarial review, split across context windows.** "The Claude that reviewed code is not the same Claude that authored it. The reviewer doesn't implement; the implementer doesn't review." Reviewers were told to assume the code is wrong. A stated review rule: "If you need a paragraph-long comment to justify why the workaround is OK, the code is wrong—fix the code." Three concrete pre-merge catches are given: a use-after-free from a `Box` dropped while libuv held the pointer (fixed with `Box::leak()`); invalid `timespec` from `trunc()` vs `floor()` on negative timestamps; and an eager-eval panic from `unwrap_or()` fixed with `unwrap_or_else()`.

**Team/velocity.** "This rewrite would've taken 3 engineers with full context on the codebase about a year. With 1 engineer using Fable & closely monitoring Claude Code, we went from start to 100% of the test suite passing on all platforms in 11 days." Peak throughput ~1,300 lines/min; 6,502 commits over 11 days.

**Tooling/workflow pain points.** False starts came from agents running destructive git commands in parallel. Fix: "instruct Claude to never run `git stash` or `git reset` or any `git` command that doesn't commit a specific file at once. No `cargo` either." Four worktrees × 16 agents avoided disk exhaustion and git conflicts.

**Regressions from language-semantics mismatch (19, all later fixed).** These are the subtle bugs a mechanical port introduces:
- `debug_assert!` is erased in release; Zig's `assert` always runs — so a side-effecting `insertStale()` inside an assert silently vanished, breaking hot reload.
- Rust keeps bounds checks that Zig's `ReleaseFast` removes, which *surfaced* latent off-by-one/overflow bugs (module resolver, filename interning) as panics.
- `comptime` format strings and color markers needed macro rework.

**What they'd do differently / caveats (mostly inferred).** The article has no explicit "lessons learned" section — that absence is itself worth noting. What reads as endorsed practice: the up-front `LIFETIMES.tsv`, banning slow/destructive commands in the agent workflow, and earlier test isolation. Sumner also admits low initial confidence: "At first, I didn't expect it to work."

## What transfers to porting `pi` to Rust

`pi` is ~100k src-LOC of TypeScript: a multi-provider LLM client, an agent tool-loop, a from-scratch differential-render TUI, and a `jiti`-based runtime-TypeScript extension system. Mapping the lessons:

- **The TS test suite is your spec/oracle — and this is the single most transferable idea.** Bun's whole method rests on a language-independent test suite. `pi`'s tests are in TypeScript and, unlike Bun, mostly test *application* logic (agent loop, provider request/response shaping, TUI diffing) rather than a runtime hosting a foreign test language. That is *more* favorable: the tests describe behavior you must reproduce in Rust, and they keep running against the TS original throughout. Before porting, audit coverage — Bun could trust "a million assertions"; `pi` needs enough behavioral coverage on the agent loop and streaming layer to serve as an oracle, or you're porting blind. *(Inference: gaps in `pi`'s current suite are the biggest risk to this method and should be filled first, on the TS side, where they're cheaper to write.)*

- **Streaming LLM layer / async surface.** Bun's async concern was GC-vs-manual-memory lifetime bugs; `pi`'s is different — it's SSE/streaming token parsing, backpressure, cancellation, and multi-provider response normalization sitting on an async runtime (Tokio). Bun didn't have to *design* a new async model (it kept its event loop and C libraries). `pi` does: Node's implicit single-threaded event loop and promise semantics don't map onto Tokio automatically. Bun's transferable move is the up-front artifact — the equivalent of `LIFETIMES.tsv` for `pi` is a written decision record on the async/cancellation model (streaming trait shape, `Stream` vs channels, how tool-call interruption propagates) *before* porting the client. *(Inference: this is where a mechanical "looks like the TS" port will fight you hardest — Rust async ownership across `.await` points has no TS analog.)*

- **The TUI.** A from-scratch differential-render TUI is stateful, mutation-heavy code — exactly where Rust's borrow checker converts sloppy shared-mutable patterns into compile errors, which is Bun's stated benefit ("compiler errors are a better feedback loop than a style guide"). But it's also where a like-for-like port of TS object-graph mutation will produce the most `Rc<RefCell<…>>` friction. Expect the TUI to be the module where "mechanical port" is least idiomatic and most tempting to redesign — treat it as its own bounded sub-project with heavy visual/behavioral tests.

- **The `jiti` extension system is `pi`'s hardest boundary — and Bun does NOT give you an answer.** Bun's port kept ~20% of code in C/C++ behind stable FFI; it never had to replace a mechanism whose entire premise (executing user-authored TypeScript at runtime) disappears when you leave Node. Rust has no `jiti`. This is a genuine architectural fork Bun didn't face, which reinforces the sibling recommendation to **decide the extension/plugin contract up front** rather than port it mechanically. Bun's only relevant precedent is negative: they preserved a hard interop boundary (C ABI) as a stable seam, which argues for defining `pi`'s plugin contract as an explicit, stable interface (e.g. a WASM/JS-engine sandbox, a subprocess protocol, or a native trait ABI) before touching dependent modules.

- **Module/crate ordering.** Bun hit cyclical-dependency hell splitting one compilation unit into ~100 crates and solved it with a classify-then-refactor pass. `pi` starts from ES modules that already have explicit import boundaries, so dependency structure is more legible — port leaf-first (provider client types → streaming → tool-loop → TUI → extension host), and expect the same crate-boundary/cycle cleanup Bun needed, just smaller.

- **Semantic-mismatch regressions.** Bun's 19 regressions came from `debug_assert!` erasure, bounds-check differences, and `comptime` formatting. `pi`'s analogues: JS number semantics (all f64, implicit coercion) vs Rust's typed integers; `undefined`/`null` vs `Option`; JSON round-tripping and error-path behavior in provider parsing; and truthiness in control flow. A mechanical port will silently import these. *(Inference: budget for a class of "passes types, fails at runtime" bugs concentrated in the provider-parsing and TUI-state layers.)*

## Does it change our recommendation?

Our recommendation: AI-accelerated **hand-rewrite** using TS + its test suite as executable spec, ported module-by-module, tsc/oxc as type oracle, extension-system plugin contract decided up front, with a napi-rs strangler-fig hybrid as fallback; we rejected a custom TS→Rust transpiler and continuous transpilation.

**Where Bun REINFORCES us:**

1. **AI-accelerated port of an existing codebase is viable at scale — strongly reinforced.** Bun did 535k LOC in 11 days. `pi` at ~100k LOC is well within demonstrated range. This is the headline validation of the whole approach.
2. **Test suite as executable spec — strongly reinforced.** This is the crux of both Bun's method and ours. Bun's success is explicitly attributed to a language-independent test suite plus "fixing the process that generates the code instead of hand-fixing the code." Our reliance on `pi`'s TS tests as the oracle is exactly right — with the caveat that we must verify coverage first.
3. **Decide hard interop boundaries up front — reinforced.** Bun's stable C ABI seam and pre-written `LIFETIMES.tsv` both argue for our "decide the extension-system plugin contract up front." Bun proves that the durable, well-specified seams are what let the mechanical bulk go fast.
4. **Adversarial review + type oracle — reinforced.** Bun's separate-context reviewers and use of the compiler as a work queue directly parallel our tsc/oxc-as-type-oracle plus AI review. Their concrete pre-merge catches show why an *independent* reviewer, not the author, matters.
5. **Rejecting a custom transpiler — reinforced.** Bun did NOT build a TS→Rust (Zig→Rust) transpiler; they used AI to produce a transpile-*like* result while a person owned the process. That is precisely our distinction between "AI-accelerated hand-rewrite" and "custom transpiler / continuous transpilation." Bun is evidence for our rejection, not against it.

**Where Bun CHALLENGES us:**

1. **Big-bang vs module-by-module strangler-fig — direct tension.** Bun is emphatic that "everything all at once is better" and that incremental ports add throwaway glue code. Our plan is module-by-module with a napi-rs strangler-fig *fallback* — i.e. incremental. This is the sharpest disagreement. **Honest read:** Bun could go big-bang because (i) they had an exhaustive language-independent test suite, (ii) they froze feature work, and (iii) they preserved architecture with a mechanical port. If `pi` can match (i) and accept (ii), the Bun evidence suggests a bolder, less-incremental port than our default. The napi-rs strangler-fig should be understood as a genuine fallback for when coverage is insufficient — not the preferred path — and our module-by-module ordering is about *authoring sequence*, which is compatible with a single-cutover release. It does **not** refute strangler-fig for a team that must keep shipping, but it does challenge us to justify incrementalism on coverage grounds rather than treating it as automatically safer.
2. **"Mechanical, transpile-like" port vs "idiomatic Rust core" — direct tension.** Bun deliberately produced non-idiomatic Rust (13k `unsafe` keywords) to ship, deferring idiomatic cleanup. Our recommendation targets an *idiomatic* Rust core from the start. **Honest read:** Bun's shortcut was viable because their `unsafe` is overwhelmingly thin FFI shims over battle-tested C libraries (78% single-line). `pi` has little such C-library surface, so a mechanical port would instead produce non-idiomatic *application* code (`Rc<RefCell>` graphs, cloned strings, un-Rustic error handling) with no comparable payoff — and the async/streaming and TUI layers actively resist mechanical translation. So Bun's mechanical-first tactic transfers poorly to `pi`; our idiomatic target is better justified for this codebase. This is a case where our recommendation should *hold against* Bun's example, with the reasoning made explicit.
3. **Resources.** Bun spent ~$165k and one expert operator running ~64 agents with custom dynamic workflows. **Honest read:** the *method* transfers; the *scale of tooling* may not. `pi` is 5x smaller, so cost scales down, but the "dynamic workflow orchestration of 64 parallel agents" was itself substantial infrastructure. Our plan should assume a more modest fan-out and correspondingly longer wall-clock, not an 11-day figure.

**Net:** Bun strongly validates the core of our recommendation (AI-accelerated hand-rewrite, tests-as-spec, up-front boundaries, no transpiler, adversarial review). It pushes back on two secondary choices: it favors a bolder single-cutover over incremental strangler-fig *when test coverage permits*, and it favors mechanical-first over idiomatic-first — but that second push does **not** transfer to `pi`, because `pi`'s hardest code is application logic and async, not thin FFI, so our idiomatic target and up-front async/extension-contract decisions remain the right call. The one concrete adjustment worth making: front-load a **coverage audit of `pi`'s TS test suite**, because that single variable is what determines whether we can be as aggressive (big-bang) as Bun or must lean on the incremental fallback.
