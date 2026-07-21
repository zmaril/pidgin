//! The `pi` object handed to a Python extension's `def extension(pi):` factory.
//!
//! [`PyPiApi`] is the Python analog of the deno engine's `globalThis.__pi`
//! (`api_ops::BOOTSTRAP_JS`): its snake_case registration methods each (1) push a
//! plain-data record into the shared [`Inventory`] and (2) stash the live Python
//! callable (`handler` / `execute`) in the [`HandlerStore`], keyed by name/event,
//! for the runner to invoke later. Only serializable metadata lands in the
//! inventory; the Python callables never leave the interpreter — they are held as
//! `Arc<Py<PyAny>>` (both `Send + Sync`) so the runner can clone a handle without
//! the GIL and bind it under a fresh `Python::with_gil` at dispatch time.
//!
//! Method set + record shapes mirror the deno `pi` surface method-for-method
//! (`on`, `register_tool`, `register_command`, `register_shortcut`,
//! `register_flag`/`get_flag`, `register_message_renderer`/
//! `register_entry_renderer`, `register_provider`/`unregister_provider`); the
//! camelCase JS names become snake_case, matching pidgin's byte-identical binding
//! convention across host languages.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::inventory::{
    CommandRecord, FlagRecord, HookRecord, Inventory, ProviderRecord, RendererRecord,
    ShortcutRecord, ToolRecord,
};

use super::convert::{json_to_py, py_to_json};

/// The live Python callables one extension registered, keyed for dispatch.
///
/// Held as `Arc<Py<PyAny>>`: `Py<PyAny>` is `Send + Sync` and keeps the object
/// alive across GIL releases; wrapping it in `Arc` lets the runner clone a
/// dispatch handle without holding the GIL (a bare `Py::clone` would need it).
#[derive(Default)]
pub struct HandlerStore {
    /// Command handlers, keyed by command name (pi's `handler(args, ctx)`).
    pub commands: HashMap<String, Arc<Py<PyAny>>>,
    /// Tool `execute` callables, keyed by tool name.
    pub tools: HashMap<String, Arc<Py<PyAny>>>,
    /// Hook handlers, keyed by snake_case event name (one event may have many).
    pub hooks: HashMap<String, Vec<Arc<Py<PyAny>>>>,
}

/// Everything a single `extension(pi)` call collected: the plain-data
/// [`Inventory`] plus the live [`HandlerStore`]. Drained out of [`PyPiApi`] once
/// the factory returns.
#[derive(Default)]
pub struct PyCollect {
    /// The plain-data registration records (shared engine-neutral core).
    pub inventory: Inventory,
    /// The live Python callables backing those records.
    pub handlers: HandlerStore,
}

/// The `pi` object bound into `extension(pi)`.
///
/// `#[pyclass]` requires `Sync` under PyO3 0.23; the shared collection sits behind
/// `Arc<Mutex<PyCollect>>`, which is `Send + Sync`, and the engine drains it after
/// the factory runs.
#[pyclass]
pub struct PyPiApi {
    inner: Arc<Mutex<PyCollect>>,
}

impl PyPiApi {
    /// Build a `pi` handle writing into the shared `inner` collection.
    pub fn new(inner: Arc<Mutex<PyCollect>>) -> Self {
        Self { inner }
    }
}

#[pymethods]
impl PyPiApi {
    /// `pi.register_command(name, description=None, handler=None)` — record the
    /// command metadata and stash its handler keyed by name.
    #[pyo3(signature = (name, description=None, handler=None))]
    fn register_command(
        &self,
        name: String,
        description: Option<String>,
        handler: Option<Bound<'_, PyAny>>,
    ) {
        let mut collect = self.inner.lock().unwrap();
        collect.inventory.commands.push(CommandRecord {
            name: name.clone(),
            description,
        });
        if let Some(handler) = handler {
            collect
                .handlers
                .commands
                .insert(name, Arc::new(handler.unbind()));
        }
    }

    /// `pi.register_tool(tool)` — record the tool metadata (`name` / `label` /
    /// `description` / `parameters`) and stash its `execute` callable by name.
    fn register_tool(&self, tool: Bound<'_, PyDict>) -> PyResult<()> {
        let name: String = match tool.get_item("name")? {
            Some(value) => value.extract()?,
            None => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "register_tool: tool dict is missing a 'name'",
                ))
            }
        };
        let label = opt_string(&tool, "label")?.unwrap_or_else(|| name.clone());
        let description = opt_string(&tool, "description")?.unwrap_or_default();
        let parameters = match tool.get_item("parameters")? {
            Some(value) => py_to_json(&value)?,
            None => serde_json::Value::Object(Default::default()),
        };

        let mut collect = self.inner.lock().unwrap();
        collect.inventory.tools.push(ToolRecord {
            name: name.clone(),
            label,
            description,
            parameters,
            ..ToolRecord::default()
        });
        if let Some(execute) = tool.get_item("execute")? {
            collect
                .handlers
                .tools
                .insert(name, Arc::new(execute.unbind()));
        }
        Ok(())
    }

    /// `pi.on(event, handler)` — record the hook subscription and stash its
    /// handler under the snake_case event name.
    fn on(&self, event: String, handler: Bound<'_, PyAny>) {
        let mut collect = self.inner.lock().unwrap();
        collect.inventory.hooks.push(HookRecord {
            event: event.clone(),
        });
        collect
            .handlers
            .hooks
            .entry(event)
            .or_default()
            .push(Arc::new(handler.unbind()));
    }

    /// `pi.register_flag(name, options)` — record the flag and initialize its
    /// runtime value to the declared default (mirrors the deno op).
    #[pyo3(signature = (name, options=None))]
    fn register_flag(&self, name: String, options: Option<Bound<'_, PyDict>>) -> PyResult<()> {
        let (flag_type, default) = match options {
            Some(options) => {
                let flag_type =
                    opt_string(&options, "type")?.unwrap_or_else(|| "boolean".to_string());
                let default = match options.get_item("default")? {
                    Some(value) => Some(py_to_json(&value)?),
                    None => None,
                };
                (flag_type, default)
            }
            None => ("boolean".to_string(), None),
        };
        let value = default.clone();
        self.inner.lock().unwrap().inventory.flags.push(FlagRecord {
            name,
            flag_type,
            default,
            value,
        });
        Ok(())
    }

    /// `pi.get_flag(name)` — the current value of a registered flag, or `None`.
    fn get_flag(&self, py: Python<'_>, name: String) -> PyResult<Py<PyAny>> {
        let collect = self.inner.lock().unwrap();
        match collect.inventory.flag_value(&name) {
            Some(value) => Ok(json_to_py(py, &value)?.unbind()),
            None => Ok(py.None()),
        }
    }

    /// `pi.register_shortcut(shortcut, options=None)` — record the shortcut
    /// metadata (the handler stays a to-parity no-op like the renderers).
    #[pyo3(signature = (shortcut, options=None))]
    fn register_shortcut(
        &self,
        shortcut: String,
        options: Option<Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let description = match &options {
            Some(options) => opt_string(options, "description")?,
            None => None,
        };
        self.inner
            .lock()
            .unwrap()
            .inventory
            .shortcuts
            .push(ShortcutRecord {
                shortcut,
                description,
            });
        Ok(())
    }

    /// `pi.register_provider(config)` — CAPTURE the provider registration's
    /// metadata (the live `oauth` callables stay in Python). Mirrors the deno op's
    /// closure-presence flags; keyed by name so a re-register replaces the prior
    /// entry.
    fn register_provider(&self, config: Bound<'_, PyDict>) -> PyResult<()> {
        let name = opt_string(&config, "name")?.unwrap_or_default();
        let base_url = opt_string(&config, "base_url")?;
        let api = opt_string(&config, "api")?;
        let auth_header = match config.get_item("auth_header")? {
            Some(value) => value.extract::<bool>().ok(),
            None => None,
        };
        let oauth = config.get_item("oauth")?;
        let (has_oauth, has_login, has_refresh_token, has_get_api_key, oauth_name) = match &oauth {
            Some(oauth) => match oauth.downcast::<PyDict>() {
                Ok(oauth) => (
                    true,
                    is_callable(oauth, "login")?,
                    is_callable(oauth, "refresh_token")?,
                    is_callable(oauth, "get_api_key")?,
                    opt_string(oauth, "name")?,
                ),
                Err(_) => (true, false, false, false, None),
            },
            None => (false, false, false, false, None),
        };

        let record = ProviderRecord {
            name,
            base_url,
            api,
            auth_header,
            has_oauth,
            has_login,
            has_refresh_token,
            has_get_api_key,
            oauth_name,
            uses_callback_server: None,
        };
        let mut collect = self.inner.lock().unwrap();
        collect
            .inventory
            .providers
            .retain(|p| p.name != record.name);
        collect.inventory.providers.push(record);
        Ok(())
    }

    /// `pi.unregister_provider(name)` — drop the captured provider record.
    fn unregister_provider(&self, name: String) {
        self.inner
            .lock()
            .unwrap()
            .inventory
            .providers
            .retain(|p| p.name != name);
    }

    /// `pi.register_message_renderer(custom_type, renderer=None)` — record the
    /// renderer type (the renderer callable is a to-parity no-op).
    #[pyo3(signature = (custom_type, renderer=None))]
    fn register_message_renderer(&self, custom_type: String, renderer: Option<Bound<'_, PyAny>>) {
        let _ = renderer;
        self.inner
            .lock()
            .unwrap()
            .inventory
            .message_renderers
            .push(RendererRecord { custom_type });
    }

    /// `pi.register_entry_renderer(custom_type, renderer=None)` — record the
    /// renderer type (the renderer callable is a to-parity no-op).
    #[pyo3(signature = (custom_type, renderer=None))]
    fn register_entry_renderer(&self, custom_type: String, renderer: Option<Bound<'_, PyAny>>) {
        let _ = renderer;
        self.inner
            .lock()
            .unwrap()
            .inventory
            .entry_renderers
            .push(RendererRecord { custom_type });
    }
}

/// Read an optional string field from a `dict`, returning `None` when the key is
/// absent or its value is `None`.
fn opt_string(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<String>> {
    match dict.get_item(key)? {
        Some(value) if value.is_none() => Ok(None),
        Some(value) => Ok(Some(value.extract()?)),
        None => Ok(None),
    }
}

/// Whether `dict[key]` exists and is a callable (pi's `typeof x === "function"`).
fn is_callable(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<bool> {
    match dict.get_item(key)? {
        Some(value) => Ok(value.is_callable()),
        None => Ok(false),
    }
}
