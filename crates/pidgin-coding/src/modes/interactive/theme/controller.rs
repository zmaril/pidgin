//! The interactive-layer theme controller.
//!
//! Faithful 1:1 port of pi's
//! `modes/interactive/theme/theme-controller.ts` (`InteractiveThemeController`).
//! Drives theme selection for the interactive shell: applies the saved / auto /
//! terminal-detected theme at startup, supports live preview + set-by-name +
//! set-by-instance, and reacts to terminal color-scheme changes (auto-sync) by
//! swapping the shared [`ActiveTheme`] and re-rendering.
//!
//! # Shape divergences from pi (all behavior-preserving)
//!
//! - **`apply_from_settings` is synchronous** where pi's is `async`. pi
//!   `await`s `detectTerminalThemeForAuto` / `detectTerminalBackgroundTheme`
//!   and `settingsManager.flush()`; pidgin's TUI stack is a synchronous
//!   poll-based run loop with no async reactor, so those become blocking calls
//!   over the injected [`TerminalAutoThemeDetector`] with a 100ms deadline. This
//!   is the same intentional async-to-sync divergence documented on prereq C's
//!   `detect_terminal_background_theme` / `detect_terminal_theme_for_auto`. The
//!   query bytes, query semantics, timeout, and branch logic are identical.
//!
//! - **The `ui` surface is passed per call** rather than stored. pi stores
//!   `this.ui: TUI` and every method reads it. In Rust the shell (`RunLoop`)
//!   owns the `Tui`, so threading `&mut impl ThemeControllerUi` into each method
//!   that needs it avoids a self-referential `&mut Tui` borrow held for the
//!   controller's lifetime. Behavior is identical — the same `invalidate` /
//!   `request_render` / `set_terminal_color_scheme_notifications` / query calls
//!   fire in the same order.
//!
//! - **The constructor does not subscribe to color-scheme changes.** pi's
//!   constructor calls `ui.onTerminalColorSchemeChange(t => this.applyTerminalTheme(t))`
//!   (line 34). A `Tui` input listener is `FnMut + 'static` and fires during a
//!   `&mut Tui` borrow, so it cannot also hold `&mut` the controller and the same
//!   `Tui`. The shell wires the equivalent route explicitly by forwarding the
//!   color-scheme report into [`InteractiveThemeController::apply_terminal_theme`].
//!   Everything else the constructor does (compute `active_theme_name` via
//!   [`resolve_theme_setting`], call [`ActiveTheme::init`]) is reproduced 1:1.
//!
//! - **`settings` / `active` are shared handles.** pi reaches a module-global
//!   `SettingsManager` and the module-global `theme` runtime; pidgin threads
//!   `Rc<RefCell<SettingsManager>>` and `Rc<ActiveTheme>` (the crate's shared,
//!   non-ambient convention), plus a [`ThemeSource`] carrying the custom-themes
//!   dir / color mode / env that pi's module functions read from global config.
// straitjacket-allow-file:duplication

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use crate::core::settings_manager::SettingsManager;

use super::{
    detect_terminal_background_from_env, detect_terminal_background_theme,
    detect_terminal_theme_for_auto, parse_auto_theme_setting, resolve_theme_setting, ActiveTheme,
    SetThemeResult, TerminalAutoThemeDetector, TerminalTheme, ThemeSource,
};

/// The terminal-query deadline for the auto / detected theme paths. Mirrors pi's
/// `{ timeoutMs: 100 }` in both `applyFromSettings` branches.
const DETECT_TIMEOUT: Duration = Duration::from_millis(100);

/// Outcome of a theme-application call. Mirrors pi's controller-local
/// `type ThemeResult = { success: boolean; error?: string }`, which is
/// structurally identical to [`ActiveTheme::set`]'s [`SetThemeResult`]; the two
/// are aliased so the controller returns exactly what the active-theme runtime
/// produced.
pub type ThemeResult = SetThemeResult;

/// The subset of the `Tui` surface the controller drives, plus the terminal
/// color-scheme query capability. Mirrors the members of pi's `this.ui` that
/// `InteractiveThemeController` uses: `invalidate`, `requestRender`,
/// `setTerminalColorSchemeNotifications`, and (via the detect functions)
/// `queryTerminalBackgroundColor` / `queryTerminalColorScheme`.
///
/// `onTerminalColorSchemeChange` is intentionally not on this trait — the shell
/// wires that subscription and routes reports into
/// [`InteractiveThemeController::apply_terminal_theme`] (see the module docs).
pub trait ThemeControllerUi: TerminalAutoThemeDetector {
    /// Invalidate every component's cached render state. Mirrors `ui.invalidate()`.
    fn invalidate(&mut self);
    /// Request a redraw on the next frame. Mirrors `ui.requestRender()`.
    fn request_render(&mut self);
    /// Enable / disable DEC 2031 color-scheme change notifications. Mirrors
    /// `ui.setTerminalColorSchemeNotifications(enabled)`.
    fn set_terminal_color_scheme_notifications(&mut self, enabled: bool);
}

/// Drives interactive theme selection. Faithful port of pi's
/// `InteractiveThemeController`.
pub struct InteractiveThemeController {
    /// Shared settings manager (pi's `this.settingsManager`).
    settings: Rc<RefCell<SettingsManager>>,
    /// Shared active-theme runtime (pi reaches the module-global `theme` runtime;
    /// pidgin threads this handle).
    active: Rc<ActiveTheme>,
    /// The custom-themes dir / color mode / env pi's module theme functions read
    /// from global config. Threaded into [`ActiveTheme::init`] / [`ActiveTheme::set`].
    source: ThemeSource,
    /// Error sink (pi's `this.showError`).
    show_error: Box<dyn Fn(&str)>,
    /// Change hook run after every applied theme change (pi's `this.onChanged`,
    /// wired by the shell to `updateEditorBorderColor`).
    on_changed: Box<dyn Fn()>,
    /// The last known terminal theme (pi's `this.terminalTheme`).
    terminal_theme: TerminalTheme,
    /// The active theme name, or `None` when a malformed setting resolves to
    /// nothing (pi's `this.activeThemeName: string | undefined`).
    active_theme_name: Option<String>,
    /// Whether auto-sync to terminal color-scheme changes is on (pi's
    /// `this.autoSyncEnabled`).
    auto_sync_enabled: bool,
}

impl InteractiveThemeController {
    /// Construct the controller. Mirrors pi's constructor: seed `terminalTheme`
    /// from the environment, resolve the active theme name from the saved setting
    /// against that terminal theme, and initialize the active theme (with the
    /// file watcher requested, matching pi's `initTheme(name, true)`).
    ///
    /// Unlike pi it does **not** subscribe to color-scheme changes here; the shell
    /// wires that (see the module docs).
    pub fn new(
        settings: Rc<RefCell<SettingsManager>>,
        active: Rc<ActiveTheme>,
        source: ThemeSource,
        show_error: Box<dyn Fn(&str)>,
        on_changed: Box<dyn Fn()>,
    ) -> Self {
        let terminal_theme = detect_terminal_background_from_env(&source.env).theme;
        let active_theme_name = resolve_theme_setting(
            settings.borrow().get_theme_setting().as_deref(),
            terminal_theme,
        );
        active.init(active_theme_name.as_deref(), &source, true);
        Self {
            settings,
            active,
            source,
            show_error,
            on_changed,
            terminal_theme,
            active_theme_name,
            auto_sync_enabled: false,
        }
    }

    /// Apply the saved / auto / detected theme. Synchronous port of pi's
    /// `applyFromSettings`:
    ///
    /// - **auto** (`light/dark` setting): detect the terminal theme, enable
    ///   auto-sync, apply the matching side (surfacing load errors).
    /// - **plain** (a bare theme name): disable auto-sync, apply it (surfacing
    ///   load errors).
    /// - **unset**: disable auto-sync, detect the terminal background theme, apply
    ///   it (silently); if that succeeds and the detection is high-confidence,
    ///   persist it to settings and flush.
    pub fn apply_from_settings(&mut self, ui: &mut impl ThemeControllerUi) {
        let theme_setting = self.settings.borrow().get_theme_setting();
        let auto_theme = parse_auto_theme_setting(theme_setting.as_deref());
        if let Some(auto_theme) = auto_theme {
            self.terminal_theme =
                detect_terminal_theme_for_auto(ui, DETECT_TIMEOUT, &self.source.env);
            self.set_auto_sync(ui, true);
            let name = if self.terminal_theme == TerminalTheme::Light {
                auto_theme.light_theme
            } else {
                auto_theme.dark_theme
            };
            self.apply_theme_name(ui, &name, true);
            return;
        }

        self.set_auto_sync(ui, false);
        if let Some(theme_setting) = theme_setting {
            self.apply_theme_name(ui, &theme_setting, true);
            return;
        }

        let detection = detect_terminal_background_theme(ui, DETECT_TIMEOUT, &self.source.env);
        self.terminal_theme = detection.theme;
        if !self
            .apply_theme_name(ui, terminal_theme_name(detection.theme), false)
            .success
        {
            return;
        }
        if detection.confidence == super::Confidence::High {
            self.settings
                .borrow_mut()
                .set_theme(terminal_theme_name(detection.theme));
            self.settings.borrow().flush();
        }
    }

    /// Set the active theme by name, disabling auto-sync first. Mirrors pi's
    /// `setThemeName`.
    pub fn set_theme_name(
        &mut self,
        ui: &mut impl ThemeControllerUi,
        theme_name: &str,
        show_error: bool,
    ) -> ThemeResult {
        self.set_auto_sync(ui, false);
        self.apply_theme_name(ui, theme_name, show_error)
    }

    /// Swap in a directly-constructed theme instance, disabling auto-sync first.
    /// Mirrors pi's `setThemeInstance`.
    pub fn set_theme_instance(
        &mut self,
        ui: &mut impl ThemeControllerUi,
        theme_instance: super::Theme,
    ) -> ThemeResult {
        self.set_auto_sync(ui, false);
        self.active.set_instance(theme_instance);
        self.active_theme_name = Some("<in-memory>".to_string());
        self.notify_changed(ui);
        ThemeResult {
            success: true,
            error: None,
        }
    }

    /// Preview a theme setting-or-name without touching auto-sync or persistence:
    /// swap the active theme, then invalidate + request a render. Mirrors pi's
    /// `preview`.
    pub fn preview(&mut self, ui: &mut impl ThemeControllerUi, theme_setting_or_name: &str) {
        let theme_name = resolve_theme_setting(Some(theme_setting_or_name), self.terminal_theme)
            .or_else(|| self.active_theme_name.clone());
        let Some(theme_name) = theme_name else {
            return;
        };
        if self.active.set(&theme_name, &self.source, true).success {
            ui.invalidate();
            ui.request_render();
        }
    }

    /// Turn off auto-sync. Mirrors pi's `disableAutoSync`.
    pub fn disable_auto_sync(&mut self, ui: &mut impl ThemeControllerUi) {
        self.set_auto_sync(ui, false);
    }

    /// The last known terminal theme. Mirrors pi's `getTerminalTheme` (a pure
    /// read).
    pub fn get_terminal_theme(&self) -> TerminalTheme {
        self.terminal_theme
    }

    /// Apply a theme by name: swap the active theme, track the resulting name
    /// (`"dark"` on failure, matching the active-theme fallback), notify, and
    /// surface a load error when asked. Mirrors pi's private `applyThemeName`.
    fn apply_theme_name(
        &mut self,
        ui: &mut impl ThemeControllerUi,
        theme_name: &str,
        show_error: bool,
    ) -> ThemeResult {
        let result = self.active.set(theme_name, &self.source, true);
        self.active_theme_name = Some(if result.success {
            theme_name.to_string()
        } else {
            "dark".to_string()
        });
        self.notify_changed(ui);
        if !result.success && show_error {
            let error = result.error.as_deref().unwrap_or_default();
            (self.show_error)(&format!(
                "Failed to load theme \"{theme_name}\": {error}\nFell back to dark theme."
            ));
        }
        result
    }

    /// Invalidate the UI and run the change hook. Mirrors pi's private
    /// `notifyChanged`.
    fn notify_changed(&self, ui: &mut impl ThemeControllerUi) {
        ui.invalidate();
        (self.on_changed)();
    }

    /// Toggle auto-sync, equality-guarded, mirroring the color-scheme notification
    /// subscription. Mirrors pi's private `setAutoSync`.
    fn set_auto_sync(&mut self, ui: &mut impl ThemeControllerUi, enabled: bool) {
        if self.auto_sync_enabled == enabled {
            return;
        }
        self.auto_sync_enabled = enabled;
        ui.set_terminal_color_scheme_notifications(enabled);
    }

    /// React to a terminal color-scheme change while auto-sync is on: re-resolve
    /// the auto setting and apply the matching side if it differs from the current
    /// theme. A no-op when auto-sync is off. Mirrors pi's private
    /// `applyTerminalTheme`.
    pub fn apply_terminal_theme(
        &mut self,
        ui: &mut impl ThemeControllerUi,
        terminal_theme: TerminalTheme,
    ) {
        if !self.auto_sync_enabled {
            return;
        }
        self.terminal_theme = terminal_theme;
        let auto_theme =
            parse_auto_theme_setting(self.settings.borrow().get_theme_setting().as_deref());
        let Some(auto_theme) = auto_theme else {
            self.set_auto_sync(ui, false);
            return;
        };
        let theme_name = if terminal_theme == TerminalTheme::Light {
            auto_theme.light_theme
        } else {
            auto_theme.dark_theme
        };
        if Some(&theme_name) != self.active_theme_name.as_ref() {
            self.apply_theme_name(ui, &theme_name, false);
        }
    }
}

/// The theme-name string a [`TerminalTheme`] resolves to. pi's `TerminalTheme` is
/// the string `"dark" | "light"` directly; pidgin's is an enum, so
/// `applyFromSettings`'s unset branch (which passes `detection.theme` to both
/// `applyThemeName` and `settingsManager.setTheme`) maps through here.
fn terminal_theme_name(theme: TerminalTheme) -> &'static str {
    match theme {
        TerminalTheme::Light => "light",
        TerminalTheme::Dark => "dark",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::settings_manager::{SettingsManager, SettingsManagerCreateOptions};
    use crate::modes::interactive::theme::{
        create_theme, load_theme_json, ColorMode, RgbColor, ThemeDirs,
    };
    use std::collections::HashMap;

    /// A configurable fake `ThemeControllerUi` that records every side effect the
    /// controller drives and serves canned terminal-query answers.
    struct MockUi {
        /// Canned OSC 11 background reply (or `None` for timeout).
        bg: Option<RgbColor>,
        /// Canned DSR color-scheme reply (or `None` for unsupported).
        scheme: Option<TerminalTheme>,
        /// Recorded `invalidate()` calls.
        invalidate_calls: u32,
        /// Recorded `request_render()` calls.
        request_render_calls: u32,
        /// Recorded `set_terminal_color_scheme_notifications` toggles, in order.
        notification_toggles: Vec<bool>,
    }

    impl MockUi {
        fn new(bg: Option<RgbColor>, scheme: Option<TerminalTheme>) -> Self {
            Self {
                bg,
                scheme,
                invalidate_calls: 0,
                request_render_calls: 0,
                notification_toggles: Vec::new(),
            }
        }
    }

    impl super::super::TerminalBackgroundThemeDetector for MockUi {
        fn query_terminal_background_color(&mut self, _timeout: Duration) -> Option<RgbColor> {
            self.bg
        }
    }

    impl TerminalAutoThemeDetector for MockUi {
        fn query_terminal_color_scheme(&mut self, _timeout: Duration) -> Option<TerminalTheme> {
            self.scheme
        }
    }

    impl ThemeControllerUi for MockUi {
        fn invalidate(&mut self) {
            self.invalidate_calls += 1;
        }
        fn request_render(&mut self) {
            self.request_render_calls += 1;
        }
        fn set_terminal_color_scheme_notifications(&mut self, enabled: bool) {
            self.notification_toggles.push(enabled);
        }
    }

    /// A `ThemeSource` over the built-ins with an injected env.
    fn source(env: HashMap<String, String>) -> ThemeSource {
        ThemeSource {
            dirs: ThemeDirs::default(),
            mode: Some(ColorMode::Color256),
            env,
        }
    }

    /// Seed an [`ActiveTheme`] with the built-in dark theme.
    fn active_dark() -> Rc<ActiveTheme> {
        let src = source(HashMap::new());
        let json = load_theme_json("dark", &src.dirs).expect("dark json");
        Rc::new(ActiveTheme::new(
            create_theme(&json, src.mode, None).expect("dark theme"),
        ))
    }

    /// An in-memory settings manager seeded with an optional `theme` setting.
    fn settings_with_theme(theme: Option<&str>) -> Rc<RefCell<SettingsManager>> {
        let mut mgr =
            SettingsManager::in_memory(Default::default(), SettingsManagerCreateOptions::default());
        if let Some(theme) = theme {
            mgr.set_theme(theme);
        }
        Rc::new(RefCell::new(mgr))
    }

    /// A controller plus the `Rc`-shared handles the assertions read back: the
    /// active theme, the settings manager, and the `on_changed` call counter.
    type ControllerFixture = (
        InteractiveThemeController,
        Rc<ActiveTheme>,
        Rc<RefCell<SettingsManager>>,
        Rc<std::cell::Cell<u32>>,
    );

    /// Build a controller plus the `Rc`-shared handles the assertions read back.
    fn controller(theme_setting: Option<&str>, env: HashMap<String, String>) -> ControllerFixture {
        let active = active_dark();
        let settings = settings_with_theme(theme_setting);
        let changed = Rc::new(std::cell::Cell::new(0u32));
        let on_changed = {
            let changed = Rc::clone(&changed);
            Box::new(move || changed.set(changed.get() + 1)) as Box<dyn Fn()>
        };
        let ctrl = InteractiveThemeController::new(
            Rc::clone(&settings),
            Rc::clone(&active),
            source(env),
            Box::new(|_| {}),
            on_changed,
        );
        (ctrl, active, settings, changed)
    }

    /// A `COLORFGBG` env that resolves to a light terminal background.
    fn light_env() -> HashMap<String, String> {
        HashMap::from([("COLORFGBG".to_string(), "0;15".to_string())])
    }

    #[test]
    fn constructor_resolves_active_theme_name_from_setting() {
        // Plain name.
        let (ctrl, active, _s, _c) = controller(Some("light"), HashMap::new());
        assert_eq!(ctrl.active_theme_name.as_deref(), Some("light"));
        assert_eq!(active.current_name().as_deref(), Some("light"));

        // Auto setting resolves against the (env-detected) terminal theme: empty
        // env -> dark side.
        let (ctrl, active, _s, _c) = controller(Some("light/dark"), HashMap::new());
        assert_eq!(ctrl.active_theme_name.as_deref(), Some("dark"));
        assert_eq!(active.current_name().as_deref(), Some("dark"));

        // Auto setting with a light terminal env -> light side.
        let (ctrl, active, _s, _c) = controller(Some("light/dark"), light_env());
        assert_eq!(ctrl.active_theme_name.as_deref(), Some("light"));
        assert_eq!(active.current_name().as_deref(), Some("light"));

        // Unset setting -> `resolve_theme_setting(None, ..)` is `None` (pi's
        // `undefined`); `init(None, ..)` then defaults the active theme to the
        // env default ("dark" for empty env) without recording a name.
        let (ctrl, active, _s, _c) = controller(None, HashMap::new());
        assert_eq!(ctrl.active_theme_name, None);
        assert_eq!(active.current_name().as_deref(), Some("dark"));
    }

    #[test]
    fn apply_from_settings_plain_disables_auto_sync_and_applies() {
        let (mut ctrl, active, _s, changed) = controller(Some("light"), HashMap::new());
        let mut ui = MockUi::new(None, None);
        ctrl.apply_from_settings(&mut ui);
        assert_eq!(active.current_name().as_deref(), Some("light"));
        assert_eq!(ctrl.active_theme_name.as_deref(), Some("light"));
        assert!(!ctrl.auto_sync_enabled);
        // Guarded off-toggle: already false at construction, so no notification.
        assert!(ui.notification_toggles.is_empty());
        assert_eq!(changed.get(), 1);
        assert_eq!(ui.invalidate_calls, 1);
    }

    #[test]
    fn apply_from_settings_auto_enables_auto_sync_and_picks_side() {
        // Auto setting; the DSR query reports light -> light side, auto-sync on.
        let (mut ctrl, active, _s, _c) = controller(Some("light/dark"), HashMap::new());
        let mut ui = MockUi::new(None, Some(TerminalTheme::Light));
        ctrl.apply_from_settings(&mut ui);
        assert!(ctrl.auto_sync_enabled);
        assert_eq!(ui.notification_toggles, vec![true]);
        assert_eq!(ctrl.terminal_theme, TerminalTheme::Light);
        assert_eq!(active.current_name().as_deref(), Some("light"));
        assert_eq!(ctrl.active_theme_name.as_deref(), Some("light"));

        // A dark color-scheme report picks the dark side.
        let (mut ctrl, active, _s, _c) = controller(Some("light/dark"), HashMap::new());
        let mut ui = MockUi::new(None, Some(TerminalTheme::Dark));
        ctrl.apply_from_settings(&mut ui);
        assert_eq!(active.current_name().as_deref(), Some("dark"));
    }

    #[test]
    fn apply_from_settings_unset_high_confidence_persists() {
        // No theme setting, light COLORFGBG env -> high-confidence "light",
        // applied and persisted.
        let (mut ctrl, active, settings, _c) = controller(None, light_env());
        let mut ui = MockUi::new(None, None);
        ctrl.apply_from_settings(&mut ui);
        assert_eq!(active.current_name().as_deref(), Some("light"));
        assert_eq!(
            settings.borrow().get_theme_setting().as_deref(),
            Some("light")
        );
        assert!(!ctrl.auto_sync_enabled);
    }

    #[test]
    fn apply_from_settings_unset_low_confidence_does_not_persist() {
        // No theme setting, no env hint -> low-confidence dark fallback: applied
        // but NOT persisted.
        let (mut ctrl, active, settings, _c) = controller(None, HashMap::new());
        let mut ui = MockUi::new(None, None);
        ctrl.apply_from_settings(&mut ui);
        assert_eq!(active.current_name().as_deref(), Some("dark"));
        assert_eq!(settings.borrow().get_theme_setting(), None);
    }

    #[test]
    fn apply_from_settings_unset_high_confidence_via_osc11() {
        // No theme setting; OSC 11 reports a light background -> high-confidence
        // "light" persisted.
        let (mut ctrl, active, settings, _c) = controller(None, HashMap::new());
        let mut ui = MockUi::new(
            Some(RgbColor {
                r: 250,
                g: 250,
                b: 250,
            }),
            None,
        );
        ctrl.apply_from_settings(&mut ui);
        assert_eq!(active.current_name().as_deref(), Some("light"));
        assert_eq!(
            settings.borrow().get_theme_setting().as_deref(),
            Some("light")
        );
    }

    #[test]
    fn set_theme_name_bad_name_falls_back_and_shows_exact_error() {
        let active = active_dark();
        let settings = settings_with_theme(Some("light"));
        let captured = Rc::new(RefCell::new(Vec::<String>::new()));
        let show_error = {
            let captured = Rc::clone(&captured);
            Box::new(move |m: &str| captured.borrow_mut().push(m.to_string())) as Box<dyn Fn(&str)>
        };
        let mut ctrl = InteractiveThemeController::new(
            Rc::clone(&settings),
            Rc::clone(&active),
            source(HashMap::new()),
            show_error,
            Box::new(|| {}),
        );
        let mut ui = MockUi::new(None, None);
        let result = ctrl.set_theme_name(&mut ui, "does-not-exist", true);
        assert!(!result.success);
        // Fell back to dark.
        assert_eq!(active.current_name().as_deref(), Some("dark"));
        assert_eq!(ctrl.active_theme_name.as_deref(), Some("dark"));
        // Exact pi showError wording.
        let messages = captured.borrow();
        assert_eq!(messages.len(), 1);
        let error = result.error.as_deref().unwrap();
        assert_eq!(
            messages[0],
            format!("Failed to load theme \"does-not-exist\": {error}\nFell back to dark theme.")
        );
        assert!(messages[0].ends_with("\nFell back to dark theme."));
    }

    #[test]
    fn set_theme_name_swap_changes_rendered_ansi() {
        // A set-by-name swap through the shared ActiveTheme changes the exact ANSI
        // a themed read-through emits. Pin dark vs light for `userMessageText`.
        let src = source(HashMap::new());
        let dark_json = load_theme_json("dark", &src.dirs).unwrap();
        let dark = create_theme(&dark_json, src.mode, None).unwrap();
        let dark_fg = dark.fg("userMessageText", "X").unwrap();
        let light_json = load_theme_json("light", &src.dirs).unwrap();
        let light = create_theme(&light_json, src.mode, None).unwrap();
        let light_fg = light.fg("userMessageText", "X").unwrap();
        assert_ne!(dark_fg, light_fg, "themes must differ for a meaningful pin");

        let (mut ctrl, active, _s, _c) = controller(None, HashMap::new());
        let mut ui = MockUi::new(None, None);
        // Start on dark: the read-through emits dark's exact ANSI.
        ctrl.set_theme_name(&mut ui, "dark", false);
        assert_eq!(
            active.current().fg("userMessageText", "X").unwrap(),
            dark_fg
        );
        // Swap to light: the SAME handle now emits light's exact ANSI.
        ctrl.set_theme_name(&mut ui, "light", false);
        assert_eq!(
            active.current().fg("userMessageText", "X").unwrap(),
            light_fg
        );
    }

    #[test]
    fn set_theme_instance_uses_sentinel_and_reports_success() {
        let (mut ctrl, active, _s, changed) = controller(Some("dark"), HashMap::new());
        let mut ui = MockUi::new(None, None);
        let src = source(HashMap::new());
        let light_json = load_theme_json("light", &src.dirs).unwrap();
        let light = create_theme(&light_json, src.mode, None).unwrap();
        let result = ctrl.set_theme_instance(&mut ui, light);
        assert!(result.success);
        assert_eq!(ctrl.active_theme_name.as_deref(), Some("<in-memory>"));
        assert_eq!(active.current().name.as_deref(), Some("light"));
        assert!(changed.get() >= 1);
    }

    #[test]
    fn preview_swaps_and_requests_render_without_persisting() {
        let (mut ctrl, active, settings, changed) = controller(Some("dark"), HashMap::new());
        let mut ui = MockUi::new(None, None);
        let before = changed.get();
        ctrl.preview(&mut ui, "light");
        assert_eq!(active.current_name().as_deref(), Some("light"));
        assert_eq!(ui.invalidate_calls, 1);
        assert_eq!(ui.request_render_calls, 1);
        // preview does not run the change hook (pi calls invalidate+requestRender,
        // not notifyChanged) nor persist.
        assert_eq!(changed.get(), before);
        assert_eq!(
            settings.borrow().get_theme_setting().as_deref(),
            Some("dark")
        );
    }

    #[test]
    fn disable_auto_sync_toggles_notifications_off_only_when_on() {
        // Turn auto-sync on via an auto apply, then disable it.
        let (mut ctrl, _a, _s, _c) = controller(Some("light/dark"), HashMap::new());
        let mut ui = MockUi::new(None, Some(TerminalTheme::Dark));
        ctrl.apply_from_settings(&mut ui);
        assert_eq!(ui.notification_toggles, vec![true]);
        ctrl.disable_auto_sync(&mut ui);
        assert_eq!(ui.notification_toggles, vec![true, false]);
        // A redundant disable is guarded.
        ctrl.disable_auto_sync(&mut ui);
        assert_eq!(ui.notification_toggles, vec![true, false]);
    }

    #[test]
    fn apply_terminal_theme_noops_when_auto_sync_disabled() {
        let (mut ctrl, active, _s, _c) = controller(Some("dark"), HashMap::new());
        let mut ui = MockUi::new(None, None);
        // auto_sync is off (plain setting path never enabled it).
        ctrl.apply_terminal_theme(&mut ui, TerminalTheme::Light);
        assert_eq!(active.current_name().as_deref(), Some("dark"));
        assert_eq!(ui.invalidate_calls, 0);
    }

    #[test]
    fn apply_terminal_theme_swaps_side_when_auto_sync_on() {
        let (mut ctrl, active, _s, _c) = controller(Some("light/dark"), HashMap::new());
        let mut ui = MockUi::new(None, Some(TerminalTheme::Dark));
        ctrl.apply_from_settings(&mut ui);
        assert_eq!(active.current_name().as_deref(), Some("dark"));
        // Terminal flips to light -> auto picks the light side.
        ctrl.apply_terminal_theme(&mut ui, TerminalTheme::Light);
        assert_eq!(active.current_name().as_deref(), Some("light"));
        assert_eq!(ctrl.active_theme_name.as_deref(), Some("light"));
    }

    #[test]
    fn get_terminal_theme_is_a_pure_read() {
        let (ctrl, _a, _s, _c) = controller(None, light_env());
        assert_eq!(ctrl.get_terminal_theme(), TerminalTheme::Light);
    }
}

/// Byte-exact pins for the DEC 2031 notification toggle the controller drives
/// through a real `Tui`, using the logging terminal's recorded write stream.
#[cfg(test)]
mod logging_terminal_tests {
    use super::*;
    use crate::core::settings_manager::{SettingsManager, SettingsManagerCreateOptions};
    use crate::modes::interactive::theme::{create_theme, load_theme_json, ColorMode, ThemeDirs};
    use pidgin_tui::terminal::LoggingTerminal;
    use pidgin_tui::Tui;
    use std::collections::HashMap;

    /// A bare `Tui<LoggingTerminal>` as the `ThemeControllerUi`. The terminal
    /// queries write-and-arm but never settle (a bare `Tui` has no run-loop pump),
    /// so they report `None`; the test drives only the auto path's notification
    /// toggle, whose bytes this pins.
    impl super::super::TerminalBackgroundThemeDetector for Tui<LoggingTerminal> {
        fn query_terminal_background_color(&mut self, _timeout: Duration) -> Option<RgbColor> {
            None
        }
    }
    impl TerminalAutoThemeDetector for Tui<LoggingTerminal> {
        fn query_terminal_color_scheme(&mut self, _timeout: Duration) -> Option<TerminalTheme> {
            None
        }
    }
    impl ThemeControllerUi for Tui<LoggingTerminal> {
        fn invalidate(&mut self) {
            Tui::invalidate(self);
        }
        fn request_render(&mut self) {
            self.request_render(true);
        }
        fn set_terminal_color_scheme_notifications(&mut self, enabled: bool) {
            Tui::set_terminal_color_scheme_notifications(self, enabled);
        }
    }

    use crate::modes::interactive::theme::{ActiveTheme, RgbColor};

    #[test]
    fn auto_sync_toggle_writes_exact_dec_2031_bytes() {
        let src = ThemeSource {
            dirs: ThemeDirs::default(),
            mode: Some(ColorMode::Color256),
            env: HashMap::new(),
        };
        let dark_json = load_theme_json("dark", &src.dirs).unwrap();
        let active = Rc::new(ActiveTheme::new(
            create_theme(&dark_json, src.mode, None).unwrap(),
        ));
        let mut settings =
            SettingsManager::in_memory(Default::default(), SettingsManagerCreateOptions::default());
        settings.set_theme("light/dark");
        let settings = Rc::new(RefCell::new(settings));
        let mut ctrl = InteractiveThemeController::new(
            Rc::clone(&settings),
            Rc::clone(&active),
            src,
            Box::new(|_| {}),
            Box::new(|| {}),
        );

        // A fresh Tui is not stopped, so notification toggles write immediately.
        let mut ui = Tui::new(LoggingTerminal::new(80, 24), true);

        ctrl.apply_from_settings(&mut ui);
        // Enabling auto-sync writes the DEC 2031 enable sequence.
        assert!(
            ui.take_writes().contains("\x1b[?2031h"),
            "enabling auto-sync must write the DEC 2031 enable sequence"
        );

        ctrl.disable_auto_sync(&mut ui);
        // Disabling writes the DEC 2031 disable sequence.
        assert!(
            ui.take_writes().contains("\x1b[?2031l"),
            "disabling auto-sync must write the DEC 2031 disable sequence"
        );
    }
}
