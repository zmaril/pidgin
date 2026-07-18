//! PHP native extension for atilla, built with `ext-php-rs` as a `cdylib`.
//!
//! This is the M0 scaffold: it proves the toolchain end to end — Rust compiles
//! an ext-php-rs extension, PHP loads the resulting `.so`, and a PHP call
//! reaches through the [`atilla-core`] façade and back. The exposed surface is
//! deliberately one call; the real bindings grow as the engine fills in.
//!
//! PHP is synchronous and request-scoped, so nothing here spins up a tokio
//! runtime. When async work does arrive it must follow the spike's rule (see
//! `throwaway/php-hello`): create one runtime lazily, per process, *after* any
//! php-fpm fork — never at module init.

use ext_php_rs::prelude::*;

/// PHP-visible handle onto the atilla engine.
///
/// Registered with PHP as the class `Atilla`. For M0 it carries no state and
/// exposes a single static method; instance surface arrives with later
/// milestones.
#[php_class]
pub struct Atilla;

#[php_impl]
impl Atilla {
    /// The atilla engine version, as reported by the [`atilla-core`] façade.
    ///
    /// Exposed to PHP as the static method `Atilla::version(): string`. The
    /// value is a real call through the façade — not a string baked into this
    /// binding — so PHP always sees the same authoritative number as the Rust
    /// core.
    pub fn version() -> String {
        atilla_core::version().to_string()
    }

    // TODO(M1): Session::open(path) — pending the sibling session-JSONL crate.
    //
    // Once version-3 JSONL parsing lands in atilla-agent and is surfaced
    // through atilla-core, this binding will expose it as:
    //
    //     /// Open a pi session file and return its messages plus stats.
    //     pub fn open_session(path: String) -> Session { ... }
    //
    // where `Session` is a second `#[php_class]` wrapping the parsed tree.
    // Intentionally left unimplemented: M0 does not ship a fake session.
}

/// Registers the extension's surface with PHP.
///
/// The extension name PHP reports (via `php -m` / `extension_loaded`) is the
/// crate *package* name, `atilla-php`; the loadable file is `libatilla_php.so`
/// (from `[lib] name`). See README.md for the naming details.
#[php_module]
pub fn module(module: ModuleBuilder) -> ModuleBuilder {
    module
}
