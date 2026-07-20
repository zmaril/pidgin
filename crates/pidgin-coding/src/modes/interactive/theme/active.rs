//! Interactive-layer active-theme singleton runtime.
//!
//! Faithful port of the "Global Theme Instance" section of pi's
//! `modes/interactive/theme/theme.ts` (`initTheme` / `setTheme` /
//! `setThemeInstance` / `onThemeChange` / `getDefaultTheme` and the module-global
//! `theme` Proxy). `theme.rs` deliberately left this out of the theme *data*
//! layer (see that file's header): the mutable global-instance runtime "belongs
//! with the interactive UI, not this data layer". This module is that runtime.
//!
//! **Shape divergence (intentional, single-threaded TUI model).** pi shares the
//! active theme through `globalThis` keyed by two `Symbol.for(...)` keys and reads
//! it via a `Proxy` — machinery that exists *only* to reconcile pi's dual module
//! loaders (tsx + jiti) in dev mode, and is meaningless in Rust. pidgin's
//! interactive loop is a single-threaded main loop; the turn worker never touches
//! `Theme` or renders. So the active theme lives in an interior-mutable shared
//! handle — `Rc<RefCell<Theme>>` — **not** a process global, **not** an
//! `Arc<Mutex>`/`once_cell`/`thread_local`, and **not** a `Proxy`. The
//! `globalThis` + dual-`Symbol` machinery is dropped entirely.
//!
//! **`get_default_theme` divergence.** pi's `getDefaultTheme()` is zero-arg and
//! reads process env internally; pidgin's `detect_terminal_background_from_env`
//! takes an injected `env: &HashMap`, so [`ActiveTheme::get_default_theme`] threads
//! that env through.
//!
//! **File-watcher stub.** pi's `startThemeWatcher`/`stopThemeWatcher`/
//! `scheduleReload` use `fs.FSWatcher` with a 100ms debounce to hot-reload a
//! *custom* theme JSON while it is being edited (built-ins `dark`/`light` are
//! never watched, even in pi). pidgin's interactive loop is synchronous with no
//! reactor, so the `enable_watcher: bool` parameter is accepted (to match the
//! controller's call shape) but no-oped. See [`ActiveTheme::start_watcher`].
// straitjacket-allow-file:duplication

use std::cell::{Ref, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use super::{
    create_theme, detect_terminal_background_from_env, load_theme_json, ColorMode, TerminalTheme,
    Theme, ThemeDirs, ThemeError,
};

/// Outcome of [`ActiveTheme::set`]. Mirrors pi's
/// `setTheme(...) -> { success: boolean; error?: string }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetThemeResult {
    /// Whether the requested theme loaded successfully.
    pub success: bool,
    /// The error message when the requested theme failed to load (mirrors pi's
    /// `error instanceof Error ? error.message : String(error)`); `None` on
    /// success.
    pub error: Option<String>,
}

/// Where a name-based theme load resolves from. Bundles the injected inputs pi
/// reads from module-global config (custom-themes directory), the color mode
/// (pi derives this from `getCapabilities()`; pidgin injects it), and the
/// environment used to compute the default theme name.
///
/// Threading these per call — rather than storing them in the singleton — keeps
/// [`ActiveTheme`] free of ambient/process state, matching the crate's
/// injected-`env`, injected-`ThemeDirs` convention.
#[derive(Clone, Debug, Default)]
pub struct ThemeSource {
    /// Directories used to locate custom `<name>.json` theme files.
    pub dirs: ThemeDirs,
    /// Color mode to bake theme colors into; `None` defers to `create_theme`'s
    /// default (`256color`).
    pub mode: Option<ColorMode>,
    /// Environment map used by [`ActiveTheme::get_default_theme`] when `init` is
    /// called with no explicit name.
    pub env: HashMap<String, String>,
}

/// Load a theme by name, mirroring pi's `loadTheme(name, mode)`.
///
/// pi's `loadTheme` first checks a registered-themes `Map` and returns the cached
/// instance when present, otherwise falls through to `loadThemeJson` +
/// `createTheme`. pidgin has **no** ported registered-themes registry (that map
/// backs pi's plugin/custom-theme discovery, which is out of scope for this
/// slice), so this collapses to the fall-through path: [`load_theme_json`]
/// resolves built-in `dark`/`light` from embedded JSON and any other name from
/// `dirs.custom_themes_dir/<name>.json`, then [`create_theme`] bakes it. As in
/// pi's `loadTheme` (which calls `createTheme(themeJson, mode)` with no
/// `sourcePath`), the source path is not recorded for name-based loads.
fn load_theme(name: &str, source: &ThemeSource) -> Result<Theme, ThemeError> {
    let theme_json = load_theme_json(name, &source.dirs)?;
    create_theme(&theme_json, source.mode, None)
}

/// The interactive layer's active theme: an interior-mutable shared handle to the
/// current [`Theme`], the current theme name, and a single change callback.
///
/// Faithful mirror of pi's module-global `theme` Proxy + `currentThemeName` +
/// `onThemeChangeCallback`, minus the `globalThis`/`Symbol` sharing (see the
/// module docs).
pub struct ActiveTheme {
    /// The active theme instance. `Rc<RefCell<..>>` so future render components
    /// can share the same handle in the single-threaded TUI (mirrors pi's shared
    /// global instance).
    theme: Rc<RefCell<Theme>>,
    /// The current theme name, or `None` before the first `init`/`set`
    /// (mirrors pi's `currentThemeName: string | undefined`). `"<in-memory>"`
    /// after [`ActiveTheme::set_instance`].
    name: RefCell<Option<String>>,
    /// The single registered change callback (mirrors pi's
    /// `onThemeChangeCallback`).
    on_change: RefCell<Option<Box<dyn Fn()>>>,
}

impl ActiveTheme {
    /// Create an active-theme handle seeded with an initial [`Theme`].
    ///
    /// pi's global instance starts *uninitialized* (the `theme` Proxy throws
    /// "Theme not initialized" until `initTheme()` runs). Rust has no lazy-throw
    /// Proxy, so the handle is seeded with a concrete theme up front and the name
    /// starts `None`, matching pi's `currentThemeName === undefined` pre-`init`
    /// state. Callers typically follow construction with [`ActiveTheme::init`].
    pub fn new(initial: Theme) -> Self {
        Self {
            theme: Rc::new(RefCell::new(initial)),
            name: RefCell::new(None),
            on_change: RefCell::new(None),
        }
    }

    /// Initialize the active theme. Mirrors pi's
    /// `initTheme(themeName?, enableWatcher = false)`.
    ///
    /// Resolves `name` (or, when `None`, [`ActiveTheme::get_default_theme`] over
    /// `source.env`), records it as the current name, then loads it. On load
    /// failure it falls back to the built-in `dark` theme **silently** — the name
    /// becomes `"dark"` and **no** change callback fires (pi's `initTheme` catch
    /// block does not invoke `onThemeChangeCallback`).
    pub fn init(&self, name: Option<&str>, source: &ThemeSource, enable_watcher: bool) {
        let name = name
            .map(str::to_string)
            .unwrap_or_else(|| Self::get_default_theme(&source.env));
        *self.name.borrow_mut() = Some(name.clone());
        match load_theme(&name, source) {
            Ok(theme) => {
                *self.theme.borrow_mut() = theme;
                if enable_watcher {
                    self.start_watcher();
                }
            }
            Err(_error) => {
                // Theme is invalid - fall back to dark theme silently.
                *self.name.borrow_mut() = Some("dark".to_string());
                *self.theme.borrow_mut() =
                    load_theme("dark", source).expect("built-in dark theme always loads");
                // Don't start the watcher for the fallback theme.
            }
        }
    }

    /// Switch the active theme by name. Mirrors pi's
    /// `setTheme(name, enableWatcher = false) -> { success, error? }`.
    ///
    /// On success: swaps the instance, updates the name, fires the change
    /// callback, and returns `{ success: true }`. On failure: falls back to the
    /// built-in `dark` theme, sets the name to `"dark"`, does **not** fire the
    /// callback, and returns `{ success: false, error: Some(message) }` where the
    /// message is the `ThemeError`'s `Display` text (pi returns
    /// `error.message`).
    pub fn set(&self, name: &str, source: &ThemeSource, enable_watcher: bool) -> SetThemeResult {
        *self.name.borrow_mut() = Some(name.to_string());
        match load_theme(name, source) {
            Ok(theme) => {
                *self.theme.borrow_mut() = theme;
                if enable_watcher {
                    self.start_watcher();
                }
                self.fire_on_change();
                SetThemeResult {
                    success: true,
                    error: None,
                }
            }
            Err(error) => {
                // Theme is invalid - fall back to dark theme.
                *self.name.borrow_mut() = Some("dark".to_string());
                *self.theme.borrow_mut() =
                    load_theme("dark", source).expect("built-in dark theme always loads");
                // Don't start the watcher for the fallback theme.
                SetThemeResult {
                    success: false,
                    error: Some(error.to_string()),
                }
            }
        }
    }

    /// Swap in a directly-constructed theme instance. Mirrors pi's
    /// `setThemeInstance(themeInstance)`.
    ///
    /// Sets the current name to the sentinel `"<in-memory>"` and fires the change
    /// callback. pi additionally calls `stopThemeWatcher()` here ("Can't watch a
    /// direct instance"); this port has no watcher (see [`Self::start_watcher`]),
    /// so there is nothing to stop.
    pub fn set_instance(&self, theme: Theme) {
        *self.theme.borrow_mut() = theme;
        *self.name.borrow_mut() = Some("<in-memory>".to_string());
        // pi stops the file watcher here; this port has no watcher to stop.
        self.fire_on_change();
    }

    /// Register the single change callback. Mirrors pi's
    /// `onThemeChange(callback)` — a later call replaces any prior callback.
    pub fn on_change(&self, callback: Box<dyn Fn()>) {
        *self.on_change.borrow_mut() = Some(callback);
    }

    /// Borrow the current active theme. Mirrors reading through pi's `theme`
    /// Proxy (`get` returns the live global instance).
    pub fn current(&self) -> Ref<'_, Theme> {
        self.theme.borrow()
    }

    /// The current theme name, or `None` before the first `init`/`set`. Mirrors
    /// pi's `currentThemeName`.
    pub fn current_name(&self) -> Option<String> {
        self.name.borrow().clone()
    }

    /// The default theme name for a terminal, from its environment. Mirrors pi's
    /// `getDefaultTheme()` (`detectTerminalBackgroundFromEnv().theme`), but with
    /// the env map injected — pidgin's `detect_terminal_background_from_env` takes
    /// an explicit `env` rather than reading `process.env`.
    pub fn get_default_theme(env: &HashMap<String, String>) -> String {
        match detect_terminal_background_from_env(env).theme {
            TerminalTheme::Dark => "dark".to_string(),
            TerminalTheme::Light => "light".to_string(),
        }
    }

    /// Invoke the registered change callback, if any. Mirrors pi's
    /// `if (onThemeChangeCallback) onThemeChangeCallback();`.
    fn fire_on_change(&self) {
        if let Some(callback) = self.on_change.borrow().as_ref() {
            callback();
        }
    }

    /// Live-reload watcher for custom theme JSON. Stubbed no-op.
    ///
    /// pi's `startThemeWatcher` opens an `fs.FSWatcher` on the custom-themes
    /// directory and, on a 100ms debounce, reloads the *custom* theme file being
    /// edited (built-in `dark`/`light` are never watched, even in pi). pidgin's
    /// interactive loop is synchronous with no reactor/event source, so there is
    /// nothing to drive a watcher.
    ///
    /// PR follow-up: wiring an actual file watcher for live-editing custom theme
    /// JSON is deferred. This is on nobody's render-byte critical path — built-ins
    /// are never watched, and a custom theme still loads correctly on the next
    /// explicit `set`.
    fn start_watcher(&self) {
        // No-op: see the doc comment. `enable_watcher` is accepted to match the
        // controller's call shape.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a [`ThemeSource`] over an optional custom-themes dir and env.
    fn source(custom_dir: Option<&std::path::Path>, env: HashMap<String, String>) -> ThemeSource {
        ThemeSource {
            dirs: ThemeDirs {
                custom_themes_dir: custom_dir.map(Into::into).unwrap_or_default(),
            },
            mode: Some(ColorMode::Color256),
            env,
        }
    }

    /// A source with no custom dir and an empty env (env-default resolves to
    /// `dark`).
    fn builtin_source() -> ThemeSource {
        source(None, HashMap::new())
    }

    /// Seed an [`ActiveTheme`] with the built-in dark theme.
    fn active_seeded() -> ActiveTheme {
        let src = builtin_source();
        ActiveTheme::new(load_theme("dark", &src).expect("built-in dark loads"))
    }

    /// A `COLORFGBG` env that `detect_terminal_background_from_env` reads as a
    /// light background (foreground/background `"0;15"` -> bright bg).
    fn light_env() -> HashMap<String, String> {
        HashMap::from([("COLORFGBG".to_string(), "0;15".to_string())])
    }

    #[test]
    fn get_default_theme_maps_terminal_background() {
        assert_eq!(ActiveTheme::get_default_theme(&HashMap::new()), "dark");
        assert_eq!(ActiveTheme::get_default_theme(&light_env()), "light");
    }

    #[test]
    fn init_without_name_uses_default_theme() {
        // Empty env -> default "dark".
        let active = active_seeded();
        active.init(None, &builtin_source(), false);
        assert_eq!(active.current_name().as_deref(), Some("dark"));
        assert_eq!(active.current().name.as_deref(), Some("dark"));

        // Light-background env -> default "light".
        let active = active_seeded();
        active.init(None, &source(None, light_env()), false);
        assert_eq!(active.current_name().as_deref(), Some("light"));
        assert_eq!(active.current().name.as_deref(), Some("light"));
    }

    #[test]
    fn init_with_explicit_name_loads_that_theme() {
        let active = active_seeded();
        active.init(Some("dark"), &builtin_source(), false);
        assert_eq!(active.current_name().as_deref(), Some("dark"));
        assert_eq!(active.current().name.as_deref(), Some("dark"));

        let active = active_seeded();
        active.init(Some("light"), &builtin_source(), false);
        assert_eq!(active.current_name().as_deref(), Some("light"));
        assert_eq!(active.current().name.as_deref(), Some("light"));
    }

    #[test]
    fn init_falls_back_to_dark_silently_on_bad_name() {
        let active = active_seeded();
        let fired = Rc::new(std::cell::Cell::new(false));
        {
            let fired = Rc::clone(&fired);
            active.on_change(Box::new(move || fired.set(true)));
        }
        active.init(Some("does-not-exist"), &builtin_source(), false);
        assert_eq!(active.current_name().as_deref(), Some("dark"));
        assert_eq!(active.current().name.as_deref(), Some("dark"));
        // init's fallback is silent - no callback fires.
        assert!(!fired.get(), "init fallback must not fire on_change");
    }

    #[test]
    fn set_builtin_themes_succeed_and_swap() {
        let active = active_seeded();
        let count = Rc::new(std::cell::Cell::new(0u32));
        {
            let count = Rc::clone(&count);
            active.on_change(Box::new(move || count.set(count.get() + 1)));
        }

        let result = active.set("light", &builtin_source(), false);
        assert_eq!(
            result,
            SetThemeResult {
                success: true,
                error: None
            }
        );
        assert_eq!(active.current_name().as_deref(), Some("light"));
        assert_eq!(active.current().name.as_deref(), Some("light"));
        assert_eq!(count.get(), 1, "set success must fire on_change once");

        let result = active.set("dark", &builtin_source(), false);
        assert!(result.success);
        assert_eq!(active.current_name().as_deref(), Some("dark"));
        assert_eq!(active.current().name.as_deref(), Some("dark"));
        assert_eq!(count.get(), 2);
    }

    #[test]
    fn set_bad_name_reports_error_and_falls_back_to_dark() {
        let active = active_seeded();
        // Start on light so the fallback to dark is observable as a change.
        active.set("light", &builtin_source(), false);

        let fired = Rc::new(std::cell::Cell::new(false));
        {
            let fired = Rc::clone(&fired);
            active.on_change(Box::new(move || fired.set(true)));
        }

        let result = active.set("does-not-exist", &builtin_source(), false);
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|m| m.contains("does-not-exist")),
            "error message should surface the theme name: {:?}",
            result.error
        );
        // Fell back to dark.
        assert_eq!(active.current_name().as_deref(), Some("dark"));
        assert_eq!(active.current().name.as_deref(), Some("dark"));
        // set's failure path does not fire on_change.
        assert!(!fired.get(), "set failure must not fire on_change");
    }

    #[test]
    fn set_instance_uses_sentinel_name_and_fires_change() {
        let active = active_seeded();
        let fired = Rc::new(std::cell::Cell::new(false));
        {
            let fired = Rc::clone(&fired);
            active.on_change(Box::new(move || fired.set(true)));
        }

        let src = builtin_source();
        let light = load_theme("light", &src).expect("light loads");
        active.set_instance(light);

        assert_eq!(active.current_name().as_deref(), Some("<in-memory>"));
        assert_eq!(active.current().name.as_deref(), Some("light"));
        assert!(fired.get(), "set_instance must fire on_change");
    }

    #[test]
    fn current_reflects_last_swap() {
        let active = active_seeded();
        assert_eq!(active.current().name.as_deref(), Some("dark"));
        active.set("light", &builtin_source(), false);
        assert_eq!(active.current().name.as_deref(), Some("light"));
        assert_eq!(active.current_name().as_deref(), Some("light"));
    }

    #[test]
    fn set_loads_custom_theme_by_path() {
        // Write a custom theme JSON into a temp dir, then load it by name. Reuse
        // the embedded dark.json body (a known-valid theme) but rename it, so it
        // resolves from the custom-themes directory rather than the built-ins.
        let dir = tempfile::tempdir().expect("tempdir");
        let custom_json =
            include_str!("dark.json").replace("\"name\": \"dark\"", "\"name\": \"my-custom\"");
        std::fs::write(dir.path().join("my-custom.json"), &custom_json)
            .expect("write custom theme");

        let active = active_seeded();
        let custom_src = source(Some(dir.path()), HashMap::new());
        let result = active.set("my-custom", &custom_src, false);
        assert!(
            result.success,
            "custom theme should load: {:?}",
            result.error
        );
        assert_eq!(active.current_name().as_deref(), Some("my-custom"));
        assert_eq!(active.current().name.as_deref(), Some("my-custom"));

        // enable_watcher is accepted but a no-op; loading still succeeds.
        let result = active.set("my-custom", &custom_src, true);
        assert!(result.success);
        assert_eq!(active.current_name().as_deref(), Some("my-custom"));
    }
}
