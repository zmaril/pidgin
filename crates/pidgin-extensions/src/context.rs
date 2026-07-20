//! The minimal extension context the registration path needs.
//!
//! pi's `ExtensionContext` (`types.ts:304`) exposes ~17 capability members
//! (`ui`, `mode`, `cwd`, `sessionManager`, `modelRegistry`, `abort()`,
//! `compact()`, …), each dragging in a large subsystem. The *registration* path
//! this PR implements reads almost none of them: pi's `createExtensionAPI`
//! closes over `cwd` only so that `exec` has a default working directory, and
//! the register-side methods (`registerTool`, `on`, `registerCommand`, …) read
//! nothing off the context at all.
//!
//! So PR-E builds only [`MinimalExtensionContext`], carrying `cwd`. pidgin-coding
//! models `ExtensionContext` as an opaque marker trait, so this type implements
//! it (and the `CommandContext` marker) with the capability surface deliberately
//! deferred — the full ~17-member context lands with the host-capability wiring
//! in PR-F (blocker #2 in the recon: keep it minimal).

use pidgin_coding::core::extensions::command::CommandContext;
use pidgin_coding::core::extensions::types::ExtensionContext;

/// The minimal [`ExtensionContext`] the registration path needs: just `cwd`.
///
/// The full capability surface (`ui`, `sessionManager`, `modelRegistry`,
/// `abort`, `compact`, …) is deferred to PR-F; see the module docs.
#[derive(Clone, Debug, Default)]
pub struct MinimalExtensionContext {
    /// The extension's working directory (pi's `ctx.cwd`).
    cwd: String,
}

impl MinimalExtensionContext {
    /// Build a context rooted at `cwd`.
    pub fn new(cwd: impl Into<String>) -> Self {
        Self { cwd: cwd.into() }
    }

    /// The working directory this context is rooted at.
    pub fn cwd(&self) -> &str {
        &self.cwd
    }
}

impl ExtensionContext for MinimalExtensionContext {}
impl CommandContext for MinimalExtensionContext {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carries_cwd() {
        let ctx = MinimalExtensionContext::new("/repo");
        assert_eq!(ctx.cwd(), "/repo");
    }

    fn assert_is_ext_context<T: ExtensionContext>(_: &T) {}
    fn assert_is_cmd_context<T: CommandContext>(_: &T) {}

    #[test]
    fn implements_the_marker_traits() {
        let ctx = MinimalExtensionContext::new("/repo");
        assert_is_ext_context(&ctx);
        assert_is_cmd_context(&ctx);
    }
}
