// straitjacket-allow-file:duplication — each `#[test]` is a standalone scenario
// that repeats a two- to three-line setup preamble (mount a dialog, type a
// query, poll) and the `run_with_progress` call's fixed argument list; keeping
// each test self-contained and readable is deliberate.
//! Behavioural tests for the llama-extension TUI port: the parts that are not
//! pure rendering (dialog resolution, the Hugging Face search debounce/cache/
//! exact-match state machine, and the [`run_with_progress`] cancel loop). These
//! mirror pi's `ui.ts` behaviour rather than its bytes.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use pidgin_ai::seams::AbortSignal;
use pidgin_coding::extensions::llama::{
    ConnectionErrorChoice, HuggingFaceModel, LlamaProgress, LlamaUi, LlamaView, ProgressState,
    ProgressUpdate, RunOutcome, SearchFn,
};
use pidgin_coding::modes::interactive::theme::{create_theme, parse_theme_json, ColorMode, Theme};
use pidgin_tui::keybindings::{tui_keybindings, KeybindingsManager};

fn dark_theme() -> Theme {
    let json_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/modes/interactive/theme/dark.json");
    let content = std::fs::read_to_string(&json_path).expect("read dark.json");
    create_theme(
        &parse_theme_json(&content).expect("parse"),
        Some(ColorMode::Color256),
        None,
    )
    .expect("theme")
}

fn new_view() -> LlamaView {
    LlamaView::new(
        dark_theme(),
        KeybindingsManager::new(tui_keybindings(), Vec::new()),
    )
}

/// A hand-driven executor over a `!Send` future + no-op waker, so a test can
/// interleave `poll` with synchronous `view.handle_input` calls (mirroring pi's
/// event loop resolving a dialog promise on keypress).
struct Stepper<'a, T> {
    fut: Pin<Box<dyn Future<Output = T> + 'a>>,
}

impl<'a, T> Stepper<'a, T> {
    fn new(fut: impl Future<Output = T> + 'a) -> Self {
        Self { fut: Box::pin(fut) }
    }
    fn poll(&mut self) -> Option<T> {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        match self.fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => Some(v),
            Poll::Pending => None,
        }
    }
}

fn render_contains(view: &LlamaView, needle: &str) -> bool {
    view.render(80).iter().any(|line| line.contains(needle))
}

// --- dialog resolution ------------------------------------------------------

#[test]
fn confirm_resolves_yes_on_enter() {
    let view = new_view();
    view.set_focused(true);
    let mut step = Stepper::new(view.confirm("Remove model", "Delete the weights?"));
    assert!(step.poll().is_none(), "screen mounts, then pends");
    view.handle_input("\r"); // enter selects the first option ("Yes")
    assert_eq!(step.poll(), Some(true));
}

#[test]
fn confirm_resolves_no_on_down_enter() {
    let view = new_view();
    view.set_focused(true);
    let mut step = Stepper::new(view.confirm("Remove model", "Delete the weights?"));
    assert!(step.poll().is_none());
    view.handle_input("\x1b[B"); // down -> "No"
    view.handle_input("\r");
    assert_eq!(step.poll(), Some(false));
}

#[test]
fn confirm_resolves_false_on_escape() {
    let view = new_view();
    view.set_focused(true);
    let mut step = Stepper::new(view.confirm("t", "m"));
    assert!(step.poll().is_none());
    view.handle_input("\x1b"); // escape -> select cancels -> None -> != "Yes"
    assert_eq!(step.poll(), Some(false));
}

#[test]
fn connection_error_maps_retry_and_close() {
    let view = new_view();
    view.set_focused(true);
    let mut retry = Stepper::new(view.connection_error("url", "boom"));
    assert!(retry.poll().is_none());
    view.handle_input("\r"); // "Retry"
    assert_eq!(retry.poll(), Some(ConnectionErrorChoice::Retry));

    let view = new_view();
    view.set_focused(true);
    let mut close = Stepper::new(view.connection_error("url", "boom"));
    assert!(close.poll().is_none());
    view.handle_input("\x1b[B"); // down -> "Close"
    view.handle_input("\r");
    assert_eq!(close.poll(), Some(ConnectionErrorChoice::Close));
}

#[test]
fn show_models_resolves_download_entry() {
    let view = new_view();
    view.set_focused(true);
    let mut step = Stepper::new(view.show_models("srv", Vec::new()));
    assert!(step.poll().is_none());
    // Only the "Download model…" entry exists (no models) -> enter resolves it.
    view.handle_input("\r");
    assert_eq!(
        step.poll(),
        Some(pidgin_coding::extensions::llama::LlamaManagerAction::Download)
    );
}

// --- Hugging Face search state machine --------------------------------------

fn counting_search(calls: Rc<Cell<u32>>, results: Vec<HuggingFaceModel>) -> SearchFn {
    Rc::new(move |_query: String, _signal: AbortSignal| {
        calls.set(calls.get() + 1);
        let results = results.clone();
        let fut: Pin<Box<dyn Future<Output = Result<Vec<HuggingFaceModel>, String>>>> =
            Box::pin(async move { Ok(results) });
        fut
    })
}

fn hf(id: &str, downloads: u64) -> HuggingFaceModel {
    HuggingFaceModel {
        id: id.to_string(),
        downloads,
    }
}

fn poll_ready<F: Future>(fut: F) {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut fut = Box::pin(fut);
    assert!(
        matches!(fut.as_mut().poll(&mut cx), Poll::Ready(_)),
        "future should be ready"
    );
}

fn type_query(view: &LlamaView, query: &str) {
    for ch in query.chars() {
        view.handle_input(&ch.to_string());
    }
}

#[test]
fn search_under_two_chars_shows_hint_and_never_fetches() {
    let view = new_view();
    view.set_focused(true);
    let calls = Rc::new(Cell::new(0));
    let mut step = Stepper::new(view.search_models(counting_search(calls.clone(), Vec::new())));
    step.poll();
    type_query(&view, "q");
    assert!(render_contains(&view, "Type at least 2 characters"));
    poll_ready(view.run_pending_search()); // no pending query -> no-op
    assert_eq!(calls.get(), 0);
}

#[test]
fn search_fetches_and_filters_then_caches() {
    let view = new_view();
    view.set_focused(true);
    let calls = Rc::new(Cell::new(0));
    let results = vec![
        hf("unsloth/Qwen3-4B-GGUF", 1000),
        hf("bartowski/Qwen2.5-GGUF", 900),
        hf("TheBloke/CodeLlama-GGUF", 800), // no "qwen" subsequence
    ];
    let mut step = Stepper::new(view.search_models(counting_search(calls.clone(), results)));
    step.poll();
    type_query(&view, "qwen");
    assert!(render_contains(&view, "Searching Hugging Face"));
    poll_ready(view.run_pending_search());
    assert_eq!(calls.get(), 1);
    // Filtered to the two Qwen ids; the CodeLlama entry is filtered out.
    assert!(render_contains(&view, "unsloth/Qwen3-4B-GGUF"));
    assert!(render_contains(&view, "bartowski/Qwen2.5-GGUF"));
    assert!(!render_contains(&view, "CodeLlama"));

    // Re-issuing the same query hits the cache: no second fetch.
    view.handle_input("\x7f"); // backspace one char -> "qwe"
    poll_ready(view.run_pending_search());
    type_query(&view, "n"); // back to "qwen" (cached)
    poll_ready(view.run_pending_search());
    assert_eq!(
        calls.get(),
        2,
        "only the intermediate 'qwe' fetched; 'qwen' was cached"
    );
}

#[test]
fn search_empty_result_reports_no_models() {
    let view = new_view();
    view.set_focused(true);
    let calls = Rc::new(Cell::new(0));
    let mut step = Stepper::new(view.search_models(counting_search(calls, Vec::new())));
    step.poll();
    type_query(&view, "nope");
    poll_ready(view.run_pending_search());
    assert!(render_contains(&view, "No GGUF models found"));
}

#[test]
fn search_exact_match_resolves_without_results() {
    let view = new_view();
    view.set_focused(true);
    let calls = Rc::new(Cell::new(0));
    let mut step = Stepper::new(view.search_models(counting_search(calls, Vec::new())));
    step.poll();
    type_query(&view, "owner/repo:Q4_K_M");
    view.handle_input("\r"); // confirm -> exact owner/repo[:quant] match
    assert_eq!(step.poll(), Some(Some("owner/repo:Q4_K_M".to_string())));
}

#[test]
fn search_cancel_resolves_none() {
    let view = new_view();
    view.set_focused(true);
    let calls = Rc::new(Cell::new(0));
    let mut step = Stepper::new(view.search_models(counting_search(calls, Vec::new())));
    step.poll();
    type_query(&view, "qw");
    view.handle_input("\x1b"); // escape -> back
    assert_eq!(step.poll(), Some(None));
}

// --- runWithProgress --------------------------------------------------------

/// A scripted [`LlamaUi`] for [`run_with_progress`]: `progress` resolves (user
/// "stop") for its first `stop_limit` calls, then pends forever; `confirm`
/// answers from a queue; `update_progress` records states.
struct MockUi {
    stop_limit: Cell<u32>,
    progress_calls: Cell<u32>,
    confirm_answers: RefCell<VecDeque<bool>>,
    updates: RefCell<Vec<ProgressState>>,
}

impl MockUi {
    fn new(stop_limit: u32, confirm_answers: &[bool]) -> Self {
        Self {
            stop_limit: Cell::new(stop_limit),
            progress_calls: Cell::new(0),
            confirm_answers: RefCell::new(confirm_answers.iter().copied().collect()),
            updates: RefCell::new(Vec::new()),
        }
    }
}

impl LlamaUi for MockUi {
    async fn show_models(
        &self,
        _server_url: &str,
        _models: Vec<pidgin_coding::extensions::llama::LlamaModelInfo>,
    ) -> pidgin_coding::extensions::llama::LlamaManagerAction {
        unimplemented!()
    }
    async fn select(&self, _title: &str, _options: Vec<String>) -> Option<String> {
        unimplemented!()
    }
    async fn confirm(&self, _title: &str, _message: &str) -> bool {
        self.confirm_answers
            .borrow_mut()
            .pop_front()
            .unwrap_or(false)
    }
    async fn connection_error(&self, _server_url: &str, _message: &str) -> ConnectionErrorChoice {
        unimplemented!()
    }
    async fn search_models(&self, _search: SearchFn) -> Option<String> {
        unimplemented!()
    }
    fn show_status(&self, _title: &str, _message: &str) {
        unimplemented!()
    }
    async fn progress(&self, _state: ProgressState) {
        let n = self.progress_calls.get() + 1;
        self.progress_calls.set(n);
        if n > self.stop_limit.get() {
            std::future::pending::<()>().await;
        }
    }
    fn update_progress(&self, state: ProgressState) {
        self.updates.borrow_mut().push(state);
    }
}

/// A future that yields (Pending) `pending_polls` times before resolving, so a
/// test can control whether the operation or a "stop" wins the race.
struct ReadyAfter {
    remaining: u32,
    value: Option<Result<i32, String>>,
}

impl Future for ReadyAfter {
    type Output = Result<i32, String>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.remaining == 0 {
            Poll::Ready(self.value.take().expect("polled after ready"))
        } else {
            self.remaining -= 1;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

#[tokio::test]
async fn run_with_progress_returns_value_on_completion() {
    let ui = MockUi::new(0, &[]); // progress never resolves -> operation wins
    let outcome = pidgin_coding::extensions::llama::run_with_progress(
        &ui,
        "Downloading".into(),
        "m".into(),
        "start".into(),
        "Stop?".into(),
        "Cancel the download?".into(),
        |_signal: AbortSignal, update: &ProgressUpdate<'_>| {
            Box::pin(async move {
                update(LlamaProgress {
                    message: "half".into(),
                    ratio: Some(0.5),
                    detail: None,
                });
                Ok::<i32, String>(42)
            })
        },
        || async {},
    )
    .await;
    assert_eq!(outcome, Ok(RunOutcome::Value(42)));
    let updates = ui.updates.borrow();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].message, "half");
    assert_eq!(updates[0].ratio, Some(0.5));
}

#[tokio::test]
async fn run_with_progress_propagates_error() {
    let ui = MockUi::new(0, &[]);
    let outcome = pidgin_coding::extensions::llama::run_with_progress(
        &ui,
        "t".into(),
        "m".into(),
        "s".into(),
        "ct".into(),
        "cm".into(),
        |_signal: AbortSignal, _update: &ProgressUpdate<'_>| {
            Box::pin(async move { Err::<i32, String>("boom".into()) })
        },
        || async {},
    )
    .await;
    assert_eq!(outcome, Err("boom".to_string()));
}

#[tokio::test]
async fn run_with_progress_cancels_on_stop_and_confirm() {
    let ui = MockUi::new(1, &[true]); // stop once, confirm yes
    let cancel_called = Rc::new(Cell::new(false));
    let cancel_flag = cancel_called.clone();
    let outcome = pidgin_coding::extensions::llama::run_with_progress(
        &ui,
        "t".into(),
        "m".into(),
        "s".into(),
        "ct".into(),
        "cm".into(),
        |signal: AbortSignal, _update: &ProgressUpdate<'_>| {
            Box::pin(async move {
                loop {
                    if signal.is_aborted() {
                        return Ok::<i32, String>(-1);
                    }
                    tokio::task::yield_now().await;
                }
            })
        },
        move || {
            let flag = cancel_flag.clone();
            async move {
                flag.set(true);
            }
        },
    )
    .await;
    assert_eq!(outcome, Ok(RunOutcome::Cancelled));
    assert!(cancel_called.get(), "cancel closure was invoked");
}

#[tokio::test]
async fn run_with_progress_declines_stop_then_completes() {
    let ui = MockUi::new(1, &[false]); // stop once, but decline the confirm
    let outcome = pidgin_coding::extensions::llama::run_with_progress(
        &ui,
        "t".into(),
        "m".into(),
        "s".into(),
        "ct".into(),
        "cm".into(),
        |_signal: AbortSignal, _update: &ProgressUpdate<'_>| {
            // Pending for the first race, ready for the second.
            Box::pin(ReadyAfter {
                remaining: 1,
                value: Some(Ok(7)),
            })
        },
        || async {},
    )
    .await;
    assert_eq!(outcome, Ok(RunOutcome::Value(7)));
    // The user was shown progress and pressed stop at least once (the exact count
    // of the second race's progress poll is left to `tokio::select!`'s
    // unspecified branch order, so it is not asserted).
    assert!(
        ui.progress_calls.get() >= 1,
        "progress was shown before the decline"
    );
    assert_eq!(
        *ui.confirm_answers.borrow(),
        VecDeque::new(),
        "the decline was consumed"
    );
}
