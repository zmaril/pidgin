//! Interactive-mode message-render components — the conversation content of pi's
//! interactive app shell (Unit 4, PR-4A).
//!
//! Byte-exact ports of pi's
//! `packages/coding-agent/src/modes/interactive/components/{assistant-message,
//! user-message,tool-execution}.ts`. They carry coding-agent semantics (they
//! reference the interactive [`Theme`], tool definitions, and `AssistantMessage`)
//! and *compose* the already-ported `pidgin-tui` primitives (`Markdown`,
//! `Container`, `Box`, `Text`, `Spacer`) — none of those primitives are re-ported
//! here.
//!
//! Correctness is verified the same way the `pidgin-tui` widget ports are: a
//! generator (`crates/pidgin-coding/vectors/gen/generate_interactive_messages.mjs`)
//! drives pi's OWN component classes over representative inputs and dumps
//! `input -> rendered string[]` JSON; the Rust replay
//! (`crates/pidgin-coding/tests/interactive_message_vectors.rs`) constructs the
//! Rust component with the same input, renders at the same width, and asserts
//! byte-identical output.
//!
//! ## Scope note (`ToolExecution` renderer path)
//!
//! pi's `ToolExecutionComponent` delegates a tool's visible framing to the tool
//! definition's `renderCall` / `renderResult` closures. Those UI-render hooks are
//! **deferred** on the Rust `ToolDefinition`
//! (`core/extensions/types.rs` — "UI-render hooks (`renderCall`/`renderResult`)
//! are deferred with the rest of the TUI layer"), so no per-tool renderer is yet
//! available in Rust. [`ToolExecution`] therefore ports the paths that do **not**
//! require those closures — pi's `createCallFallback` / `createResultFallback`
//! and the generic `formatToolExecution` fallback — which is exactly the branch
//! pi itself takes when a tool has no `renderCall` / `renderResult`. The
//! renderer-definition path (the real per-tool visuals for read/bash/edit/…, plus
//! `DiffComponent`) lands when that suite is ported.

pub mod assistant_message;
pub mod dynamic_border;
pub mod footer;
pub mod keybinding_hints;
pub mod status_indicator;
pub mod tool_execution;
pub mod user_message;

pub use assistant_message::AssistantMessage;
pub use dynamic_border::{ColorFn, DynamicBorder};
pub use footer::{FooterComponent, FooterData};
pub use keybinding_hints::{format_key_text, key_display_text, key_hint, key_text, raw_key_hint};
pub use status_indicator::{
    IdleStatus, StatusIndicator, StatusIndicatorKind, WorkingStatusIndicator,
};
pub use tool_execution::{ToolExecution, ToolExecutionOptions, ToolExecutionResult};
pub use user_message::UserMessage;

use pidgin_tui::markdown::{Markdown, MarkdownTheme, StyleFn};
use pidgin_tui::renderer::Component;

use crate::modes::interactive::theme::Theme;

/// Minimal [`Component`] adapter over `pidgin-tui`'s standalone [`Markdown`]
/// renderer. `pidgin-tui` exposes `Markdown` as a pure `render(width)` renderer
/// rather than a `Component` (see the napi shim note in `pidgin-napi`), so this
/// glue lets the message components add markdown as a container child. It is not
/// a re-port of the markdown engine — it forwards `render` verbatim; `Markdown`
/// recomputes from its text on every render, so there is no cache to invalidate.
pub(crate) struct MarkdownComponent(pub Markdown);

impl Component for MarkdownComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.0.render(width)
    }
}

/// OSC-133 semantic-prompt zone markers wrapping a rendered message. Ported
/// verbatim from pi's `assistant-message.ts` / `user-message.ts`:
/// `\x1b]133;A\x07` (prompt start), `\x1b]133;B\x07` (prompt end),
/// `\x1b]133;C\x07` (command output start).
pub(crate) const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
pub(crate) const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
pub(crate) const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";

/// Build an owned foreground-styling closure for `color` from the theme's
/// pre-baked ANSI escape, reproducing pi's `theme.fg(color, text)` exactly:
/// `{fgAnsi}{text}\x1b[39m`. The escape is captured by value so the closure is
/// `'static` (and `Send + Sync`, as [`StyleFn`] requires).
fn fg_style(theme: &Theme, color: &str) -> StyleFn {
    let ansi = theme.get_fg_ansi(color).unwrap_or_default();
    Box::new(move |text: &str| format!("{ansi}{text}\x1b[39m"))
}

/// The interactive markdown theme, a faithful port of pi's `getMarkdownTheme()`
/// (`theme/theme.ts:1230`): each element color is `theme.fg("md*", …)` and the
/// text styles are the chalk SGR pass-throughs (`theme.bold`/`italic`/… ==
/// `chalk.*`).
///
/// **Documented divergence — syntax highlighting.** pi's `highlightCode` calls
/// `cli-highlight` for a valid language, else colors each line with
/// `theme.fg("mdCodeBlock", …)`. `cli-highlight` is not ported, so this
/// `highlight_code` only implements the no-valid-language fallback (the branch
/// pi takes for a bare or unsupported code fence). Callers/vectors that need
/// byte-exact output must avoid syntax-highlighted (valid-language) code fences;
/// every other markdown construct is byte-exact.
pub fn get_markdown_theme(theme: &Theme) -> MarkdownTheme {
    // Capture the mdCodeBlock foreground escape for the no-valid-language
    // highlight fallback: pi does `code.split("\n").map(theme.fg("mdCodeBlock"))`.
    let code_block_ansi = theme.get_fg_ansi("mdCodeBlock").unwrap_or_default();
    MarkdownTheme {
        heading: fg_style(theme, "mdHeading"),
        link: fg_style(theme, "mdLink"),
        link_url: fg_style(theme, "mdLinkUrl"),
        code: fg_style(theme, "mdCode"),
        code_block: fg_style(theme, "mdCodeBlock"),
        code_block_border: fg_style(theme, "mdCodeBlockBorder"),
        quote: fg_style(theme, "mdQuote"),
        quote_border: fg_style(theme, "mdQuoteBorder"),
        hr: fg_style(theme, "mdHr"),
        list_bullet: fg_style(theme, "mdListBullet"),
        bold: Box::new(|text: &str| format!("\x1b[1m{text}\x1b[22m")),
        italic: Box::new(|text: &str| format!("\x1b[3m{text}\x1b[23m")),
        underline: Box::new(|text: &str| format!("\x1b[4m{text}\x1b[24m")),
        strikethrough: Box::new(|text: &str| format!("\x1b[9m{text}\x1b[29m")),
        highlight_code: Some(Box::new(move |code: &str, _lang: Option<&str>| {
            code.split('\n')
                .map(|line| format!("{code_block_ansi}{line}\x1b[39m"))
                .collect()
        })),
        code_block_indent: None,
    }
}
