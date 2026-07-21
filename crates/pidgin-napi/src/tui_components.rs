//! Stateful `#[napi]` TUI component cores, extracted from `lib.rs` to keep the
//! crate root under straitjacket's file-size ceiling. These are the three
//! interactive pi-tui components whose editing/selection/resolution logic runs
//! natively behind a JS shim that keeps pi's callbacks and accessors: the
//! keybindings manager (`keybindings.ts`), the single-line input
//! (`components/input.ts`), and the select list (`components/select-list.ts`).
//!
//! `#[napi]` registration is global (linkme/ctor), so the JS-facing export
//! names are unchanged by living in this module rather than the crate root.

use napi_derive::napi;

// --- tui keybindings layer (packages/tui/src/keybindings.ts) ----------------
//
// A stateful `#[napi]` class wrapping `pidgin_tui::KeybindingsManager`. The
// hand-written `keybindings.ts` shim re-implements pi's `KeybindingsManager`
// class (keeping `definitions`/`userBindings`/`getDefinition`/`getUserBindings`
// as JS, identical to pi) and routes the resolution logic — `matches`,
// `getKeys`, `getConflicts`, `getResolvedBindings` — through this core. The core
// is immutable per construction; the shim's `setUserBindings` builds a fresh
// core. Definitions and user bindings cross as JSON arrays (not objects) so
// JS insertion order is preserved without relying on serde_json's
// `preserve_order` feature.

#[derive(serde::Deserialize)]
struct KeybindingDefinitionIn {
    id: String,
    #[serde(rename = "defaultKeys")]
    default_keys: Vec<String>,
    description: Option<String>,
}

#[derive(serde::Deserialize)]
struct UserBindingIn {
    id: String,
    // `null` = pi's explicit `undefined` (falls back to the default keys).
    keys: Option<Vec<String>>,
}

/// The Rust-backed keybindings core, exposed to JavaScript as
/// `KeybindingsManagerCore`.
#[napi(js_name = "KeybindingsManagerCore")]
pub struct KeybindingsManagerCore {
    inner: pidgin_tui::KeybindingsManager,
}

#[napi]
impl KeybindingsManagerCore {
    /// Build a core from pi's `definitions`/`userBindings`, each JSON-encoded as
    /// an ordered array (`[{ id, defaultKeys, description? }]` and
    /// `[{ id, keys }]`, `keys: null` for an explicit `undefined`).
    #[napi(constructor)]
    pub fn new(definitions_json: String, user_bindings_json: String) -> napi::Result<Self> {
        let defs_in: Vec<KeybindingDefinitionIn> = serde_json::from_str(&definitions_json)
            .map_err(|e| napi::Error::from_reason(format!("invalid definitions: {e}")))?;
        let user_in: Vec<UserBindingIn> = serde_json::from_str(&user_bindings_json)
            .map_err(|e| napi::Error::from_reason(format!("invalid userBindings: {e}")))?;

        let defs_owned: Vec<(String, pidgin_tui::KeybindingDefinition)> = defs_in
            .into_iter()
            .map(|d| {
                (
                    d.id,
                    pidgin_tui::KeybindingDefinition {
                        default_keys: d.default_keys,
                        description: d.description,
                    },
                )
            })
            .collect();
        let definitions: Vec<(&str, pidgin_tui::KeybindingDefinition)> = defs_owned
            .iter()
            .map(|(id, def)| (id.as_str(), def.clone()))
            .collect();
        let user_bindings: Vec<(&str, Option<Vec<String>>)> = user_in
            .iter()
            .map(|u| (u.id.as_str(), u.keys.clone()))
            .collect();

        Ok(Self {
            inner: pidgin_tui::KeybindingsManager::new(definitions, user_bindings),
        })
    }

    /// pi's `matches(data, keybinding)`: does `data` match any bound key?
    #[napi(js_name = "matches")]
    pub fn matches(&self, data: String, keybinding: String) -> bool {
        self.inner.matches(&data, &keybinding)
    }

    /// pi's `getKeys(keybinding)`: the keys bound to `keybinding` (empty if
    /// unknown).
    #[napi(js_name = "getKeys")]
    pub fn get_keys(&self, keybinding: String) -> Vec<String> {
        self.inner.get_keys(&keybinding)
    }

    /// pi's `getConflicts()` as JSON: an ordered array of
    /// `{ key, keybindings }`.
    #[napi(js_name = "getConflictsJson")]
    pub fn get_conflicts_json(&self) -> napi::Result<String> {
        let conflicts: Vec<serde_json::Value> = self
            .inner
            .get_conflicts()
            .into_iter()
            .map(|c| serde_json::json!({ "key": c.key, "keybindings": c.keybindings }))
            .collect();
        serde_json::to_string(&conflicts).map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// pi's `getResolvedBindings()` as JSON: an ordered array of `[id, keys]`
    /// pairs, in definition order. The shim rebuilds pi's `key | key[]` shape.
    #[napi(js_name = "getResolvedBindingsJson")]
    pub fn get_resolved_bindings_json(&self) -> napi::Result<String> {
        let resolved: Vec<(String, Vec<String>)> = self.inner.get_resolved_bindings();
        serde_json::to_string(&resolved).map_err(|e| napi::Error::from_reason(e.to_string()))
    }
}

// --- tui input layer (packages/tui/src/components/input.ts) -----------------
//
// A stateful `#[napi]` class wrapping `pidgin_tui::Input`. The hand-written
// `input.ts` shim re-implements pi's `Input` class, keeping `onSubmit`/
// `onEscape` as JS callbacks and the `focused` accessor as JS, and routing the
// editing/render logic through this core. pi's `handleInput` fires `onSubmit`/
// `onEscape` synchronously; the core cannot call JS closures, so instead it
// records any submit/escape that fired during a `handleInput` call and returns
// it as an [`InputEvent`], which the shim replays onto the JS callbacks. Value
// and cursor arithmetic is UTF-16 on both sides (as in pi).

/// Event surfaced by [`InputCore::handle_input`] so the JS shim can fire pi's
/// `onSubmit`/`onEscape` callbacks. `submit` is the submitted value (pi passes
/// the current value) or `null` when no submit fired; `escape` is `true` when a
/// cancel/escape fired.
#[napi(object)]
pub struct InputEvent {
    pub submit: Option<String>,
    pub escape: bool,
}

#[derive(Default)]
struct InputEventState {
    submit: Option<String>,
    escape: bool,
}

/// The Rust-backed single-line input core, exposed to JavaScript as
/// `InputCore`.
#[napi(js_name = "InputCore")]
pub struct InputCore {
    inner: pidgin_tui::Input,
    events: std::rc::Rc<std::cell::RefCell<InputEventState>>,
}

#[napi]
impl InputCore {
    /// Create an empty input core, wiring pi's `onSubmit`/`onEscape` seams to a
    /// shared event cell that `handle_input` drains after each call.
    #[napi(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let events = std::rc::Rc::new(std::cell::RefCell::new(InputEventState::default()));
        let mut inner = pidgin_tui::Input::new();
        {
            let ev = events.clone();
            inner.on_submit = Some(Box::new(move |value| {
                ev.borrow_mut().submit = Some(value);
            }));
            let ev = events.clone();
            inner.on_escape = Some(Box::new(move || {
                ev.borrow_mut().escape = true;
            }));
        }
        Self { inner, events }
    }

    /// pi's `getValue()`: the current value.
    #[napi(js_name = "getValue")]
    pub fn get_value(&self) -> String {
        self.inner.get_value()
    }

    /// pi's `setValue(value)`: set the value, clamping the cursor.
    #[napi(js_name = "setValue")]
    pub fn set_value(&mut self, value: String) {
        self.inner.set_value(&value);
    }

    /// pi's `focused` field setter — routed here because render reads it.
    #[napi(js_name = "setFocused")]
    pub fn set_focused(&mut self, focused: bool) {
        self.inner.focused = focused;
    }

    /// pi's `handleInput(data)`: process a chunk of terminal input, returning any
    /// `onSubmit`/`onEscape` that fired so the shim can replay it onto the JS
    /// callbacks.
    #[napi(js_name = "handleInput")]
    pub fn handle_input(&mut self, data: String) -> InputEvent {
        *self.events.borrow_mut() = InputEventState::default();
        self.inner.handle_input_str(&data);
        let ev = self.events.borrow();
        InputEvent {
            submit: ev.submit.clone(),
            escape: ev.escape,
        }
    }

    /// pi's `render(width)`: render the input to a single line.
    #[napi(js_name = "render")]
    pub fn render(&self, width: u32) -> Vec<String> {
        self.inner.render_lines(width as usize)
    }
}

// --- tui select-list layer (packages/tui/src/components/select-list.ts) -----
//
// A stateful `#[napi]` class wrapping `pidgin_tui::SelectList`. pi's `render`
// composes JS theme callbacks (`selectedText`, `description`, `scrollInfo`,
// `noMatch`, `selectedPrefix`) and an optional `truncatePrimary` override — JS
// closures that cannot cross the addon boundary. The hand-written
// `select-list.ts` shim therefore routes `render` through this core ONLY when
// the theme functions are all identity and no `truncatePrimary` override is
// supplied (the core bakes in an identity theme and no override); every other
// construction delegates to pi's original class. Item text and layout bounds
// cross as JSON / numbers; selection and filter state live in the core so the
// shim can keep it in sync for `render`.

#[derive(serde::Deserialize)]
struct SelectItemIn {
    value: String,
    label: String,
    description: Option<String>,
}

fn identity_select_theme() -> pidgin_tui::SelectListTheme {
    pidgin_tui::SelectListTheme {
        selected_prefix: Box::new(|s| s.to_string()),
        selected_text: Box::new(|s| s.to_string()),
        description: Box::new(|s| s.to_string()),
        scroll_info: Box::new(|s| s.to_string()),
        no_match: Box::new(|s| s.to_string()),
    }
}

/// The Rust-backed select-list core, exposed to JavaScript as `SelectListCore`.
/// Constructed with an identity theme and no `truncatePrimary` override; the
/// shim only builds one when pi's theme is identity and no override is set.
#[napi(js_name = "SelectListCore")]
pub struct SelectListCore {
    inner: pidgin_tui::SelectList,
}

#[napi]
impl SelectListCore {
    /// Build a core from pi's `items` (JSON array of `{ value, label,
    /// description? }`), `maxVisible`, and the optional
    /// `minPrimaryColumnWidth`/`maxPrimaryColumnWidth` layout bounds.
    #[napi(constructor)]
    pub fn new(
        items_json: String,
        max_visible: i64,
        min_primary_column_width: Option<i64>,
        max_primary_column_width: Option<i64>,
    ) -> napi::Result<Self> {
        let items_in: Vec<SelectItemIn> = serde_json::from_str(&items_json)
            .map_err(|e| napi::Error::from_reason(format!("invalid items: {e}")))?;
        let items: Vec<pidgin_tui::SelectItem> = items_in
            .into_iter()
            .map(|i| pidgin_tui::SelectItem {
                value: i.value,
                label: i.label,
                description: i.description,
            })
            .collect();
        let layout = pidgin_tui::SelectListLayoutOptions {
            min_primary_column_width,
            max_primary_column_width,
            truncate_primary: None,
        };
        Ok(Self {
            inner: pidgin_tui::SelectList::new(items, max_visible, identity_select_theme(), layout),
        })
    }

    /// pi's `setFilter(filter)`: case-insensitive `value` prefix filter.
    #[napi(js_name = "setFilter")]
    pub fn set_filter(&mut self, filter: String) {
        self.inner.set_filter(&filter);
    }

    /// pi's `setSelectedIndex(index)`: clamp the selection into range.
    #[napi(js_name = "setSelectedIndex")]
    pub fn set_selected_index(&mut self, index: i64) {
        self.inner.set_selected_index(index);
    }

    /// pi's `handleInput(keyData)`: move/confirm/cancel. Callbacks are handled by
    /// the shim's original instance; the core only advances selection state.
    #[napi(js_name = "handleInput")]
    pub fn handle_input(&mut self, key_data: String) {
        self.inner.handle_input_str(&key_data);
    }

    /// pi's `getSelectedItem()` as JSON (`{ value, label, description? }`), or
    /// `null` when the filtered list is empty.
    #[napi(js_name = "getSelectedItemJson")]
    pub fn get_selected_item_json(&self) -> napi::Result<Option<String>> {
        match self.inner.get_selected_item() {
            Some(item) => serde_json::to_string(&serde_json::json!({
                "value": item.value,
                "label": item.label,
                "description": item.description,
            }))
            .map(Some)
            .map_err(|e| napi::Error::from_reason(e.to_string())),
            None => Ok(None),
        }
    }

    /// pi's `render(width)`: render the list to lines (identity theme baked in).
    #[napi(js_name = "render")]
    pub fn render(&self, width: u32) -> Vec<String> {
        self.inner.render_lines(width as usize)
    }
}
