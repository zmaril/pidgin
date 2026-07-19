//! Interactive input widgets — byte-exact ports of pi's `components/*.ts`.
//!
//! Each type implements [`crate::renderer::Component`] and reproduces pi's
//! `render(width)` output and edit-state transitions byte-for-byte. Ports:
//! - [`Input`] (`input.ts`) — single-line text input.
//! - [`SelectList`] (`select-list.ts`) — scrollable single-select list.
//! - [`SettingsList`] (`settings-list.ts`) — scrollable settings list.

pub mod input;
pub mod select_list;
pub mod settings_list;

pub use input::Input;
pub use select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
    SelectListTruncatePrimaryContext,
};
pub use settings_list::{
    SettingItem, SettingsList, SettingsListOptions, SettingsListTheme, SubmenuDone, SubmenuFactory,
};
