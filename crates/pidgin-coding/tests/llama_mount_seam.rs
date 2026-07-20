// straitjacket-allow-file:duplication — the theme/keybindings/scripted-input
// setup preamble is the same two-helper boilerplate every llama TUI test file
// carries (see `llama_ui_behavior`); keeping each test file self-contained is
// deliberate.
//! Behavioural tests for the `/llama` mount seam: the widened
//! [`ExtensionContext::ui`] `custom` surface and the interactive
//! [`TuiExtensionUi`] host that mounts a [`LlamaView`] as a focused overlay and
//! drives its `run` future to completion by manual poll interleaved with pumped
//! terminal input (`show_llama_ui`, the Rust port of pi's `showLlamaUi`).
//!
//! These exercise the seam behaviour — mount → drive → unmount, the error →
//! notify branch, and the no-surface `Unavailable` guard — rather than any
//! rendered bytes; the `LlamaView` render/dialog vectors stay in
//! `llama_ui_behavior` / `llama_ui_vectors`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::pin::Pin;
use std::rc::Rc;

use pidgin_coding::core::extensions::types::{
    CustomHost, CustomMount, ExtensionContext, ExtensionUi, NotifyLevel, UiError,
};
use pidgin_coding::extensions::llama::{show_llama_ui, LlamaUi, LlamaView};
use pidgin_coding::modes::interactive::extension_ui::{InputSource, TuiExtensionUi};
use pidgin_coding::modes::interactive::theme::{create_theme, parse_theme_json, ColorMode, Theme};
use pidgin_tui::keybindings::{tui_keybindings, KeybindingsManager};
use pidgin_tui::renderer::Component;
use pidgin_tui::widgets::Text;
use pidgin_tui::{LoggingTerminal, Tui};

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

fn keybindings() -> KeybindingsManager {
    KeybindingsManager::new(tui_keybindings(), Vec::new())
}

/// A scripted input source over a queue of decoded chunks (pi's event loop
/// delivering keypresses); `None` once drained.
fn scripted(chunks: Vec<&str>) -> (InputSource<'static>, Rc<RefCell<VecDeque<String>>>) {
    let queue = Rc::new(RefCell::new(
        chunks
            .into_iter()
            .map(str::to_string)
            .collect::<VecDeque<_>>(),
    ));
    let handle = Rc::clone(&queue);
    let source: InputSource<'static> = Box::new(move || handle.borrow_mut().pop_front());
    (source, queue)
}

/// An [`ExtensionContext`] whose `ui()` returns a borrowed live host.
struct HostCtx<'a> {
    ui: &'a dyn ExtensionUi,
}
impl ExtensionContext for HostCtx<'_> {
    fn ui(&self) -> &dyn ExtensionUi {
        self.ui
    }
}

/// A default marker context: `ui()` falls back to the no-op surface.
struct BareCtx;
impl ExtensionContext for BareCtx {}

// --- mount → drive → unmount ------------------------------------------------

#[test]
fn custom_mounts_drives_and_unmounts_a_llama_view() {
    let mut tui = Tui::new(LoggingTerminal::new(80, 24), false);
    let (input, _queue) = scripted(vec!["\r"]); // Enter -> confirm "Yes"
    let host = TuiExtensionUi::new(&mut tui, dark_theme(), keybindings(), input);
    let ctx = HostCtx { ui: &host };

    let outcome = show_llama_ui(&ctx, |view: Rc<LlamaView>| async move {
        // A single confirm dialog, resolved by the scripted Enter.
        if view.confirm("Unload model?", "Unload owner/model?").await {
            Ok(())
        } else {
            Err("declined".to_string())
        }
    });

    assert!(
        matches!(outcome, Ok(())),
        "run completed, custom returns Ok"
    );
    assert!(
        !host.has_overlay(),
        "the overlay is unmounted once the view completes"
    );
    assert!(
        host.notifications().is_empty(),
        "a clean completion does not notify"
    );
}

// --- error → notify ---------------------------------------------------------

#[test]
fn custom_maps_run_error_to_failed_and_notifies() {
    let mut tui = Tui::new(LoggingTerminal::new(80, 24), false);
    // Down then Enter -> confirm resolves "No" -> the run closure fails.
    let (input, _queue) = scripted(vec!["\x1b[B", "\r"]);
    let host = TuiExtensionUi::new(&mut tui, dark_theme(), keybindings(), input);
    let ctx = HostCtx { ui: &host };

    let outcome = show_llama_ui(&ctx, |view: Rc<LlamaView>| async move {
        if view.confirm("Unload model?", "Unload owner/model?").await {
            Ok(())
        } else {
            Err("could not unload owner/model".to_string())
        }
    });

    match outcome {
        Err(UiError::Failed(message)) => {
            assert_eq!(message, "could not unload owner/model");
        }
        other => panic!("expected UiError::Failed, got {other:?}"),
    }
    assert!(
        !host.has_overlay(),
        "the overlay is unmounted on the error path"
    );
    let notes = host.notifications();
    assert_eq!(notes.len(), 1, "the error is notified exactly once");
    assert_eq!(notes[0].message, "could not unload owner/model");
    assert_eq!(notes[0].level, NotifyLevel::Error);
}

// --- Unavailable in non-TUI (no ui surface) ---------------------------------

#[test]
fn custom_is_unavailable_without_an_interactive_surface() {
    let ctx = BareCtx; // ui() -> the defaulted no-op surface
    let outcome = show_llama_ui(&ctx, |_view: Rc<LlamaView>| async move { Ok(()) });
    assert!(
        matches!(outcome, Err(UiError::Unavailable)),
        "the no-op surface reports Unavailable (pi's non-tui guard)"
    );
}

// --- generic mount over a hand-built CustomMount ----------------------------

/// Proves the host is view-agnostic: a factory that builds its own component +
/// input closure + `run` future mounts and drives identically. The `run` future
/// completes once the input closure has fired twice.
#[test]
fn custom_drives_a_generic_mount_to_completion() {
    let mut tui = Tui::new(LoggingTerminal::new(80, 24), false);
    let (input, _queue) = scripted(vec!["a", "b"]);
    let host = TuiExtensionUi::new(&mut tui, dark_theme(), keybindings(), input);
    let ctx = HostCtx { ui: &host };

    let seen = Rc::new(RefCell::new(Vec::<String>::new()));

    let outcome = {
        let seen_for_run = Rc::clone(&seen);
        ctx.ui().custom(Box::new(move |host: &dyn CustomHost| {
            let seen_for_input = Rc::clone(&seen_for_run);
            host.set_input_handler(Rc::new(move |data: &str| {
                seen_for_input.borrow_mut().push(data.to_string());
            }));
            let component: Rc<dyn Component> = Rc::new(Text::new("custom overlay", 1, 0, None));
            let run: Pin<Box<dyn std::future::Future<Output = Result<(), String>>>> =
                Box::pin(CountingFuture {
                    seen: seen_for_run,
                    needed: 2,
                });
            CustomMount { component, run }
        }))
    };

    assert!(matches!(outcome, Ok(())));
    assert_eq!(*seen.borrow(), vec!["a".to_string(), "b".to_string()]);
    assert!(!host.has_overlay());
}

/// A future that resolves once `seen` has accumulated `needed` inputs.
struct CountingFuture {
    seen: Rc<RefCell<Vec<String>>>,
    needed: usize,
}
impl std::future::Future for CountingFuture {
    type Output = Result<(), String>;
    fn poll(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if self.seen.borrow().len() >= self.needed {
            std::task::Poll::Ready(Ok(()))
        } else {
            std::task::Poll::Pending
        }
    }
}
