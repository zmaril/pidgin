//! Model and thinking-level management, ported from pi's `AgentSession`
//! (`packages/coding-agent/src/core/agent-session.ts:1543-1733`).
//!
//! This slice ports the model / thinking-level surface: [`AgentSession::set_model`]
//! (direct model selection with the configured-auth gate), [`AgentSession::cycle_model`]
//! (scoped and available-model cycling), [`AgentSession::set_thinking_level`] /
//! [`AgentSession::cycle_thinking_level`] (clamping to model capabilities), and the
//! `set_scoped_models` / `get_available_thinking_levels` / `supports_thinking`
//! accessors. Each mutation follows pi's ordering exactly: read the previous model,
//! resolve the thinking level for the switch, apply the model to the wrapped agent,
//! append the session-tree entry, persist the default, re-clamp the thinking level,
//! then dispatch the `model_select` extension event.
//!
//! The `model_select` and `thinking_level_select` extension events ride the
//! [`ExtensionRunner`](crate::core::extensions::runner::ExtensionRunner) seam's
//! opaque `Value` dispatch variants
//! ([`ExtensionDispatchEvent::ModelSelect`] /
//! [`ExtensionDispatchEvent::ThinkingLevelChanged`]); the payloads are built to
//! match pi's dispatched event JSON (`{ model, previousModel, source }` and
//! `{ level, previousLevel }`), the real runner tagging each with its `type`
//! discriminant. The TUI-facing [`AgentSessionEvent::ThinkingLevelChanged`] is
//! emitted through the session's own listener registry.

// straitjacket-allow-file:duplication

use serde_json::{json, Value};

use pidgin_agent::types::ThinkingLevel;
use pidgin_ai::{
    clamp_thinking_level, get_supported_thinking_levels, models_are_equal, Model,
    ThinkingLevel as RequestThinkingLevel,
};

use crate::core::defaults::DEFAULT_THINKING_LEVEL;
use crate::core::extensions::events::selection::ModelSelectSource;
use crate::core::extensions::runner::ExtensionDispatchEvent;

use super::events::AgentSessionEvent;
use super::session::{AgentSession, ScopedModel};

/// pi's `THINKING_LEVELS` (`agent-session.ts:278`): the base levels used as the
/// fallback when no model is selected. Excludes `xhigh`/`max`, which are only
/// offered by a model whose `thinkingLevelMap` lists them.
const THINKING_LEVELS: [ThinkingLevel; 5] = [
    ThinkingLevel::Off,
    ThinkingLevel::Minimal,
    ThinkingLevel::Low,
    ThinkingLevel::Medium,
    ThinkingLevel::High,
];

/// The direction [`AgentSession::cycle_model`] steps through the model list (pi's
/// `"forward" | "backward"`, default `"forward"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CycleDirection {
    /// Advance to the next model.
    #[default]
    Forward,
    /// Step back to the previous model.
    Backward,
}

/// The result of [`AgentSession::cycle_model`] (pi's `ModelCycleResult`,
/// `agent-session.ts:232`).
#[derive(Debug, Clone)]
pub struct ModelCycleResult {
    /// The newly selected model.
    pub model: Model,
    /// The thinking level after re-clamping to the new model's capabilities.
    pub thinking_level: ThinkingLevel,
    /// Whether the model came from the scoped `--models` list.
    pub is_scoped: bool,
}

/// The error [`AgentSession::set_model`] returns when the target provider has no
/// configured auth (pi's `setModel` throw, `agent-session.ts:1568`). Its
/// [`Display`](std::fmt::Display) string matches pi's thrown message verbatim.
#[derive(Debug)]
pub struct SetModelError(String);

impl std::fmt::Display for SetModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SetModelError {}

/// pi's `DEFAULT_THINKING_LEVEL` (`"medium"`), parsed into the enum.
fn default_thinking_level() -> ThinkingLevel {
    serde_json::from_value(Value::String(DEFAULT_THINKING_LEVEL.to_string()))
        .unwrap_or(ThinkingLevel::Medium)
}

/// Widen a requestable [`RequestThinkingLevel`] (pi's base `ThinkingLevel`, used
/// for scoped `--models` entries) into the model-capability [`ThinkingLevel`]
/// (pi's `ModelThinkingLevel`). The base levels are a strict subset, so the
/// mapping is total and never yields `off`.
fn widen_thinking_level(level: RequestThinkingLevel) -> ThinkingLevel {
    match level {
        RequestThinkingLevel::Minimal => ThinkingLevel::Minimal,
        RequestThinkingLevel::Low => ThinkingLevel::Low,
        RequestThinkingLevel::Medium => ThinkingLevel::Medium,
        RequestThinkingLevel::High => ThinkingLevel::High,
        RequestThinkingLevel::Xhigh => ThinkingLevel::Xhigh,
        RequestThinkingLevel::Max => ThinkingLevel::Max,
    }
}

/// The lowercase wire string for a thinking level (pi persists the raw string).
fn thinking_level_str(level: ThinkingLevel) -> String {
    serde_json::to_value(level)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

impl AgentSession {
    // =========================================================================
    // Model Management (pi `agent-session.ts:1543-1654`)
    // =========================================================================

    /// Replace the scoped models cycled with Ctrl+P (pi's `setScopedModels`,
    /// `agent-session.ts:976`).
    pub fn set_scoped_models(&self, scoped_models: Vec<ScopedModel>) {
        *self.scoped_models.lock().unwrap() = scoped_models;
    }

    /// Dispatch the `model_select` extension event unless the model is unchanged
    /// (pi's `_emitModelSelect`, `agent-session.ts:1547`).
    fn emit_model_select(
        &self,
        next_model: &Model,
        previous_model: Option<&Model>,
        source: ModelSelectSource,
    ) {
        if models_are_equal(previous_model, Some(next_model)) {
            return;
        }
        let payload = json!({
            "model": serde_json::to_value(next_model).unwrap_or(Value::Null),
            "previousModel": previous_model
                .map(|model| serde_json::to_value(model).unwrap_or(Value::Null)),
            "source": serde_json::to_value(source).unwrap_or(Value::Null),
        });
        self.extension_runner()
            .emit(&ExtensionDispatchEvent::ModelSelect(payload));
    }

    /// Set the model directly (pi's `setModel`, `agent-session.ts:1566`).
    ///
    /// Validates that the provider has configured auth, applies the model to the
    /// agent, records the session-tree entry, persists the default, re-clamps the
    /// thinking level for the new model's capabilities, and dispatches
    /// `model_select`.
    pub fn set_model(&mut self, model: Model) -> Result<(), SetModelError> {
        if !matches!(
            self.model_runtime().check_auth(&model.provider),
            Ok(Some(_))
        ) {
            return Err(SetModelError(format!(
                "No API key for {}/{}",
                model.provider, model.id
            )));
        }

        let previous_model = self.model();
        let thinking_level = self.thinking_level_for_model_switch(None);
        self.agent.set_model(model.clone());
        self.session_manager
            .lock()
            .unwrap()
            .append_model_change(&model.provider, &model.id);
        self.settings_manager
            .set_default_model_and_provider(&model.provider, &model.id);

        // Re-clamp thinking level for the new model's capabilities.
        self.set_thinking_level(thinking_level);

        self.emit_model_select(&model, previous_model.as_ref(), ModelSelectSource::Set);
        Ok(())
    }

    /// Cycle to the next/previous model (pi's `cycleModel`,
    /// `agent-session.ts:1589`). Uses the scoped `--models` list when present,
    /// otherwise the available-model set. Returns `None` when only one model is
    /// available.
    pub fn cycle_model(&mut self, direction: CycleDirection) -> Option<ModelCycleResult> {
        if !self.scoped_models().is_empty() {
            return self.cycle_scoped_model(direction);
        }
        self.cycle_available_model(direction)
    }

    /// Cycle within the scoped `--models` list (pi's `_cycleScopedModel`,
    /// `agent-session.ts:1596`). Scoped entries whose provider lacks configured
    /// auth are filtered out; an explicit scoped thinking level overrides the
    /// current session preference.
    fn cycle_scoped_model(&mut self, direction: CycleDirection) -> Option<ModelCycleResult> {
        let scoped: Vec<ScopedModel> = self
            .scoped_models()
            .into_iter()
            .filter(|entry| {
                matches!(
                    self.model_runtime().check_auth(&entry.model.provider),
                    Ok(Some(_))
                )
            })
            .collect();
        if scoped.len() <= 1 {
            return None;
        }

        let current_model = self.model();
        let current_index = scoped
            .iter()
            .position(|entry| models_are_equal(Some(&entry.model), current_model.as_ref()))
            .unwrap_or(0);
        let len = scoped.len();
        let next_index = match direction {
            CycleDirection::Forward => (current_index + 1) % len,
            CycleDirection::Backward => (current_index + len - 1) % len,
        };
        let next = scoped[next_index].clone();
        let thinking_level =
            self.thinking_level_for_model_switch(next.thinking_level.map(widen_thinking_level));

        self.agent.set_model(next.model.clone());
        self.session_manager
            .lock()
            .unwrap()
            .append_model_change(&next.model.provider, &next.model.id);
        self.settings_manager
            .set_default_model_and_provider(&next.model.provider, &next.model.id);

        // An explicit scoped thinking level overrides the current session level; an
        // undefined one inherits it. `set_thinking_level` clamps to the new model.
        self.set_thinking_level(thinking_level);

        self.emit_model_select(
            &next.model,
            current_model.as_ref(),
            ModelSelectSource::Cycle,
        );

        Some(ModelCycleResult {
            model: next.model,
            thinking_level: self.thinking_level(),
            is_scoped: true,
        })
    }

    /// Cycle within the available-model set (pi's `_cycleAvailableModel`,
    /// `agent-session.ts:1631`).
    fn cycle_available_model(&mut self, direction: CycleDirection) -> Option<ModelCycleResult> {
        let available = self.model_runtime().get_available(None).unwrap_or_default();
        if available.len() <= 1 {
            return None;
        }

        let current_model = self.model();
        let current_index = available
            .iter()
            .position(|model| models_are_equal(Some(model), current_model.as_ref()))
            .unwrap_or(0);
        let len = available.len();
        let next_index = match direction {
            CycleDirection::Forward => (current_index + 1) % len,
            CycleDirection::Backward => (current_index + len - 1) % len,
        };
        let next_model = available[next_index].clone();

        let thinking_level = self.thinking_level_for_model_switch(None);
        self.agent.set_model(next_model.clone());
        self.session_manager
            .lock()
            .unwrap()
            .append_model_change(&next_model.provider, &next_model.id);
        self.settings_manager
            .set_default_model_and_provider(&next_model.provider, &next_model.id);

        self.set_thinking_level(thinking_level);

        self.emit_model_select(
            &next_model,
            current_model.as_ref(),
            ModelSelectSource::Cycle,
        );

        Some(ModelCycleResult {
            model: next_model,
            thinking_level: self.thinking_level(),
            is_scoped: false,
        })
    }

    // =========================================================================
    // Thinking Level Management (pi `agent-session.ts:1656-1733`)
    // =========================================================================

    /// Set the thinking level (pi's `setThinkingLevel`, `agent-session.ts:1665`).
    ///
    /// Clamps `level` to the current model's capabilities and, only when the
    /// effective level actually changes, records the session-tree entry, persists
    /// the default, emits [`AgentSessionEvent::ThinkingLevelChanged`], and
    /// dispatches the `thinking_level_select` extension event.
    pub fn set_thinking_level(&mut self, level: ThinkingLevel) {
        let available = self.get_available_thinking_levels();
        let effective = if available.contains(&level) {
            level
        } else {
            self.clamp_level(level)
        };

        let previous = self.agent.thinking_level();
        let is_changing = effective != previous;

        self.agent.set_thinking_level(effective);

        if is_changing {
            self.session_manager
                .lock()
                .unwrap()
                .append_thinking_level_change(&thinking_level_str(effective));
            if self.supports_thinking() || effective != ThinkingLevel::Off {
                self.settings_manager.set_default_thinking_level(effective);
            }
            self.emit(&AgentSessionEvent::ThinkingLevelChanged { level: effective });
            let payload = json!({
                "level": serde_json::to_value(effective).unwrap_or(Value::Null),
                "previousLevel": serde_json::to_value(previous).unwrap_or(Value::Null),
            });
            self.extension_runner()
                .emit(&ExtensionDispatchEvent::ThinkingLevelChanged(payload));
        }
    }

    /// Cycle to the next thinking level (pi's `cycleThinkingLevel`,
    /// `agent-session.ts:1693`). Returns `None` when the model does not support
    /// thinking.
    pub fn cycle_thinking_level(&mut self) -> Option<ThinkingLevel> {
        if !self.supports_thinking() {
            return None;
        }

        let levels = self.get_available_thinking_levels();
        let current = self.thinking_level();
        let next_index = match levels.iter().position(|candidate| *candidate == current) {
            Some(index) => (index + 1) % levels.len(),
            // pi's `indexOf` returns -1 when the current level is unavailable, so
            // `(-1 + 1) % len` wraps to the first level.
            None => 0,
        };
        let next_level = levels[next_index];

        self.set_thinking_level(next_level);
        Some(next_level)
    }

    /// The thinking levels the current model supports (pi's
    /// `getAvailableThinkingLevels`, `agent-session.ts:1709`). Falls back to the
    /// base [`THINKING_LEVELS`] when no model is selected.
    pub fn get_available_thinking_levels(&self) -> Vec<ThinkingLevel> {
        match self.model() {
            Some(model) => get_supported_thinking_levels(&model),
            None => THINKING_LEVELS.to_vec(),
        }
    }

    /// Whether the current model supports thinking/reasoning (pi's
    /// `supportsThinking`, `agent-session.ts:1717`).
    pub fn supports_thinking(&self) -> bool {
        self.model().map(|model| model.reasoning).unwrap_or(false)
    }

    /// Resolve the thinking level to apply on a model switch (pi's
    /// `_getThinkingLevelForModelSwitch`, `agent-session.ts:1721`). An explicit
    /// level wins; otherwise a non-reasoning source model falls back to the saved
    /// default and a reasoning one inherits the current level.
    fn thinking_level_for_model_switch(&self, explicit: Option<ThinkingLevel>) -> ThinkingLevel {
        if let Some(level) = explicit {
            return level;
        }
        if !self.supports_thinking() {
            return self
                .settings_manager
                .get_default_thinking_level()
                .unwrap_or_else(default_thinking_level);
        }
        self.thinking_level()
    }

    /// Clamp a requested level to the current model (pi's `_clampThinkingLevel`,
    /// `agent-session.ts:1731`); `off` when no model is selected.
    fn clamp_level(&self, level: ThinkingLevel) -> ThinkingLevel {
        match self.model() {
            Some(model) => clamp_thinking_level(&model, level),
            None => ThinkingLevel::Off,
        }
    }
}

#[cfg(test)]
mod tests;
