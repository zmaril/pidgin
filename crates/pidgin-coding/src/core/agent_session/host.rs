//! The `bindCore` host-trait impls the `AgentSession` provides to the extension
//! runner, ported from pi's `AgentSession._buildRuntime` `runner.bindCore(...)`
//! call (`packages/coding-agent/src/core/agent-session.ts:2337`).
//!
//! # Send + Sync bridge vs. `!Send` session
//!
//! The four host traits are `Send + Sync` (the runner holds them as `Arc<dyn
//! ...>` and may call them from its own worker), but [`AgentSession`] is
//! intentionally **`!Send`** (see the [module docs](super)). The host impls
//! therefore live on a separate lightweight [`SessionHostBridge`] that holds only
//! `Send + Sync` handles into the session's shared state — the agent handle, the
//! `Arc<Mutex<..>>` session manager, the listener registry, the queue mirrors, and
//! the lifted run-active / project-trust / base-system-prompt handles — **never**
//! an `Arc<AgentSession>`.
//!
//! Callbacks whose pi implementation reaches a collaborator that is `!Send`
//! (`SettingsManager`, `ModelRuntime`, `DefaultResourceLoader`) or the `!Send`
//! session itself (`sendCustomMessage` / `compact`, which drive a turn), or that
//! reaches a subsystem not yet ported (the tool registry, `_baseSystemPromptOptions`,
//! provider overrides, the shutdown handler), are deferred to their owning slice
//! with a `// unit5:` note: they answer with a safe default rather than crossing
//! the `!Send` boundary. The reachable callbacks — session-manager entries, the
//! agent-backed reads/controls, the idle / pending / trust / system-prompt reads —
//! are wired faithfully.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use pidgin_agent::agent::Agent;
use pidgin_agent::types::ThinkingLevel;
use pidgin_ai::seams::AbortSignal;
use pidgin_ai::Model;

use crate::core::extensions::command::ResolvedCommand;
use crate::core::extensions::events::common::BuildSystemPromptOptions;
use crate::core::extensions::runner::{
    ExtensionCommandContextHost, ExtensionDispatchEvent, ExtensionMode, FlagValue,
    ProviderRegistrationHost, SessionContextHost, SessionControlHost,
};
use crate::core::session_manager::SessionManager;

use super::events::{AgentSessionEvent, AgentSessionEventListener};
use super::session::AgentSession;
use super::turn::emit_to_listeners;

/// The `Send + Sync` carrier of the `bindCore` host callbacks (pi's `bindCore`
/// `actions` / `contextActions` / `providerActions`, L2337). One value implements
/// all four host traits; [`AgentSession::bind_extensions`] passes it to the runner
/// as four `Arc<dyn ...>` trait objects.
///
/// It holds only `Send + Sync` handles into the session's shared state; see the
/// [module docs](self) for why it cannot hold the `!Send` [`AgentSession`].
pub struct SessionHostBridge {
    /// The agent handle (pi `this.agent`), for model / thinking / signal / abort /
    /// system-prompt callbacks.
    agent: Agent,
    /// The shared session manager (pi `this.sessionManager`), for entry / label /
    /// name callbacks.
    session_manager: Arc<Mutex<SessionManager>>,
    /// The listener registry (pi `_eventListeners`), for the `entry_appended` emit
    /// on `appendEntry`.
    listeners: Arc<Mutex<Vec<(u64, AgentSessionEventListener)>>>,
    /// The steering-message mirror (pi `_steeringMessages`), for `hasPendingMessages`.
    steering_messages: Arc<Mutex<Vec<String>>>,
    /// The follow-up-message mirror (pi `_followUpMessages`), for `hasPendingMessages`.
    follow_up_messages: Arc<Mutex<Vec<String>>>,
    /// The run-active flag (pi `_isAgentRunActive`), for `isIdle`.
    is_agent_run_active: Arc<AtomicBool>,
    /// The project-trust snapshot (pi `settingsManager.isProjectTrusted()`).
    project_trusted: Arc<Mutex<bool>>,
    /// The base system prompt (pi `_baseSystemPrompt`); `getSystemPrompt` reads the
    /// agent's live prompt, this is retained for parity with pi's field.
    #[allow(dead_code)]
    base_system_prompt: Arc<Mutex<String>>,
}

impl SessionControlHost for SessionHostBridge {
    fn send_message(&self, _content: &Value, _options: Option<&Value>) {
        // unit5: pi routes `sendMessage` to `sendCustomMessage`, which drives a
        // turn on the `!Send` session; under the session-actor model (see module
        // docs) it enqueues a command instead. Wired with the RPC command channel.
    }

    fn send_user_message(&self, _content: &Value, _options: Option<&Value>) {
        // unit5: as `send_message` — `sendUserMessage` drives a turn on the `!Send`
        // session and is delivered through the actor command channel.
    }

    fn append_entry(&self, custom_type: &str, data: &Value) -> String {
        // pi: appendCustomEntry, then emit `entry_appended` for the new entry.
        let (entry_id, entry) = {
            let mut manager = self.session_manager.lock().unwrap();
            let entry_id = manager.append_custom_entry(custom_type, Some(data.clone()));
            let entry = manager.get_entry(&entry_id);
            (entry_id, entry)
        };
        if let Some(entry) = entry {
            emit_to_listeners(&self.listeners, &AgentSessionEvent::EntryAppended { entry });
        }
        entry_id
    }

    fn set_session_name(&self, name: &str) {
        self.session_manager
            .lock()
            .unwrap()
            .append_session_info(name);
    }

    fn get_session_name(&self) -> Option<String> {
        self.session_manager.lock().unwrap().get_session_name()
    }

    fn set_label(&self, entry_id: &str, label: &str) {
        // pi's `appendLabelChange` is fire-and-forget here; ignore a missing-entry
        // error to match the callback's `void` contract.
        let _ = self
            .session_manager
            .lock()
            .unwrap()
            .append_label_change(entry_id, Some(label));
    }

    fn get_active_tools(&self) -> Vec<String> {
        // unit5: the active-tool set lives in the tool registry (`_refreshToolRegistry`),
        // ported with the runtime slice; empty until then.
        Vec::new()
    }

    fn get_all_tools(&self) -> Vec<Value> {
        // unit5: `getAllTools` reads the tool registry (runtime slice); empty until then.
        Vec::new()
    }

    fn set_active_tools(&self, _names: &[String]) {
        // unit5: `setActiveToolsByName` mutates the tool registry (runtime slice).
    }

    fn refresh_tools(&self) {
        // unit5: `_refreshToolRegistry` lands with the runtime slice.
    }

    fn get_commands(&self) -> Vec<ResolvedCommand> {
        // unit5: pi's `getCommands` fans the runner's registered commands with the
        // loaded templates + skills; that aggregation lands with the runtime slice.
        Vec::new()
    }

    fn set_model(&self, model: &Value) {
        // The base action of pi's `setModel` is `agent.state.model = model`; apply
        // it when the descriptor deserializes.
        // unit5: the `model_change` session entry, the `model_select` extension
        // event, and thinking-level clamping land with the model-management slice.
        if let Ok(model) = serde_json::from_value::<Model>(model.clone()) {
            self.agent.set_model(model);
        }
    }

    fn get_thinking_level(&self) -> ThinkingLevel {
        self.agent.thinking_level()
    }

    fn set_thinking_level(&self, level: ThinkingLevel) {
        // unit5: pi's `setThinkingLevel` also clamps to model capabilities and emits
        // `thinking_level_changed`; those land with the model-management slice.
        self.agent.set_thinking_level(level);
    }
}

impl SessionContextHost for SessionHostBridge {
    fn get_model(&self) -> Value {
        serde_json::to_value(self.agent.model()).unwrap_or(Value::Null)
    }

    fn is_idle(&self) -> bool {
        !self.is_agent_run_active.load(Ordering::Relaxed)
    }

    fn is_project_trusted(&self) -> bool {
        *self.project_trusted.lock().unwrap()
    }

    fn get_signal(&self) -> AbortSignal {
        // pi returns `this.agent.signal`; when no run is active the port mints a
        // fresh (non-aborted) signal.
        self.agent.signal().unwrap_or_default()
    }

    fn abort(&self) {
        // unit5: pi prefers `_extensionAbortHandler` when set; that handler wiring
        // lands with the interactive/RPC slice. The base action is `agent.abort`.
        self.agent.abort();
    }

    fn has_pending_messages(&self) -> bool {
        !self.steering_messages.lock().unwrap().is_empty()
            || !self.follow_up_messages.lock().unwrap().is_empty()
    }

    fn shutdown(&self) {
        // unit5: pi invokes `_extensionShutdownHandler`; that handler is wired by
        // `bindExtensions` in the lifecycle slice.
    }

    fn get_context_usage(&self) -> Option<Value> {
        // pi's `getContextUsage: () => this.getContextUsage()` seam (L2402). The
        // bridge holds the raw agent handle, so it maps the "unknown" model
        // sentinel to `None` itself before delegating to the shared computation.
        let model = super::stats::model_or_none(self.agent.model());
        let messages = self.agent.messages();
        let branch = self.session_manager.lock().unwrap().get_branch(None);
        super::stats::compute_context_usage(model.as_ref(), &messages, &branch)
            .and_then(|usage| serde_json::to_value(usage).ok())
    }

    fn compact(&self) {
        // unit5: `compact` drives the `!Send` session; delivered via the actor
        // command channel (see module docs).
    }

    fn get_system_prompt(&self) -> String {
        self.agent.system_prompt()
    }

    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        // unit5: `_baseSystemPromptOptions` is built by `_rebuildSystemPrompt` from
        // the tool registry (runtime slice); `Null` until then.
        Value::Null
    }
}

impl ProviderRegistrationHost for SessionHostBridge {
    fn register_provider(&self, _provider: &Value) {
        // unit5: provider registration mutates the `!Send` `ModelRuntime` and
        // rebuilds the active model (`_refreshCurrentModelFromRegistry`); lands with
        // the provider-registration slice.
    }

    fn register_native_provider(&self, _provider: &Value) {
        // unit5: as `register_provider` (native pi-ai provider path).
    }

    fn unregister_provider(&self, _id: &str) {
        // unit5: as `register_provider` (unregister path).
    }
}

impl ExtensionCommandContextHost for SessionHostBridge {
    fn get_args(&self) -> String {
        // unit5: per-command args/flags are bound dynamically by `bindCommandContext`
        // at dispatch time (interactive/RPC slice); command handlers in this slice
        // receive their raw argument string directly.
        String::new()
    }

    fn get_flags(&self) -> BTreeMap<String, FlagValue> {
        BTreeMap::new()
    }
}

impl AgentSession {
    /// Build the `Send + Sync` [`SessionHostBridge`] over this session's shared
    /// state (the carrier of the `bindCore` host callbacks).
    ///
    /// Kept separate from [`AgentSession::bind_extensions`] so tests can drive the
    /// host callbacks directly.
    pub(super) fn host_bridge(&self) -> Arc<SessionHostBridge> {
        Arc::new(SessionHostBridge {
            agent: self.agent.clone(),
            session_manager: Arc::clone(&self.session_manager),
            listeners: Arc::clone(&self.listeners),
            steering_messages: Arc::clone(&self.steering_messages),
            follow_up_messages: Arc::clone(&self.follow_up_messages),
            is_agent_run_active: Arc::clone(&self.is_agent_run_active),
            project_trusted: Arc::clone(&self.project_trusted),
            base_system_prompt: Arc::clone(&self.base_system_prompt),
        })
    }

    /// Bind this session's host callbacks into the extension runner (pi's
    /// `runner.bindCore(actions, contextActions, providerActions)` +
    /// `bindCommandContext`, L2337).
    ///
    /// pi calls this from `_buildRuntime`, **after** the resource loader's
    /// post-trust reload, so the runner's inventory reflects the final extension
    /// set (see the runner-construction-ordering note in the port design). The
    /// offline suites drive the [`StubExtensionRunner`](crate::core::extensions::runner::StubExtensionRunner)
    /// / test runner, which retain the bound hosts without a live plane; the
    /// deno-backed runner consumes them.
    pub fn bind_extensions(&self) {
        let bridge = self.host_bridge();
        self.extension_runner().bind_core(
            Arc::clone(&bridge) as Arc<dyn SessionControlHost>,
            Arc::clone(&bridge) as Arc<dyn SessionContextHost>,
            Some(Arc::clone(&bridge) as Arc<dyn ProviderRegistrationHost>),
        );
        self.extension_runner()
            .bind_command_context(Some(bridge as Arc<dyn ExtensionCommandContextHost>));
        self.extension_runner()
            .set_ui_context(None, ExtensionMode::Print);
        // pi's `bindExtensions` (agent-session.ts:2230) emits the stored
        // `session_start` event through the runner once the hosts are bound. The
        // subsequent `extendResourcesFromExtensions` (`resources_discover`) pass is
        // deferred to the resources slice (unit5).
        self.extension_runner()
            .emit(&ExtensionDispatchEvent::SessionStart(
                self.session_start_event().clone(),
            ));
    }
}

#[cfg(test)]
mod tests;
