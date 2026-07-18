//! Leaf display widgets — byte-exact ports of pi's `components/*.ts`.
//!
//! Each type implements [`crate::renderer::Component`] and reproduces pi's
//! `render(width)` output byte-for-byte. Ports:
//! - [`Spacer`] (`spacer.ts`)
//! - [`Text`] (`text.ts`)
//! - [`TruncatedText`] (`truncated-text.ts`)
//! - [`BoxWidget`] (`box.ts`; renamed to avoid shadowing `std::boxed::Box`)
//! - [`Loader`] (`loader.ts`)
//! - [`Image`] (`image.ts`)

pub mod box_widget;
pub mod image;
pub mod loader;
pub mod spacer;
pub mod text;
pub mod truncated_text;

pub use box_widget::BoxWidget;
pub use image::{Image, ImageOptions, ImageTheme};
pub use loader::{Loader, LoaderIndicatorOptions};
pub use spacer::Spacer;
pub use text::{BgFn, Text};
pub use truncated_text::TruncatedText;
