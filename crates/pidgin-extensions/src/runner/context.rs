//! The minimal `ExtensionContext` data threaded into JS handlers.
//!
//! pi's `createContext()` (`runner.ts:665`) builds a `ctx` bag with ~17 members
//! — getters (`ui`, `mode`, `hasUI`, `cwd`, `sessionManager`, `modelRegistry`,
//! `model`, `signal`) and bound host closures (`getSystemPrompt`,
//! `isProjectTrusted`, `abort`, `compact`, …). The offline acceptance suite reads
//! almost none of them: the only `ctx` call any fixture makes is
//! `ctx.getSystemPrompt()` (a data getter the chained `before_agent_start` test
//! relies on). No acceptance fixture calls an action method (`sendMessage`,
//! `appendEntry`, `setModel`, `exec`, `setActiveTools`, …) — see the
//! dispatch-boundary analysis.
//!
//! So this carries only the data the getters report. It is serialized to JSON and
//! passed to `globalThis.__pidgin.makeContext` (see [`crate::api_ops`]), which
//! reconstitutes the getters JS-side and leaves the action methods as
//! present-but-no-op stubs. Building the real host-backed action methods (the
//! deno→Rust-host ops) is deferred — nothing in the acceptance suite exercises
//! them.

use serde_json::{json, Value};

/// The minimal context configuration threaded into every handler as JSON.
#[derive(Debug, Clone)]
pub struct ContextConfig {
    /// The extension working directory (pi's `ctx.cwd`).
    pub cwd: String,
    /// The system prompt `ctx.getSystemPrompt()` reports (the base value; the
    /// `before_agent_start` emit overrides it per handler with the chained one).
    pub system_prompt: String,
    /// The UI mode (`"print"` / `"rpc"` / `"tui"`); pi's `ctx.mode`.
    pub mode: String,
    /// Whether a UI is attached; pi's `ctx.hasUI`.
    pub has_ui: bool,
    /// Whether the project directory is trusted; pi's `ctx.isProjectTrusted()`.
    pub project_trusted: bool,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            cwd: String::new(),
            system_prompt: String::new(),
            mode: "print".into(),
            has_ui: false,
            project_trusted: true,
        }
    }
}

impl ContextConfig {
    /// A config rooted at `cwd`, with the default print-mode / no-UI context.
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            ..Self::default()
        }
    }

    /// Set the base system prompt `ctx.getSystemPrompt()` reports.
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = system_prompt.into();
        self
    }

    /// The ctx JSON passed to `makeContext`, using the base system prompt.
    pub fn to_json(&self) -> Value {
        self.to_json_with_prompt(&self.system_prompt)
    }

    /// The ctx JSON with `getSystemPrompt()` overridden to `system_prompt` — used
    /// by `emitBeforeAgentStart`, where the prompt chains across handlers and
    /// `ctx.getSystemPrompt()` must stay in sync with it (`runner.ts:1070`).
    pub fn to_json_with_prompt(&self, system_prompt: &str) -> Value {
        json!({
            "cwd": self.cwd,
            "systemPrompt": system_prompt,
            "mode": self.mode,
            "hasUI": self.has_ui,
            "projectTrusted": self.project_trusted,
        })
    }
}
