//! Overlay compositing and the overlay focus-restore state machine (PR-R2),
//! split out of `renderer.rs` to keep each module a reasonable size. Ported
//! from the overlay methods of pi's `TUI` in `vendor/pi/packages/tui/src/tui.ts`.
//!
//! Component identity, which pi obtains from JavaScript reference equality, is
//! provided here by [`ComponentId`]; the methods below are an `impl` block on
//! the [`Tui`](crate::renderer::Tui) type defined in `renderer.rs`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::renderer::{Component, Tui, SEGMENT_RESET};
use crate::terminal::Terminal;
use crate::terminal_colors::parse_terminal_color_scheme_report;
use crate::{
    extract_segments, is_image_line, is_key_release, slice_by_column, slice_with_width,
    visible_width,
};

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
pub(crate) enum OverlayBlockedFocusResume {
    RestoreOverlay,
    FocusTarget(Option<ComponentId>),
}

/// The overlay focus-restore state machine. Ported from
/// `OverlayFocusRestoreState`. `overlay` is stored as a handle id so the state
/// survives the entry being spliced out of the stack (pi keeps the object
/// reference alive through the closure).
#[derive(Debug, Clone)]
pub(crate) enum OverlayFocusRestoreState {
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
pub(crate) struct OverlayStackEntry {
    handle_id: OverlayHandleId,
    component: ComponentId,
    options: OverlayOptions,
    pre_focus: Option<ComponentId>,
    hidden: bool,
    focus_order: u64,
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

    /// If `component` currently holds focus, hand it to the topmost visible
    /// overlay, falling back to `pre_focus`. This is the focus-restore idiom pi
    /// repeats in `hideOverlay`, the `hide` closure, and `setHidden`.
    fn restore_focus_from(&mut self, component: ComponentId, pre_focus: Option<ComponentId>) {
        if self.focused_component == Some(component) {
            let top = self.get_topmost_visible_overlay();
            let target = top.and_then(|h| self.handle_component(h)).or(pre_focus);
            self.set_focus(target);
        }
    }

    /// Shared tail of overlay removal (pi's `hideOverlay` and the `hide`
    /// closure are byte-identical from here on): restore focus, hide the cursor
    /// when the stack empties, and request a render.
    fn finish_overlay_removal(&mut self, component: ComponentId, pre_focus: Option<ComponentId>) {
        self.restore_focus_from(component, pre_focus);
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
            self.restore_focus_from(component, pre_focus);
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

    /// Consume a DEC private mode 2031 color-scheme report, ported from pi's
    /// `TUI.consumeTerminalColorSchemeReport`. If `data` parses as a color-scheme
    /// report, each registered listener is invoked with the reported scheme and
    /// the input is consumed (`true`); otherwise this is a no-op (`false`).
    fn consume_terminal_color_scheme_report(&mut self, data: &str) -> bool {
        let scheme = match parse_terminal_color_scheme_report(data) {
            Some(scheme) => scheme,
            None => return false,
        };
        for listener in self.terminal_color_scheme_listeners.iter_mut() {
            listener(scheme);
        }
        true
    }

    /// Invalidate every overlay component's cached render state. Used by
    /// [`Tui::invalidate`](crate::renderer::Tui::invalidate) to reach the overlay
    /// stack, which lives in this module (pi's `override invalidate` loop over
    /// `overlayStack`).
    pub(crate) fn invalidate_overlays(&mut self) {
        for entry in &self.overlay_stack {
            if let Some(component) = self.components.get(entry.component) {
                component.borrow_mut().invalidate();
            }
        }
    }

    /// Handle terminal input. Ported from `TUI.handleInput` (`tui.ts`): first a
    /// DEC 2031 color-scheme report is consumed and fanned out to the
    /// color-scheme listeners; then the registered input listeners get a chance to
    /// consume or rewrite the input; then the overlay focus-restore reconciliation
    /// runs, and finally the input is delivered to the focused component. The
    /// remaining OSC/cell-size/debug interceptors pi runs before the listeners are
    /// not part of this port (the run loop feeds already-decoded
    /// [`crate::TerminalInput`]s, so those terminal query replies are handled
    /// inside [`crate::ProcessTerminal::feed`]).
    pub fn handle_input(&mut self, data: &str) {
        // pi's `consumeTerminalColorSchemeReport` runs first: a DEC 2031
        // color-scheme report is consumed (never delivered to the focused
        // component) and fanned out to the registered listeners.
        if self.consume_terminal_color_scheme_report(data) {
            return;
        }

        // Offer the input to each listener in registration order (pi's
        // `inputListeners` loop). A listener may consume the input outright or
        // rewrite it; an empty rewrite drops it. Listeners are taken out during
        // the call so a listener that mutates the Tui cannot alias the vector.
        let mut current = data.to_string();
        if !self.input_listeners.is_empty() {
            let mut listeners = std::mem::take(&mut self.input_listeners);
            let mut dropped = false;
            for listener in listeners.iter_mut() {
                let result = listener(&current);
                if result.consume {
                    dropped = true;
                    break;
                }
                if let Some(next) = result.data {
                    current = next;
                }
            }
            self.input_listeners = listeners;
            if dropped || current.is_empty() {
                return;
            }
        }
        let data = current.as_str();

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

        // Deliver to the focused component. This fills the seam pi calls
        // `this.focusedComponent.handleInput(data)`: the delivery is recorded for
        // the input-routing vectors, the real registered component's
        // `handle_input` is invoked (honoring the Kitty key-release opt-in), and
        // any scripted reaction runs. Key-release events are dropped unless the
        // component opts in via `wants_key_release`, matching pi's filter.
        if let Some(fc) = self.focused_component {
            self.input_deliveries.push((fc, data.to_string()));
            if let Some(component) = self.components.get(fc).cloned() {
                let wants_release = component.borrow().wants_key_release();
                if !is_key_release(data) || wants_release {
                    component.borrow_mut().handle_input(data);
                }
            }
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

    pub(crate) fn composite_overlays(
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
    use crate::renderer::SharedLines;
    use crate::terminal::LoggingTerminal;

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
}
