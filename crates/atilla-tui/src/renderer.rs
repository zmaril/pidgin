//! Line-string differential renderer, a faithful port of pi's `TUI` class in
//! `vendor/pi/packages/tui/src/tui.ts`.
//!
//! The renderer is not a cell grid. Components implement [`Component::render`],
//! returning whole ANSI-encoded strings, one per logical line. [`Tui::do_render`]
//! diffs the new lines against the previous frame by plain string comparison to
//! find the first and last changed index, then rewrites only that band with
//! relative cursor motion. It renders inline in the primary screen buffer (never
//! the alt-screen) and manages its own scrollback: full redraws clear with
//! `\x1b[2J\x1b[H\x1b[3J`, and every write batch is wrapped in DEC 2026
//! synchronized output (`\x1b[?2026h` .. `\x1b[?2026l`).
//!
//! This module is PR-R1: the core line-diff, full-redraw ladder, viewport
//! bookkeeping, and the width-overflow crash contract. Overlays/compositing and
//! the full Kitty image lifecycle (encode/reserved-row draw fallbacks) are
//! PR-R2. The Kitty header *parse* helpers and reserved-row/delete plumbing live
//! here because `do_render` invokes them on every frame; with no image lines
//! present they are exercised as faithful no-ops.
//!
//! Byte-parity is the contract: the emitted write stream is validated
//! byte-for-byte against vectors extracted from pi itself (see
//! `tests/renderer_vectors.rs`).

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use crate::terminal::Terminal;
use crate::{
    extract_segments, normalize_terminal_output, slice_by_column, slice_with_width, visible_width,
};

/// Cursor position marker: a zero-width APC sequence terminals ignore.
/// Focusable components emit this at the cursor position when focused; the
/// renderer finds and strips it, then positions the hardware cursor there for
/// IME candidate-window placement.
pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

/// Appended to every non-image line after normalization: reset SGR + close any
/// open OSC 8 hyperlink so styles never leak into the next line.
const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

const KITTY_SEQUENCE_PREFIX: &str = "\x1b_G";
const ITERM2_PREFIX: &str = "\x1b]1337;File=";

const MIN_RENDER_INTERVAL_MS: u64 = 16;

/// Fatal render error. Mirrors pi's width-overflow `throw`: a rendered line
/// whose visible width exceeds the terminal width is a crash, not a cosmetic
/// drift. On this error the renderer has already written a crash log and torn
/// down the terminal (via [`Tui::stop`]), matching pi's contract.
#[derive(Debug, Clone)]
pub enum RenderError {
    /// A rendered line's visible width exceeded the terminal width.
    WidthOverflow {
        /// Index of the offending line in the rendered frame.
        line: usize,
        /// Visible width of the offending line.
        width: usize,
        /// Terminal width at render time.
        terminal_width: usize,
        /// The full human-readable message pi would throw.
        message: String,
    },
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::WidthOverflow { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for RenderError {}

/// A renderable component. Ported from pi's `Component` interface: `render`
/// returns one ANSI string per logical line for the given viewport width.
pub trait Component {
    /// Render to lines for the given viewport width.
    fn render(&self, width: usize) -> Vec<String>;
    /// Optional keyboard input handler when focused. Unused in PR-R1.
    fn handle_input(&mut self, _data: &str) {}
    /// Whether this component wants key-release events (Kitty protocol).
    fn wants_key_release(&self) -> bool {
        false
    }
    /// Invalidate cached render state (e.g. on theme or cell-size change).
    fn invalidate(&mut self) {}
}

/// A component backed by a shared, externally-mutable line buffer. This mirrors
/// the test-suite `TestComponent`: a driver holds a clone of the handle and
/// swaps the lines between renders. Useful for tests and simple static content.
#[derive(Debug, Clone, Default)]
pub struct SharedLines {
    lines: Rc<RefCell<Vec<String>>>,
}

impl SharedLines {
    /// Create an empty shared-lines component.
    pub fn new() -> Self {
        Self::default()
    }

    /// Clone the shared handle so a driver can mutate the lines out of band.
    pub fn handle(&self) -> Rc<RefCell<Vec<String>>> {
        Rc::clone(&self.lines)
    }

    /// Replace the current lines.
    pub fn set(&self, lines: Vec<String>) {
        *self.lines.borrow_mut() = lines;
    }
}

impl Component for SharedLines {
    fn render(&self, _width: usize) -> Vec<String> {
        self.lines.borrow().clone()
    }
}

/// Container of child components. Ported from pi's `Container`: `render`
/// concatenates each child's lines in order.
#[derive(Default)]
pub struct Container {
    children: Vec<Box<dyn Component>>,
}

impl Container {
    /// Create an empty container.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a child component.
    pub fn add_child(&mut self, component: Box<dyn Component>) {
        self.children.push(component);
    }

    /// Remove all children.
    pub fn clear(&mut self) {
        self.children.clear();
    }

    /// Render all children and concatenate their lines.
    pub fn render(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for child in &self.children {
            lines.extend(child.render(width));
        }
        lines
    }

    /// Invalidate every child.
    pub fn invalidate(&mut self) {
        for child in &mut self.children {
            child.invalidate();
        }
    }
}

/// `true` if `line` looks like a terminal image placement (Kitty graphics or
/// iTerm2 inline image). Ported verbatim from `terminal-image.ts::isImageLine`.
pub fn is_image_line(line: &str) -> bool {
    if line.starts_with(KITTY_SEQUENCE_PREFIX) || line.starts_with(ITERM2_PREFIX) {
        return true;
    }
    line.contains(KITTY_SEQUENCE_PREFIX) || line.contains(ITERM2_PREFIX)
}

/// Kitty graphics deletion sequence for `image_id`. Ported from
/// `terminal-image.ts::deleteKittyImage`.
pub fn delete_kitty_image(image_id: u64) -> String {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")
}

struct KittyImageHeader {
    ids: Vec<u64>,
    rows: i64,
}

/// Parse a Kitty graphics header (`\x1b_G<params>;...`) for `i=` ids and `r=`
/// rows. Ported from `tui.ts::parseKittyImageHeader`.
fn parse_kitty_image_header(line: &str) -> Option<KittyImageHeader> {
    let seq_start = line.find(KITTY_SEQUENCE_PREFIX)?;
    let params_start = seq_start + KITTY_SEQUENCE_PREFIX.len();
    let params_end_rel = line[params_start..].find(';')?;
    let params_end = params_start + params_end_rel;

    let mut ids = Vec::new();
    let mut rows: i64 = 1;
    let params = &line[params_start..params_end];
    for param in params.split(',') {
        let mut kv = param.splitn(2, '=');
        let key = kv.next().unwrap_or("");
        let value = match kv.next() {
            Some(v) => v,
            None => continue,
        };
        // pi uses Number(value): integral, in (0, 0xffffffff].
        let number_value: f64 = match value.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if number_value.fract() != 0.0
            || number_value <= 0.0
            || number_value > 0xffff_ffffu32 as f64
        {
            continue;
        }
        let number_value = number_value as u64;
        if key == "i" {
            ids.push(number_value);
        } else if key == "r" {
            rows = number_value as i64;
        }
    }
    Some(KittyImageHeader { ids, rows })
}

fn extract_kitty_image_ids(line: &str) -> Vec<u64> {
    parse_kitty_image_header(line)
        .map(|h| h.ids)
        .unwrap_or_default()
}

fn extract_kitty_image_rows(line: &str) -> i64 {
    parse_kitty_image_header(line).map(|h| h.rows).unwrap_or(1)
}

/// Stable identifier for a component registered with the renderer for focus or
/// overlay purposes. pi relies on JavaScript reference equality (`===`) to track
/// the focused component and overlay stack entries; Rust has no such intrinsic
/// identity for `dyn` trait objects, so each registered component is assigned an
/// id and identity comparisons go through it. This is a faithful stand-in for
/// pi's reference equality, not a new concept.
pub type ComponentId = usize;

/// Stable identifier for an overlay handle returned by [`Tui::show_overlay`].
/// pi's `OverlayHandle` closes over the stack entry object; the entry may be
/// spliced out of the stack yet the handle keeps working, so a monotonic id
/// (not a stack index) reproduces that behavior.
pub type OverlayHandleId = usize;

/// Anchor position for overlays. Ported from `OverlayAnchor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAnchor {
    /// Centered horizontally and vertically.
    Center,
    /// Top-left corner.
    TopLeft,
    /// Top-right corner.
    TopRight,
    /// Bottom-left corner.
    BottomLeft,
    /// Bottom-right corner.
    BottomRight,
    /// Top edge, horizontally centered.
    TopCenter,
    /// Bottom edge, horizontally centered.
    BottomCenter,
    /// Left edge, vertically centered.
    LeftCenter,
    /// Right edge, vertically centered.
    RightCenter,
}

/// Margin from the terminal edges. Ported from `OverlayMargin`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OverlayMargin {
    /// Top margin.
    pub top: Option<i64>,
    /// Right margin.
    pub right: Option<i64>,
    /// Bottom margin.
    pub bottom: Option<i64>,
    /// Left margin.
    pub left: Option<i64>,
}

/// A margin spec that is either a single number applied to all sides, or a
/// per-side object. Ported from `OverlayMargin | number`.
#[derive(Debug, Clone, Copy)]
pub enum MarginSpec {
    /// Same margin on all four sides.
    All(i64),
    /// Per-side margins.
    Sides(OverlayMargin),
}

/// A value that is either an absolute number of cells or a percentage of a
/// reference size. Ported from `SizeValue = number | \`${number}%\``.
#[derive(Debug, Clone, Copy)]
pub enum SizeValue {
    /// Absolute number of cells.
    Abs(i64),
    /// Percentage of the reference size (e.g. `Pct(50.0)` for `"50%"`).
    Pct(f64),
}

/// Parse a [`SizeValue`] into an absolute value given a reference size. Ported
/// from `parseSizeValue`: `Math.floor(referenceSize * percent / 100)`.
fn parse_size_value(value: Option<SizeValue>, reference_size: i64) -> Option<i64> {
    match value {
        None => None,
        Some(SizeValue::Abs(n)) => Some(n),
        Some(SizeValue::Pct(p)) => Some(((reference_size as f64 * p) / 100.0).floor() as i64),
    }
}

/// Options for overlay positioning and sizing. Ported from `OverlayOptions`.
#[derive(Default)]
pub struct OverlayOptions {
    /// Width in columns, or percentage of terminal width.
    pub width: Option<SizeValue>,
    /// Minimum width in columns.
    pub min_width: Option<i64>,
    /// Maximum height in rows, or percentage of terminal height.
    pub max_height: Option<SizeValue>,
    /// Anchor point for positioning (default: center).
    pub anchor: Option<OverlayAnchor>,
    /// Horizontal offset from the anchor position.
    pub offset_x: Option<i64>,
    /// Vertical offset from the anchor position.
    pub offset_y: Option<i64>,
    /// Row position: absolute, or percentage from top.
    pub row: Option<SizeValue>,
    /// Column position: absolute, or percentage from left.
    pub col: Option<SizeValue>,
    /// Margin from the terminal edges.
    pub margin: Option<MarginSpec>,
    /// Visibility gate, evaluated each render. Modeled as a shared boolean flag
    /// (pi uses a `(w, h) => boolean` closure; the tests toggle a captured
    /// variable, which this reproduces exactly).
    pub visible: Option<Rc<Cell<bool>>>,
    /// If true, do not capture keyboard focus when shown.
    pub non_capturing: bool,
}

/// How a blocked overlay should resume once the blocking component releases
/// focus. Ported from `OverlayBlockedFocusResume`.
#[derive(Debug, Clone)]
enum OverlayBlockedFocusResume {
    RestoreOverlay,
    FocusTarget(Option<ComponentId>),
}

/// The overlay focus-restore state machine. Ported from
/// `OverlayFocusRestoreState`. `overlay` is stored as a handle id so the state
/// survives the entry being spliced out of the stack (pi keeps the object
/// reference alive through the closure).
#[derive(Debug, Clone)]
enum OverlayFocusRestoreState {
    Inactive,
    Eligible {
        overlay: OverlayHandleId,
    },
    Blocked {
        overlay: OverlayHandleId,
        blocked_by: ComponentId,
        resume: OverlayBlockedFocusResume,
    },
}

impl OverlayFocusRestoreState {
    fn is_inactive(&self) -> bool {
        matches!(self, OverlayFocusRestoreState::Inactive)
    }

    /// The overlay handle this state references, if any (pi's `restoreState.overlay`).
    fn overlay_handle(&self) -> Option<OverlayHandleId> {
        match self {
            OverlayFocusRestoreState::Inactive => None,
            OverlayFocusRestoreState::Eligible { overlay }
            | OverlayFocusRestoreState::Blocked { overlay, .. } => Some(*overlay),
        }
    }
}

/// Whether `setFocusInternal` should clear the restore state when focus goes to
/// null. Ported from `OverlayFocusRestorePolicy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayFocusRestorePolicy {
    Clear,
    Preserve,
}

/// An overlay stack entry. Ported from `OverlayStackEntry`.
struct OverlayStackEntry {
    handle_id: OverlayHandleId,
    component: ComponentId,
    options: OverlayOptions,
    pre_focus: Option<ComponentId>,
    hidden: bool,
    focus_order: u64,
}

/// The main renderer. Owns the terminal sink and the mutable viewport
/// bookkeeping. Ported from pi's `TUI` (PR-R1 core subset).
pub struct Tui<T: Terminal> {
    terminal: T,
    container: Container,

    previous_lines: Vec<String>,
    previous_kitty_image_ids: Vec<u64>,
    previous_width: i64,
    previous_height: i64,

    render_requested: bool,
    #[allow(dead_code)]
    last_render_at_ms: u64,

    cursor_row: i64,
    hardware_cursor_row: i64,
    show_hardware_cursor: bool,
    clear_on_shrink: bool,
    max_lines_rendered: i64,
    previous_viewport_top: i64,
    full_redraw_count: u64,
    stopped: bool,

    /// Whether `isTermuxSession()` should be treated as true. Modeled as config
    /// because the Rust port does not read `TERMUX_VERSION` from the environment.
    termux: bool,
    /// Whether the terminal advertises image capability (gates `queryCellSize`).
    images_capable: bool,
    /// Override for the crash-log directory (defaults to `~/.pi/agent`).
    crash_log_dir: Option<PathBuf>,

    // --- Overlay + focus subsystem (PR-R2) ---
    /// Registry of components addressable by [`ComponentId`] for overlay
    /// rendering and focus tracking. pi keeps direct object references; the
    /// registry is the Rust stand-in for reference identity.
    components: Vec<Rc<RefCell<dyn Component>>>,
    /// The currently focused component, if any.
    focused_component: Option<ComponentId>,
    /// Components mounted in the base tree, for `isComponentMounted`. Populated
    /// by [`Tui::mount_base`]; empty means no registered component is a base
    /// child (the common case in the overlay tests).
    mounted: Vec<ComponentId>,
    /// The overlay stack (bottom-to-top by insertion; z-order by focus_order).
    overlay_stack: Vec<OverlayStackEntry>,
    /// The overlay focus-restore state machine.
    overlay_focus_restore: OverlayFocusRestoreState,
    /// Monotonic focus-order counter (higher = visually in front).
    focus_order_counter: u64,
    /// Monotonic handle-id counter for overlay handles.
    handle_id_counter: usize,
    /// Log of `(component, data)` input deliveries, for input-routing vectors.
    input_deliveries: Vec<(ComponentId, String)>,
    /// Scripted reactions a focused component runs on given input, keyed by
    /// `(component, data)`. pi's tests attach ad-hoc `handleInput` closures that
    /// call back into the TUI (`setFocus`, `unfocus`, tree mutation); encoding
    /// them as data lets `handle_input` apply them without re-entrant borrows.
    input_reactions: std::collections::HashMap<(ComponentId, String), Vec<ReactionAction>>,
}

/// A scripted action a component performs in reaction to receiving input,
/// mirroring the callbacks pi's overlay tests attach to `handleInput`.
#[derive(Debug, Clone)]
pub enum ReactionAction {
    /// `tui.setFocus(target)`.
    SetFocus(Option<ComponentId>),
    /// `base.clear()` — empties the base mounted set.
    ClearBase,
    /// `base.addChild(component)` — marks a component mounted in the base tree.
    MountBase(ComponentId),
    /// `tui.hideOverlay()`.
    HideOverlay,
    /// `handle.hide()` for the given overlay handle.
    CloseOverlay(OverlayHandleId),
    /// `handle.unfocus()` (no options).
    Unfocus(OverlayHandleId),
    /// `handle.unfocus({ target })`.
    UnfocusTarget(OverlayHandleId, Option<ComponentId>),
}

impl<T: Terminal> Tui<T> {
    /// Create a renderer over `terminal`. `show_hardware_cursor` mirrors pi's
    /// `PI_HARDWARE_CURSOR` opt-in (default `false`).
    pub fn new(terminal: T, show_hardware_cursor: bool) -> Self {
        Self {
            terminal,
            container: Container::new(),
            previous_lines: Vec::new(),
            previous_kitty_image_ids: Vec::new(),
            previous_width: 0,
            previous_height: 0,
            render_requested: false,
            last_render_at_ms: 0,
            cursor_row: 0,
            hardware_cursor_row: 0,
            show_hardware_cursor,
            clear_on_shrink: false,
            max_lines_rendered: 0,
            previous_viewport_top: 0,
            full_redraw_count: 0,
            stopped: false,
            termux: false,
            images_capable: false,
            crash_log_dir: None,
            components: Vec::new(),
            focused_component: None,
            mounted: Vec::new(),
            overlay_stack: Vec::new(),
            overlay_focus_restore: OverlayFocusRestoreState::Inactive,
            focus_order_counter: 0,
            handle_id_counter: 0,
            input_deliveries: Vec::new(),
            input_reactions: std::collections::HashMap::new(),
        }
    }

    /// Add a child component (delegates to the embedded container).
    pub fn add_child(&mut self, component: Box<dyn Component>) {
        self.container.add_child(component);
    }

    /// Remove all children.
    pub fn clear(&mut self) {
        self.container.clear();
    }

    /// Access the terminal backend (e.g. to resize or inspect a logging sink).
    pub fn terminal_mut(&mut self) -> &mut T {
        &mut self.terminal
    }

    /// Number of full redraws performed (pi's `fullRedraws` getter). Pins the
    /// full-redraw decision ladder in tests.
    pub fn full_redraws(&self) -> u64 {
        self.full_redraw_count
    }

    /// Logical cursor row (end of rendered content).
    pub fn cursor_row(&self) -> i64 {
        self.cursor_row
    }

    /// Actual hardware cursor row.
    pub fn hardware_cursor_row(&self) -> i64 {
        self.hardware_cursor_row
    }

    /// Previous viewport top (resize-aware cursor bookkeeping).
    pub fn previous_viewport_top(&self) -> i64 {
        self.previous_viewport_top
    }

    /// High-water working area (max lines ever rendered since last clear).
    pub fn max_lines_rendered(&self) -> i64 {
        self.max_lines_rendered
    }

    /// Enable/disable clearing empty rows when content shrinks (pi's
    /// `setClearOnShrink`, env `PI_CLEAR_ON_SHRINK`).
    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        self.clear_on_shrink = enabled;
    }

    /// Whether clearing on shrink is enabled.
    pub fn get_clear_on_shrink(&self) -> bool {
        self.clear_on_shrink
    }

    /// Model `isTermuxSession()`: on Termux, height changes do not force a full
    /// redraw (the soft keyboard toggles height constantly).
    pub fn set_termux(&mut self, termux: bool) {
        self.termux = termux;
    }

    /// Model terminal image capability (gates `queryCellSize`, PR-R2).
    pub fn set_images_capable(&mut self, capable: bool) {
        self.images_capable = capable;
    }

    /// Override the crash-log directory (tests point this at a temp dir so the
    /// width-overflow contract does not pollute `~/.pi`).
    pub fn set_crash_log_dir(&mut self, dir: PathBuf) {
        self.crash_log_dir = Some(dir);
    }

    /// Render the container tree to lines.
    fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    /// Start a session: wire the terminal, hide the cursor, query cell size (if
    /// image-capable), and request the first render. Ported from `TUI::start`.
    pub fn start(&mut self) {
        self.stopped = false;
        self.terminal.start();
        self.terminal.hide_cursor();
        self.query_cell_size();
        self.request_render(false);
    }

    fn query_cell_size(&mut self) {
        // Only query if the terminal supports images (cell size is only used for
        // image rendering). Response handling is PR-R2.
        if !self.images_capable {
            return;
        }
        self.terminal.write("\x1b[16t");
    }

    /// Move the cursor past the content and restore terminal state on exit.
    /// Ported from `TUI::stop`.
    pub fn stop(&mut self) {
        self.stopped = true;
        if !self.previous_lines.is_empty() {
            let target_row = self.previous_lines.len() as i64; // line after last content
            let line_diff = target_row - self.hardware_cursor_row;
            if line_diff > 0 {
                self.terminal.write(&format!("\x1b[{line_diff}B"));
            } else if line_diff < 0 {
                self.terminal.write(&format!("\x1b[{}A", -line_diff));
            }
            self.terminal.write("\r\n");
        }
        self.terminal.show_cursor();
        self.terminal.stop();
    }

    /// Request a render. `force` resets the diff state so the next render is a
    /// full redraw (pi resets `previousLines`, sets width/height to -1, and
    /// zeroes the cursor/viewport counters). Rendering itself is driven by
    /// [`Tui::flush`], which models pi's 16ms coalesced scheduler: any number of
    /// requests between flushes collapse to a single `do_render`.
    pub fn request_render(&mut self, force: bool) {
        if force {
            self.previous_lines = Vec::new();
            self.previous_width = -1; // -1 triggers widthChanged -> full clear
            self.previous_height = -1; // -1 triggers heightChanged -> full clear
            self.cursor_row = 0;
            self.hardware_cursor_row = 0;
            self.max_lines_rendered = 0;
            self.previous_viewport_top = 0;
            self.render_requested = true;
            return;
        }
        if self.render_requested {
            return;
        }
        self.render_requested = true;
    }

    /// Interval pi coalesces renders to (about 60fps). Exposed for parity docs.
    pub const fn min_render_interval_ms() -> u64 {
        MIN_RENDER_INTERVAL_MS
    }

    /// Synchronously flush a pending render. Runs at most one `do_render`,
    /// matching a single fire of pi's coalesced timer (which is what the test
    /// harness's `waitForRender()` produces per step). No-op if no render is
    /// pending or the renderer is stopped.
    pub fn flush(&mut self) -> Result<(), RenderError> {
        if self.stopped || !self.render_requested {
            return Ok(());
        }
        self.render_requested = false;
        self.do_render()
    }

    fn collect_kitty_image_ids(lines: &[String]) -> Vec<u64> {
        let mut ids = Vec::new();
        for line in lines {
            for id in extract_kitty_image_ids(line) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        ids
    }

    fn delete_kitty_images(ids: &[u64]) -> String {
        let mut buffer = String::new();
        for id in ids {
            buffer.push_str(&delete_kitty_image(*id));
        }
        buffer
    }

    /// Number of terminal rows a Kitty image at `index` reserves (its `r=` count
    /// bounded by trailing blank, non-image rows). Ported from
    /// `getKittyImageReservedRows`.
    fn get_kitty_image_reserved_rows(lines: &[String], index: usize, max_index: usize) -> i64 {
        let rows = extract_kitty_image_rows(lines.get(index).map(String::as_str).unwrap_or(""));
        if rows <= 1 {
            return 1;
        }
        let max_rows = rows
            .min((max_index as i64) - (index as i64) + 1)
            .min((lines.len() as i64) - (index as i64));
        let mut reserved_rows: i64 = 1;
        while reserved_rows < max_rows {
            let line = lines
                .get(index + reserved_rows as usize)
                .map(String::as_str)
                .unwrap_or("");
            if is_image_line(line) || visible_width(line) > 0 {
                break;
            }
            reserved_rows += 1;
        }
        reserved_rows
    }

    fn expand_changed_range_for_kitty_images(
        &self,
        first_changed: i64,
        last_changed: i64,
        new_lines: &[String],
    ) -> (i64, i64) {
        let mut expanded_first = first_changed;
        let mut expanded_last = last_changed;
        let mut expand_for = |lines: &[String]| {
            for i in 0..lines.len() {
                if extract_kitty_image_ids(&lines[i]).is_empty() {
                    continue;
                }
                let block_end =
                    i as i64 + Self::get_kitty_image_reserved_rows(lines, i, lines.len() - 1) - 1;
                let i = i as i64;
                if i >= first_changed || (i <= last_changed && block_end >= first_changed) {
                    expanded_first = expanded_first.min(i);
                    expanded_last = expanded_last.max(block_end);
                }
            }
        };
        expand_for(&self.previous_lines);
        expand_for(new_lines);
        (expanded_first, expanded_last)
    }

    fn delete_changed_kitty_images(&self, first_changed: i64, last_changed: i64) -> String {
        if first_changed < 0 || last_changed < first_changed {
            return String::new();
        }
        let mut ids: Vec<u64> = Vec::new();
        let max_line = last_changed.min(self.previous_lines.len() as i64 - 1);
        let mut i = first_changed;
        while i <= max_line {
            if let Some(line) = self.previous_lines.get(i as usize) {
                for id in extract_kitty_image_ids(line) {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
            i += 1;
        }
        Self::delete_kitty_images(&ids)
    }

    /// Append SEGMENT_RESET to each non-image line after normalization. Ported
    /// from `applyLineResets`.
    fn apply_line_resets(lines: &mut [String]) {
        for line in lines.iter_mut() {
            if !is_image_line(line) {
                *line = format!("{}{SEGMENT_RESET}", normalize_terminal_output(line));
            }
        }
    }

    /// Find and strip the cursor marker in the visible viewport, returning its
    /// (row, col). Ported from `extractCursorPosition`.
    fn extract_cursor_position(lines: &mut [String], height: usize) -> Option<(i64, i64)> {
        let viewport_top = lines.len().saturating_sub(height);
        let mut row = lines.len() as i64 - 1;
        while row >= viewport_top as i64 {
            let idx = row as usize;
            if let Some(marker_index) = lines[idx].find(CURSOR_MARKER) {
                let before_marker = &lines[idx][..marker_index];
                let col = visible_width(before_marker) as i64;
                let mut stripped = String::with_capacity(lines[idx].len() - CURSOR_MARKER.len());
                stripped.push_str(&lines[idx][..marker_index]);
                stripped.push_str(&lines[idx][marker_index + CURSOR_MARKER.len()..]);
                lines[idx] = stripped;
                return Some((row, col));
            }
            row -= 1;
        }
        None
    }

    /// Full clear-and-redraw. Ported from the `fullRender` closure in `doRender`.
    fn full_render(
        &mut self,
        clear: bool,
        new_lines: &[String],
        cursor_pos: Option<(i64, i64)>,
        width: i64,
        height: i64,
    ) {
        self.full_redraw_count += 1;
        let mut buffer = String::from("\x1b[?2026h"); // Begin synchronized output
        if clear {
            buffer.push_str(&Self::delete_kitty_images(&self.previous_kitty_image_ids));
            buffer.push_str("\x1b[2J\x1b[H\x1b[3J"); // Clear screen, home, clear scrollback
        }
        let mut i = 0usize;
        while i < new_lines.len() {
            if i > 0 {
                buffer.push_str("\r\n");
            }
            let line = &new_lines[i];
            let is_image = is_image_line(line);
            let image_reserved_rows = if is_image {
                Self::get_kitty_image_reserved_rows(new_lines, i, new_lines.len() - 1)
            } else {
                1
            };
            if image_reserved_rows > 1 && image_reserved_rows <= height {
                for _ in 1..image_reserved_rows {
                    buffer.push_str("\r\n");
                }
                buffer.push_str(&format!("\x1b[{}A", image_reserved_rows - 1));
                buffer.push_str(line);
                buffer.push_str(&format!("\x1b[{}B", image_reserved_rows - 1));
                i += image_reserved_rows as usize - 1;
                i += 1;
                continue;
            }
            buffer.push_str(line);
            i += 1;
        }
        buffer.push_str("\x1b[?2026l"); // End synchronized output
        self.terminal.write(&buffer);
        self.cursor_row = (new_lines.len() as i64 - 1).max(0);
        self.hardware_cursor_row = self.cursor_row;
        if clear {
            self.max_lines_rendered = new_lines.len() as i64;
        } else {
            self.max_lines_rendered = self.max_lines_rendered.max(new_lines.len() as i64);
        }
        let buffer_length = height.max(new_lines.len() as i64);
        self.previous_viewport_top = (buffer_length - height).max(0);
        self.position_hardware_cursor(cursor_pos, new_lines.len() as i64);
        self.previous_lines = new_lines.to_vec();
        self.previous_kitty_image_ids = Self::collect_kitty_image_ids(new_lines);
        self.previous_width = width;
        self.previous_height = height;
    }

    /// Emit a relative vertical cursor move onto `buffer`: `\x1b[nB` (down) for a
    /// positive delta, `\x1b[nA` (up) for a negative one, nothing for zero.
    fn emit_vertical_move(buffer: &mut String, delta: i64) {
        if delta > 0 {
            buffer.push_str(&format!("\x1b[{delta}B"));
        } else if delta < 0 {
            buffer.push_str(&format!("\x1b[{}A", -delta));
        }
    }

    /// pi's `computeLineDiff` closure plus the vertical move it always drives:
    /// translate `target_row` into a screen-relative delta from the current
    /// hardware cursor and emit the move. pi reuses one `computeLineDiff` closure
    /// from both the all-deleted and band paths; this restores that factoring.
    fn emit_line_diff_move(
        buffer: &mut String,
        target_row: i64,
        hardware_cursor_row: i64,
        prev_viewport_top: i64,
        viewport_top: i64,
    ) {
        let current_screen_row = hardware_cursor_row - prev_viewport_top;
        let target_screen_row = target_row - viewport_top;
        Self::emit_vertical_move(buffer, target_screen_row - current_screen_row);
    }

    /// The heart of the renderer. Ported from `TUI::doRender`.
    fn do_render(&mut self) -> Result<(), RenderError> {
        if self.stopped {
            return Ok(());
        }
        let width = self.terminal.columns();
        let height = self.terminal.rows();
        let width_i = width as i64;
        let height_i = height as i64;
        let width_changed = self.previous_width != 0 && self.previous_width != width_i;
        let height_changed = self.previous_height != 0 && self.previous_height != height_i;
        let previous_buffer_length = if self.previous_height > 0 {
            self.previous_viewport_top + self.previous_height
        } else {
            height_i
        };
        let mut prev_viewport_top = if height_changed {
            (previous_buffer_length - height_i).max(0)
        } else {
            self.previous_viewport_top
        };
        let mut viewport_top = prev_viewport_top;
        let mut hardware_cursor_row = self.hardware_cursor_row;

        // Render all components, then extract cursor marker, then apply resets.
        let mut new_lines = self.render(width);
        // Composite overlays into the rendered lines before the differential
        // compare (ported from doRender's `overlayStack.length > 0` branch).
        if !self.overlay_stack.is_empty() {
            new_lines = self.composite_overlays(new_lines, width_i, height_i);
        }
        let cursor_pos = Self::extract_cursor_position(&mut new_lines, height);
        Self::apply_line_resets(&mut new_lines);

        // First render - output everything without clearing (assumes clean screen).
        if self.previous_lines.is_empty() && !width_changed && !height_changed {
            self.full_render(false, &new_lines, cursor_pos, width_i, height_i);
            return Ok(());
        }

        // Width changes always need a full re-render (wrapping changes).
        if width_changed {
            self.full_render(true, &new_lines, cursor_pos, width_i, height_i);
            return Ok(());
        }

        // Height changes need a full re-render, except on Termux (soft keyboard).
        if height_changed && !self.termux {
            self.full_render(true, &new_lines, cursor_pos, width_i, height_i);
            return Ok(());
        }

        // Content shrunk below the working area and no overlays: re-render to
        // clear empty rows (configurable via clearOnShrink).
        if self.clear_on_shrink
            && (new_lines.len() as i64) < self.max_lines_rendered
            && self.overlay_stack.is_empty()
        {
            self.full_render(true, &new_lines, cursor_pos, width_i, height_i);
            return Ok(());
        }

        // Find first and last changed lines by plain string comparison.
        let mut first_changed: i64 = -1;
        let mut last_changed: i64 = -1;
        let max_lines = new_lines.len().max(self.previous_lines.len());
        for i in 0..max_lines {
            let old_line = self.previous_lines.get(i).map(String::as_str).unwrap_or("");
            let new_line = new_lines.get(i).map(String::as_str).unwrap_or("");
            if old_line != new_line {
                if first_changed == -1 {
                    first_changed = i as i64;
                }
                last_changed = i as i64;
            }
        }
        let appended_lines = new_lines.len() > self.previous_lines.len();
        if appended_lines {
            if first_changed == -1 {
                first_changed = self.previous_lines.len() as i64;
            }
            last_changed = new_lines.len() as i64 - 1;
        }
        if first_changed != -1 {
            let (f, l) =
                self.expand_changed_range_for_kitty_images(first_changed, last_changed, &new_lines);
            first_changed = f;
            last_changed = l;
        }
        let append_start = appended_lines
            && first_changed == self.previous_lines.len() as i64
            && first_changed > 0;

        // No changes - only update hardware cursor if it moved.
        if first_changed == -1 {
            self.position_hardware_cursor(cursor_pos, new_lines.len() as i64);
            self.previous_viewport_top = prev_viewport_top;
            self.previous_height = height_i;
            return Ok(());
        }

        // All changes are in deleted lines (nothing to render, just clear).
        if first_changed >= new_lines.len() as i64 {
            if self.previous_lines.len() > new_lines.len() {
                let mut buffer = String::from("\x1b[?2026h");
                buffer.push_str(&self.delete_changed_kitty_images(first_changed, last_changed));
                let target_row = (new_lines.len() as i64 - 1).max(0);
                if target_row < prev_viewport_top {
                    self.full_render(true, &new_lines, cursor_pos, width_i, height_i);
                    return Ok(());
                }
                // computeLineDiff(targetRow), then emit the vertical move.
                Self::emit_line_diff_move(
                    &mut buffer,
                    target_row,
                    hardware_cursor_row,
                    prev_viewport_top,
                    viewport_top,
                );
                buffer.push('\r');
                let extra_lines = self.previous_lines.len() as i64 - new_lines.len() as i64;
                if extra_lines > height_i {
                    self.full_render(true, &new_lines, cursor_pos, width_i, height_i);
                    return Ok(());
                }
                let clear_start_offset: i64 = if new_lines.is_empty() { 0 } else { 1 };
                if extra_lines > 0 && clear_start_offset > 0 {
                    buffer.push_str(&format!("\x1b[{clear_start_offset}B"));
                }
                for i in 0..extra_lines {
                    buffer.push_str("\r\x1b[2K");
                    if i < extra_lines - 1 {
                        buffer.push_str("\x1b[1B");
                    }
                }
                let move_back = (extra_lines - 1 + clear_start_offset).max(0);
                if move_back > 0 {
                    buffer.push_str(&format!("\x1b[{move_back}A"));
                }
                buffer.push_str("\x1b[?2026l");
                self.terminal.write(&buffer);
                self.cursor_row = target_row;
                self.hardware_cursor_row = target_row;
            }
            self.position_hardware_cursor(cursor_pos, new_lines.len() as i64);
            self.previous_lines = new_lines.clone();
            self.previous_kitty_image_ids = Self::collect_kitty_image_ids(&new_lines);
            self.previous_width = width_i;
            self.previous_height = height_i;
            self.previous_viewport_top = prev_viewport_top;
            return Ok(());
        }

        // Differential rendering can only touch what was actually visible.
        if first_changed < prev_viewport_top {
            self.full_render(true, &new_lines, cursor_pos, width_i, height_i);
            return Ok(());
        }

        // Band rewrite: from first changed line to last changed line.
        let mut buffer = String::from("\x1b[?2026h");
        buffer.push_str(&self.delete_changed_kitty_images(first_changed, last_changed));
        let prev_viewport_bottom = prev_viewport_top + height_i - 1;
        let move_target_row = if append_start {
            first_changed - 1
        } else {
            first_changed
        };
        if move_target_row > prev_viewport_bottom {
            let current_screen_row = (hardware_cursor_row - prev_viewport_top)
                .max(0)
                .min(height_i - 1);
            let move_to_bottom = height_i - 1 - current_screen_row;
            if move_to_bottom > 0 {
                buffer.push_str(&format!("\x1b[{move_to_bottom}B"));
            }
            let scroll = move_target_row - prev_viewport_bottom;
            for _ in 0..scroll {
                buffer.push_str("\r\n");
            }
            prev_viewport_top += scroll;
            viewport_top += scroll;
            hardware_cursor_row = move_target_row;
        }

        // Move cursor to first changed line (computeLineDiff(moveTargetRow)).
        Self::emit_line_diff_move(
            &mut buffer,
            move_target_row,
            hardware_cursor_row,
            prev_viewport_top,
            viewport_top,
        );

        buffer.push_str(if append_start { "\r\n" } else { "\r" });

        // Only render changed lines (firstChanged..=renderEnd).
        let render_end = last_changed.min(new_lines.len() as i64 - 1);
        let mut i = first_changed;
        while i <= render_end {
            if i > first_changed {
                buffer.push_str("\r\n");
            }
            let idx = i as usize;
            let line = &new_lines[idx];
            let is_image = is_image_line(line);
            let image_reserved_rows = if is_image {
                Self::get_kitty_image_reserved_rows(&new_lines, idx, render_end as usize)
            } else {
                1
            };
            if image_reserved_rows > 1 {
                let image_start_screen_row = i - viewport_top;
                if image_start_screen_row < 0
                    || image_start_screen_row + image_reserved_rows > height_i
                {
                    self.full_render(true, &new_lines, cursor_pos, width_i, height_i);
                    return Ok(());
                }
                buffer.push_str("\x1b[2K");
                for _ in 1..image_reserved_rows {
                    buffer.push_str("\r\n\x1b[2K");
                }
                buffer.push_str(&format!("\x1b[{}A", image_reserved_rows - 1));
                buffer.push_str(line);
                buffer.push_str(&format!("\x1b[{}B", image_reserved_rows - 1));
                i += image_reserved_rows;
                continue;
            }

            buffer.push_str("\x1b[2K"); // Clear current line
            if !is_image && visible_width(line) > width {
                return Err(self.width_overflow_crash(idx, &new_lines, width));
            }
            buffer.push_str(line);
            i += 1;
        }

        // Track where the cursor ended up after rendering.
        let mut final_cursor_row = render_end;

        // If we had more lines before, clear them and move cursor back.
        if self.previous_lines.len() > new_lines.len() {
            if render_end < new_lines.len() as i64 - 1 {
                let move_down = new_lines.len() as i64 - 1 - render_end;
                buffer.push_str(&format!("\x1b[{move_down}B"));
                final_cursor_row = new_lines.len() as i64 - 1;
            }
            let extra_lines = self.previous_lines.len() as i64 - new_lines.len() as i64;
            for _ in new_lines.len()..self.previous_lines.len() {
                buffer.push_str("\r\n\x1b[2K");
            }
            buffer.push_str(&format!("\x1b[{extra_lines}A"));
        }

        buffer.push_str("\x1b[?2026l"); // End synchronized output

        self.terminal.write(&buffer);

        self.cursor_row = (new_lines.len() as i64 - 1).max(0);
        self.hardware_cursor_row = final_cursor_row;
        self.max_lines_rendered = self.max_lines_rendered.max(new_lines.len() as i64);
        self.previous_viewport_top = prev_viewport_top.max(final_cursor_row - height_i + 1);

        self.position_hardware_cursor(cursor_pos, new_lines.len() as i64);

        self.previous_lines = new_lines.clone();
        self.previous_kitty_image_ids = Self::collect_kitty_image_ids(&new_lines);
        self.previous_width = width_i;
        self.previous_height = height_i;
        Ok(())
    }

    /// Build the crash log + tear down the terminal, then return the error. This
    /// reproduces pi's width-overflow contract (`tui.ts:1519-1547`): it writes
    /// `~/.pi/agent/pi-crash.log`, calls `stop()`, and surfaces the same message.
    fn width_overflow_crash(
        &mut self,
        line: usize,
        new_lines: &[String],
        width: usize,
    ) -> RenderError {
        let line_width = visible_width(&new_lines[line]);
        let mut crash_lines = vec![
            "Crash at (timestamp elided)".to_string(),
            format!("Terminal width: {width}"),
            format!("Line {line} visible width: {line_width}"),
            String::new(),
            "=== All rendered lines ===".to_string(),
        ];
        for (idx, l) in new_lines.iter().enumerate() {
            crash_lines.push(format!("[{idx}] (w={}) {l}", visible_width(l)));
        }
        crash_lines.push(String::new());
        let crash_data = crash_lines.join("\n");

        let dir = self.crash_log_dir.clone().unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join(".pi").join("agent")
        });
        let crash_path = dir.join("pi-crash.log");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&crash_path, crash_data);

        // Clean up terminal state before surfacing the crash.
        self.stop();

        let message = [
            format!("Rendered line {line} exceeds terminal width ({line_width} > {width})."),
            String::new(),
            "This is likely caused by a custom TUI component not truncating its output."
                .to_string(),
            "Use visibleWidth() to measure and truncateToWidth() to truncate lines.".to_string(),
            String::new(),
            format!("Debug log written to: {}", crash_path.display()),
        ]
        .join("\n");

        RenderError::WidthOverflow {
            line,
            width: line_width,
            terminal_width: width,
            message,
        }
    }

    /// Position the hardware cursor for IME candidate window. Ported from
    /// `positionHardwareCursor`.
    fn position_hardware_cursor(&mut self, cursor_pos: Option<(i64, i64)>, total_lines: i64) {
        let (row, col) = match cursor_pos {
            Some(p) if total_lines > 0 => p,
            _ => {
                self.terminal.hide_cursor();
                return;
            }
        };
        let target_row = row.max(0).min(total_lines - 1);
        let target_col = col.max(0);
        let row_delta = target_row - self.hardware_cursor_row;
        let mut buffer = String::new();
        Self::emit_vertical_move(&mut buffer, row_delta);
        buffer.push_str(&format!("\x1b[{}G", target_col + 1));
        if !buffer.is_empty() {
            self.terminal.write(&buffer);
        }
        self.hardware_cursor_row = target_row;
        if self.show_hardware_cursor {
            self.terminal.show_cursor();
        } else {
            self.terminal.hide_cursor();
        }
    }
}

/// Overlay compositing + focus-restore state machine (PR-R2). Ported from the
/// overlay methods of pi's `TUI` in `tui.ts`. Component identity, which pi gets
/// from JavaScript reference equality, is provided here by [`ComponentId`].
impl<T: Terminal> Tui<T> {
    /// Register a component for overlay rendering and focus tracking, returning
    /// its [`ComponentId`]. This is the identity handle pi obtains for free from
    /// object references.
    pub fn register_component(&mut self, component: Rc<RefCell<dyn Component>>) -> ComponentId {
        let id = self.components.len();
        self.components.push(component);
        id
    }

    /// Mark a registered component as mounted in the base tree (pi's
    /// `addChild` for focus targets, exercised by `isComponentMounted`).
    pub fn mount_base(&mut self, id: ComponentId) {
        if !self.mounted.contains(&id) {
            self.mounted.push(id);
        }
    }

    /// Empty the base mounted set (pi's `base.clear()`).
    pub fn clear_base(&mut self) {
        self.mounted.clear();
    }

    /// The currently focused component, if any.
    pub fn focused_component(&self) -> Option<ComponentId> {
        self.focused_component
    }

    /// The `(component, data)` input deliveries recorded so far.
    pub fn input_deliveries(&self) -> &[(ComponentId, String)] {
        &self.input_deliveries
    }

    /// Register a scripted reaction: when `component` is focused and receives
    /// `data`, run `actions` (mirrors pi's ad-hoc `handleInput` callbacks).
    pub fn set_input_reaction(
        &mut self,
        component: ComponentId,
        data: &str,
        actions: Vec<ReactionAction>,
    ) {
        self.input_reactions
            .insert((component, data.to_string()), actions);
    }

    /// A compact snapshot of the focus-restore state, for vector assertions.
    /// Returns `(focused, status, overlay_component, blocked_by, resume,
    /// resume_target)`.
    #[allow(clippy::type_complexity)]
    pub fn focus_snapshot(
        &self,
    ) -> (
        Option<ComponentId>,
        &'static str,
        Option<ComponentId>,
        Option<ComponentId>,
        &'static str,
        Option<ComponentId>,
    ) {
        match &self.overlay_focus_restore {
            OverlayFocusRestoreState::Inactive => {
                (self.focused_component, "inactive", None, None, "none", None)
            }
            OverlayFocusRestoreState::Eligible { overlay } => (
                self.focused_component,
                "eligible",
                self.handle_component(*overlay),
                None,
                "none",
                None,
            ),
            OverlayFocusRestoreState::Blocked {
                overlay,
                blocked_by,
                resume,
            } => {
                let (resume_kind, target) = match resume {
                    OverlayBlockedFocusResume::RestoreOverlay => ("restore-overlay", None),
                    OverlayBlockedFocusResume::FocusTarget(t) => ("focus-target", *t),
                };
                (
                    self.focused_component,
                    "blocked",
                    self.handle_component(*overlay),
                    Some(*blocked_by),
                    resume_kind,
                    target,
                )
            }
        }
    }

    /// The component a given overlay handle refers to (works even after the
    /// entry is spliced out, mirroring pi's persistent closure reference).
    fn handle_component(&self, handle: OverlayHandleId) -> Option<ComponentId> {
        self.overlay_stack
            .iter()
            .find(|e| e.handle_id == handle)
            .map(|e| e.component)
    }

    fn entry_index(&self, handle: OverlayHandleId) -> Option<usize> {
        self.overlay_stack
            .iter()
            .position(|e| e.handle_id == handle)
    }

    fn is_overlay_visible_entry(&self, entry: &OverlayStackEntry) -> bool {
        if entry.hidden {
            return false;
        }
        if let Some(flag) = &entry.options.visible {
            return flag.get();
        }
        true
    }

    fn is_component_mounted(&self, component: ComponentId) -> bool {
        self.mounted.contains(&component)
    }

    // --- Focus-restore state machine (ported from setFocus/setFocusInternal
    // and its helpers) ---

    /// Set the focused component and clear any overlay focus-restore. Ported
    /// from `setFocus`.
    pub fn set_focus(&mut self, component: Option<ComponentId>) {
        self.set_focus_internal(component, OverlayFocusRestorePolicy::Clear);
    }

    fn set_focus_internal(
        &mut self,
        component: Option<ComponentId>,
        overlay_focus_restore: OverlayFocusRestorePolicy,
    ) {
        let previous_focus = self.focused_component;
        let mut next_focus = component;
        let previous_focused_overlay: Option<OverlayHandleId> = previous_focus.and_then(|pf| {
            self.overlay_stack
                .iter()
                .find(|e| e.component == pf && self.is_overlay_visible_entry(e))
                .map(|e| e.handle_id)
        });
        let next_focus_is_overlay = next_focus
            .map(|nf| self.overlay_stack.iter().any(|e| e.component == nf))
            .unwrap_or(false);
        let restore_state = self.get_visible_overlay_focus_restore();

        if let Some(nf) = next_focus {
            if !next_focus_is_overlay {
                if let OverlayFocusRestoreState::Blocked {
                    overlay,
                    blocked_by,
                    resume,
                } = &restore_state
                {
                    if Some(*blocked_by) == previous_focus {
                        let is_focus_target =
                            matches!(resume, OverlayBlockedFocusResume::FocusTarget(_));
                        if is_focus_target || !self.is_component_mounted(*blocked_by) {
                            let (overlay, resume) = (*overlay, resume.clone());
                            next_focus =
                                self.resolve_blocked_overlay_focus_resume(overlay, &resume);
                        } else {
                            self.overlay_focus_restore = OverlayFocusRestoreState::Blocked {
                                overlay: *overlay,
                                blocked_by: nf,
                                resume: resume.clone(),
                            };
                        }
                        // handled; fall through to focus assignment below
                        self.finish_set_focus(next_focus);
                        return;
                    }
                }
                // else-if branch: block the previous focused overlay
                if let Some(pfo) = previous_focused_overlay {
                    if !restore_state.is_inactive()
                        && restore_state.overlay_handle() == Some(pfo)
                        && !self.is_overlay_focus_ancestor(pfo, nf)
                    {
                        self.overlay_focus_restore = OverlayFocusRestoreState::Blocked {
                            overlay: pfo,
                            blocked_by: nf,
                            resume: OverlayBlockedFocusResume::RestoreOverlay,
                        };
                    }
                }
            }
        } else {
            // nextFocus === null
            let mut handled = false;
            if let OverlayFocusRestoreState::Blocked {
                overlay,
                blocked_by,
                resume,
            } = &restore_state
            {
                if Some(*blocked_by) == previous_focus {
                    let (overlay, resume) = (*overlay, resume.clone());
                    next_focus = self.resolve_blocked_overlay_focus_resume(overlay, &resume);
                    handled = true;
                }
            }
            if !handled && overlay_focus_restore == OverlayFocusRestorePolicy::Clear {
                self.clear_overlay_focus_restore();
            }
        }

        self.finish_set_focus(next_focus);
    }

    /// The tail of `setFocusInternal`: assign focus and mark the newly focused
    /// overlay eligible for restore.
    fn finish_set_focus(&mut self, next_focus: Option<ComponentId>) {
        self.focused_component = next_focus;
        if let Some(nf) = next_focus {
            if let Some(handle) = self
                .overlay_stack
                .iter()
                .find(|e| e.component == nf && self.is_overlay_visible_entry(e))
                .map(|e| e.handle_id)
            {
                self.overlay_focus_restore = OverlayFocusRestoreState::Eligible { overlay: handle };
            }
        }
    }

    fn clear_overlay_focus_restore(&mut self) {
        self.overlay_focus_restore = OverlayFocusRestoreState::Inactive;
    }

    fn clear_overlay_focus_restore_for(&mut self, handle: OverlayHandleId) {
        if !self.overlay_focus_restore.is_inactive()
            && self.overlay_focus_restore.overlay_handle() == Some(handle)
        {
            self.clear_overlay_focus_restore();
        }
    }

    fn resolve_blocked_overlay_focus_resume(
        &mut self,
        overlay: OverlayHandleId,
        resume: &OverlayBlockedFocusResume,
    ) -> Option<ComponentId> {
        match resume {
            OverlayBlockedFocusResume::RestoreOverlay => self.handle_component(overlay),
            OverlayBlockedFocusResume::FocusTarget(target) => {
                self.clear_overlay_focus_restore();
                *target
            }
        }
    }

    fn get_visible_overlay_focus_restore(&self) -> OverlayFocusRestoreState {
        if self.overlay_focus_restore.is_inactive() {
            return OverlayFocusRestoreState::Inactive;
        }
        let handle = self.overlay_focus_restore.overlay_handle();
        let present_visible = handle
            .and_then(|h| self.overlay_stack.iter().find(|e| e.handle_id == h))
            .map(|e| self.is_overlay_visible_entry(e))
            .unwrap_or(false);
        if present_visible {
            self.overlay_focus_restore.clone()
        } else {
            OverlayFocusRestoreState::Inactive
        }
    }

    fn is_overlay_focus_ancestor(&self, entry: OverlayHandleId, component: ComponentId) -> bool {
        let mut visited: Vec<ComponentId> = Vec::new();
        let mut current = self
            .overlay_stack
            .iter()
            .find(|e| e.handle_id == entry)
            .and_then(|e| e.pre_focus);
        while let Some(cur) = current {
            if visited.contains(&cur) {
                break;
            }
            visited.push(cur);
            if cur == component {
                return true;
            }
            current = self
                .overlay_stack
                .iter()
                .find(|e| e.component == cur)
                .and_then(|e| e.pre_focus);
        }
        false
    }

    fn retarget_overlay_pre_focus(&mut self, removed: OverlayHandleId) {
        let (removed_component, removed_pre_focus) =
            match self.overlay_stack.iter().find(|e| e.handle_id == removed) {
                Some(e) => (e.component, e.pre_focus),
                None => return,
            };
        for overlay in &mut self.overlay_stack {
            if overlay.handle_id != removed && overlay.pre_focus == Some(removed_component) {
                overlay.pre_focus = removed_pre_focus;
            }
        }
    }

    // --- Overlay stack management (ported from showOverlay/hideOverlay and the
    // OverlayHandle closures) ---

    /// Show an overlay, returning a handle id for controlling it. Ported from
    /// `showOverlay`.
    pub fn show_overlay(
        &mut self,
        component: ComponentId,
        options: OverlayOptions,
    ) -> OverlayHandleId {
        self.focus_order_counter += 1;
        self.handle_id_counter += 1;
        let handle_id = self.handle_id_counter;
        let non_capturing = options.non_capturing;
        let entry = OverlayStackEntry {
            handle_id,
            component,
            options,
            pre_focus: self.focused_component,
            hidden: false,
            focus_order: self.focus_order_counter,
        };
        self.overlay_stack.push(entry);
        let idx = self.overlay_stack.len() - 1;
        let visible = self.is_overlay_visible_entry(&self.overlay_stack[idx]);
        if !non_capturing && visible {
            self.set_focus(Some(component));
        }
        self.terminal.hide_cursor();
        self.request_render(false);
        handle_id
    }

    /// Hide the topmost overlay and restore previous focus. Ported from
    /// `hideOverlay`.
    pub fn hide_overlay(&mut self) {
        let Some(entry) = self.overlay_stack.last() else {
            return;
        };
        let handle = entry.handle_id;
        let component = entry.component;
        let pre_focus = entry.pre_focus;
        self.clear_overlay_focus_restore_for(handle);
        self.retarget_overlay_pre_focus(handle);
        self.overlay_stack.pop();
        self.finish_overlay_removal(component, pre_focus);
    }

    /// Shared tail of overlay removal (pi's `hideOverlay` and the `hide`
    /// closure are byte-identical from here on): if the removed overlay held
    /// focus, restore to the topmost visible overlay or the removed overlay's
    /// preFocus; hide the cursor when the stack empties; request a render.
    fn finish_overlay_removal(&mut self, component: ComponentId, pre_focus: Option<ComponentId>) {
        if self.focused_component == Some(component) {
            let top = self.get_topmost_visible_overlay();
            let target = top.and_then(|h| self.handle_component(h)).or(pre_focus);
            self.set_focus(target);
        }
        if self.overlay_stack.is_empty() {
            self.terminal.hide_cursor();
        }
        self.request_render(false);
    }

    /// Whether any overlay is currently visible. Ported from `hasOverlay`.
    pub fn has_overlay(&self) -> bool {
        self.overlay_stack
            .iter()
            .any(|e| self.is_overlay_visible_entry(e))
    }

    fn get_topmost_visible_overlay(&self) -> Option<OverlayHandleId> {
        let mut topmost: Option<&OverlayStackEntry> = None;
        for e in &self.overlay_stack {
            if e.options.non_capturing || !self.is_overlay_visible_entry(e) {
                continue;
            }
            if topmost.is_none() || e.focus_order > topmost.unwrap().focus_order {
                topmost = Some(e);
            }
        }
        topmost.map(|e| e.handle_id)
    }

    /// `handle.hide()`: permanently remove the overlay.
    pub fn overlay_hide(&mut self, handle: OverlayHandleId) {
        let Some(idx) = self.entry_index(handle) else {
            return;
        };
        let component = self.overlay_stack[idx].component;
        let pre_focus = self.overlay_stack[idx].pre_focus;
        self.clear_overlay_focus_restore_for(handle);
        self.retarget_overlay_pre_focus(handle);
        self.overlay_stack.remove(idx);
        self.finish_overlay_removal(component, pre_focus);
    }

    /// `handle.setHidden(hidden)`.
    pub fn overlay_set_hidden(&mut self, handle: OverlayHandleId, hidden: bool) {
        let Some(idx) = self.entry_index(handle) else {
            return;
        };
        if self.overlay_stack[idx].hidden == hidden {
            return;
        }
        self.overlay_stack[idx].hidden = hidden;
        let component = self.overlay_stack[idx].component;
        let pre_focus = self.overlay_stack[idx].pre_focus;
        let non_capturing = self.overlay_stack[idx].options.non_capturing;
        if hidden {
            self.clear_overlay_focus_restore_for(handle);
            if self.focused_component == Some(component) {
                let top = self.get_topmost_visible_overlay();
                let target = top.and_then(|h| self.handle_component(h)).or(pre_focus);
                self.set_focus(target);
            }
        } else {
            let visible = self.is_overlay_visible_entry(&self.overlay_stack[idx]);
            if !non_capturing && visible {
                self.focus_order_counter += 1;
                self.overlay_stack[idx].focus_order = self.focus_order_counter;
                self.set_focus(Some(component));
            }
        }
        self.request_render(false);
    }

    /// `handle.isHidden()`.
    pub fn overlay_is_hidden(&self, handle: OverlayHandleId) -> bool {
        self.entry_index(handle)
            .map(|idx| self.overlay_stack[idx].hidden)
            .unwrap_or(false)
    }

    /// `handle.focus()`.
    pub fn overlay_focus(&mut self, handle: OverlayHandleId) {
        let Some(idx) = self.entry_index(handle) else {
            return;
        };
        if !self.is_overlay_visible_entry(&self.overlay_stack[idx]) {
            return;
        }
        let component = self.overlay_stack[idx].component;
        self.focus_order_counter += 1;
        self.overlay_stack[idx].focus_order = self.focus_order_counter;
        self.set_focus(Some(component));
        self.request_render(false);
    }

    /// `handle.unfocus()` / `handle.unfocus({ target })`. `options` is `None`
    /// for no options, `Some(target)` for `{ target }`.
    pub fn overlay_unfocus(
        &mut self,
        handle: OverlayHandleId,
        options: Option<Option<ComponentId>>,
    ) {
        let Some(idx) = self.entry_index(handle) else {
            return;
        };
        let component = self.overlay_stack[idx].component;
        let pre_focus = self.overlay_stack[idx].pre_focus;
        let is_focused = self.focused_component == Some(component);
        let restore_state = self.overlay_focus_restore.clone();
        let has_pending_restore =
            !restore_state.is_inactive() && restore_state.overlay_handle() == Some(handle);
        if !is_focused && !has_pending_restore {
            return;
        }
        if let OverlayFocusRestoreState::Blocked {
            overlay,
            blocked_by,
            ..
        } = &restore_state
        {
            if *overlay == handle && self.focused_component == Some(*blocked_by) {
                if let Some(target) = options {
                    self.overlay_focus_restore = OverlayFocusRestoreState::Blocked {
                        overlay: handle,
                        blocked_by: *blocked_by,
                        resume: OverlayBlockedFocusResume::FocusTarget(target),
                    };
                } else {
                    self.clear_overlay_focus_restore();
                }
                self.request_render(false);
                return;
            }
        }
        self.clear_overlay_focus_restore_for(handle);
        if is_focused || options.is_some() {
            let top = self.get_topmost_visible_overlay();
            let fallback = match top {
                Some(h) if h != handle => self.handle_component(h),
                _ => pre_focus,
            };
            let target = match options {
                Some(t) => t,
                None => fallback,
            };
            self.set_focus(target);
        }
        self.request_render(false);
    }

    /// `handle.isFocused()`.
    pub fn overlay_is_focused(&self, handle: OverlayHandleId) -> bool {
        self.handle_component(handle)
            .map(|c| self.focused_component == Some(c))
            .unwrap_or(false)
    }

    /// Handle terminal input. Ported from the focus-routing portion of
    /// `handleInput` (the OSC/color-scheme/cell-size/debug interceptors and
    /// input listeners are not part of the overlay port).
    pub fn handle_input(&mut self, data: &str) {
        // If the focused component is an overlay that is no longer visible,
        // redirect to the topmost visible overlay (or its preFocus).
        let focused_overlay = self.focused_component.and_then(|fc| {
            self.overlay_stack
                .iter()
                .find(|e| e.component == fc)
                .map(|e| e.handle_id)
        });
        if let Some(h) = focused_overlay {
            let visible = self
                .overlay_stack
                .iter()
                .find(|e| e.handle_id == h)
                .map(|e| self.is_overlay_visible_entry(e))
                .unwrap_or(false);
            if !visible {
                if let Some(top) = self.get_topmost_visible_overlay() {
                    let c = self.handle_component(top);
                    self.set_focus(c);
                } else {
                    let pre = self
                        .overlay_stack
                        .iter()
                        .find(|e| e.handle_id == h)
                        .and_then(|e| e.pre_focus);
                    self.set_focus_internal(pre, OverlayFocusRestorePolicy::Preserve);
                }
            }
        }

        let focus_is_overlay = self
            .focused_component
            .map(|fc| self.overlay_stack.iter().any(|e| e.component == fc))
            .unwrap_or(false);
        if !focus_is_overlay {
            let restore_state = self.get_visible_overlay_focus_restore();
            match restore_state {
                OverlayFocusRestoreState::Eligible { overlay } => {
                    let c = self.handle_component(overlay);
                    self.set_focus(c);
                }
                OverlayFocusRestoreState::Blocked {
                    overlay,
                    blocked_by,
                    resume,
                } if Some(blocked_by) != self.focused_component => match resume {
                    OverlayBlockedFocusResume::RestoreOverlay => {
                        let c = self.handle_component(overlay);
                        self.set_focus(c);
                    }
                    OverlayBlockedFocusResume::FocusTarget(target) => {
                        self.clear_overlay_focus_restore();
                        self.set_focus(target);
                    }
                },
                _ => {}
            }
        }

        // Deliver to the focused component and run its scripted reaction.
        if let Some(fc) = self.focused_component {
            self.input_deliveries.push((fc, data.to_string()));
            if let Some(actions) = self.input_reactions.get(&(fc, data.to_string())).cloned() {
                self.apply_reactions(&actions);
            }
            self.request_render(false);
        }
    }

    fn apply_reactions(&mut self, actions: &[ReactionAction]) {
        for action in actions {
            match action {
                ReactionAction::SetFocus(target) => self.set_focus(*target),
                ReactionAction::ClearBase => self.clear_base(),
                ReactionAction::MountBase(id) => self.mount_base(*id),
                ReactionAction::HideOverlay => self.hide_overlay(),
                ReactionAction::CloseOverlay(handle) => self.overlay_hide(*handle),
                ReactionAction::Unfocus(handle) => self.overlay_unfocus(*handle, None),
                ReactionAction::UnfocusTarget(handle, target) => {
                    self.overlay_unfocus(*handle, Some(*target))
                }
            }
        }
    }

    // --- Layout + compositing (ported from resolveOverlayLayout,
    // resolveAnchorRow/Col, compositeOverlays, compositeLineAt) ---

    #[allow(clippy::too_many_arguments)]
    fn resolve_overlay_layout(
        options: &OverlayOptions,
        overlay_height: i64,
        term_width: i64,
        term_height: i64,
    ) -> (i64, i64, i64, Option<i64>) {
        let (margin_top, margin_right, margin_bottom, margin_left) = match options.margin {
            Some(MarginSpec::All(m)) => (m.max(0), m.max(0), m.max(0), m.max(0)),
            Some(MarginSpec::Sides(s)) => (
                s.top.unwrap_or(0).max(0),
                s.right.unwrap_or(0).max(0),
                s.bottom.unwrap_or(0).max(0),
                s.left.unwrap_or(0).max(0),
            ),
            None => (0, 0, 0, 0),
        };

        let avail_width = (term_width - margin_left - margin_right).max(1);
        let avail_height = (term_height - margin_top - margin_bottom).max(1);

        let mut width =
            parse_size_value(options.width, term_width).unwrap_or_else(|| 80.min(avail_width));
        if let Some(min_width) = options.min_width {
            width = width.max(min_width);
        }
        width = width.min(avail_width).max(1);

        let mut max_height = parse_size_value(options.max_height, term_height);
        if let Some(mh) = max_height {
            max_height = Some(mh.min(avail_height).max(1));
        }

        let effective_height = match max_height {
            Some(mh) => overlay_height.min(mh),
            None => overlay_height,
        };

        // Row
        let mut row: i64 = if let Some(row_val) = options.row {
            match row_val {
                SizeValue::Pct(p) => {
                    let max_row = (avail_height - effective_height).max(0);
                    let percent = p / 100.0;
                    margin_top + ((max_row as f64) * percent).floor() as i64
                }
                SizeValue::Abs(n) => n,
            }
        } else {
            let anchor = options.anchor.unwrap_or(OverlayAnchor::Center);
            Self::resolve_anchor_row(anchor, effective_height, avail_height, margin_top)
        };

        // Col
        let mut col: i64 = if let Some(col_val) = options.col {
            match col_val {
                SizeValue::Pct(p) => {
                    let max_col = (avail_width - width).max(0);
                    let percent = p / 100.0;
                    margin_left + ((max_col as f64) * percent).floor() as i64
                }
                SizeValue::Abs(n) => n,
            }
        } else {
            let anchor = options.anchor.unwrap_or(OverlayAnchor::Center);
            Self::resolve_anchor_col(anchor, width, avail_width, margin_left)
        };

        if let Some(oy) = options.offset_y {
            row += oy;
        }
        if let Some(ox) = options.offset_x {
            col += ox;
        }

        row = row
            .min(term_height - margin_bottom - effective_height)
            .max(margin_top);
        col = col.min(term_width - margin_right - width).max(margin_left);

        (width, row, col, max_height)
    }

    fn resolve_anchor_row(
        anchor: OverlayAnchor,
        height: i64,
        avail_height: i64,
        margin_top: i64,
    ) -> i64 {
        match anchor {
            OverlayAnchor::TopLeft | OverlayAnchor::TopCenter | OverlayAnchor::TopRight => {
                margin_top
            }
            OverlayAnchor::BottomLeft
            | OverlayAnchor::BottomCenter
            | OverlayAnchor::BottomRight => margin_top + avail_height - height,
            OverlayAnchor::LeftCenter | OverlayAnchor::Center | OverlayAnchor::RightCenter => {
                margin_top + ((avail_height - height) as f64 / 2.0).floor() as i64
            }
        }
    }

    fn resolve_anchor_col(
        anchor: OverlayAnchor,
        width: i64,
        avail_width: i64,
        margin_left: i64,
    ) -> i64 {
        match anchor {
            OverlayAnchor::TopLeft | OverlayAnchor::LeftCenter | OverlayAnchor::BottomLeft => {
                margin_left
            }
            OverlayAnchor::TopRight | OverlayAnchor::RightCenter | OverlayAnchor::BottomRight => {
                margin_left + avail_width - width
            }
            OverlayAnchor::TopCenter | OverlayAnchor::Center | OverlayAnchor::BottomCenter => {
                margin_left + ((avail_width - width) as f64 / 2.0).floor() as i64
            }
        }
    }

    fn composite_overlays(
        &self,
        lines: Vec<String>,
        term_width: i64,
        term_height: i64,
    ) -> Vec<String> {
        if self.overlay_stack.is_empty() {
            return lines;
        }
        let mut result = lines;

        struct Rendered {
            overlay_lines: Vec<String>,
            row: i64,
            col: i64,
            w: i64,
        }
        let mut rendered: Vec<Rendered> = Vec::new();
        let mut min_lines_needed = result.len() as i64;

        // Visible entries sorted by focus_order ascending (higher = on top).
        let mut visible: Vec<&OverlayStackEntry> = self
            .overlay_stack
            .iter()
            .filter(|e| self.is_overlay_visible_entry(e))
            .collect();
        visible.sort_by_key(|e| e.focus_order);

        for entry in visible {
            let (width, _, _, max_height) =
                Self::resolve_overlay_layout(&entry.options, 0, term_width, term_height);
            let mut overlay_lines = self.components[entry.component]
                .borrow()
                .render(width.max(0) as usize);
            if let Some(mh) = max_height {
                if overlay_lines.len() as i64 > mh {
                    overlay_lines.truncate(mh.max(0) as usize);
                }
            }
            let (_, row, col, _) = Self::resolve_overlay_layout(
                &entry.options,
                overlay_lines.len() as i64,
                term_width,
                term_height,
            );
            min_lines_needed = min_lines_needed.max(row + overlay_lines.len() as i64);
            rendered.push(Rendered {
                overlay_lines,
                row,
                col,
                w: width,
            });
        }

        let working_height = (result.len() as i64).max(term_height).max(min_lines_needed);
        while (result.len() as i64) < working_height {
            result.push(String::new());
        }
        let viewport_start = (working_height - term_height).max(0);

        for r in &rendered {
            for (i, overlay_line) in r.overlay_lines.iter().enumerate() {
                let idx = viewport_start + r.row + i as i64;
                if idx >= 0 && idx < result.len() as i64 {
                    let truncated: String = if visible_width(overlay_line) as i64 > r.w {
                        slice_by_column(overlay_line, 0, r.w, true)
                    } else {
                        overlay_line.clone()
                    };
                    result[idx as usize] = Self::composite_line_at(
                        &result[idx as usize],
                        &truncated,
                        r.col,
                        r.w,
                        term_width,
                    );
                }
            }
        }

        result
    }

    fn composite_line_at(
        base_line: &str,
        overlay_line: &str,
        start_col: i64,
        overlay_width: i64,
        total_width: i64,
    ) -> String {
        if is_image_line(base_line) {
            return base_line.to_string();
        }
        let after_start = start_col + overlay_width;
        let base = extract_segments(
            base_line,
            start_col,
            after_start,
            total_width - after_start,
            true,
        );
        let (overlay_text, overlay_w) = slice_with_width(overlay_line, 0, overlay_width, true);

        let before_pad = (start_col - base.before_width).max(0);
        let overlay_pad = (overlay_width - overlay_w).max(0);
        let actual_before_width = start_col.max(base.before_width);
        let actual_overlay_width = overlay_width.max(overlay_w);
        let after_target = (total_width - actual_before_width - actual_overlay_width).max(0);
        let after_pad = (after_target - base.after_width).max(0);

        let mut result = String::new();
        result.push_str(&base.before);
        result.push_str(&" ".repeat(before_pad as usize));
        result.push_str(SEGMENT_RESET);
        result.push_str(&overlay_text);
        result.push_str(&" ".repeat(overlay_pad as usize));
        result.push_str(SEGMENT_RESET);
        result.push_str(&base.after);
        result.push_str(&" ".repeat(after_pad as usize));

        if (visible_width(&result) as i64) <= total_width {
            return result;
        }
        slice_by_column(&result, 0, total_width, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::LoggingTerminal;

    fn child(lines: &[&str]) -> (SharedLines, Box<dyn Component>) {
        let sl = SharedLines::new();
        sl.set(lines.iter().map(|s| s.to_string()).collect());
        let boxed: Box<dyn Component> = Box::new(sl.clone());
        (sl, boxed)
    }

    #[test]
    fn is_image_line_detects_kitty_and_iterm() {
        assert!(is_image_line("\x1b_Ga=T,f=100;AAAA\x1b\\"));
        assert!(is_image_line("\x1b]1337;File=inline=1:AAAA\x07"));
        assert!(is_image_line("prefix\x1b_Gi=1;x\x1b\\"));
        assert!(!is_image_line("plain text"));
        assert!(!is_image_line(""));
    }

    #[test]
    fn delete_kitty_image_sequence() {
        assert_eq!(delete_kitty_image(42), "\x1b_Ga=d,d=I,i=42,q=2\x1b\\");
    }

    #[test]
    fn width_overflow_triggers_crash_contract() {
        // A line wider than the terminal on the differential path crashes: pi
        // writes a crash log, tears down the terminal, and throws. We surface
        // that as Err after doing the same teardown.
        let dir = std::env::temp_dir().join(format!("atilla-tui-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let terminal = LoggingTerminal::new(10, 5);
        let mut tui = Tui::new(terminal, false);
        tui.set_crash_log_dir(dir.clone());

        let (handle, boxed) = child(&["short"]);
        tui.add_child(boxed);

        tui.start();
        tui.flush().expect("first render is clean");

        // Now force a too-wide line on the differential path.
        handle.set(vec!["this line is far too wide for ten cols".to_string()]);
        tui.request_render(false);
        let err = tui.flush().expect_err("overflow must crash");
        match err {
            RenderError::WidthOverflow {
                line,
                terminal_width,
                ..
            } => {
                assert_eq!(line, 0);
                assert_eq!(terminal_width, 10);
            }
        }
        // Crash log written, matching pi's contract.
        assert!(dir.join("pi-crash.log").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn composite_line_at_excludes_wide_grapheme_at_boundary() {
        // Ported from regression-overlay-cjk-boundary.test.ts: an overlay that
        // starts inside a wide grapheme must not leave a half of it in the
        // output, and the result must be exactly totalWidth wide.
        let out = Tui::<LoggingTerminal>::composite_line_at(
            "abcd\u{8ba9}EFGH",
            "\u{2502}XX\u{2502}",
            5,
            4,
            20,
        );
        assert!(
            !out.contains('\u{8ba9}'),
            "wide grapheme must be dropped: {out:?}"
        );
        assert_eq!(visible_width(&out), 20);
        assert_eq!(visible_width(&slice_by_column(&out, 0, 5, true)), 5);
        let overlay = slice_by_column(&out, 5, 4, true);
        assert_eq!(visible_width(&overlay), 4);
        assert!(
            overlay.contains("\u{2502}XX\u{2502}"),
            "overlay text present: {overlay:?}"
        );
    }

    #[test]
    fn composite_line_at_at_wide_grapheme_boundary() {
        let out = Tui::<LoggingTerminal>::composite_line_at(
            "abcd\u{8ba9}EFGH",
            "\u{2502}XX\u{2502}",
            4,
            4,
            20,
        );
        assert!(!out.contains('\u{8ba9}'));
        assert_eq!(visible_width(&out), 20);
        let overlay = slice_by_column(&out, 4, 4, true);
        assert_eq!(visible_width(&overlay), 4);
        assert!(overlay.contains("\u{2502}XX\u{2502}"));
    }

    #[test]
    fn blocked_replacement_moves_focus_before_overlay_restore() {
        // Ported from overlay-non-capturing.test.ts "blocked replacement can
        // move focus internally before overlay restore" (the isComponentMounted
        // branch, which the JSON vectors do not exercise because their focus
        // targets are standalone). editor/first/second are base-mounted; the
        // overlay's blocked restore must not fire until the replacements leave
        // the mounted tree.
        let terminal = LoggingTerminal::new(80, 24);
        let mut tui = Tui::new(terminal, false);
        // ids: 0 empty base, 1 editor, 2 first, 3 second, 4 overlay.
        for lines in [
            vec![],
            vec!["EDITOR".to_string()],
            vec!["FIRST".to_string()],
            vec!["SECOND".to_string()],
            vec!["OVERLAY".to_string()],
        ] {
            let sl = SharedLines::new();
            sl.set(lines);
            tui.register_component(Rc::new(RefCell::new(sl)));
        }
        // base = Container[editor, first, second] (all mounted).
        for id in [1, 2, 3] {
            tui.mount_base(id);
        }
        tui.set_input_reaction(4, "b", vec![ReactionAction::SetFocus(Some(2))]);
        tui.set_input_reaction(2, "n", vec![ReactionAction::SetFocus(Some(3))]);
        tui.set_input_reaction(
            3,
            "\r",
            vec![
                ReactionAction::ClearBase,
                ReactionAction::MountBase(1),
                ReactionAction::SetFocus(Some(1)),
            ],
        );
        tui.set_focus(Some(1));
        tui.show_overlay(4, OverlayOptions::default());
        assert_eq!(tui.focused_component(), Some(4));

        tui.handle_input("b");
        assert_eq!(
            tui.focused_component(),
            Some(2),
            "b focuses first replacement"
        );
        tui.handle_input("n");
        assert_eq!(
            tui.focused_component(),
            Some(3),
            "n focuses second replacement"
        );
        tui.handle_input("2");
        tui.handle_input("\r");
        assert_eq!(
            tui.focused_component(),
            Some(4),
            "carriage return resolves the blocked overlay restore"
        );

        let deliveries: Vec<(ComponentId, &str)> = tui
            .input_deliveries()
            .iter()
            .map(|(c, d)| (*c, d.as_str()))
            .collect();
        assert_eq!(
            deliveries,
            vec![(4, "b"), (2, "n"), (3, "2"), (3, "\r")],
            "input routed to the focused component at each step"
        );
    }

    #[test]
    fn cursor_marker_positions_hardware_cursor() {
        // A focusable component emits CURSOR_MARKER; the renderer strips it and
        // moves the hardware cursor to the (row, col) with show enabled.
        let terminal = LoggingTerminal::new(20, 5);
        let mut tui = Tui::new(terminal, true);
        let (_h, boxed) = child(&[&format!("ab{CURSOR_MARKER}cd")]);
        tui.add_child(boxed);
        tui.start();
        tui.flush().expect("clean render");
        let writes = tui.terminal_mut().get_writes();
        // Column 2 (width of "ab") -> \x1b[3G; marker must not appear in output.
        assert!(writes.contains("\x1b[3G"), "writes: {writes:?}");
        assert!(!writes.contains(CURSOR_MARKER));
    }
}
