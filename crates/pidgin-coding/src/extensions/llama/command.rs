//! The `/llama` command run-driver, ported from pi's `llamaExtension` command
//! handler (`packages/coding-agent/src/extensions/llama/index.ts:174-220`) and
//! its `syncCatalog` / `unloadModel` / `loadModel` / `downloadModel` helpers.
//!
//! [`run_llama_command`] is the Rust analog of pi's
//! ```ts
//! pi.registerCommand("llama", { handler: async (_args, ctx) => {
//!   if (ctx.mode !== "tui") { ctx.ui.notify("/llama is available in interactive mode", "warning"); return; }
//!   const client = await configuredClient(ctx);
//!   if (!client) return;
//!   await showLlamaUi(ctx, async (ui) => { /* readCatalog -> showModels loop */ });
//! }});
//! ```
//! It mounts the [`LlamaView`] via [`show_llama_ui`] (the widened
//! [`ExtensionContext::ui`] mount seam) and drives the model-manager loop: read
//! the catalog (retrying past connection errors), show the model list, and
//! dispatch the chosen action (download / load / unload / already-in-progress).
//!
//! # Faithfulness scope
//!
//! * The **catalog read**, the **model-manager loop**, the **close** exit, the
//!   **unload** action, and the **in-progress** notice are ported end-to-end over
//!   the synchronous Rust [`LlamaClient`].
//! * **`configuredClient`** (provider-auth lookup) is *not* reconstructed here:
//!   pi reads `ctx.modelRegistry.getProviderAuth(...)`, and the Rust
//!   [`ExtensionContext`] does not yet carry `modelRegistry`. The client (and the
//!   provider controller for `syncCatalog`) are therefore **injected** by the
//!   caller; wiring them from `ctx` lands with the `modelRegistry` seam.
//! * **`syncCatalog`** sets the provider catalog but omits pi's
//!   `ctx.modelRegistry.refresh()` (same missing seam).
//! * **`load` / `download`** need the llama SSE event-stream transport, which the
//!   [`LlamaClient`] itself marks deferred (`load_and_wait` /
//!   `download_and_wait` take a `&mut dyn LlamaEventStream`). Those two actions
//!   are dispatched to [`notify`] deferral notices here and land with that
//!   transport seam.
//! * **Mid-loop `ctx.ui.notify`** is threaded through an injected `notify` sink:
//!   the mount seam's `run` future is `'static` (it cannot borrow `ctx`), so the
//!   host wires an owned notification sink for the loop's informational messages
//!   (the fatal-error path still flows through the mount's `Err` → `notify`).

use std::rc::Rc;

use crate::core::extensions::types::{ExtensionContext, NotifyLevel, UiError};

use super::client::{LlamaClient, LlamaListOptions, LlamaModelInfo, LlamaModelStatus};
use super::mount::show_llama_ui;
use super::provider::LlamaProviderController;
use super::ui::{ConnectionErrorChoice, LlamaManagerAction, LlamaUi, LlamaView};

/// An owned notification sink for the loop's informational `ctx.ui.notify` calls
/// (see the module note on why the `'static` `run` future needs an owned sink).
pub type NotifyFn = Rc<dyn Fn(&str, NotifyLevel)>;

/// The lowercase status literal pi reads off `model.status.value` — taken
/// straight from the enum's serde `rename_all = "lowercase"` wire form so the
/// mapping has a single source of truth.
fn status_label(status: LlamaModelStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// `modelIsLoaded(model)` — loaded or sleeping (`index.ts:7`).
fn model_is_loaded(model: &LlamaModelInfo) -> bool {
    matches!(
        model.status.value,
        LlamaModelStatus::Loaded | LlamaModelStatus::Sleeping
    )
}

/// `isConnectionError(error)` (`index.ts:11`): a fetch/timeout/network failure.
fn is_connection_error(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("fetch failed") || message.contains("timeout") || message.contains("network")
}

/// `connectionErrorMessage(error)` (`index.ts:16`).
fn connection_error_message(message: &str) -> String {
    if is_connection_error(message) {
        "Could not connect to the server.".to_string()
    } else {
        message.to_string()
    }
}

/// `syncCatalog(ctx, client, catalog?)` (`index.ts:45`): push the catalog to the
/// provider and return it.
///
/// Omits pi's `ctx.modelRegistry.refresh()` — the Rust [`ExtensionContext`] does
/// not carry `modelRegistry` yet (see the module note).
fn sync_catalog(
    provider: &LlamaProviderController,
    client: &LlamaClient,
    catalog: Option<Vec<LlamaModelInfo>>,
) -> Result<Vec<LlamaModelInfo>, String> {
    let current = match catalog {
        Some(catalog) => catalog,
        None => client
            .list(LlamaListOptions {
                reload: false,
                signal: None,
            })
            .map_err(|error| error.to_string())?,
    };
    provider
        .set_catalog(&current, &client.server_url)
        .map_err(|error| error.to_string())?;
    Ok(current)
}

/// `unloadModel(ctx, ui, client, model)` (`index.ts:117`): confirm, unload, and
/// resync the catalog.
async fn unload_model(
    view: &LlamaView,
    provider: &LlamaProviderController,
    client: &LlamaClient,
    notify: &NotifyFn,
    model: &LlamaModelInfo,
) -> Result<(), String> {
    if !view
        .confirm("Unload model?", &format!("Unload {}?", model.id))
        .await
    {
        return Ok(());
    }
    client
        .unload_and_wait(&model.id, None)
        .map_err(|error| error.to_string())?;
    sync_catalog(provider, client, None)?;
    notify(&format!("Unloaded {}", model.id), NotifyLevel::Info);
    Ok(())
}

/// Run the `/llama` model-manager loop against an injected `client` + `provider`.
///
/// Mounts the [`LlamaView`] via [`show_llama_ui`] and drives pi's `index.ts`
/// run-driver loop. Returns [`UiError::Unavailable`] when no interactive surface
/// is mounted (pi's `ctx.mode !== "tui"` guard) — the caller reports it — and
/// [`UiError::Failed`] when the mounted view's `run` fails.
pub fn run_llama_command<C: ExtensionContext>(
    ctx: &C,
    client: Rc<LlamaClient>,
    provider: Rc<LlamaProviderController>,
    notify: NotifyFn,
) -> Result<(), UiError> {
    show_llama_ui(ctx, move |view: Rc<LlamaView>| async move {
        // `readCatalog` (`index.ts:177`): sync the catalog, retrying past
        // connection errors via the connection-error dialog until it succeeds or
        // the user closes.
        let read_catalog = |view: Rc<LlamaView>,
                            client: Rc<LlamaClient>,
                            provider: Rc<LlamaProviderController>| async move {
            loop {
                match sync_catalog(&provider, &client, None) {
                    Ok(catalog) => return Ok::<Option<Vec<LlamaModelInfo>>, String>(Some(catalog)),
                    Err(error) => {
                        let choice = view
                            .connection_error(&client.server_url, &connection_error_message(&error))
                            .await;
                        if choice == ConnectionErrorChoice::Close {
                            return Ok(None);
                        }
                    }
                }
            }
        };

        let Some(mut catalog) =
            read_catalog(Rc::clone(&view), Rc::clone(&client), Rc::clone(&provider)).await?
        else {
            return Ok(());
        };

        loop {
            let action = view.show_models(&client.server_url, catalog.clone()).await;
            let action_error: Option<String> = match action {
                LlamaManagerAction::Close => return Ok(()),
                LlamaManagerAction::Download => {
                    // `downloadModel` (`index.ts:130`): needs the Hugging Face
                    // search box + `runWithProgress` over the SSE download stream
                    // (deferred transport seam — see the module note).
                    notify(
                        "Model download from /llama needs the llama SSE event-stream seam (deferred)",
                        NotifyLevel::Warning,
                    );
                    None
                }
                LlamaManagerAction::Model(model) if model_is_loaded(&model) => {
                    unload_model(&view, &provider, &client, &notify, &model)
                        .await
                        .err()
                }
                LlamaManagerAction::Model(model)
                    if model.status.value == LlamaModelStatus::Unloaded =>
                {
                    // `loadModel` (`index.ts:56`): the replace-select dialog +
                    // `runWithProgress` over the SSE load stream (deferred
                    // transport seam — see the module note).
                    notify(
                        "Model load from /llama needs the llama SSE event-stream seam (deferred)",
                        NotifyLevel::Warning,
                    );
                    None
                }
                LlamaManagerAction::Model(model) => {
                    notify(
                        &format!("{} is {}", model.id, status_label(model.status.value)),
                        NotifyLevel::Warning,
                    );
                    None
                }
            };

            // `const refreshed = await readCatalog(); if (!refreshed) return;`
            let Some(refreshed) =
                read_catalog(Rc::clone(&view), Rc::clone(&client), Rc::clone(&provider)).await?
            else {
                return Ok(());
            };
            catalog = refreshed;

            // Non-connection action errors are surfaced and the loop continues
            // (`index.ts:215`).
            if let Some(error) = action_error {
                if !is_connection_error(&error) {
                    notify(&error, NotifyLevel::Error);
                }
            }
        }
    })
}
