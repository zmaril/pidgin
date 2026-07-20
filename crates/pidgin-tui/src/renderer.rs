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

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use crate::overlay::{ComponentId, OverlayFocusRestoreState, OverlayStackEntry, ReactionAction};
use crate::terminal::Terminal;
use crate::{normalize_terminal_output, visible_width};

/// Cursor position marker: a zero-width APC sequence terminals ignore.
/// Focusable components emit this at the cursor position when focused; the
/// renderer finds and strips it, then positions the hardware cursor there for
/// IME candidate-window placement.
pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

/// Appended to every non-image line after normalization: reset SGR + close any
/// open OSC 8 hyperlink so styles never leak into the next line.
pub(crate) const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

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

/// The outcome an input listener returns from [`Tui::add_input_listener`],
/// ported from pi's `InputListener` return union `{ consume?: boolean; data?:
/// string }` (`tui.ts`). A listener sees each raw input string before it reaches
/// the focused component and may drop it (`consume`) or rewrite it (`data`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputListenerResult {
    /// When `true`, the input is consumed and never reaches the focused
    /// component or later listeners (pi's `result.consume`).
    pub consume: bool,
    /// When `Some`, replaces the input string passed to the next listener and,
    /// ultimately, the focused component (pi's `result.data`). An empty string
    /// drops the input, exactly like pi's `current.length === 0` check.
    pub data: Option<String>,
}

impl InputListenerResult {
    /// A listener result that consumes the input (stops propagation).
    pub fn consumed() -> Self {
        Self {
            consume: true,
            data: None,
        }
    }

    /// A pass-through result: the input is unchanged and continues to the next
    /// listener / focused component.
    pub fn pass() -> Self {
        Self::default()
    }
}

/// A registered input listener. Invoked with each raw input string (a
/// [`crate::TerminalInput`] as delivered) before focus dispatch; mirrors pi's
/// `addInputListener` callback signature.
pub type InputListener = Box<dyn FnMut(&str) -> InputListenerResult>;

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

/// The main renderer. Owns the terminal sink and the mutable viewport
/// bookkeeping. Ported from pi's `TUI` (PR-R1 core subset).
pub struct Tui<T: Terminal> {
    pub(crate) terminal: T,
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
    pub(crate) components: Vec<Rc<RefCell<dyn Component>>>,
    /// The currently focused component, if any.
    pub(crate) focused_component: Option<ComponentId>,
    /// Components mounted in the base tree, for `isComponentMounted`. Populated
    /// by [`Tui::mount_base`]; empty means no registered component is a base
    /// child (the common case in the overlay tests).
    pub(crate) mounted: Vec<ComponentId>,
    /// The overlay stack (bottom-to-top by insertion; z-order by focus_order).
    pub(crate) overlay_stack: Vec<OverlayStackEntry>,
    /// The overlay focus-restore state machine.
    pub(crate) overlay_focus_restore: OverlayFocusRestoreState,
    /// Monotonic focus-order counter (higher = visually in front).
    pub(crate) focus_order_counter: u64,
    /// Monotonic handle-id counter for overlay handles.
    pub(crate) handle_id_counter: usize,
    /// Log of `(component, data)` input deliveries, for input-routing vectors.
    pub(crate) input_deliveries: Vec<(ComponentId, String)>,
    /// Scripted reactions a focused component runs on given input, keyed by
    /// `(component, data)`. pi's tests attach ad-hoc `handleInput` closures that
    /// call back into the TUI (`setFocus`, `unfocus`, tree mutation); encoding
    /// them as data lets `handle_input` apply them without re-entrant borrows.
    pub(crate) input_reactions:
        std::collections::HashMap<(ComponentId, String), Vec<ReactionAction>>,
    /// Registered input listeners (pi's `inputListeners` set). Each is offered
    /// every input string before focus dispatch and may consume or rewrite it.
    pub(crate) input_listeners: Vec<InputListener>,
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
            input_listeners: Vec::new(),
        }
    }

    /// Register an input listener, ported from pi's `TUI.addInputListener`. The
    /// listener is offered every input string (in registration order) before it
    /// reaches the focused component and can drop or rewrite it via
    /// [`InputListenerResult`]. Unlike pi (which returns an unsubscribe closure),
    /// listeners live for the lifetime of the `Tui`; this matches the run loop's
    /// usage, where the shell registers its exit-policy listener once at startup.
    pub fn add_input_listener<F>(&mut self, listener: F)
    where
        F: FnMut(&str) -> InputListenerResult + 'static,
    {
        self.input_listeners.push(Box::new(listener));
    }

    /// Add a child component (delegates to the embedded container).
    pub fn add_child(&mut self, component: Box<dyn Component>) {
        self.container.add_child(component);
    }

    /// Remove all children.
    pub fn clear(&mut self) {
        self.container.clear();
    }

    /// Replace the entire base frame with `lines`, backed by a single
    /// [`SharedLines`] child. Clears any existing base children and installs one
    /// line-buffer child that renders exactly `lines`. Overlays are untouched:
    /// only the base container is swapped (the conformance shim uses this to feed
    /// pi's TS-rendered lines into the Rust renderer wholesale).
    pub fn set_base_lines(&mut self, lines: Vec<String>) {
        self.container.clear();
        let child = SharedLines::new();
        child.set(lines);
        self.container.add_child(Box::new(child));
    }

    /// Access the terminal backend (e.g. to resize or inspect a logging sink).
    pub fn terminal_mut(&mut self) -> &mut T {
        &mut self.terminal
    }

    /// Shared access to the terminal backend (e.g. to query dimensions or pending
    /// input/negotiation state from the run loop without a mutable borrow).
    pub fn terminal(&self) -> &T {
        &self.terminal
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

impl Tui<crate::terminal::LoggingTerminal> {
    /// Drain the accumulated write stream: return every recorded `write()` since
    /// the last clear and reset the sink, i.e. `get_writes()` followed by
    /// `clear_writes()` on the underlying [`LoggingTerminal`]. This is only
    /// available for the logging sink because `get_writes`/`clear_writes` are not
    /// part of the [`Terminal`] trait (the real [`CrosstermTerminal`] has no
    /// recorded stream to drain).
    pub fn take_writes(&mut self) -> String {
        let writes = self.terminal.get_writes();
        self.terminal.clear_writes();
        writes
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
        let dir = std::env::temp_dir().join(format!("pidgin-tui-crash-{}", std::process::id()));
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
    fn set_base_lines_replaces_whole_base_frame() {
        // set_base_lines installs a single line-buffer child rendering exactly the
        // given lines; a subsequent call replaces (does not append to) the frame.
        let terminal = LoggingTerminal::new(20, 5);
        let mut tui = Tui::new(terminal, false);

        tui.set_base_lines(vec!["alpha".to_string(), "beta".to_string()]);
        tui.start();
        tui.flush().expect("clean first render");
        let writes = tui.take_writes();
        assert!(writes.contains("alpha"), "writes: {writes:?}");
        assert!(writes.contains("beta"), "writes: {writes:?}");
        assert_eq!(
            tui.render(20),
            vec!["alpha".to_string(), "beta".to_string()]
        );

        // Replace with a different, single-line frame: the old lines must be gone.
        tui.set_base_lines(vec!["gamma".to_string()]);
        tui.request_render(false);
        tui.flush().expect("clean second render");
        assert_eq!(tui.render(20), vec!["gamma".to_string()]);
        assert!(!tui.render(20).contains(&"alpha".to_string()));
        assert!(!tui.render(20).contains(&"beta".to_string()));
    }

    #[test]
    fn take_writes_returns_and_clears_stream() {
        // take_writes returns the accumulated writes and empties the sink, so a
        // second call returns nothing.
        let terminal = LoggingTerminal::new(20, 5);
        let mut tui = Tui::new(terminal, false);
        tui.set_base_lines(vec!["hello".to_string()]);
        tui.start();
        tui.flush().expect("clean render");

        let first = tui.take_writes();
        assert!(!first.is_empty());
        assert!(first.contains("hello"), "writes: {first:?}");

        let second = tui.take_writes();
        assert!(second.is_empty(), "second take must be empty: {second:?}");
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
