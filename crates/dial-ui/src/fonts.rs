//! Bundled typefaces (ADR 0011): Inter 4.1 for the UI and JetBrains Mono 2.304 for code,
//! shipped unmodified under the SIL OFL 1.1 (license texts sit next to the files in
//! `assets/fonts/`). They are registered before the first window because GPUI panics when
//! a named family cannot be resolved.

use std::borrow::Cow;

use gpui_kit::App;

use crate::{Error, Result};

/// UI typeface family name.
pub const UI_FONT_FAMILY: &str = "Inter";
/// Code / terminal typeface family name.
pub const MONO_FONT_FAMILY: &str = "JetBrains Mono";
/// GPUI's alias for the platform UI font (San Francisco, Segoe UI, the desktop's sans).
pub const SYSTEM_UI_FONT_FAMILY: &str = ".SystemUIFont";
/// The platform's default monospace family.
#[cfg(target_os = "macos")]
pub const SYSTEM_MONO_FONT_FAMILY: &str = "Menlo";
/// The platform's default monospace family.
#[cfg(target_os = "windows")]
pub const SYSTEM_MONO_FONT_FAMILY: &str = "Consolas";
/// The platform's default monospace family.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const SYSTEM_MONO_FONT_FAMILY: &str = "DejaVu Sans Mono";

/// Regular, Medium and SemiBold only: the design language uses weights 400/500/600.
const FONT_FILES: [&[u8]; 6] = [
    include_bytes!("../assets/fonts/inter/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/inter/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/inter/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Medium.ttf"),
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-SemiBold.ttf"),
];

/// Registers the bundled fonts with the platform text system.
pub(crate) fn register(cx: &App) -> Result<()> {
    let fonts = FONT_FILES
        .iter()
        .map(|bytes| Cow::Borrowed(*bytes))
        .collect();
    cx.text_system()
        .add_fonts(fonts)
        .map_err(|err| Error::Fonts(format!("{err:#}")))
}
