//! Command contract: the [`Command`] trait and the [`RegisteredCommand`]
//! registration descriptor.
//!
//! The Rust successor to pi's `registerCommand(name, options)` surface. pi's
//! `RegisteredCommand` (`types.ts:1144`) is the descriptor an extension
//! registers; [`Command`] is the design's object-safe trait
//! (`extensibility.md` §5) that every command mechanism lowers onto.
//!
//! # Faithfulness notes
//!
//! - pi's handler signature is `(args: string, ctx: ExtensionCommandContext) =>
//!   Promise<void>` — a *single* argument string, not a token vector. The port
//!   follows pi (`args: &str`) rather than the `Vec<String>` in the
//!   `extensibility.md` sketch, since pi is the source of truth.
//! - Handlers are lowered from `async` to eager synchronous closures, matching
//!   the [`super::types::ToolDefinition`] port convention.
//! - `getArgumentCompletions` returns `AutocompleteItem[] | null`; the port
//!   keeps each completion item opaque as a [`Value`] and maps the nullable
//!   array to `Option<Vec<Value>>`.
//! - [`RegisteredCommand`] carries closures, so it is runtime-only (`Clone`, not
//!   serde), exactly like [`super::types::ToolDefinition`].
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/types.ts`.

use std::sync::Arc;

use serde_json::Value;

use crate::core::source_info::SourceInfo;

use super::types::ExtensionContext;

/// Context passed to a command handler (pi's `ExtensionCommandContext`,
/// `types.ts:343`, which extends `ExtensionContext`).
///
/// Modeled as an opaque marker trait extending [`ExtensionContext`], mirroring
/// how the existing port models `ExtensionContext` itself: the command-specific
/// capability members (`args`, `flags`, session controls, …) are deferred to the
/// full extension port.
pub trait CommandContext: ExtensionContext {}

/// A single argument-completion item (pi's `AutocompleteItem`). Opaque [`Value`].
pub type AutocompleteItem = Value;

/// Produces argument completions for a partially typed command argument (pi's
/// `getArgumentCompletions`, `types.ts:1148`). Returns `None` when the command
/// offers no completions for the prefix (pi's `null`).
pub type ArgumentCompletions = Arc<dyn Fn(&str) -> Option<Vec<AutocompleteItem>> + Send + Sync>;

/// Runs a registered command (pi's command `handler`, `types.ts:1149`). Eager
/// synchronous analog of pi's `(args, ctx) => Promise<void>`.
pub type CommandHandler =
    Arc<dyn Fn(&str, &dyn CommandContext) -> anyhow::Result<()> + Send + Sync>;

/// A command registered through `registerCommand` (pi's `RegisteredCommand`,
/// `types.ts:1144`).
///
/// Runtime-only (carries closures); not serde.
#[derive(Clone)]
pub struct RegisteredCommand {
    /// The command name (without the leading slash).
    pub name: String,
    /// Provenance of the extension that registered the command.
    pub source_info: SourceInfo,
    /// Optional human-readable description.
    pub description: Option<String>,
    /// Optional argument-completion provider.
    pub get_argument_completions: Option<ArgumentCompletions>,
    /// The command handler.
    pub handler: CommandHandler,
}

/// A [`RegisteredCommand`] resolved to the name it is invoked under (pi's
/// `ResolvedCommand`, `types.ts:1152`).
///
/// The `invocation_name` differs from `command.name` when a name collision was
/// disambiguated with a suffix during collection.
#[derive(Clone)]
pub struct ResolvedCommand {
    /// The underlying registered command.
    pub command: RegisteredCommand,
    /// The name the command is invoked under after conflict resolution.
    pub invocation_name: String,
}

/// A registered command (the design's `Command` trait, `extensibility.md` §5).
///
/// Every command mechanism — the embedded JS plane and each host-language
/// binding — lowers onto this object-safe trait. Synchronous and eager, matching
/// the [`Hook`](super::hook::Hook) trait; async dispatch lands with the
/// `ExtensionRunner` port.
pub trait Command: Send + Sync {
    /// The command name (without the leading slash).
    fn name(&self) -> &str;

    /// Run the command with its raw argument string.
    fn run(&self, args: &str, ctx: &dyn CommandContext) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::source_info::{SourceOrigin, SourceScope};
    use std::sync::Mutex;

    struct TestCtx;
    impl ExtensionContext for TestCtx {}
    impl CommandContext for TestCtx {}

    fn source() -> SourceInfo {
        SourceInfo {
            path: "/repo/.pi/extensions/greet.ts".into(),
            source: "greet".into(),
            scope: SourceScope::Project,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        }
    }

    #[test]
    fn registered_command_holds_and_runs_its_handler() {
        let seen = Arc::new(Mutex::new(String::new()));
        let seen_for_handler = seen.clone();
        let cmd = RegisteredCommand {
            name: "greet".into(),
            source_info: source(),
            description: Some("greet the user".into()),
            get_argument_completions: Some(Arc::new(|prefix: &str| {
                if prefix.is_empty() {
                    None
                } else {
                    Some(vec![serde_json::json!({ "value": prefix })])
                }
            })),
            handler: Arc::new(move |args: &str, _ctx: &dyn CommandContext| {
                *seen_for_handler.lock().unwrap() = args.to_string();
                Ok(())
            }),
        };

        assert_eq!(cmd.name, "greet");
        assert_eq!(cmd.description.as_deref(), Some("greet the user"));

        (cmd.handler)("world", &TestCtx).unwrap();
        assert_eq!(*seen.lock().unwrap(), "world");

        let completions = cmd.get_argument_completions.as_ref().unwrap();
        assert_eq!(completions(""), None);
        assert_eq!(
            completions("wo"),
            Some(vec![serde_json::json!({ "value": "wo" })]),
        );
    }

    #[test]
    fn command_trait_object_runs() {
        struct Echo;
        impl Command for Echo {
            fn name(&self) -> &str {
                "echo"
            }
            fn run(&self, args: &str, _ctx: &dyn CommandContext) -> anyhow::Result<()> {
                if args.is_empty() {
                    anyhow::bail!("no args");
                }
                Ok(())
            }
        }

        let cmd: Box<dyn Command> = Box::new(Echo);
        assert_eq!(cmd.name(), "echo");
        assert!(cmd.run("hi", &TestCtx).is_ok());
        assert!(cmd.run("", &TestCtx).is_err());
    }

    #[test]
    fn resolved_command_records_invocation_name() {
        let resolved = ResolvedCommand {
            command: RegisteredCommand {
                name: "greet".into(),
                source_info: source(),
                description: None,
                get_argument_completions: None,
                handler: Arc::new(|_args, _ctx| Ok(())),
            },
            invocation_name: "greet-2".into(),
        };
        assert_eq!(resolved.command.name, "greet");
        assert_eq!(resolved.invocation_name, "greet-2");
    }
}
