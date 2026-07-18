//! Injection seams: the production-grade abstraction boundaries the Rust core is
//! built on from the start, not test-only scaffolding.
//!
//! # Why these live here
//!
//! `design.md` mandates that the Rust core "builds injection seams in from the
//! start … as production-grade traits, not test hacks. This is the difference
//! between passing most of the suite and passing all of it." pi's own test suite
//! injects mocks *between* its internal modules and at the runtime boundary; a
//! monolithic Rust core cannot honor a JS mock of one of its internal collaborators
//! unless it exposes the *same* seam as an injectable dependency. The mock-seam
//! inventory (`notes/mock-inventory.md`) enumerates every such site and maps it to
//! one of these seams.
//!
//! # The five seams
//!
//! The inventory confirmed that the four seams `design.md` originally named are
//! necessary but not sufficient, and that a fifth — a subprocess / command runner
//! — is the single highest-leverage addition (it alone reaches 44 sites). This
//! module defines all five:
//!
//! 1. [`provider`] — the model/streaming provider seam (22 sites).
//! 2. [`http`] — the HTTP transport, including the WebSocket path (80 sites).
//! 3. [`clock`] — settable `now` **and** deterministic timer advance (29
//!    fake-timer / 58 advance-timer / 16 set-system-time sites).
//! 4. [`storage`] — the storage / execution-environment seam (3 sites).
//! 5. [`subprocess`] — the subprocess / command-runner seam (44 sites).
//!
//! Each seam is a trait with a production implementation and a deterministic test
//! implementation (a real clock and a controllable fake clock; a real subprocess
//! runner and a scripted one; and so on). Every language binding and the real
//! providers depend on the traits, so the same seams that make pi's mock-based
//! tests pass are also the ones production uses.
//!
//! # Location
//!
//! The seams live in `atilla-ai`, the leaf crate in the workspace dependency
//! graph (`ai ──▶ agent ──▶ coding-agent`). Placing them at the leaf lets the
//! faux provider (in `atilla-ai`) implement [`provider::Provider`] without a
//! dependency cycle, while the `atilla-core` façade re-exports this module so the
//! seams are reachable as part of the core surface. The 19 irreducible
//! "port-the-test" sites the inventory calls out (spies on a unit's own private
//! methods) are deliberately **out** of seam scope — no external seam reaches
//! them.

pub mod clock;
pub mod http;
pub mod provider;
pub mod storage;
pub mod subprocess;

pub use clock::{Clock, FakeClock, SystemClock, TimerId, Timers};
pub use http::{
    HostTransport, HttpRequest, HttpResponse, HttpTransport, ScriptedTransport, WebSocket,
    WsMessage,
};
pub use provider::{AbortSignal, Provider, StreamResult};
pub use storage::{ExecutionEnv, MemoryEnv, SystemEnv};
pub use subprocess::{
    CommandOutput, CommandRequest, CommandRunner, ScriptedCommandRunner, SystemCommandRunner,
};
