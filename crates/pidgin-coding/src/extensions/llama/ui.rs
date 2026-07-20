//! Faithful port of pi's llama-extension TUI
//! (`packages/coding-agent/src/extensions/llama/ui.ts`): the `/llama` command's
//! pure-TUI overlay — a model manager/picker, a Hugging Face GGUF search box, a
//! download/load progress bar, and the generic select / confirm /
//! connection-error / status dialogs — plus the [`run_with_progress`] driver.
//!
//! The port mirrors `ui.ts` 1:1: the [`LlamaUi`] trait (pi's `LlamaUi`
//! interface), [`LlamaView`] (pi's `LlamaView`), [`HuggingFaceSearch`], the
//! [`frame`] helper, the formatting helpers ([`context_label`],
//! [`model_description`], [`compact_count`], [`select_theme`]), and
//! [`run_with_progress`].
//!
//! ## Async model
//!
//! pi's view methods return `Promise`s that a JS event loop resolves when the
//! user acts, while the same loop dispatches keyboard input into the view. Rust
//! has no such ambient loop, so the port models it with the primitives the
//! codebase already carries:
//!
//! * The view's dialog methods (`show_models`, `select`, `confirm`,
//!   `connection_error`, `search_models`, `progress`) are `async fn`s that stash
//!   a [`oneshot`](tokio::sync::oneshot) sender in the live widget's
//!   `on_select`/`on_cancel` closure and `.await` the receiver. Keyboard input
//!   arrives through the synchronous [`LlamaView::handle_input`] (interior-mutable
//!   via `RefCell`), which fires the widget callback and thereby the sender —
//!   exactly pi's "input resolves the promise" flow.
//! * The download progress cancel loop ([`run_with_progress`]) mirrors pi's
//!   `Promise.race([settled, ui.progress()])` with [`tokio::select!`], and the
//!   `AbortController` with [`pidgin_ai::seams::AbortSignal`] (the codebase's
//!   cooperative abort flag).
//! * The Hugging Face search's 500 ms debounce is the one spot the missing
//!   ambient loop shows through: [`HuggingFaceSearch::schedule_search`] performs
//!   the synchronous parts (cache lookup, `< 2` chars guard, status transitions,
//!   filtering) immediately and records a [`HuggingFaceSearch::pending_query`]
//!   marker in place of pi's `setTimeout(runSearch, 500)`. The awaited network
//!   fetch is then driven by [`LlamaView::run_pending_search`], which ports
//!   `runSearch` verbatim (AbortController, in-flight cache, `closed` / aborted /
//!   stale-query guards). Wiring the literal 500 ms timer needs the same ambient
//!   loop the mount seam provides and lands with it.
//!
//! ## Mount seam — deferred
//!
//! pi's `showLlamaUi` mounts the view via `ctx.ui.custom(...)` and reports errors
//! via `ctx.ui.notify(...)`. The Rust [`ExtensionContext`] is an opaque marker
//! trait and that `ctx.ui` capability is not yet defined, so [`show_llama_ui`] is
//! a documented stub (see its body). The [`LlamaView`] itself is fully
//! implemented and testable against the [`LlamaUi`] trait without it.

// straitjacket-allow-file:duplication — faithful line-for-line mirror of pi's
// `ui.ts`; the frame/dialog composition and the HuggingFaceSearch state machine
// are reproduced from upstream by design so this tracks `ui.ts` exactly.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::rc::Rc;
use std::sync::OnceLock;

use regex::Regex;
use tokio::sync::oneshot;

use pidgin_ai::seams::AbortSignal;
use pidgin_tui::components::{
    Input, SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
};
use pidgin_tui::keybindings::KeybindingsManager;
use pidgin_tui::renderer::{Component, Container};
use pidgin_tui::widgets::{Spacer, Text};
use pidgin_tui::width::{truncate_to_width, visible_width};

use crate::core::extensions::types::ExtensionContext;
use crate::modes::interactive::components::{key_hint, DynamicBorder};
use crate::modes::interactive::theme::Theme;

use super::client::{LlamaModelInfo, LlamaModelStatus, LlamaProgress};
use super::huggingface::HuggingFaceModel;

/// Sentinel `value` for the "Download model…" list entry (pi's `DOWNLOAD_VALUE`).
const DOWNLOAD_VALUE: &str = "\u{0}download";

/// The "Searching Hugging Face…" status literal (kept as a constant because the
/// render path compares the current status against it).
const SEARCHING_STATUS: &str = "Searching Hugging Face\u{2026}";
/// The "< 2 characters" status literal.
const TYPE_MORE_STATUS: &str = "Type at least 2 characters";
/// The empty-search-result status literal.
const NO_MODELS_STATUS: &str = "No GGUF models found";

// ---------------------------------------------------------------------------
// Public value types
// ---------------------------------------------------------------------------

/// The action the model manager resolves to (pi's `LlamaManagerAction`).
// The `Model` variant carries a full `LlamaModelInfo` by value, mirroring pi's
// union `{ type: "model"; model: LlamaModelInfo }`; boxing it would diverge from
// the faithful shape and the catalog type is unboxed everywhere else.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum LlamaManagerAction {
    /// The user chose a model to load/unload.
    Model(LlamaModelInfo),
    /// The user chose "Download model…".
    Download,
    /// The user closed the manager.
    Close,
}

/// The connection-error dialog outcome (pi's `"retry" | "close"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionErrorChoice {
    /// Retry the connection.
    Retry,
    /// Close the dialog.
    Close,
}

/// A progress dialog's state (pi's `ProgressState extends LlamaProgress`): the
/// [`LlamaProgress`] fields plus a `title` and a `model` label.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProgressState {
    /// The frame title.
    pub title: String,
    /// The model label rendered above the message.
    pub model: String,
    /// The status line (`LlamaProgress.message`).
    pub message: String,
    /// Fractional completion in `[0, 1]`, when known (`LlamaProgress.ratio`).
    pub ratio: Option<f64>,
    /// A supplemental detail line (`LlamaProgress.detail`).
    pub detail: Option<String>,
}

/// The search callback passed to [`LlamaUi::search_models`]: `(query, signal) ->
/// Future<Result<models, error message>>` (pi's `(query, signal) =>
/// Promise<HuggingFaceModel[]>`, where a rejection carries the error message).
pub type SearchFn = Rc<
    dyn Fn(
        String,
        AbortSignal,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<HuggingFaceModel>, String>>>>,
>;

// ---------------------------------------------------------------------------
// Formatting helpers (pi module functions)
// ---------------------------------------------------------------------------

/// `theme.fg(color, text)` — the theme colours used by the llama UI are always
/// baked into the interactive themes, so a lookup miss is a programmer error.
fn fg(theme: &Theme, color: &str, text: &str) -> String {
    theme
        .fg(color, text)
        .expect("llama ui theme colour is present")
}

/// JS `String(n)` for a context-window number: integral values format without a
/// decimal point (context windows are always integers).
fn number_to_string(value: f64) -> String {
    if value.fract() == 0.0 && value.is_finite() {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// JS `Number(str)` for the ctx-size arg branch: trims, treats the empty string
/// as `0`, and otherwise parses as an f64 (`NaN` on failure).
fn js_number(value: &str) -> f64 {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return 0.0;
    }
    trimmed.parse::<f64>().unwrap_or(f64::NAN)
}

/// Render a context-window count: `>= 1000` collapses to `"<n>k"` (rounded),
/// otherwise the raw number.
fn format_context(value: f64) -> String {
    if value >= 1000.0 {
        format!("{}k", (value / 1000.0).round() as i64)
    } else {
        number_to_string(value)
    }
}

/// `contextLabel(model)` — the model's context window as a compact label, from
/// `meta.n_ctx ?? meta.n_ctx_train`, else the `--ctx-size`/`-c`/`-ctx` launch arg.
fn context_label(model: &LlamaModelInfo) -> Option<String> {
    let context = model
        .meta
        .as_ref()
        .and_then(|meta| meta.n_ctx.or(meta.n_ctx_train));
    if let Some(context) = context {
        if context != 0.0 && !context.is_nan() {
            return Some(format_context(context));
        }
    }
    let empty: Vec<String> = Vec::new();
    let args = model.status.args.as_ref().unwrap_or(&empty);
    // `for (index = 0; index < args.length - 1; index++)` — with `length < 1`
    // the loop body never runs (JS `0 < -1` is false); `saturating_sub` matches.
    for index in 0..args.len().saturating_sub(1) {
        let flag = args[index].as_str();
        if flag != "--ctx-size" && flag != "-c" && flag != "-ctx" {
            continue;
        }
        let value = js_number(&args[index + 1]);
        if value.is_finite() && value > 0.0 {
            return Some(format_context(value));
        }
    }
    None
}

/// The lowercase string for a lifecycle status (matches the `serde` lowercase
/// rename); used where pi pushes `model.status.value` verbatim.
fn status_str(status: LlamaModelStatus) -> &'static str {
    match status {
        LlamaModelStatus::Unloaded => "unloaded",
        LlamaModelStatus::Loading => "loading",
        LlamaModelStatus::Loaded => "loaded",
        LlamaModelStatus::Downloading => "downloading",
        LlamaModelStatus::Sleeping => "sleeping",
    }
}

/// `modelDescription(model)` — the `loaded · <n>k context`-style description
/// shown next to each model in the manager.
fn model_description(model: &LlamaModelInfo) -> String {
    let mut details: Vec<String> = Vec::new();
    let loaded = matches!(
        model.status.value,
        LlamaModelStatus::Loaded | LlamaModelStatus::Sleeping
    );
    if loaded {
        details.push("loaded".to_string());
    } else if model.status.value != LlamaModelStatus::Unloaded {
        details.push(status_str(model.status.value).to_string());
    }
    let context = if loaded { context_label(model) } else { None };
    if let Some(context) = context {
        details.push(format!("{context} context"));
    }
    details.join(" \u{00b7} ")
}

/// `compactCount(value)` — a compact download count (`1.2k`, `3M`, …).
fn compact_count(value: u64) -> String {
    if value >= 1_000_000 {
        let digits = if value >= 10_000_000 { 0 } else { 1 };
        format!("{:.*}M", digits, value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        let digits = if value >= 100_000 { 0 } else { 1 };
        format!("{:.*}k", digits, value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

/// Build an owned `theme.fg(color, …)` styling closure from the theme's pre-baked
/// foreground escape (`{fgAnsi}{text}\x1b[39m`), matching the message components'
/// `fg_style` helper.
fn fg_style(theme: &Theme, color: &str) -> Box<dyn Fn(&str) -> String> {
    let ansi = theme.get_fg_ansi(color).unwrap_or_default();
    Box::new(move |text: &str| format!("{ansi}{text}\x1b[39m"))
}

/// `selectTheme(theme)` — the [`SelectListTheme`] the llama pickers use.
fn select_theme(theme: &Theme) -> SelectListTheme {
    SelectListTheme {
        selected_prefix: fg_style(theme, "accent"),
        selected_text: fg_style(theme, "accent"),
        description: fg_style(theme, "muted"),
        scroll_info: fg_style(theme, "dim"),
        no_match: fg_style(theme, "warning"),
    }
}

/// A [`Component`] wrapping a fixed set of already-rendered lines. Lets
/// [`frame`] compose a live widget's `render(width)` output (which depends on
/// state the frame does not own) into its child list while staying byte-identical
/// to pi's `Container`-of-children render.
struct PreRendered(Vec<String>);

impl Component for PreRendered {
    fn render(&self, _width: usize) -> Vec<String> {
        self.0.clone()
    }
}

/// `frame(theme, title, body, footer)` — wrap `body` in the accent border + bold
/// title (+ optional dim footer) chrome shared by every llama screen.
fn frame(
    theme: &Theme,
    title: &str,
    body: Vec<Box<dyn Component>>,
    footer: Option<&str>,
) -> Container {
    let mut container = Container::new();
    container.add_child(Box::new(DynamicBorder::new(fg_style(theme, "accent"))));
    container.add_child(Box::new(Text::new(
        &fg(theme, "accent", &theme.bold(title)),
        1,
        0,
        None,
    )));
    for child in body {
        container.add_child(child);
    }
    if let Some(footer) = footer {
        container.add_child(Box::new(Spacer::new(1)));
        container.add_child(Box::new(Text::new(&fg(theme, "dim", footer), 1, 0, None)));
    }
    container.add_child(Box::new(DynamicBorder::new(fg_style(theme, "accent"))));
    container
}

// ---------------------------------------------------------------------------
// LlamaUi trait (pi's `LlamaUi` interface)
// ---------------------------------------------------------------------------

/// The llama UI surface the extension logic drives (pi's `LlamaUi` interface).
///
/// The dialog methods are `async fn`s: they mount their screen and resolve when
/// the user acts. `#[allow(async_fn_in_trait)]` — the trait is consumed
/// generically (never as `dyn LlamaUi`), so the auto-trait-leakage the lint warns
/// about does not apply.
#[allow(async_fn_in_trait)]
pub trait LlamaUi {
    /// Show the model manager (`showModels`).
    async fn show_models(
        &self,
        server_url: &str,
        models: Vec<LlamaModelInfo>,
    ) -> LlamaManagerAction;
    /// Show a generic single-select dialog (`select`).
    async fn select(&self, title: &str, options: Vec<String>) -> Option<String>;
    /// Show a yes/no confirmation (`confirm`).
    async fn confirm(&self, title: &str, message: &str) -> bool;
    /// Show the "llama.cpp unavailable" retry/close dialog (`connectionError`).
    async fn connection_error(&self, server_url: &str, message: &str) -> ConnectionErrorChoice;
    /// Show the Hugging Face GGUF search box (`searchModels`).
    async fn search_models(&self, search: SearchFn) -> Option<String>;
    /// Show a static status frame (`showStatus`).
    fn show_status(&self, title: &str, message: &str);
    /// Show the progress frame and resolve when the user presses stop
    /// (`progress`).
    async fn progress(&self, state: ProgressState);
    /// Update the live progress frame in place (`updateProgress`).
    fn update_progress(&self, state: ProgressState);
}

// ---------------------------------------------------------------------------
// HuggingFaceSearch (pi's `HuggingFaceSearch`)
// ---------------------------------------------------------------------------

/// The `owner/repo[:quant]` exact-match test (pi's
/// `/^[^/\s]+\/[^:\s]+(?::[^\s:]+)?$/u`).
fn exact_match_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^[^/\s]+/[^:\s]+(?::[^\s:]+)?$").expect("valid exact-match regex")
    })
}

/// The interactive Hugging Face GGUF search widget (pi's `HuggingFaceSearch`).
///
/// Owns the query [`Input`], the raw + fuzzy-filtered result lists, the scroll
/// window, and the in-flight/abort state. See the module async note for how the
/// 500 ms debounce is split between [`Self::schedule_search`] (synchronous) and
/// the view's `run_pending_search` (awaited).
struct HuggingFaceSearch {
    input: Input,
    search: SearchFn,
    results: Vec<HuggingFaceModel>,
    filtered_results: Vec<HuggingFaceModel>,
    selected_index: i64,
    query: String,
    status: String,
    closed: bool,
    /// Set by [`Self::schedule_search`] when a network fetch is due (stands in for
    /// pi's `setTimeout(runSearch, 500)` handle).
    pending_query: Option<String>,
    /// The in-flight request's abort signal (pi's `request: AbortController`).
    request: Option<AbortSignal>,
    /// Monotonic request generation, the Rust stand-in for pi's
    /// `this.request === request` object-identity check (bumped per fetch).
    request_seq: u64,
    focused: bool,
}

impl HuggingFaceSearch {
    /// `new HuggingFaceSearch(...)`.
    fn new(search: SearchFn, focused: bool) -> Self {
        let mut input = Input::new();
        input.focused = focused;
        Self {
            input,
            search,
            results: Vec::new(),
            filtered_results: Vec::new(),
            selected_index: 0,
            query: String::new(),
            status: TYPE_MORE_STATUS.to_string(),
            closed: false,
            pending_query: None,
            request: None,
            request_seq: 0,
            focused,
        }
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.input.focused = value;
    }

    /// `filterResults` — restrict `results` to the fuzzy matches for `query`
    /// (preserving `results` order), then clamp `selected_index`.
    fn filter_results(&mut self) {
        if !self.query.is_empty() {
            let matched = pidgin_tui::fuzzy::fuzzy_filter(self.results.clone(), &self.query, |m| {
                m.id.clone()
            });
            let ids: HashSet<String> = matched.into_iter().map(|m| m.id).collect();
            self.filtered_results = self
                .results
                .iter()
                .filter(|m| ids.contains(&m.id))
                .cloned()
                .collect();
        } else {
            self.filtered_results = self.results.clone();
        }
        self.selected_index = self
            .selected_index
            .min((self.filtered_results.len() as i64 - 1).max(0));
    }

    /// `scheduleSearch` — debounce/cache/`< 2` chars handling. The awaited fetch
    /// is deferred to `run_pending_search` via [`Self::pending_query`].
    fn schedule_search(&mut self, cache: &RefCell<HashMap<String, Vec<HuggingFaceModel>>>) {
        // clearTimeout(debounce) + request?.abort(); request = undefined
        self.pending_query = None;
        if let Some(request) = self.request.take() {
            request.abort();
        }
        if self.query.encode_utf16().count() < 2 {
            self.status = TYPE_MORE_STATUS.to_string();
            self.filter_results();
            return;
        }
        if let Some(cached) = cache.borrow().get(&self.query.to_lowercase()).cloned() {
            self.results = cached.clone();
            self.status = if cached.is_empty() {
                NO_MODELS_STATUS.to_string()
            } else {
                String::new()
            };
            self.filter_results();
            return;
        }
        self.status = SEARCHING_STATUS.to_string();
        self.filter_results();
        // setTimeout(() => runSearch(query), 500)
        self.pending_query = Some(self.query.clone());
    }

    /// `close(model)` — mark closed, abort any in-flight request, and return the
    /// resolution (`onSelectModel(model)`). Returns `None` if already closed.
    fn close(&mut self, model: Option<String>) -> Option<Option<String>> {
        if self.closed {
            return None;
        }
        self.closed = true;
        self.pending_query = None;
        if let Some(request) = self.request.take() {
            request.abort();
        }
        Some(model)
    }

    /// `handleInput(data)` — navigation, confirm (with exact-match), cancel, and
    /// query editing. Returns `Some(resolution)` when the widget closes.
    fn handle_input(
        &mut self,
        data: &str,
        keybindings: &KeybindingsManager,
        cache: &RefCell<HashMap<String, Vec<HuggingFaceModel>>>,
    ) -> Option<Option<String>> {
        if keybindings.matches(data, "tui.select.up") {
            if !self.filtered_results.is_empty() {
                self.selected_index = if self.selected_index == 0 {
                    self.filtered_results.len() as i64 - 1
                } else {
                    self.selected_index - 1
                };
            }
            return None;
        }
        if keybindings.matches(data, "tui.select.down") {
            if !self.filtered_results.is_empty() {
                self.selected_index =
                    if self.selected_index == self.filtered_results.len() as i64 - 1 {
                        0
                    } else {
                        self.selected_index + 1
                    };
            }
            return None;
        }
        if keybindings.matches(data, "tui.select.confirm") {
            let exact = if exact_match_regex().is_match(&self.query) {
                Some(self.query.clone())
            } else {
                None
            };
            let selected = exact.or_else(|| {
                self.filtered_results
                    .get(self.selected_index as usize)
                    .map(|m| m.id.clone())
            });
            if let Some(selected) = selected {
                return self.close(Some(selected));
            }
            return None;
        }
        if keybindings.matches(data, "tui.select.cancel") {
            return self.close(None);
        }
        self.input.handle_input_str(data);
        let query = self.input.get_value().trim().to_string();
        if query == self.query {
            return None;
        }
        self.query = query;
        self.schedule_search(cache);
        None
    }

    /// The result-list lines (pi's `updateResults` body — a pure function of the
    /// widget's current state).
    fn result_lines(&self, width: usize, theme: &Theme) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let max_visible: i64 = 10;
        let count = self.filtered_results.len() as i64;
        let start = 0.max((self.selected_index - max_visible / 2).min(count - max_visible));
        let end = (start + max_visible).min(count);
        let mut index = start;
        while index < end {
            if let Some(model) = self.filtered_results.get(index as usize) {
                let prefix = if index == self.selected_index {
                    "\u{2192} "
                } else {
                    "  "
                };
                let details = format!("{} downloads", compact_count(model.downloads));
                let text = if index == self.selected_index {
                    fg(theme, "accent", &format!("{prefix}{}  {details}", model.id))
                } else {
                    format!(
                        "{prefix}{}{}",
                        model.id,
                        fg(theme, "muted", &format!("  {details}"))
                    )
                };
                lines.extend(Text::new(&text, 0, 0, None).render(width));
            }
            index += 1;
        }
        if start > 0 || end < count {
            let text = fg(
                theme,
                "dim",
                &format!("  ({}/{})", self.selected_index + 1, count),
            );
            lines.extend(Text::new(&text, 0, 0, None).render(width));
        }
        // pi renders the status line in two branches with identical bodies:
        // `if (filtered.length === 0) {…} else if (status === "Searching…") {…}`.
        // The disjunction is behaviourally identical and avoids the duplicated
        // block.
        if count == 0 || self.status == SEARCHING_STATUS {
            lines.extend(
                Text::new(&fg(theme, "dim", &format!("  {}", self.status)), 0, 0, None)
                    .render(width),
            );
        }
        lines
    }

    /// The widget's full render (its four constructor children: the dim hint, the
    /// input, a spacer, and the result list).
    fn render(&self, width: usize, theme: &Theme) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        lines.extend(
            Text::new(
                &fg(theme, "dim", "Model name or owner/repository[:quant]"),
                1,
                0,
                None,
            )
            .render(width),
        );
        lines.extend(self.input.render_lines(width));
        lines.extend(Spacer::new(1).render(width));
        lines.extend(self.result_lines(width, theme));
        lines
    }
}

// ---------------------------------------------------------------------------
// LlamaView (pi's `LlamaView`)
// ---------------------------------------------------------------------------

/// The current screen being displayed (pi keeps a `content: Container` plus an
/// `inputHandler`/`inputTarget`; the enum couples the two so the live input
/// widget is both rendered and routed to).
enum Screen {
    /// The initial "Loading…" frame.
    Loading,
    /// The model manager.
    Models {
        server_url: String,
        list: SelectList,
        footer: String,
    },
    /// A generic select dialog.
    Select {
        title: String,
        list: SelectList,
        footer: String,
    },
    /// The Hugging Face search box.
    Search {
        search: HuggingFaceSearch,
        footer: String,
        resolver: Rc<RefCell<Option<oneshot::Sender<Option<String>>>>>,
    },
    /// A static status frame.
    Status { title: String, message: String },
    /// The progress frame.
    Progress(ProgressState),
}

/// The interior-mutable view state.
struct Inner {
    screen: Screen,
    /// The progress "stop" resolver (pi's `progressResolver`).
    progress_resolver: Option<oneshot::Sender<()>>,
    /// Whether a progress frame is active (pi's `showingProgress`).
    showing_progress: bool,
}

impl Inner {
    /// `setContent(...)` — install a new screen, clearing progress state.
    fn set_content(&mut self, screen: Screen, focused: bool) {
        self.progress_resolver = None;
        self.showing_progress = false;
        self.screen = screen;
        self.apply_focus(focused);
    }

    /// Propagate focus to the live input widget (pi's `inputTarget.focused`).
    fn apply_focus(&mut self, focused: bool) {
        if let Screen::Search { search, .. } = &mut self.screen {
            search.set_focused(focused);
        }
    }
}

/// The llama UI view (pi's `LlamaView`), an interior-mutable [`LlamaUi`].
pub struct LlamaView {
    theme: Theme,
    keybindings: KeybindingsManager,
    search_cache: RefCell<HashMap<String, Vec<HuggingFaceModel>>>,
    inner: RefCell<Inner>,
    focused: Cell<bool>,
    /// pi's `tui.requestRender()`; a no-op sink unless a host installs one.
    request_render: Box<dyn Fn()>,
}

impl LlamaView {
    /// `new LlamaView(tui, theme, keybindings)`. `request_render` stands in for
    /// `tui.requestRender` (defaults to a no-op via [`LlamaView::new`]).
    pub fn with_render_host(
        theme: Theme,
        keybindings: KeybindingsManager,
        request_render: Box<dyn Fn()>,
    ) -> Self {
        Self {
            theme,
            keybindings,
            search_cache: RefCell::new(HashMap::new()),
            inner: RefCell::new(Inner {
                screen: Screen::Loading,
                progress_resolver: None,
                showing_progress: false,
            }),
            focused: Cell::new(false),
            request_render,
        }
    }

    /// Convenience constructor with a no-op render sink.
    pub fn new(theme: Theme, keybindings: KeybindingsManager) -> Self {
        Self::with_render_host(theme, keybindings, Box::new(|| {}))
    }

    /// Set the view's focus (pi's `Focusable.focused` setter).
    pub fn set_focused(&self, value: bool) {
        self.focused.set(value);
        self.inner.borrow_mut().apply_focus(value);
    }

    /// Whether the view is focused.
    pub fn is_focused(&self) -> bool {
        self.focused.get()
    }

    fn request_render(&self) {
        (self.request_render)();
    }

    /// `handleInput(data)` (pi's `LlamaView.handleInput`), interior-mutable so the
    /// async dialog futures can run concurrently.
    pub fn handle_input(&self, data: &str) {
        // Progress "stop" path.
        {
            let mut inner = self.inner.borrow_mut();
            if inner.progress_resolver.is_some()
                && self.keybindings.matches(data, "tui.select.cancel")
            {
                let resolver = inner.progress_resolver.take();
                drop(inner);
                if let Some(resolver) = resolver {
                    let _ = resolver.send(());
                }
                return;
            }
        }
        let mut inner = self.inner.borrow_mut();
        match &mut inner.screen {
            Screen::Models { list, .. } | Screen::Select { list, .. } => {
                list.handle_input_str(data);
            }
            Screen::Search {
                search, resolver, ..
            } => {
                if let Some(resolution) =
                    search.handle_input(data, &self.keybindings, &self.search_cache)
                {
                    let sender = resolver.borrow_mut().take();
                    drop(inner);
                    if let Some(sender) = sender {
                        let _ = sender.send(resolution);
                    }
                    self.request_render();
                    return;
                }
            }
            Screen::Loading | Screen::Status { .. } | Screen::Progress(_) => {}
        }
        drop(inner);
        self.request_render();
    }

    /// Drive the pending Hugging Face fetch (pi's `runSearch`, the body of the
    /// 500 ms debounce timer). No-op unless [`HuggingFaceSearch::schedule_search`]
    /// recorded a pending query.
    pub async fn run_pending_search(&self) {
        let (query, search, signal, seq) = {
            let mut inner = self.inner.borrow_mut();
            let Screen::Search { search, .. } = &mut inner.screen else {
                return;
            };
            let Some(query) = search.pending_query.take() else {
                return;
            };
            let signal = AbortSignal::new();
            search.request_seq += 1;
            let seq = search.request_seq;
            search.request = Some(signal.clone());
            (query, search.search.clone(), signal, seq)
        };

        let result = (search)(query.clone(), signal.clone()).await;

        let mut inner = self.inner.borrow_mut();
        let Screen::Search { search, .. } = &mut inner.screen else {
            return;
        };
        // `finally { if (this.request === request) this.request = undefined }`.
        let clear_request = |search: &mut HuggingFaceSearch| {
            if search.request_seq == seq {
                search.request = None;
            }
        };
        match result {
            Ok(models) => {
                self.search_cache
                    .borrow_mut()
                    .insert(query.to_lowercase(), models.clone());
                if search.closed || signal.is_aborted() || search.query != query {
                    clear_request(search);
                    return;
                }
                search.results = models.clone();
                search.selected_index = 0;
                search.status = if models.is_empty() {
                    NO_MODELS_STATUS.to_string()
                } else {
                    String::new()
                };
                search.filter_results();
            }
            Err(message) => {
                if search.closed || signal.is_aborted() || search.query != query {
                    clear_request(search);
                    return;
                }
                search.results = Vec::new();
                search.status = message;
                search.filter_results();
            }
        }
        clear_request(search);
        drop(inner);
        self.request_render();
    }

    /// The progress frame's rendered lines.
    fn render_progress(&self, state: &ProgressState, width: usize) -> Vec<String> {
        let mut body: Vec<Box<dyn Component>> = vec![
            Box::new(Text::new(
                &fg(&self.theme, "text", &state.model),
                1,
                0,
                None,
            )),
            Box::new(Spacer::new(1)),
            Box::new(Text::new(
                &fg(&self.theme, "muted", &state.message),
                1,
                0,
                None,
            )),
        ];
        if let Some(ratio) = state.ratio {
            let available: i64 = 40;
            let filled = (ratio.clamp(0.0, 1.0) * available as f64).round() as i64;
            let bar = format!(
                "{}{} {}%",
                "\u{2588}".repeat(filled.max(0) as usize),
                "\u{2500}".repeat((available - filled).max(0) as usize),
                (ratio * 100.0).round() as i64
            );
            body.push(Box::new(Text::new(
                &fg(&self.theme, "accent", &bar),
                1,
                0,
                None,
            )));
        }
        if let Some(detail) = state.detail.as_ref().filter(|d| !d.is_empty()) {
            body.push(Box::new(Text::new(
                &fg(&self.theme, "dim", detail),
                1,
                0,
                None,
            )));
        }
        let footer = key_hint(&self.theme, &self.keybindings, "tui.select.cancel", "stop");
        frame(&self.theme, &state.title, body, Some(&footer)).render(width)
    }

    /// `render(width)` (pi's `LlamaView.render`): the current screen's frame, with
    /// any over-wide line truncated to `width`.
    pub fn render(&self, width: usize) -> Vec<String> {
        let inner = self.inner.borrow();
        let lines = match &inner.screen {
            Screen::Loading => frame(
                &self.theme,
                "llama.cpp models",
                vec![Box::new(Text::new(
                    &fg(&self.theme, "muted", "Loading\u{2026}"),
                    1,
                    1,
                    None,
                ))],
                None,
            )
            .render(width),
            Screen::Models {
                server_url,
                list,
                footer,
            } => {
                let body: Vec<Box<dyn Component>> = vec![
                    Box::new(Text::new(&fg(&self.theme, "dim", server_url), 1, 0, None)),
                    Box::new(Spacer::new(1)),
                    Box::new(PreRendered(list.render_lines(width))),
                ];
                frame(&self.theme, "llama.cpp models", body, Some(footer)).render(width)
            }
            Screen::Select {
                title,
                list,
                footer,
            } => {
                let body: Vec<Box<dyn Component>> = vec![
                    Box::new(Spacer::new(1)),
                    Box::new(PreRendered(list.render_lines(width))),
                ];
                frame(&self.theme, title, body, Some(footer)).render(width)
            }
            Screen::Search { search, footer, .. } => {
                let body: Vec<Box<dyn Component>> = vec![
                    Box::new(Spacer::new(1)),
                    Box::new(PreRendered(search.render(width, &self.theme))),
                ];
                frame(&self.theme, "Download model", body, Some(footer)).render(width)
            }
            Screen::Status { title, message } => {
                let body: Vec<Box<dyn Component>> = vec![
                    Box::new(Spacer::new(1)),
                    Box::new(Text::new(&fg(&self.theme, "muted", message), 1, 0, None)),
                ];
                frame(&self.theme, title, body, None).render(width)
            }
            Screen::Progress(state) => self.render_progress(state, width),
        };
        lines
            .into_iter()
            .map(|line| {
                if visible_width(&line) > width {
                    truncate_to_width(&line, width as i64, "", false)
                } else {
                    line
                }
            })
            .collect()
    }

    /// `invalidate()` — no cached render state to invalidate (each `render`
    /// recomposes from live state).
    pub fn invalidate(&self) {}
}

impl LlamaUi for LlamaView {
    async fn show_models(
        &self,
        server_url: &str,
        models: Vec<LlamaModelInfo>,
    ) -> LlamaManagerAction {
        let (tx, rx) = oneshot::channel::<LlamaManagerAction>();
        let tx = Rc::new(RefCell::new(Some(tx)));

        // Sort loaded-first, then by id. pi uses `localeCompare`; the port
        // approximates it with a case-insensitive primary key and a
        // case-sensitive tiebreak (see the module note — a full ICU collation is
        // out of scope). This ordering is behavioural, unit-tested, not vectored.
        let mut sorted = models;
        sorted.sort_by(|left, right| {
            let loaded = (right.status.value == LlamaModelStatus::Loaded) as i64
                - (left.status.value == LlamaModelStatus::Loaded) as i64;
            if loaded != 0 {
                return loaded.cmp(&0);
            }
            left.id
                .to_lowercase()
                .cmp(&right.id.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        let by_id: HashMap<String, LlamaModelInfo> =
            sorted.iter().map(|m| (m.id.clone(), m.clone())).collect();

        let mut items: Vec<SelectItem> = sorted
            .iter()
            .map(|model| {
                // pi passes `description: modelDescription(model)` (possibly "")
                // and its `SelectList` collapses an empty description via a truthy
                // check (`item.description ? … : undefined`); pidgin's `SelectList`
                // keeps `Some("")`, so map an empty description to `None` here to
                // reproduce pi's rendering.
                let description = model_description(model);
                SelectItem {
                    value: model.id.clone(),
                    label: model.id.clone(),
                    description: (!description.is_empty()).then_some(description),
                }
            })
            .collect();
        items.push(SelectItem {
            value: DOWNLOAD_VALUE.to_string(),
            label: "Download model\u{2026}".to_string(),
            description: Some("Hugging Face owner/repository[:quant]".to_string()),
        });

        let max_visible = (items.len().min(12)) as i64;
        let mut list = SelectList::new(
            items,
            max_visible,
            select_theme(&self.theme),
            SelectListLayoutOptions {
                min_primary_column_width: Some(36),
                max_primary_column_width: Some(56),
                truncate_primary: None,
            },
        );
        let tx_select = tx.clone();
        list.on_select = Some(Box::new(move |item: SelectItem| {
            let action = if item.value == DOWNLOAD_VALUE {
                Some(LlamaManagerAction::Download)
            } else {
                by_id
                    .get(&item.value)
                    .cloned()
                    .map(LlamaManagerAction::Model)
            };
            if let Some(action) = action {
                if let Some(tx) = tx_select.borrow_mut().take() {
                    let _ = tx.send(action);
                }
            }
        }));
        let tx_cancel = tx.clone();
        list.on_cancel = Some(Box::new(move || {
            if let Some(tx) = tx_cancel.borrow_mut().take() {
                let _ = tx.send(LlamaManagerAction::Close);
            }
        }));

        let footer = format!(
            "{} \u{2022} {}",
            key_hint(
                &self.theme,
                &self.keybindings,
                "tui.select.confirm",
                "load/unload/download"
            ),
            key_hint(&self.theme, &self.keybindings, "tui.select.cancel", "close")
        );
        self.inner.borrow_mut().set_content(
            Screen::Models {
                server_url: server_url.to_string(),
                list,
                footer,
            },
            self.focused.get(),
        );
        self.request_render();
        rx.await.unwrap_or(LlamaManagerAction::Close)
    }

    async fn select(&self, title: &str, options: Vec<String>) -> Option<String> {
        let (tx, rx) = oneshot::channel::<Option<String>>();
        let tx = Rc::new(RefCell::new(Some(tx)));

        let items: Vec<SelectItem> = options
            .iter()
            .map(|option| SelectItem {
                value: option.clone(),
                label: option.clone(),
                description: None,
            })
            .collect();
        let max_visible = (options.len().min(12)) as i64;
        let mut list = SelectList::new(
            items,
            max_visible,
            select_theme(&self.theme),
            SelectListLayoutOptions::default(),
        );
        let tx_select = tx.clone();
        list.on_select = Some(Box::new(move |item: SelectItem| {
            if let Some(tx) = tx_select.borrow_mut().take() {
                let _ = tx.send(Some(item.value));
            }
        }));
        let tx_cancel = tx.clone();
        list.on_cancel = Some(Box::new(move || {
            if let Some(tx) = tx_cancel.borrow_mut().take() {
                let _ = tx.send(None);
            }
        }));

        let footer = format!(
            "{} \u{2022} {}",
            key_hint(
                &self.theme,
                &self.keybindings,
                "tui.select.confirm",
                "select"
            ),
            key_hint(
                &self.theme,
                &self.keybindings,
                "tui.select.cancel",
                "cancel"
            )
        );
        self.inner.borrow_mut().set_content(
            Screen::Select {
                title: title.to_string(),
                list,
                footer,
            },
            self.focused.get(),
        );
        self.request_render();
        rx.await.unwrap_or(None)
    }

    async fn confirm(&self, title: &str, message: &str) -> bool {
        self.select(
            &format!("{title}\n{message}"),
            vec!["Yes".to_string(), "No".to_string()],
        )
        .await
            == Some("Yes".to_string())
    }

    async fn connection_error(&self, server_url: &str, message: &str) -> ConnectionErrorChoice {
        let choice = self
            .select(
                &format!("llama.cpp unavailable\n{server_url}\n\n{message}"),
                vec!["Retry".to_string(), "Close".to_string()],
            )
            .await;
        if choice == Some("Retry".to_string()) {
            ConnectionErrorChoice::Retry
        } else {
            ConnectionErrorChoice::Close
        }
    }

    async fn search_models(&self, search: SearchFn) -> Option<String> {
        let (tx, rx) = oneshot::channel::<Option<String>>();
        let resolver = Rc::new(RefCell::new(Some(tx)));
        let component = HuggingFaceSearch::new(search, self.focused.get());
        let footer = format!(
            "{} \u{2022} {}",
            key_hint(
                &self.theme,
                &self.keybindings,
                "tui.select.confirm",
                "select"
            ),
            key_hint(&self.theme, &self.keybindings, "tui.select.cancel", "back")
        );
        self.inner.borrow_mut().set_content(
            Screen::Search {
                search: component,
                footer,
                resolver: resolver.clone(),
            },
            self.focused.get(),
        );
        self.request_render();
        rx.await.unwrap_or(None)
    }

    fn show_status(&self, title: &str, message: &str) {
        self.inner.borrow_mut().set_content(
            Screen::Status {
                title: title.to_string(),
                message: message.to_string(),
            },
            self.focused.get(),
        );
        self.request_render();
    }

    async fn progress(&self, state: ProgressState) {
        let rx = {
            let mut inner = self.inner.borrow_mut();
            let (tx, rx) = oneshot::channel::<()>();
            // pi reuses one `progressPromise`; the cancel loop never calls
            // `progress` twice without a reset, so creating a fresh channel each
            // call (dropping any prior sender) is observably identical.
            inner.progress_resolver = Some(tx);
            inner.showing_progress = true;
            rx
        };
        self.update_progress(state);
        let _ = rx.await;
    }

    fn update_progress(&self, state: ProgressState) {
        let mut inner = self.inner.borrow_mut();
        if !inner.showing_progress {
            return;
        }
        inner.screen = Screen::Progress(state);
        drop(inner);
        self.request_render();
    }
}

// ---------------------------------------------------------------------------
// runWithProgress
// ---------------------------------------------------------------------------

/// The outcome of [`run_with_progress`] (pi's `{ cancelled }` union).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome<T> {
    /// The user cancelled the operation.
    Cancelled,
    /// The operation completed with `value`.
    Value(T),
}

/// The progress-update callback handed to the operation (pi's `update:
/// (progress: LlamaProgress) => void`).
pub type ProgressUpdate<'a> = dyn Fn(LlamaProgress) + 'a;

/// The boxed future the operation returns from [`run_with_progress`]'s `run`
/// callback. Boxed with an explicit lifetime so the operation may hold the
/// borrowed [`ProgressUpdate`] across its await points (pi's `run` is a JS async
/// function that closes over `update`).
pub type RunFuture<'u, T, E> = std::pin::Pin<Box<dyn Future<Output = Result<T, E>> + 'u>>;

/// Merge a [`LlamaProgress`] into the running [`ProgressState`], mirroring pi's
/// `Object.assign(state, progress)` (absent optional keys are retained).
fn merge_progress(state: &mut ProgressState, progress: LlamaProgress) {
    state.message = progress.message;
    if progress.ratio.is_some() {
        state.ratio = progress.ratio;
    }
    if progress.detail.is_some() {
        state.detail = progress.detail;
    }
}

/// `runWithProgress(ui, options)` — run `run` while showing a progress frame,
/// letting the user cancel via the stop key + confirm dialog (pi's
/// `Promise.race` cancel loop). Cancellation trips `signal` and awaits the
/// operation before returning [`RunOutcome::Cancelled`]; a completed operation's
/// error is propagated.
#[allow(clippy::too_many_arguments)]
pub async fn run_with_progress<U, T, E, RunFn, CancelFn, CancelFut>(
    ui: &U,
    title: String,
    model: String,
    initial_message: String,
    cancel_title: String,
    cancel_message: String,
    run: RunFn,
    cancel: CancelFn,
) -> Result<RunOutcome<T>, E>
where
    U: LlamaUi,
    RunFn: for<'u> FnOnce(AbortSignal, &'u ProgressUpdate<'u>) -> RunFuture<'u, T, E>,
    CancelFn: FnOnce() -> CancelFut,
    CancelFut: Future<Output = ()>,
{
    let signal = AbortSignal::new();
    let state = Rc::new(RefCell::new(ProgressState {
        title,
        model,
        message: initial_message,
        ratio: None,
        detail: None,
    }));

    let update_state = state.clone();
    let update = move |progress: LlamaProgress| {
        let snapshot = {
            let mut current = update_state.borrow_mut();
            merge_progress(&mut current, progress);
            current.clone()
        };
        ui.update_progress(snapshot);
    };

    // `run_fut` is a `Pin<Box<dyn Future>>` (already pinned + `Unpin`), so it is
    // driven directly via `&mut run_fut` in the races below.
    let mut run_fut = run(signal.clone(), &update);

    loop {
        enum Ev<T, E> {
            Settled(Result<T, E>),
            Stop,
        }
        let snapshot = state.borrow().clone();
        let event = tokio::select! {
            result = &mut run_fut => Ev::Settled(result),
            () = ui.progress(snapshot) => Ev::Stop,
        };
        match event {
            Ev::Settled(result) => {
                return result.map(RunOutcome::Value);
            }
            Ev::Stop => {
                let stop = ui.confirm(&cancel_title, &cancel_message).await;
                if !stop {
                    continue;
                }
                // Re-check completion: the operation may have settled while the
                // confirm dialog was open (pi's `|| completed`). Poll `run_fut`
                // once without blocking.
                let settled: Option<Result<T, E>> = tokio::select! {
                    biased;
                    result = &mut run_fut => Some(result),
                    () = std::future::ready(()) => None,
                };
                if let Some(result) = settled {
                    return result.map(RunOutcome::Value);
                }
                cancel().await;
                signal.abort();
                // `await settled; return { cancelled: true }` — the operation's
                // own result is discarded on cancel.
                let _ = (&mut run_fut).await;
                return Ok(RunOutcome::Cancelled);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// showLlamaUi — mount seam stub
// ---------------------------------------------------------------------------

/// `showLlamaUi(ctx, run)` — mount the [`LlamaView`] and drive `run(view)` (pi's
/// `showLlamaUi`).
///
/// **Deferred.** pi mounts via `ctx.ui.custom(...)` and reports errors via
/// `ctx.ui.notify(...)`. The Rust [`ExtensionContext`] is an opaque marker trait
/// and that `ctx.ui` capability is not yet defined (see the module docs). This is
/// a stub for the extension-framework lane to wire once `ctx.ui.custom`/`notify`
/// exist: it must construct a [`LlamaView`], mount it as a focused overlay,
/// `await run(&view)`, and unmount + surface any error via `notify`.
pub fn show_llama_ui<C: ExtensionContext>(_ctx: &C) {
    // PR follow-up: wire to `ctx.ui.custom`/`ctx.ui.notify` when the
    // extension-framework lane defines the `ctx.ui` capability. The `LlamaView`
    // (above) is already complete and testable against `LlamaUi`.
    unimplemented!(
        "showLlamaUi requires the ctx.ui.custom/notify mount seam (deferred to the \
         extension-framework lane); LlamaView is fully implemented and testable"
    )
}

#[cfg(test)]
mod tests {
    //! Unit coverage for the pure formatting helpers whose edge cases the render
    //! vectors don't reach directly (notably the `contextLabel` launch-arg
    //! branch, shadowed by `meta` in the vectored data).

    use super::*;
    use crate::extensions::llama::client::{LlamaMeta, LlamaModelStatusInfo};

    fn model(
        status: LlamaModelStatus,
        args: Option<&[&str]>,
        meta: Option<LlamaMeta>,
    ) -> LlamaModelInfo {
        LlamaModelInfo {
            id: "owner/model".into(),
            aliases: None,
            status: LlamaModelStatusInfo {
                value: status,
                args: args.map(|a| a.iter().map(|s| s.to_string()).collect()),
                failed: None,
                exit_code: None,
                progress: None,
            },
            architecture: None,
            source: None,
            meta,
        }
    }

    #[test]
    fn compact_count_thresholds() {
        assert_eq!(compact_count(0), "0");
        assert_eq!(compact_count(999), "999");
        assert_eq!(compact_count(1_000), "1.0k");
        assert_eq!(compact_count(88_400), "88.4k");
        assert_eq!(compact_count(100_000), "100k");
        assert_eq!(compact_count(1_534_221), "1.5M");
        assert_eq!(compact_count(12_000_000), "12M");
    }

    #[test]
    fn context_label_prefers_meta_then_args() {
        let meta = LlamaMeta {
            n_ctx: Some(8192.0),
            n_ctx_train: Some(32768.0),
            size: None,
            ftype: None,
        };
        assert_eq!(
            context_label(&model(LlamaModelStatus::Loaded, None, Some(meta))),
            Some("8k".to_string())
        );
        // n_ctx_train fallback.
        let train_only = LlamaMeta {
            n_ctx: None,
            n_ctx_train: Some(131072.0),
            size: None,
            ftype: None,
        };
        assert_eq!(
            context_label(&model(LlamaModelStatus::Loaded, None, Some(train_only))),
            Some("131k".to_string())
        );
        // No meta -> the `--ctx-size` launch arg (this branch is shadowed by meta
        // in the render vectors).
        assert_eq!(
            context_label(&model(
                LlamaModelStatus::Loaded,
                Some(&["--ctx-size", "4096"]),
                None
            )),
            Some("4k".to_string())
        );
        assert_eq!(
            context_label(&model(LlamaModelStatus::Loaded, Some(&["-c", "512"]), None)),
            Some("512".to_string())
        );
        // No context info at all.
        assert_eq!(
            context_label(&model(LlamaModelStatus::Loaded, None, None)),
            None
        );
    }

    #[test]
    fn model_description_by_status() {
        let meta = LlamaMeta {
            n_ctx: Some(8192.0),
            n_ctx_train: None,
            size: None,
            ftype: None,
        };
        assert_eq!(
            model_description(&model(LlamaModelStatus::Loaded, None, Some(meta))),
            "loaded \u{00b7} 8k context"
        );
        // Sleeping renders as "loaded" (pi pushes the literal), no context without meta.
        assert_eq!(
            model_description(&model(LlamaModelStatus::Sleeping, None, None)),
            "loaded"
        );
        assert_eq!(
            model_description(&model(LlamaModelStatus::Downloading, None, None)),
            "downloading"
        );
        assert_eq!(
            model_description(&model(LlamaModelStatus::Unloaded, None, None)),
            ""
        );
    }
}
