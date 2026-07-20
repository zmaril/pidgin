// straitjacket-allow-file:duplication — the `load()` vector-reading helper and
// the per-surface replay loops mirror the two-line boilerplate the other
// interactive vector-test binaries use (interactive_message_vectors.rs etc.);
// each integration-test binary is standalone.
//! Drives the Rust port of pi's llama-extension TUI against vectors extracted
//! from pi itself (`crates/pidgin-coding/vectors/gen/generate_llama_ui.mjs`).
//! Every assertion is byte-identical: pi's render output is the source of truth.
//! The theme is baked at 256-color to match the generator.
//!
//! Each llama-UI surface is reached through [`LlamaView`]'s real public API
//! (`show_models`, `select`, `confirm`, `connection_error`, `search_models` +
//! keyboard input + `run_pending_search`, `show_status`, `progress`), so the
//! render output exercises the actual port, not a reconstruction.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use serde::Deserialize;

use pidgin_ai::seams::AbortSignal;
use pidgin_coding::extensions::llama::{
    HuggingFaceModel, LlamaMeta, LlamaModelInfo, LlamaModelStatus, LlamaModelStatusInfo, LlamaUi,
    LlamaView, ProgressState, SearchFn,
};
use pidgin_coding::modes::interactive::components::{
    format_key_text, key_hint, key_text, raw_key_hint, DynamicBorder,
};
use pidgin_coding::modes::interactive::theme::{create_theme, parse_theme_json, ColorMode, Theme};
use pidgin_tui::keybindings::{tui_keybindings, KeybindingsManager};
use pidgin_tui::renderer::Component;

fn load<T: serde::de::DeserializeOwned>(name: &str) -> Vec<T> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join(format!("{name}.json"));
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

/// The runtime `dark` theme at 256-color — the same theme the generator loads.
fn dark_theme() -> Theme {
    let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("modes")
        .join("interactive")
        .join("theme")
        .join("dark.json");
    let content = std::fs::read_to_string(&json_path).expect("read dark.json");
    let theme_json = parse_theme_json(&content).expect("parse dark.json");
    create_theme(&theme_json, Some(ColorMode::Color256), None).expect("create dark theme")
}

fn keybindings() -> KeybindingsManager {
    KeybindingsManager::new(tui_keybindings(), Vec::new())
}

fn view() -> LlamaView {
    LlamaView::new(dark_theme(), keybindings())
}

/// Poll `fut` once with a no-op waker: enough to advance a `LlamaView` dialog
/// future to its first `.await` (which is after it has installed its screen), or
/// to completion for a future backed by ready sub-futures (a ready mock search).
fn poll_once<F: Future>(fut: F) -> Option<F::Output> {
    let mut fut = Box::pin(fut);
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

// --- DynamicBorder ----------------------------------------------------------

#[derive(Deserialize)]
struct BorderVec {
    label: String,
    width: usize,
    expected: Vec<String>,
}

#[test]
fn llama_dynamic_border_vectors() {
    let theme = dark_theme();
    let ansi = theme.get_fg_ansi("accent").unwrap();
    let vectors: Vec<BorderVec> = load("llama_dynamic_border");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let ansi = ansi.clone();
        let border = DynamicBorder::new(Box::new(move |t: &str| format!("{ansi}{t}\x1b[39m")));
        assert_eq!(
            border.render(v.width),
            v.expected,
            "DynamicBorder[{}] width={}",
            v.label,
            v.width
        );
    }
}

// --- keybinding hints -------------------------------------------------------

#[derive(Deserialize)]
struct HintVec {
    kind: String,
    binding: Option<String>,
    key: Option<String>,
    description: String,
    expected: String,
}

#[test]
fn llama_keybinding_hint_vectors() {
    let theme = dark_theme();
    let kb = keybindings();
    let vectors: Vec<HintVec> = load("llama_keybinding_hints");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let actual = match v.kind.as_str() {
            "keyHint" => key_hint(&theme, &kb, v.binding.as_ref().unwrap(), &v.description),
            "keyText" => key_text(&kb, v.binding.as_ref().unwrap()),
            "rawKeyHint" => raw_key_hint(&theme, v.key.as_ref().unwrap(), &v.description),
            "formatKeyText" => format_key_text(v.key.as_ref().unwrap(), false),
            other => panic!("unknown hint kind {other}"),
        };
        assert_eq!(actual, v.expected, "hint[{}]", v.kind);
    }
}

// --- llama UI surfaces ------------------------------------------------------

#[derive(Deserialize)]
struct UiVec {
    label: String,
    width: usize,
    expected: Vec<String>,
}

fn model(
    id: &str,
    status: LlamaModelStatus,
    args: Option<&[&str]>,
    meta: Option<LlamaMeta>,
) -> LlamaModelInfo {
    LlamaModelInfo {
        id: id.to_string(),
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

fn models() -> Vec<LlamaModelInfo> {
    vec![
        model(
            "unsloth/Qwen3-4B-GGUF",
            LlamaModelStatus::Loaded,
            Some(&["--ctx-size", "8192"]),
            Some(LlamaMeta {
                n_ctx: Some(8192.0),
                n_ctx_train: Some(32768.0),
                size: None,
                ftype: None,
            }),
        ),
        model(
            "bartowski/Llama-3.2-3B-Instruct-GGUF",
            LlamaModelStatus::Unloaded,
            None,
            None,
        ),
        model(
            "TheBloke/Mistral-7B-Instruct-v0.2-GGUF",
            LlamaModelStatus::Downloading,
            None,
            None,
        ),
        model(
            "ggml-org/gemma-3-1b",
            LlamaModelStatus::Sleeping,
            None,
            Some(LlamaMeta {
                n_ctx: None,
                n_ctx_train: Some(131072.0),
                size: None,
                ftype: None,
            }),
        ),
    ]
}

fn hf(id: &str, downloads: u64) -> HuggingFaceModel {
    HuggingFaceModel {
        id: id.to_string(),
        downloads,
    }
}

fn search_results() -> Vec<HuggingFaceModel> {
    vec![
        hf("unsloth/Qwen3-4B-GGUF", 1_534_221),
        hf("bartowski/Qwen2.5-Coder-7B-GGUF", 88_400),
        hf("TheBloke/CodeLlama-13B-GGUF", 2_100),
        hf("ggml-org/tinyllama", 512),
    ]
}

fn scroll_results() -> Vec<HuggingFaceModel> {
    (0..15)
        .map(|i| hf(&format!("ggml-org/model-{i}-GGUF"), 1000 + i * 111))
        .collect()
}

/// A ready (non-pending) mock search: returns `[]` for `zzzznope`, the scroll set
/// for `ml`, else the standard result set — regardless of the abort signal.
fn mock_search() -> SearchFn {
    Rc::new(|query: String, _signal: AbortSignal| {
        let results = if query == "zzzznope" {
            Vec::new()
        } else if query == "ml" {
            scroll_results()
        } else {
            search_results()
        };
        let fut: Pin<Box<dyn Future<Output = Result<Vec<HuggingFaceModel>, String>>>> =
            Box::pin(async move { Ok(results) });
        fut
    })
}

fn type_query(view: &LlamaView, query: &str) {
    for ch in query.chars() {
        view.handle_input(&ch.to_string());
    }
}

#[test]
fn llama_ui_vectors() {
    let vectors: Vec<UiVec> = load("llama_ui");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let (surface, _) = v.label.split_once("/w=").expect("label surface/w=width");
        let view = view();
        // Focus is set before setup so the search input's cursor state matches
        // (`search_models` reads the view's focus when mounting the widget).
        view.set_focused(surface != "search-unfocused");
        setup_into(&view, surface);
        assert_eq!(view.render(v.width), v.expected, "surface[{}]", v.label);
    }
}

/// Apply the `surface` setup (the label before `/w=`) onto a `view` whose focus
/// has already been set.
fn setup_into(view: &LlamaView, surface: &str) {
    match surface {
        "loading" => {}
        "models" => {
            poll_once(view.show_models("http://127.0.0.1:8080", models()));
        }
        "models-empty" => {
            poll_once(view.show_models("http://127.0.0.1:8080", Vec::new()));
        }
        "select" => {
            poll_once(view.select(
                "Choose an action",
                vec!["Load".into(), "Unload".into(), "Remove".into()],
            ));
        }
        "confirm" => {
            poll_once(view.confirm(
                "Stop download?",
                "Cancel the in-progress download and discard the partial weights?",
            ));
        }
        "connection-error" => {
            poll_once(view.connection_error(
                "http://127.0.0.1:8080",
                "Connection refused after 3 retries.",
            ));
        }
        "status" => {
            view.show_status("Working", "Contacting llama.cpp router\u{2026}");
        }
        "progress-none" => {
            poll_once(view.progress(ProgressState {
                title: "Downloading".into(),
                model: "unsloth/Qwen3-4B-GGUF".into(),
                message: "Starting\u{2026}".into(),
                ratio: None,
                detail: None,
            }));
        }
        "search-empty" => {
            poll_once(view.search_models(mock_search()));
        }
        "search-searching" => {
            poll_once(view.search_models(mock_search()));
            type_query(view, "qwen");
        }
        "search-results" => {
            poll_once(view.search_models(mock_search()));
            type_query(view, "qwen");
            poll_once(view.run_pending_search());
        }
        "search-results-sel1" | "search-unfocused" => {
            poll_once(view.search_models(mock_search()));
            type_query(view, "qwen");
            poll_once(view.run_pending_search());
            view.handle_input("\x1b[B");
        }
        "search-no-models" => {
            poll_once(view.search_models(mock_search()));
            type_query(view, "zzzznope");
            poll_once(view.run_pending_search());
        }
        "search-scroll" => {
            poll_once(view.search_models(mock_search()));
            type_query(view, "ml");
            poll_once(view.run_pending_search());
        }
        _ if surface.starts_with("progress-") => {
            let ratio: f64 = surface["progress-".len()..].parse().expect("ratio");
            poll_once(view.progress(ProgressState {
                title: "Downloading".into(),
                model: "unsloth/Qwen3-4B-GGUF".into(),
                message: "Fetching weights".into(),
                ratio: Some(ratio),
                detail: Some("1.00 GiB / 2.00 GiB".into()),
            }));
        }
        other => panic!("unknown surface {other}"),
    }
}
