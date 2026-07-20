//! The interactive-shell [`ExtensionUi`] host — the TUI side of pi's `ctx.ui`
//! (`packages/coding-agent/src/modes/interactive/interactive-mode.ts`, the
//! `custom` / `notify` members of the `ExtensionUi` object the interactive mode
//! passes into command contexts).
//!
//! [`TuiExtensionUi`] implements the narrowed [`ExtensionUi`] surface
//! ([`custom`](ExtensionUi::custom) + [`notify`](ExtensionUi::notify)) over a live
//! [`Tui`], and is itself the [`CustomHost`] a [`CustomFactory`] reads while
//! building its view.
//!
//! ## Driving `custom` on the synchronous shell
//!
//! pi's `ctx.ui.custom(factory)` mounts the returned component as a focused
//! overlay and lets the Node event loop deliver input to it while the view's
//! `run` promise settles. pidgin's shell is fully synchronous with no ambient
//! runtime, so [`custom`](TuiExtensionUi::custom) drives the mount itself:
//!
//! 1. Call the factory to build the [`CustomMount`] (the render `component` plus
//!    the `run` future), capturing the view's input closure the factory registers
//!    via [`CustomHost::set_input_handler`].
//! 2. Register the mount as a focused overlay ([`Tui::show_overlay`]).
//! 3. Drive `run` to completion by **manual poll** (a no-op waker, the same
//!    `Stepper` idiom the llama behavioural tests use), interleaved with pumping
//!    one decoded input chunk per idle iteration into the overlay via
//!    [`Tui::handle_input`] — which the overlay focus dispatch routes to the
//!    mount's input closure, i.e. the view's `handle_input`. The synchronous
//!    llama client's blocking calls execute inside a `run` poll; only the
//!    input-driven dialogs pend, and each resolves when its keypress arrives.
//! 4. Unmount ([`Tui::overlay_hide`]) and map the `run` outcome: `Ok(())` →
//!    `Ok(())`; `Err(msg)` → `notify(msg, Error)` + [`UiError::Failed`] (pi's
//!    `run(view).then(done, err => { notify(err); done() })` error branch).
//!
//! The input source is injected ([`InputSource`]) so the host runs headless in
//! tests over a scripted chunk sequence and live over the shell's decoded-input
//! pump.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use pidgin_tui::keybindings::KeybindingsManager;
use pidgin_tui::renderer::Component;
use pidgin_tui::{OverlayOptions, SizeValue, Terminal, Tui};

use crate::core::extensions::types::{
    CustomFactory, CustomHost, ExtensionUi, NotifyLevel, UiError,
};
use crate::modes::interactive::theme::Theme;

/// A transient notification recorded by [`ExtensionUi::notify`] (pi's
/// `ctx.ui.notify(message, level)`).
///
/// The interactive shell renders these in its notification region; here they are
/// also retained so a caller (and the tests) can observe the notify stream, and
/// forwarded to an optional live sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// The notification text.
    pub message: String,
    /// The notification severity.
    pub level: NotifyLevel,
}

/// Yields the next decoded input chunk to pump into a mounted custom view, or
/// `None` when the input stream closes before the view completes. Each chunk is a
/// terminal-input string as delivered to [`Tui::handle_input`] (the shell's
/// decoded-input pump in production; a scripted sequence in tests).
pub type InputSource<'a> = Box<dyn FnMut() -> Option<String> + 'a>;

/// An optional live notification sink (the shell's notification region). Called
/// for each [`ExtensionUi::notify`] in addition to the retained history.
pub type NotifySink<'a> = Box<dyn FnMut(&Notification) + 'a>;

/// A mounted view's interior-mutable keyboard-input closure (pi delivers input to
/// the mounted component's `handleInput`; the shared `Rc<dyn Component>` cannot,
/// so the factory registers this `&self` closure via
/// [`CustomHost::set_input_handler`]).
type InputHandler = Rc<dyn Fn(&str)>;

/// The render + input overlay adapter over a [`CustomMount`]: renders the mount's
/// shared `component` and routes overlay-dispatched input to the view's input
/// closure.
///
/// [`CustomMount::component`](crate::core::extensions::types::CustomMount::component)
/// is a shared `Rc<dyn Component>` (so [`Component::handle_input`]'s `&mut self`
/// cannot reach it); the interior-mutable view therefore registers a `&self`
/// input closure via [`CustomHost::set_input_handler`], which this adapter invokes
/// from its `&mut self` `handle_input`.
struct CustomOverlayComponent {
    render: Rc<dyn Component>,
    input: Option<InputHandler>,
}

impl Component for CustomOverlayComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.render.render(width)
    }
    fn handle_input(&mut self, data: &str) {
        if let Some(input) = &self.input {
            input(data);
        }
    }
}

/// The mutable half of [`TuiExtensionUi`]: the live [`Tui`] and the injected
/// input source, borrowed for the duration of a [`custom`](TuiExtensionUi::custom)
/// drive.
struct Driver<'a, T: Terminal> {
    tui: &'a mut Tui<T>,
    input: InputSource<'a>,
}

/// The interactive-shell [`ExtensionUi`] host and [`CustomHost`].
///
/// Holds the live [`Tui`] (behind a [`RefCell`] so the `&self` trait methods can
/// drive it), the active theme + keybindings a factory reads, the shared
/// render-dirty flag ([`CustomHost::request_render`]), the slot the factory writes
/// its input closure to ([`CustomHost::set_input_handler`]), and the notification
/// history + optional live sink.
pub struct TuiExtensionUi<'a, T: Terminal> {
    driver: RefCell<Driver<'a, T>>,
    theme: Theme,
    keybindings: KeybindingsManager,
    /// pi's `tui.requestRender()`: set by [`CustomHost::request_render`] and by
    /// each input pump; the drive loop coalesces it into one repaint per idle
    /// iteration.
    dirty: Cell<bool>,
    /// The input closure the current factory registers (drained by `custom`).
    pending_input: RefCell<Option<InputHandler>>,
    notifications: RefCell<Vec<Notification>>,
    notify_sink: RefCell<Option<NotifySink<'a>>>,
}

impl<'a, T: Terminal> TuiExtensionUi<'a, T> {
    /// Build the host over a live [`Tui`], the active `theme` + `keybindings`, and
    /// the injected `input` source.
    pub fn new(
        tui: &'a mut Tui<T>,
        theme: Theme,
        keybindings: KeybindingsManager,
        input: InputSource<'a>,
    ) -> Self {
        Self {
            driver: RefCell::new(Driver { tui, input }),
            theme,
            keybindings,
            dirty: Cell::new(false),
            pending_input: RefCell::new(None),
            notifications: RefCell::new(Vec::new()),
            notify_sink: RefCell::new(None),
        }
    }

    /// Install a live notification sink (the shell's notification region). Each
    /// [`ExtensionUi::notify`] is forwarded here in addition to being retained.
    pub fn set_notify_sink(&self, sink: NotifySink<'a>) {
        *self.notify_sink.borrow_mut() = Some(sink);
    }

    /// The notifications recorded so far (test/introspection helper).
    pub fn notifications(&self) -> Vec<Notification> {
        self.notifications.borrow().clone()
    }

    /// Whether the underlying [`Tui`] currently has a visible overlay
    /// (test/introspection helper — the host borrows the `Tui`, so callers query
    /// it through here rather than the borrowed handle).
    pub fn has_overlay(&self) -> bool {
        self.driver.borrow().tui.has_overlay()
    }

    /// The overlay geometry for a mounted custom view: centered, at most 80
    /// columns wide, a comfortable box over the base chat. Mirrors pi's default
    /// custom-overlay sizing closely enough for the model-manager frame.
    fn overlay_options() -> OverlayOptions {
        OverlayOptions {
            width: Some(SizeValue::Pct(80.0)),
            max_height: Some(SizeValue::Pct(80.0)),
            ..OverlayOptions::default()
        }
    }

    /// Repaint if a `requestRender` is pending, clearing the flag.
    fn repaint_if_dirty(driver: &mut Driver<'a, T>, dirty: &Cell<bool>) {
        if dirty.replace(false) {
            driver.tui.request_render(true);
            let _ = driver.tui.flush();
        }
    }
}

impl<T: Terminal> CustomHost for TuiExtensionUi<'_, T> {
    fn theme(&self) -> &Theme {
        &self.theme
    }
    fn keybindings(&self) -> &KeybindingsManager {
        &self.keybindings
    }
    fn request_render(&self) {
        self.dirty.set(true);
    }
    fn set_input_handler(&self, handler: InputHandler) {
        *self.pending_input.borrow_mut() = Some(handler);
    }
}

impl<T: Terminal> ExtensionUi for TuiExtensionUi<'_, T> {
    fn custom(&self, factory: CustomFactory<'_>) -> Result<(), UiError> {
        // Fresh input slot for this mount (the factory registers the view's input
        // closure into `pending_input` via `set_input_handler`).
        self.pending_input.borrow_mut().take();
        let mount = factory(self);
        let input = self.pending_input.borrow_mut().take();
        let adapter: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(CustomOverlayComponent {
            render: mount.component,
            input,
        }));
        let mut run = mount.run;

        let outcome = {
            let mut driver = self.driver.borrow_mut();
            let component = driver.tui.register_component(adapter);
            let handle = driver.tui.show_overlay(component, Self::overlay_options());
            self.dirty.set(true);
            Self::repaint_if_dirty(&mut driver, &self.dirty);

            let waker = Waker::noop();
            let result = loop {
                let mut cx = Context::from_waker(waker);
                if let Poll::Ready(result) = run.as_mut().poll(&mut cx) {
                    break result;
                }
                // The view repaints after the driving poll (blocking client work
                // and dialog transitions have run); coalesce any `requestRender`.
                self.dirty.set(true);
                Self::repaint_if_dirty(&mut driver, &self.dirty);
                match (driver.input)() {
                    Some(chunk) => driver.tui.handle_input(&chunk),
                    None => {
                        break Err(
                            "interactive input stream closed before the view completed".to_string()
                        )
                    }
                }
            };

            driver.tui.overlay_hide(handle);
            self.dirty.set(true);
            Self::repaint_if_dirty(&mut driver, &self.dirty);
            result
        };

        match outcome {
            Ok(()) => Ok(()),
            Err(message) => {
                // pi's error branch: notify then complete.
                self.notify(&message, NotifyLevel::Error);
                Err(UiError::Failed(message))
            }
        }
    }

    fn notify(&self, message: &str, level: NotifyLevel) {
        let notification = Notification {
            message: message.to_string(),
            level,
        };
        if let Some(sink) = self.notify_sink.borrow_mut().as_mut() {
            sink(&notification);
        }
        self.notifications.borrow_mut().push(notification);
    }
}
