//! The llama `showLlamaUi` mount seam (pi's `showLlamaUi`,
//! `packages/coding-agent/src/extensions/llama/ui.ts:480`).
//!
//! Split out of [`ui`](super::ui) (which stays under the file-size budget): the
//! function that lowers pi's `ctx.ui.custom<void>((tui, theme, keybindings, done)
//! => …)` mount onto the widened [`ExtensionContext::ui`] seam.

use std::future::Future;
use std::rc::Rc;

use pidgin_tui::renderer::Component;

use crate::core::extensions::types::{
    CustomFactory, CustomHost, CustomMount, ExtensionContext, UiError,
};

use super::ui::LlamaView;

/// `showLlamaUi(ctx, run)` — mount the [`LlamaView`] as a detached custom overlay
/// and drive `run(view)` to completion (pi's `showLlamaUi`,
/// `extensions/llama/ui.ts:480`).
///
/// Ports pi's
/// ```ts
/// await ctx.ui.custom<void>((tui, theme, keybindings, done) => {
///   const view = new LlamaView(tui, theme, keybindings);
///   void run(view).then(() => done(), (error) => { ctx.ui.notify(error, "error"); done(); });
///   return view;
/// });
/// ```
/// onto the widened [`ExtensionContext::ui`] seam: the [`CustomFactory`] builds
/// the [`LlamaView`] from the [`CustomHost`], registers the view's
/// interior-mutable input closure (which also drives the pending Hugging Face
/// search — pi's debounce timer — whose fetch is synchronous in the Rust client),
/// and returns a [`CustomMount`] whose `run` future is `run(view)`. The host
/// mounts the view as a focused overlay, drives `run` to completion, unmounts, and
/// maps a `run` error to `notify(msg, Error)` + [`UiError::Failed`] (pi's error
/// branch). With no interactive surface mounted the default no-op
/// [`ExtensionUi`](crate::core::extensions::types::ExtensionUi) returns
/// [`UiError::Unavailable`] (pi's `ctx.mode !== "tui"` guard, surfaced to the
/// caller as an error).
pub fn show_llama_ui<C, R, Fut>(ctx: &C, run: R) -> Result<(), UiError>
where
    C: ExtensionContext,
    R: FnOnce(Rc<LlamaView>) -> Fut + 'static,
    Fut: Future<Output = Result<(), String>> + 'static,
{
    let factory: CustomFactory = Box::new(move |host: &dyn CustomHost| {
        let view = Rc::new(LlamaView::new(
            host.theme().clone(),
            host.keybindings().clone(),
        ));
        // pi mounts the view focused; the dialog widgets accept input only when
        // focused (`Focusable.focused`).
        view.set_focused(true);
        // pi delivers keyboard input straight to the mounted component's
        // `handleInput`. The Rust `component` is a shared `Rc<dyn Component>` used
        // for rendering, so register the view's `&self` input closure here; it
        // also drives the pending Hugging Face search (pi's `setTimeout(runSearch)`
        // debounce body), whose fetch is synchronous in the Rust client.
        let input_view = Rc::clone(&view);
        host.set_input_handler(Rc::new(move |data: &str| {
            input_view.handle_input(data);
            drive_pending_search(&input_view);
        }));
        host.request_render();
        let run_view = Rc::clone(&view);
        CustomMount {
            component: view as Rc<dyn Component>,
            run: Box::pin(run(run_view)),
        }
    });
    ctx.ui().custom(factory)
}

/// Drive the view's pending Hugging Face fetch to completion (pi's `runSearch`
/// debounce body). [`LlamaView::run_pending_search`] is a no-op unless the last
/// input scheduled a query; when it did, the underlying Hugging Face transport is
/// synchronous, so the future resolves in a single manual poll. If it were to
/// pend (an async transport), the scheduled query is retried on the next input.
fn drive_pending_search(view: &Rc<LlamaView>) {
    use std::task::{Context, Poll, Waker};
    let mut fut = Box::pin(view.run_pending_search());
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(()) | Poll::Pending => {}
    }
}
