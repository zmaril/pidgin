// straitjacket-allow-file:duplication — the `load()` vector-reading helper and
// the per-scenario replay loops intentionally mirror the same two-line
// boilerplate used in widget_vectors.rs / components_vectors.rs; each
// integration-test binary is standalone and cannot share a private helper
// without a common module, more indirection than it warrants.
//! Drives the Rust port of pi's interactive input widgets (Input, SelectList,
//! SettingsList) against vectors extracted from pi itself
//! (`crates/pidgin-tui/vectors/gen/generate_input_lists.mjs`). Every assertion
//! is byte-identical: pi's `render(width)` output and edit-state transitions are
//! the source of truth. The Input and SelectList scenarios replay the exact
//! cases from pi's `test/input.test.ts` and `test/select-list.test.ts`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use serde::Deserialize;

use pidgin_tui::components::input::Input;
use pidgin_tui::components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
    SelectListTruncatePrimaryContext, TruncatePrimaryFn,
};
use pidgin_tui::components::settings_list::{
    SettingItem, SettingsList, SettingsListOptions, SettingsListTheme,
};

fn load<T: serde::de::DeserializeOwned>(name: &str) -> Vec<T> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join(format!("{name}.json"));
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

// ===========================================================================
// Input
// ===========================================================================

#[derive(Deserialize)]
struct InputStep {
    op: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default, rename = "setValue")]
    set_value: Option<String>,
    #[serde(default)]
    focused: Option<bool>,
    #[serde(default)]
    width: Option<usize>,
    #[serde(rename = "valueAfter")]
    value_after: String,
    #[serde(default)]
    render: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct InputScenario {
    name: String,
    steps: Vec<InputStep>,
    submitted: Option<String>,
    escaped: i32,
}

#[test]
fn input_scenario_vectors() {
    let scenarios: Vec<InputScenario> = load("input_scenarios");
    assert!(!scenarios.is_empty());

    for sc in &scenarios {
        let submitted: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let escaped: Rc<RefCell<i32>> = Rc::new(RefCell::new(0));

        let mut input = Input::new();
        {
            let submitted = Rc::clone(&submitted);
            input.on_submit = Some(Box::new(move |v: String| {
                *submitted.borrow_mut() = Some(v);
            }));
        }
        {
            let escaped = Rc::clone(&escaped);
            input.on_escape = Some(Box::new(move || {
                *escaped.borrow_mut() += 1;
            }));
        }

        for (i, step) in sc.steps.iter().enumerate() {
            let mut got_render: Option<Vec<String>> = None;
            match step.op.as_str() {
                "input" => input.handle_input_str(step.data.as_deref().expect("input data")),
                "setValue" => input.set_value(step.set_value.as_deref().expect("setValue arg")),
                "focus" => input.focused = step.focused.expect("focused arg"),
                "render" => {
                    got_render = Some(input.render_lines(step.width.expect("render width")))
                }
                other => panic!("unknown input op: {other}"),
            }
            assert_eq!(
                input.get_value(),
                step.value_after,
                "scenario {:?} step {i} ({}): value mismatch",
                sc.name,
                step.op
            );
            if let Some(expected) = &step.render {
                assert_eq!(
                    got_render.as_ref().expect("render captured"),
                    expected,
                    "scenario {:?} step {i}: render mismatch",
                    sc.name
                );
            }
        }

        assert_eq!(
            *submitted.borrow(),
            sc.submitted,
            "scenario {:?}: submitted mismatch",
            sc.name
        );
        assert_eq!(
            *escaped.borrow(),
            sc.escaped,
            "scenario {:?}: escaped mismatch",
            sc.name
        );
    }
}

// ===========================================================================
// SelectList
// ===========================================================================

#[derive(Deserialize)]
struct JsonSelectItem {
    value: String,
    label: String,
    #[serde(default)]
    description: Option<String>,
}

impl JsonSelectItem {
    fn to_item(&self) -> SelectItem {
        SelectItem {
            value: self.value.clone(),
            label: self.label.clone(),
            description: self.description.clone(),
        }
    }
}

#[derive(Deserialize)]
struct JsonLayout {
    #[serde(default)]
    min: Option<i64>,
    #[serde(default)]
    max: Option<i64>,
    #[serde(default, rename = "truncateTag")]
    truncate_tag: Option<String>,
}

#[derive(Deserialize)]
struct SelectStep {
    op: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    width: Option<usize>,
    #[serde(default)]
    filter: Option<String>,
    #[serde(default)]
    index: Option<i64>,
    #[serde(rename = "selectedItem")]
    selected_item: Option<String>,
    #[serde(default)]
    render: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct SelectScenario {
    name: String,
    items: Vec<JsonSelectItem>,
    #[serde(rename = "maxVisible")]
    max_visible: i64,
    layout: Option<JsonLayout>,
    steps: Vec<SelectStep>,
    selections: Vec<String>,
    selected: Vec<String>,
    cancelled: i32,
}

fn identity_select_theme() -> SelectListTheme {
    SelectListTheme {
        selected_prefix: Box::new(|t: &str| t.to_string()),
        selected_text: Box::new(|t: &str| t.to_string()),
        description: Box::new(|t: &str| t.to_string()),
        scroll_info: Box::new(|t: &str| t.to_string()),
        no_match: Box::new(|t: &str| t.to_string()),
    }
}

fn truncate_primary_for(tag: &str) -> TruncatePrimaryFn {
    match tag {
        "ellipsis" => Box::new(|ctx: &SelectListTruncatePrimaryContext| {
            // `text.length <= maxWidth` and `text.slice(0, maxWidth-1)` are
            // measured/sliced in UTF-16 units, matching JS string indexing.
            let units: Vec<u16> = ctx.text.encode_utf16().collect();
            let len = units.len() as i64;
            if len <= ctx.max_width {
                ctx.text.clone()
            } else {
                let end = (ctx.max_width - 1).max(0) as usize;
                let sliced = String::from_utf16(&units[..end]).expect("slice on UTF-16 boundary");
                format!("{sliced}\u{2026}")
            }
        }),
        other => panic!("unknown truncatePrimary tag: {other}"),
    }
}

fn build_layout(layout: &Option<JsonLayout>) -> SelectListLayoutOptions {
    let mut opts = SelectListLayoutOptions::default();
    if let Some(l) = layout {
        opts.min_primary_column_width = l.min;
        opts.max_primary_column_width = l.max;
        if let Some(tag) = &l.truncate_tag {
            opts.truncate_primary = Some(truncate_primary_for(tag));
        }
    }
    opts
}

#[test]
fn select_list_scenario_vectors() {
    let scenarios: Vec<SelectScenario> = load("select_list_scenarios");
    assert!(!scenarios.is_empty());

    for sc in &scenarios {
        let items: Vec<SelectItem> = sc.items.iter().map(JsonSelectItem::to_item).collect();
        let mut list = SelectList::new(
            items,
            sc.max_visible,
            identity_select_theme(),
            build_layout(&sc.layout),
        );

        let selections: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let selected: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let cancelled: Rc<RefCell<i32>> = Rc::new(RefCell::new(0));
        {
            let selections = Rc::clone(&selections);
            list.on_selection_change = Some(Box::new(move |item: SelectItem| {
                selections.borrow_mut().push(item.value)
            }));
        }
        {
            let selected = Rc::clone(&selected);
            list.on_select = Some(Box::new(move |item: SelectItem| {
                selected.borrow_mut().push(item.value)
            }));
        }
        {
            let cancelled = Rc::clone(&cancelled);
            list.on_cancel = Some(Box::new(move || *cancelled.borrow_mut() += 1));
        }

        for (i, step) in sc.steps.iter().enumerate() {
            let mut got_render: Option<Vec<String>> = None;
            match step.op.as_str() {
                "render" => got_render = Some(list.render_lines(step.width.expect("render width"))),
                "input" => list.handle_input_str(step.data.as_deref().expect("input data")),
                "setFilter" => list.set_filter(step.filter.as_deref().expect("filter")),
                "setSelectedIndex" => list.set_selected_index(step.index.expect("index")),
                other => panic!("unknown select-list op: {other}"),
            }
            let got_sel = list.get_selected_item().map(|it| it.value);
            assert_eq!(
                got_sel, step.selected_item,
                "scenario {:?} step {i}: selected item mismatch",
                sc.name
            );
            if let Some(expected) = &step.render {
                assert_eq!(
                    got_render.as_ref().expect("render captured"),
                    expected,
                    "scenario {:?} step {i}: render mismatch",
                    sc.name
                );
            }
        }

        assert_eq!(
            *selections.borrow(),
            sc.selections,
            "scenario {:?}: selectionChange sequence mismatch",
            sc.name
        );
        assert_eq!(
            *selected.borrow(),
            sc.selected,
            "scenario {:?}: onSelect sequence mismatch",
            sc.name
        );
        assert_eq!(
            *cancelled.borrow(),
            sc.cancelled,
            "scenario {:?}: cancel count mismatch",
            sc.name
        );
    }
}

// ===========================================================================
// SettingsList
// ===========================================================================

#[derive(Deserialize)]
struct JsonSettingItem {
    id: String,
    label: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "currentValue")]
    current_value: String,
    #[serde(default)]
    values: Option<Vec<String>>,
}

impl JsonSettingItem {
    fn to_item(&self) -> SettingItem {
        SettingItem {
            id: self.id.clone(),
            label: self.label.clone(),
            description: self.description.clone(),
            current_value: self.current_value.clone(),
            values: self.values.clone(),
            submenu: None,
        }
    }
}

#[derive(Deserialize)]
struct JsonSettingsOptions {
    #[serde(default, rename = "enableSearch")]
    enable_search: bool,
}

#[derive(Deserialize)]
struct SettingsStep {
    op: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    width: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    render: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct SettingsScenario {
    name: String,
    items: Vec<JsonSettingItem>,
    #[serde(rename = "maxVisible")]
    max_visible: i64,
    options: JsonSettingsOptions,
    steps: Vec<SettingsStep>,
    changes: Vec<(String, String)>,
    cancelled: i32,
}

fn identity_settings_theme() -> SettingsListTheme {
    SettingsListTheme {
        label: Box::new(|t: &str, _s: bool| t.to_string()),
        value: Box::new(|t: &str, _s: bool| t.to_string()),
        description: Box::new(|t: &str| t.to_string()),
        cursor: "\u{2192} ".to_string(),
        hint: Box::new(|t: &str| t.to_string()),
    }
}

#[test]
fn settings_list_scenario_vectors() {
    let scenarios: Vec<SettingsScenario> = load("settings_list_scenarios");
    assert!(!scenarios.is_empty());

    for sc in &scenarios {
        let items: Vec<SettingItem> = sc.items.iter().map(JsonSettingItem::to_item).collect();

        let changes: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let cancelled: Rc<RefCell<i32>> = Rc::new(RefCell::new(0));

        let on_change: Box<dyn FnMut(String, String)> = {
            let changes = Rc::clone(&changes);
            Box::new(move |id: String, v: String| changes.borrow_mut().push((id, v)))
        };
        let on_cancel: Box<dyn FnMut()> = {
            let cancelled = Rc::clone(&cancelled);
            Box::new(move || *cancelled.borrow_mut() += 1)
        };

        let mut list = SettingsList::new(
            items,
            sc.max_visible,
            identity_settings_theme(),
            on_change,
            on_cancel,
            SettingsListOptions {
                enable_search: sc.options.enable_search,
            },
        );

        for (i, step) in sc.steps.iter().enumerate() {
            let mut got_render: Option<Vec<String>> = None;
            match step.op.as_str() {
                "render" => got_render = Some(list.render_lines(step.width.expect("render width"))),
                "input" => list.handle_input_str(step.data.as_deref().expect("input data")),
                "updateValue" => list.update_value(
                    step.id.as_deref().expect("id"),
                    step.value.as_deref().expect("value"),
                ),
                other => panic!("unknown settings-list op: {other}"),
            }
            if let Some(expected) = &step.render {
                assert_eq!(
                    got_render.as_ref().expect("render captured"),
                    expected,
                    "scenario {:?} step {i}: render mismatch",
                    sc.name
                );
            }
        }

        assert_eq!(
            *changes.borrow(),
            sc.changes,
            "scenario {:?}: onChange sequence mismatch",
            sc.name
        );
        assert_eq!(
            *cancelled.borrow(),
            sc.cancelled,
            "scenario {:?}: cancel count mismatch",
            sc.name
        );
    }
}
