//! The Rust-side registration inventory.
//!
//! When an extension's factory runs, every `pi.registerTool` / `pi.on` /
//! `pi.registerCommand` / … call crosses from JS into Rust through a
//! `deno_core` op (see the `api_ops` module) and lands here. The [`Inventory`]
//! is the plain-data record of everything one extension registered: it carries
//! only metadata (names, descriptions, schemas, flag values), never a V8 handle
//! — the JS closures (`tool.execute`, hook handlers, renderers) stay inside the
//! `JsRuntime`, keyed by name, exactly as pi's loader keeps them.
//!
//! This mirrors the collections pi's `createExtension` builds
//! (`loader.ts` — `handlers`, `tools`, `commands`, `flags`, `shortcuts`,
//! `messageRenderers`, `entryRenderers`). Because it is plain owned data it is
//! `Send`, so it round-trips back across the off-thread rendezvous to the caller.
//!
//! [`Inventory::lower_onto`] then lowers the inventory onto pidgin-coding's
//! [`ExtensionHost`](pidgin_coding::core::extensions::registry::ExtensionHost) —
//! the single Rust source of truth from `notes/design.md`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use pidgin_coding::core::extensions::registry::ExtensionHost;

/// A tool registered through `pi.registerTool` (pi's `ToolDefinition`,
/// `types.ts:439`). Only the serializable metadata is recorded; the `execute`
/// and `prepareArguments` closures stay in JS.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolRecord {
    /// Tool name used in LLM tool calls.
    pub name: String,
    /// Human-readable label for UI display (defaults to `name` when omitted).
    pub label: String,
    /// Description for the LLM.
    pub description: String,
    /// The TypeBox parameter schema, kept opaque.
    pub parameters: Value,
    /// Optional one-line "Available tools" snippet (pi's `promptSnippet`).
    pub prompt_snippet: Option<String>,
    /// Optional system-prompt guideline bullets (pi's `promptGuidelines`).
    pub prompt_guidelines: Option<Vec<String>>,
    /// Optional per-tool execution mode override (pi's `executionMode`).
    pub execution_mode: Option<String>,
    /// Optional render-shell hint (pi's `renderShell`).
    pub render_shell: Option<String>,
}

/// A hook registered through `pi.on(event, handler)`. The handler closure stays
/// in JS; only the event name is recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRecord {
    /// The snake_case event name (`"tool_call"`, `"input"`, …).
    pub event: String,
}

/// A command registered through `pi.registerCommand(name, options)`. The handler
/// and completion closures stay in JS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRecord {
    /// The command name (without the leading slash).
    pub name: String,
    /// Optional human-readable description.
    pub description: Option<String>,
}

/// A shortcut registered through `pi.registerShortcut(shortcut, options)`. The
/// handler closure stays in JS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutRecord {
    /// The key id the shortcut is bound to.
    pub shortcut: String,
    /// Optional human-readable description.
    pub description: Option<String>,
}

/// A flag registered through `pi.registerFlag(name, options)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlagRecord {
    /// The flag name.
    pub name: String,
    /// The flag type (`"boolean"` or `"string"`).
    pub flag_type: String,
    /// The declared default value, if any.
    pub default: Option<Value>,
    /// The current runtime value (initialized to `default`).
    pub value: Option<Value>,
}

/// A renderer registered through `registerMessageRenderer` /
/// `registerEntryRenderer`. The renderer closure stays in JS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RendererRecord {
    /// The custom message/entry `type` this renderer handles.
    pub custom_type: String,
}

/// A provider registered through `pi.registerProvider(config)` (pi's
/// `ProviderConfigInput`, `provider-composer.ts:43`). Only the serializable
/// metadata is recorded; the live `oauth` closures (`login`/`refreshToken`/
/// `getApiKey`) and the streaming closures stay in JS, keyed by provider name in
/// `globalThis.__pidgin.registry.providers`, and are invoked over the one-shot
/// [`JsPlaneHandle::invoke_stored`](crate::runtime::JsPlaneHandle::invoke_stored)
/// primitive.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRecord {
    /// The provider name (the registry key, pi's `config.name`).
    pub name: String,
    /// The provider base URL, if declared (pi's `config.baseUrl`).
    pub base_url: Option<String>,
    /// The provider api family, if declared (pi's `config.api`).
    pub api: Option<String>,
    /// Whether the provider authenticates via an `Authorization` header (pi's
    /// `config.authHeader`).
    pub auth_header: Option<bool>,
    /// Whether the provider declared an `oauth` config block at all.
    pub has_oauth: bool,
    /// Whether `oauth.login` is a live closure (pi's interactive login).
    pub has_login: bool,
    /// Whether `oauth.refreshToken` is a live closure.
    pub has_refresh_token: bool,
    /// Whether `oauth.getApiKey` is a live closure.
    pub has_get_api_key: bool,
    /// The `oauth.name` the config carries, if any (the OAuth flow's provider
    /// name, which may differ from the provider name).
    pub oauth_name: Option<String>,
    /// pi's deprecated `oauth.usesCallbackServer` flag, retained for source
    /// compatibility.
    pub uses_callback_server: Option<bool>,
}

/// Everything one extension registered, in registration order.
///
/// The plain-data mirror of pi's per-extension collections. Empty until a
/// factory runs; populated by the registration ops; returned to the caller once
/// loading finishes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Inventory {
    /// Tools, in registration order.
    pub tools: Vec<ToolRecord>,
    /// Hooks, in registration order (one entry per `pi.on` call).
    pub hooks: Vec<HookRecord>,
    /// Commands, in registration order.
    pub commands: Vec<CommandRecord>,
    /// Shortcuts, in registration order.
    pub shortcuts: Vec<ShortcutRecord>,
    /// Flags, in registration order.
    pub flags: Vec<FlagRecord>,
    /// Message renderers, in registration order.
    pub message_renderers: Vec<RendererRecord>,
    /// Entry renderers, in registration order.
    pub entry_renderers: Vec<RendererRecord>,
    /// Providers, in registration order (one entry per `pi.registerProvider`
    /// still registered — `unregisterProvider` removes its entry).
    pub providers: Vec<ProviderRecord>,
}

impl Inventory {
    /// An empty inventory.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether nothing at all was registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
            && self.hooks.is_empty()
            && self.commands.is_empty()
            && self.shortcuts.is_empty()
            && self.flags.is_empty()
            && self.message_renderers.is_empty()
            && self.entry_renderers.is_empty()
            && self.providers.is_empty()
    }

    /// The current value of a registered flag (its runtime value, falling back
    /// to its declared default). `None` if the flag was never registered —
    /// mirroring pi's `getFlag` returning `undefined`.
    pub fn flag_value(&self, name: &str) -> Option<Value> {
        self.flags
            .iter()
            .find(|f| f.name == name)
            .and_then(|f| f.value.clone().or_else(|| f.default.clone()))
    }

    /// The set of distinct hook event names registered — the runtime side of
    /// design.md's implemented-only exposure policy for this extension.
    pub fn hook_events(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for hook in &self.hooks {
            if !seen.contains(&hook.event) {
                seen.push(hook.event.clone());
            }
        }
        seen
    }

    /// Lower this inventory onto pidgin-coding's registration surface — the
    /// single Rust source of truth from `notes/design.md`.
    ///
    /// Hooks and commands are lowered onto `host` as stub trait objects
    /// (`handle`/`run` return the no-op continue path) so the registry inventory
    /// is populated and queryable; the live JS dispatch that runs the real
    /// handlers lands in PR-F (hook dispatch + session wiring). Tool lowering is
    /// deferred with it: an pidgin-coding `ToolDefinition.execute` needs the
    /// JS-backed dispatch path, so PR-E records tools in [`Inventory::tools`] but
    /// does not synthesize a stub `execute`. See the `host` module.
    pub fn lower_onto(&self, host: &mut dyn ExtensionHost, source_path: &str) {
        crate::host::lower_inventory(self, host, source_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_inventory_reports_empty() {
        let inv = Inventory::new();
        assert!(inv.is_empty());
        assert_eq!(inv.flag_value("missing"), None);
        assert!(inv.hook_events().is_empty());
    }

    #[test]
    fn flag_value_falls_back_to_default() {
        let inv = Inventory {
            flags: vec![
                FlagRecord {
                    name: "verbose".into(),
                    flag_type: "boolean".into(),
                    default: Some(json!(true)),
                    value: None,
                },
                FlagRecord {
                    name: "mode".into(),
                    flag_type: "string".into(),
                    default: Some(json!("fast")),
                    value: Some(json!("slow")),
                },
            ],
            ..Inventory::default()
        };
        assert_eq!(inv.flag_value("verbose"), Some(json!(true)));
        assert_eq!(inv.flag_value("mode"), Some(json!("slow")));
        assert_eq!(inv.flag_value("nope"), None);
        assert!(!inv.is_empty());
    }

    #[test]
    fn hook_events_dedupe_in_order() {
        let inv = Inventory {
            hooks: vec![
                HookRecord {
                    event: "tool_call".into(),
                },
                HookRecord {
                    event: "input".into(),
                },
                HookRecord {
                    event: "tool_call".into(),
                },
            ],
            ..Inventory::default()
        };
        assert_eq!(inv.hook_events(), vec!["tool_call", "input"]);
    }

    #[test]
    fn provider_record_marks_inventory_non_empty() {
        let inv = Inventory {
            providers: vec![ProviderRecord {
                name: "acme".into(),
                has_oauth: true,
                has_get_api_key: true,
                has_refresh_token: true,
                has_login: true,
                ..ProviderRecord::default()
            }],
            ..Inventory::default()
        };
        assert!(!inv.is_empty());
        assert_eq!(inv.providers.len(), 1);
        assert_eq!(inv.providers[0].name, "acme");
        assert!(inv.providers[0].has_get_api_key);
    }
}
