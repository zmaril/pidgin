//! Drives the Rust port of pi's TUI leaf widgets (spacer, text, truncated-text,
//! box, loader, image) against vectors extracted from pi itself
//! (`crates/pidgin-tui/vectors/gen/generate_widgets.mjs`). Every assertion is
//! byte-identical: pi's `render(width)` output is the source of truth.

use std::path::PathBuf;

use serde::Deserialize;

use pidgin_tui::renderer::Component;
use pidgin_tui::terminal_image::{
    get_image_dimensions, set_capabilities, set_cell_dimensions, CellDimensions, ImageDimensions,
    ImageProtocol, TerminalCapabilities,
};
use pidgin_tui::widgets::box_widget::BoxWidget;
use pidgin_tui::widgets::image::{Image, ImageOptions, ImageTheme};
use pidgin_tui::widgets::loader::{Loader, LoaderIndicatorOptions};
use pidgin_tui::widgets::spacer::Spacer;
use pidgin_tui::widgets::text::{BgFn, Text};
use pidgin_tui::widgets::truncated_text::TruncatedText;

fn load<T: serde::de::DeserializeOwned>(name: &str) -> Vec<T> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join(format!("{name}.json"));
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

// Deterministic style closures matching the generator's `bgFnFor` / `colorFnFor`.
fn bg_fn_for(tag: &str) -> Option<BgFn> {
    match tag {
        "none" => None,
        "redbg" => Some(Box::new(|s: &str| format!("\x1b[41m{s}\x1b[0m"))),
        "bold" => Some(Box::new(|s: &str| format!("\x1b[1m{s}\x1b[0m"))),
        "cyan" => Some(Box::new(|s: &str| format!("\x1b[36m{s}\x1b[0m"))),
        other => panic!("unknown bg tag: {other}"),
    }
}

fn color_fn_for(tag: &str) -> Box<dyn Fn(&str) -> String> {
    match tag {
        "plain" => Box::new(|s: &str| s.to_string()),
        "cyan" => Box::new(|s: &str| format!("\x1b[36m{s}\x1b[0m")),
        "bold" => Box::new(|s: &str| format!("\x1b[1m{s}\x1b[0m")),
        "yellow" => Box::new(|s: &str| format!("\x1b[33m{s}\x1b[0m")),
        other => panic!("unknown color tag: {other}"),
    }
}

// --- spacer ---------------------------------------------------------------

#[derive(Deserialize)]
struct SpacerVec {
    n: usize,
    width: usize,
    expected: Vec<String>,
}

#[test]
fn widget_spacer_vectors() {
    let vectors: Vec<SpacerVec> = load("widget_spacer");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let s = Spacer::new(v.n);
        assert_eq!(
            s.render(v.width),
            v.expected,
            "Spacer({}).render({})",
            v.n,
            v.width
        );
    }
}

// --- text -----------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextVec {
    text: String,
    padding_x: usize,
    padding_y: usize,
    bg_tag: String,
    width: usize,
    expected: Vec<String>,
}

#[test]
fn widget_text_vectors() {
    let vectors: Vec<TextVec> = load("widget_text");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let t = Text::new(&v.text, v.padding_x, v.padding_y, bg_fn_for(&v.bg_tag));
        assert_eq!(
            t.render(v.width),
            v.expected,
            "Text({:?}, px={}, py={}, bg={}).render({})",
            v.text,
            v.padding_x,
            v.padding_y,
            v.bg_tag,
            v.width
        );
    }
}

// --- truncated-text -------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TruncatedVec {
    text: String,
    padding_x: usize,
    padding_y: usize,
    width: usize,
    expected: Vec<String>,
}

#[test]
fn widget_truncated_text_vectors() {
    let vectors: Vec<TruncatedVec> = load("widget_truncated_text");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let t = TruncatedText::new(&v.text, v.padding_x, v.padding_y);
        assert_eq!(
            t.render(v.width),
            v.expected,
            "TruncatedText({:?}, px={}, py={}).render({})",
            v.text,
            v.padding_x,
            v.padding_y,
            v.width
        );
    }
}

#[test]
fn truncated_text_render_matches_component() {
    // The public one-shot wrapper must return exactly what the Component
    // trait's `render` produces for the same construction.
    let vectors: Vec<TruncatedVec> = load("widget_truncated_text");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let direct = TruncatedText::new(&v.text, v.padding_x, v.padding_y).render(v.width);
        let wrapped = pidgin_tui::truncated_text_render(&v.text, v.padding_x, v.padding_y, v.width);
        assert_eq!(
            wrapped, direct,
            "truncated_text_render({:?}, {}, {}, {})",
            v.text, v.padding_x, v.padding_y, v.width
        );
    }
}

// --- box ------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChildSpec {
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    padding_x: usize,
    #[serde(default)]
    padding_y: usize,
    #[serde(default)]
    bg_tag: Option<String>,
    #[serde(default)]
    n: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BoxVec {
    padding_x: usize,
    padding_y: usize,
    bg_tag: String,
    children: Vec<ChildSpec>,
    width: usize,
    expected: Vec<String>,
}

fn build_child(spec: &ChildSpec) -> Box<dyn Component> {
    match spec.kind.as_str() {
        "text" => Box::new(Text::new(
            &spec.text,
            spec.padding_x,
            spec.padding_y,
            bg_fn_for(spec.bg_tag.as_deref().unwrap_or("none")),
        )),
        "truncated" => Box::new(TruncatedText::new(
            &spec.text,
            spec.padding_x,
            spec.padding_y,
        )),
        "spacer" => Box::new(Spacer::new(spec.n)),
        other => panic!("unknown child kind: {other}"),
    }
}

#[test]
fn widget_box_vectors() {
    let vectors: Vec<BoxVec> = load("widget_box");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let mut b = BoxWidget::new(v.padding_x, v.padding_y, bg_fn_for(&v.bg_tag));
        for child in &v.children {
            b.add_child(build_child(child));
        }
        assert_eq!(
            b.render(v.width),
            v.expected,
            "Box(px={}, py={}, bg={}, {} children).render({})",
            v.padding_x,
            v.padding_y,
            v.bg_tag,
            v.children.len(),
            v.width
        );
    }
}

// --- loader ---------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndicatorSpec {
    #[serde(default)]
    frames: Option<Vec<String>>,
    #[serde(default)]
    interval_ms: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoaderVec {
    message: String,
    indicator: Option<IndicatorSpec>,
    ticks: usize,
    set_message_to: Option<String>,
    spinner_tag: String,
    message_tag: String,
    width: usize,
    expected: Vec<String>,
}

#[test]
fn widget_loader_vectors() {
    let vectors: Vec<LoaderVec> = load("widget_loader");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let indicator = v.indicator.as_ref().map(|i| LoaderIndicatorOptions {
            frames: i.frames.clone(),
            interval_ms: i.interval_ms,
        });
        let mut loader = Loader::new(
            color_fn_for(&v.spinner_tag),
            color_fn_for(&v.message_tag),
            &v.message,
            indicator,
        );
        for _ in 0..v.ticks {
            loader.tick();
        }
        if let Some(msg) = &v.set_message_to {
            loader.set_message(msg);
        }
        assert_eq!(
            loader.render(v.width),
            v.expected,
            "Loader({:?}, ticks={}).render({})",
            v.message,
            v.ticks,
            v.width
        );
    }
}

// --- image dimensions -----------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DimSpec {
    width_px: u32,
    height_px: u32,
}

#[derive(Deserialize)]
struct ImageDimVec {
    mime: String,
    data: String,
    dims: Option<DimSpec>,
}

#[test]
fn widget_image_dimensions_vectors() {
    let vectors: Vec<ImageDimVec> = load("widget_image_dimensions");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let got = get_image_dimensions(&v.data, &v.mime);
        match (&got, &v.dims) {
            (Some(g), Some(e)) => {
                assert_eq!(
                    (g.width_px, g.height_px),
                    (e.width_px, e.height_px),
                    "get_image_dimensions({}, {:?})",
                    v.mime,
                    v.data
                );
            }
            (None, None) => {}
            _ => panic!(
                "get_image_dimensions({}, {:?}) presence mismatch: got {:?}",
                v.mime, v.data, got
            ),
        }
    }
}

// --- image render ---------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CellSpec {
    width_px: u32,
    height_px: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OptSpec {
    #[serde(default)]
    image_id: Option<u64>,
    #[serde(default)]
    max_width_cells: Option<u32>,
    #[serde(default)]
    max_height_cells: Option<u32>,
    #[serde(default)]
    filename: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageRenderVec {
    caps: Option<String>,
    cell: CellSpec,
    base64: String,
    mime: String,
    dims: DimSpec,
    options: OptSpec,
    fallback_tag: String,
    width: usize,
    expected: Vec<String>,
}

#[test]
fn widget_image_render_vectors() {
    let vectors: Vec<ImageRenderVec> = load("widget_image_render");
    assert!(!vectors.is_empty());
    for v in &vectors {
        set_cell_dimensions(CellDimensions {
            width_px: v.cell.width_px,
            height_px: v.cell.height_px,
        });
        let images = match v.caps.as_deref() {
            None => None,
            Some("kitty") => Some(ImageProtocol::Kitty),
            Some("iterm2") => Some(ImageProtocol::ITerm2),
            Some(other) => panic!("unknown caps: {other}"),
        };
        set_capabilities(TerminalCapabilities {
            images,
            true_color: true,
            hyperlinks: true,
        });
        let theme = ImageTheme {
            fallback_color: color_fn_for(&v.fallback_tag),
        };
        let options = ImageOptions {
            max_width_cells: v.options.max_width_cells,
            max_height_cells: v.options.max_height_cells,
            filename: v.options.filename.clone(),
            image_id: v.options.image_id,
        };
        let dims = ImageDimensions {
            width_px: v.dims.width_px,
            height_px: v.dims.height_px,
        };
        let img = Image::new(&v.base64, &v.mime, theme, options, Some(dims));
        assert_eq!(
            img.render(v.width),
            v.expected,
            "Image(caps={:?}, mime={}).render({})",
            v.caps,
            v.mime,
            v.width
        );
    }
}
