//! Design tokens (ADR 0011). Views take spacing, sizes and type sizes from here and colours
//! from the active theme (`cx.theme()`), never literals.
//!
//! Type sizes are rems: the root rem is the theme's UI font size (default 16 px), so the
//! "UI font size" setting scales dial's own text together with gpui-kit components.

use gpui_kit::{Pixels, Rems, Size, px, rems, size};

/// 4 px spacing grid.
pub mod space {
    use super::{Pixels, px};

    /// 2 px.
    pub const XXS: Pixels = px(2.);
    /// 4 px.
    pub const XS: Pixels = px(4.);
    /// 6 px.
    pub const SM: Pixels = px(6.);
    /// 8 px.
    pub const MD: Pixels = px(8.);
    /// 12 px.
    pub const LG: Pixels = px(12.);
    /// 16 px.
    pub const XL: Pixels = px(16.);
    /// 24 px.
    pub const XXL: Pixels = px(24.);
    /// 32 px.
    pub const XXXL: Pixels = px(32.);
}

/// Type scale (rems of the root UI font size; px values at the default 16 px root).
pub mod text {
    use super::{Rems, rems};

    /// 11 px: captions, key hints.
    pub const CAPTION: Rems = rems(0.6875);
    /// 13 px: body copy and controls.
    pub const BODY: Rems = rems(0.8125);
    /// 18 px line height for body copy.
    pub const BODY_LINE_HEIGHT: Rems = rems(1.125);
    /// 15 px: section titles.
    pub const TITLE: Rems = rems(0.9375);
    /// 24 px: empty-state headline.
    pub const DISPLAY: Rems = rems(1.5);
}

/// Fixed chrome and window geometry.
pub mod chrome {
    use super::{Pixels, Size, px, size};

    /// Client-side titlebar height on every platform; matches Zed's
    /// `platform_title_bar_height` at the default 16 px rem (`max(1.75rem, 34px)`).
    pub const TITLE_BAR_HEIGHT: Pixels = px(34.);
    /// macOS traffic-light offset from the window's top-left corner; centres the 16 px
    /// button frame vertically in [`TITLE_BAR_HEIGHT`] (Zed uses the same 9 px).
    pub const TRAFFIC_LIGHT_INSET: Pixels = px(9.);
    /// Lucide icon size (1.5 stroke).
    pub const ICON: Pixels = px(16.);
    /// Main window initial size.
    pub const MAIN_WINDOW: Size<Pixels> = size(px(1280.), px(820.));
    /// Main window minimum size.
    pub const MAIN_WINDOW_MIN: Size<Pixels> = size(px(720.), px(480.));
    /// Settings window initial size.
    pub const SETTINGS_WINDOW: Size<Pixels> = size(px(860.), px(620.));
    /// Settings window minimum size.
    pub const SETTINGS_WINDOW_MIN: Size<Pixels> = size(px(640.), px(420.));
    /// Width of the empty-state column.
    pub const EMPTY_STATE_WIDTH: Pixels = px(420.);
}
