//! Design tokens (ADR 0011, UI v2 spec). Views take spacing, sizes and type sizes from here
//! and colours from the active theme (`cx.theme()`), never literals. Components in
//! [`crate::ui`] are the main consumers; pages rarely need more than [`space`], [`text`]
//! and [`page`].
//!
//! Type sizes are rems: the root rem is the theme's UI font size (default 16 px), so the
//! "UI font size" setting scales the app's own text together with gpui-kit components.
//! Control heights are rems too (derived from the 13/18 text line plus padding), so text
//! and controls grow together; px values below are at the default 16 px root.
//!
//! Radii are not tokens: they come from the theme (`theme.radius` for controls,
//! `theme.radius_lg` for cards) so the corner-style setting reaches every component;
//! [`radius`] derives the nested ones, and [`row::radius`] caps list-row state shapes.

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
/// Weights: 400 body, 500 controls/meta, 600 titles — never others.
pub mod text {
    use super::{Rems, rems};

    /// 11 px: meta, badges, captions, key hints, status bar.
    pub const CAPTION: Rems = rems(0.6875);
    /// 14 px line for [`CAPTION`].
    pub const CAPTION_LINE_HEIGHT: Rems = rems(0.875);
    /// 12 px: secondary text, paths, labels, small controls.
    pub const SMALL: Rems = rems(0.75);
    /// 16 px line for [`SMALL`].
    pub const SMALL_LINE_HEIGHT: Rems = rems(1.);
    /// 13 px: body copy, list names and controls.
    pub const BODY: Rems = rems(0.8125);
    /// 18 px line height for body copy.
    pub const BODY_LINE_HEIGHT: Rems = rems(1.125);
    /// 14 px: section titles (weight 600).
    pub const SECTION: Rems = rems(0.875);
    /// 20 px line for [`SECTION`].
    pub const SECTION_LINE_HEIGHT: Rems = rems(1.25);
    /// 20 px: page titles (weight 600).
    pub const PAGE_TITLE: Rems = rems(1.25);
    /// 26 px line for [`PAGE_TITLE`].
    pub const PAGE_TITLE_LINE_HEIGHT: Rems = rems(1.625);
    /// 28 px: hero numbers (overview, summaries; weight 600, tabular).
    pub const HERO: Rems = rems(1.75);
    /// 32 px line for [`HERO`].
    pub const HERO_LINE_HEIGHT: Rems = rems(2.);
}

/// Control scale shared by buttons, inputs, selects and segmented controls so toolbars
/// align on one baseline (see [`crate::ui::ControlSize`]).
pub mod control {
    use super::{Pixels, Rems, px, rems};

    /// 24 px: row actions, chips, toolbar segmented controls.
    pub const HEIGHT_SM: Rems = rems(1.5);
    /// 28 px: the default for every button, input and select.
    pub const HEIGHT_MD: Rems = rems(1.75);
    /// 32 px: at most one per screen (the empty-state primary action).
    pub const HEIGHT_LG: Rems = rems(2.);
    /// Horizontal text padding of `sm` controls.
    pub const PAD_X_SM: Pixels = px(8.);
    /// Horizontal padding of `md` controls with an icon.
    pub const PAD_X_MD: Pixels = px(10.);
    /// Horizontal padding of text-only `md` controls.
    pub const PAD_X_MD_TEXT: Pixels = px(12.);
    /// Horizontal padding of `lg` controls.
    pub const PAD_X_LG: Pixels = px(14.);
    /// Icon inside `sm`/`md` controls.
    pub const ICON: Pixels = px(14.);
    /// Icon inside `lg` controls.
    pub const ICON_LG: Pixels = px(16.);
    /// Gap between a control's icon and label.
    pub const GAP: Pixels = px(6.);
    /// Opacity of disabled controls.
    pub const DISABLED_OPACITY: f32 = 0.45;
    /// Inset of a segmented control's track around its segments.
    pub const SEGMENT_INSET: Pixels = px(2.);
}

/// Nested radii derived from the theme's control radius (`theme.radius`), so the corner
/// style setting scales them too.
pub mod radius {
    use super::{Pixels, px};

    /// Badges and checkboxes: control radius − 2 (4 px by default).
    pub fn inner(control: Pixels) -> Pixels {
        px((f32::from(control) - 2.).max(0.))
    }

    /// Large buttons and icon tiles: control radius + 2 (8 px by default); square stays
    /// square.
    pub fn outer(control: Pixels) -> Pixels {
        let r = f32::from(control);
        if r <= 0. { px(0.) } else { px(r + 2.) }
    }
}

/// Selection controls.
pub mod selection {
    use super::{Pixels, px};

    /// Checkbox box.
    pub const CHECKBOX: Pixels = px(16.);
    /// Checkbox border width.
    pub const CHECKBOX_BORDER: Pixels = px(1.5);
    /// Check glyph inside the box.
    pub const CHECK_ICON: Pixels = px(12.);
    /// Indeterminate bar width.
    pub const INDETERMINATE_BAR: Pixels = px(8.);
    /// Indeterminate bar height.
    pub const INDETERMINATE_BAR_HEIGHT: Pixels = px(2.);
    /// Switch track width.
    pub const SWITCH_WIDTH: Pixels = px(28.);
    /// Switch track height.
    pub const SWITCH_HEIGHT: Pixels = px(16.);
    /// Switch thumb.
    pub const SWITCH_THUMB: Pixels = px(12.);
    /// Minimum hit target of any control (ADR 0011).
    pub const HIT_TARGET: Pixels = px(24.);
}

/// Badges and status dots.
pub mod badge {
    use super::{Pixels, px};

    /// Badge height.
    pub const HEIGHT: Pixels = px(18.);
    /// Badge horizontal padding.
    pub const PAD_X: Pixels = px(6.);
    /// Tint alpha of a badge's fill (text is the tone colour).
    pub const TINT: f32 = 0.12;
    /// Status dot.
    pub const DOT: Pixels = px(6.);
    /// Muted fact icon beside a row (tooltip explains it).
    pub const FACT_ICON: Pixels = px(14.);
}

/// Lists and tables.
///
/// Row states (every list row and group header, see [`crate::ui::ListRow`]): rows sit
/// [`INSET`] inside their card's edge and draw every state on that inset box with
/// [`radius`] corners — hover = muted at [`HOVER_ALPHA`], pressed = muted at
/// [`PRESS_ALPHA`], the one opened row of a master-detail list = muted at
/// [`CURRENT_ALPHA`], keyboard focus = the 2 px focus ring around the same box, disabled =
/// [`super::control::DISABLED_OPACITY`]. Checked rows get no fill: the checkbox shows the
/// selection, so stacked selected rows never form a column of rounded notches. Group
/// headers hover like rows and never keep a fill.
///
/// Leading grid ([`crate::ui::RowGrid`]): `[disclosure][checkbox][icon] title`, each column
/// a [`SLOT`] cell [`SLOT_GAP`] apart, so a header's checkbox and title line up with its
/// items'. Trailing cells (badge, age, size) are [`GAP`] apart and end at the same right
/// padding, so fixed-width columns share their right edges across headers and items.
pub mod row {
    use super::{Pixels, px};

    /// Single-line row.
    pub const HEIGHT: Pixels = px(32.);
    /// Two-line row (name 13/18 over detail 11/14).
    pub const HEIGHT_TWO_LINE: Pixels = px(40.);
    /// Group header row.
    pub const GROUP_HEIGHT: Pixels = px(32.);
    /// Rows keep this far off their card's edge (the list's padding), so the rounded hover
    /// and focus shapes never touch the card border.
    pub const INSET: Pixels = px(4.);
    /// Largest corner radius of a row's state shape.
    pub const RADIUS: Pixels = px(6.);
    /// Horizontal padding inside the (inset) row box.
    pub const PAD_X: Pixels = px(12.);
    /// Gap between trailing cells, and between the title and the first of them.
    pub const GAP: Pixels = px(12.);
    /// Width of one leading grid column (disclosure, checkbox, icon).
    pub const SLOT: Pixels = px(16.);
    /// Gap between leading grid columns and before the title.
    pub const SLOT_GAP: Pixels = px(8.);
    /// Hover fill alpha over the muted colour.
    pub const HOVER_ALPHA: f32 = 0.5;
    /// Pressed fill alpha over the muted colour.
    pub const PRESS_ALPHA: f32 = 0.8;
    /// Fill alpha of the one opened (current) row of a master-detail list.
    pub const CURRENT_ALPHA: f32 = 1.;
    /// Disclosure chevron glyph (centred in its [`SLOT`]).
    pub const CHEVRON: Pixels = px(14.);
    /// Right-aligned tabular size column.
    pub const SIZE_COLUMN: Pixels = px(88.);
    /// Muted age column.
    pub const AGE_COLUMN: Pixels = px(96.);

    /// Corner radius of a row's state shape: the theme's control radius, at most
    /// [`RADIUS`] (square corner styles stay square).
    pub fn radius(control: Pixels) -> Pixels {
        px(f32::from(control).clamp(0., f32::from(RADIUS)))
    }
}

/// Cards (content sections).
pub mod card {
    use super::{Pixels, px};

    /// Inner padding.
    pub const PADDING: Pixels = px(16.);
    /// Header padding (vertical).
    pub const HEADER_PAD_Y: Pixels = px(12.);
    /// Hairline border alpha over the theme border.
    pub const BORDER_ALPHA: f32 = 0.6;
}

/// Empty and idle states.
pub mod empty {
    use super::{Pixels, px};

    /// Muted circle behind the icon.
    pub const ICON_CIRCLE: Pixels = px(40.);
    /// Icon inside the circle.
    pub const ICON: Pixels = px(20.);
    /// Maximum width of the explanation line.
    pub const TEXT_WIDTH: Pixels = px(360.);
}

/// Progress indicators.
pub mod progress {
    use std::time::Duration;

    use super::{Pixels, px};

    /// Determinate bar thickness.
    pub const BAR_HEIGHT: Pixels = px(4.);
    /// Width of the indeterminate shimmer segment, as a fraction of the track.
    pub const SHIMMER_FRACTION: f32 = 0.3;
    /// Frame cap of the indeterminate shimmer loop.
    pub const SHIMMER_FPS: f32 = 30.;
    /// One sweep of the indeterminate shimmer (a decorative loop, so a duration).
    pub const SHIMMER_PERIOD: Duration = Duration::from_millis(1400);
    /// Opacity of the static indeterminate bar under reduced motion.
    pub const STATIC_OPACITY: f32 = 0.6;
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
    /// Width of the overview and area page column.
    pub const EMPTY_STATE_WIDTH: Pixels = px(420.);
    /// Prompt window (a pending automation run: summary, top items, decision buttons).
    pub const PROMPT_WINDOW: Size<Pixels> = size(px(460.), px(560.));
    /// Prompt window minimum size.
    pub const PROMPT_WINDOW_MIN: Size<Pixels> = size(px(400.), px(440.));
}

/// Main-window layout.
pub mod layout {
    use super::{Pixels, px};

    /// Footer status bar height.
    pub const STATUS_BAR_HEIGHT: Pixels = px(24.);
    /// Connection-state dot in the status bar.
    pub const STATUS_DOT: Pixels = px(6.);
    /// Sidebar width when shown.
    pub const SIDEBAR_WIDTH: Pixels = px(224.);
    /// Sidebar item height.
    pub const SIDEBAR_ITEM_HEIGHT: Pixels = px(28.);
    /// Height of a foldable sidebar group heading.
    pub const SIDEBAR_GROUP_HEIGHT: Pixels = px(24.);
    /// Hover fill of an unselected sidebar item, as a share of the selection fill.
    pub const SIDEBAR_HOVER_ALPHA: f32 = 0.6;
    /// Tile behind an area's icon in its page header.
    pub const PAGE_ICON_TILE: Pixels = px(28.);
    /// Horizontal page padding.
    pub const PAGE_PAD_X: Pixels = px(24.);
    /// Top page padding.
    pub const PAGE_PAD_TOP: Pixels = px(20.);
    /// Gap between blocks inside a section.
    pub const BLOCK_GAP: Pixels = px(16.);
    /// Gap between sections.
    pub const SECTION_GAP: Pixels = px(24.);
}

/// Area pages: result lists, size bars, cards and stream coalescing.
pub mod page {
    use std::time::Duration;

    use super::{Pixels, px};

    /// Thickness of a proportional size bar.
    pub const SIZE_BAR_HEIGHT: Pixels = px(6.);
    /// Width of the size-bar column in the space lens.
    pub const SIZE_BAR_WIDTH: Pixels = px(160.);
    /// Maximum width of a page's content column.
    pub const CONTENT_MAX_WIDTH: Pixels = px(960.);
    /// Narrowest overview area tile: all five fit one row of the content column; narrower
    /// windows wrap.
    pub const TILE_MIN_WIDTH: Pixels = px(160.);
    /// Width of the confirmation dialogs.
    pub const DIALOG_WIDTH: Pixels = px(440.);
    /// Width of the rule list beside the rule editor.
    pub const RULES_LIST_WIDTH: Pixels = px(300.);
    /// The rule editor wraps below the list when it would get narrower than this.
    pub const RULES_EDITOR_MIN_WIDTH: Pixels = px(360.);
    /// Width of the When/Where/If/Then/Before labels of the rule editor.
    pub const RULES_LABEL_WIDTH: Pixels = px(56.);
    /// Width of the command palette.
    pub const PALETTE_WIDTH: Pixels = px(520.);
    /// Tallest the command palette's result list grows before it scrolls.
    pub const PALETTE_LIST_MAX_HEIGHT: Pixels = px(320.);
    /// Distance of the command palette from the top of the window.
    pub const PALETTE_TOP: Pixels = px(72.);
    /// Narrowest automation card of the overview dashboard; three fit a row of the content
    /// column, narrower windows wrap.
    pub const DASHBOARD_CARD_MIN_WIDTH: Pixels = px(200.);
    /// Streamed job progress re-renders a page at most this often (one frame at 30 Hz).
    pub const STREAM_COALESCE: Duration = Duration::from_millis(33);
}

/// Space-lens treemap (see [`crate::ui::Treemap`]). Layout lengths are plain `f32` px
/// because the layout runs in `f32`.
pub mod treemap {
    use std::time::Duration;

    use super::{Pixels, px};

    /// Gap between neighbouring tiles (each tile gives up half on every side).
    pub const GAP: f32 = 1.;
    /// Corner radius of level-1 tiles.
    pub const RADIUS: Pixels = px(4.);
    /// Corner radius of level-2 tiles (nested inside a level-1 tile's 2 px padding).
    pub const RADIUS_NESTED: Pixels = px(2.);
    /// Smallest tile side worth drawing on its own: smaller items merge into one
    /// "other" tile.
    pub const MIN_SIDE: f32 = 3.;
    /// Padding between a level-1 tile's edge and its level-2 tiles.
    pub const NEST_PAD: f32 = 2.;
    /// A level-1 tile is subdivided only when both sides are at least this long.
    pub const NEST_MIN_SIDE: f32 = 24.;
    /// Label strip on top of a subdivided level-1 tile (12/16 text + padding).
    pub const HEADER: f32 = 20.;
    /// Narrowest tile that gets a label.
    pub const LABEL_MIN_WIDTH: f32 = 56.;
    /// Lowest tile that gets a two-line label (name 12/16 over size 11/14 + padding).
    pub const LABEL_MIN_HEIGHT: f32 = 38.;
    /// Horizontal label inset.
    pub const LABEL_PAD_X: f32 = 6.;
    /// Vertical label inset.
    pub const LABEL_PAD_Y: f32 = 3.;
    /// At most this many labels per map (the largest tiles get them).
    pub const MAX_LABELS: usize = 160;
    /// Opacity of the size line under a tile's name.
    pub const SIZE_OPACITY: f32 = 0.78;
    /// Width of the hover outline and the keyboard-focus outline.
    pub const OUTLINE: Pixels = px(2.);
    /// Accent overlay on selected tiles.
    pub const SELECTED_ALPHA: f32 = 0.4;
    /// Stripe width of the hatched "rest" tile.
    pub const HATCH_WIDTH: f32 = 1.;
    /// Stripe interval of the hatched "rest" tile.
    pub const HATCH_INTERVAL: f32 = 5.;
    /// Child directories whose own children are fetched after each navigation.
    pub const PREFETCH: usize = 16;
    /// Window width from which the ranked list sits beside the map (else below it).
    pub const SIDE_PANEL_MIN_WINDOW: Pixels = px(1100.);
    /// Width of the ranked list beside the map.
    pub const SIDE_PANEL_WIDTH: Pixels = px(340.);
    /// Height of the ranked list below the map (eight 32 px rows).
    pub const LIST_BELOW_HEIGHT: Pixels = px(256.);
    /// Lowest map.
    pub const MAP_MIN_HEIGHT: Pixels = px(240.);
    /// Content width cap of the space-lens page (the map benefits from room).
    pub const PAGE_MAX_WIDTH: Pixels = px(1440.);
    /// Colour swatch of a ranked-list row.
    pub const SWATCH: Pixels = px(10.);
    /// Size-bar column of a ranked-list row.
    pub const LIST_BAR_WIDTH: Pixels = px(56.);
    /// Drill-down crossfade (one-shot, ≤ 150 ms; the same under reduced motion).
    pub const FADE: Duration = Duration::from_millis(120);
    /// One breath of the scanning placeholder (a decorative loop, so a duration).
    pub const PULSE_PERIOD: Duration = Duration::from_millis(1600);
    /// Frame cap of the placeholder loop.
    pub const PULSE_FPS: f32 = 30.;
    /// Lowest opacity of the placeholder pulse (and its static opacity under reduced
    /// motion).
    pub const PULSE_MIN_OPACITY: f32 = 0.45;
    /// Hues of the palette (one per level-1 child, repeating).
    pub const HUES: usize = 8;
    /// Hue step between palette entries, degrees.
    pub const HUE_STEP: f32 = 45.;

    /// OKLCH lightness/chroma of a mode's tiles.
    #[derive(Clone, Copy, Debug)]
    pub struct Shades {
        /// Folder tiles.
        pub dir: (f32, f32),
        /// File tiles (lighter than folders in both modes).
        pub file: (f32, f32),
        /// Lightness shift of level-2 tiles (alternating ±, away from the parent).
        pub nested: f32,
        /// Label text on tiles.
        pub ink: (f32, f32),
        /// Stronger tone of a hue for thin marks (the ranked list's size bars), which
        /// need contrast against the muted track.
        pub strong: (f32, f32),
        /// Neutral "rest"/"other" tiles.
        pub neutral: (f32, f32),
        /// Lightness shift of the hatching stripes on neutral tiles.
        pub stripe: f32,
    }

    /// Light mode: quiet pastel tiles under dark text.
    pub const LIGHT: Shades = Shades {
        dir: (0.8, 0.075),
        file: (0.89, 0.05),
        nested: 0.035,
        ink: (0.27, 0.03),
        strong: (0.62, 0.11),
        neutral: (0.9, 0.004),
        stripe: -0.07,
    };

    /// Dark mode: muted mid-tones under light text (never pure black).
    pub const DARK: Shades = Shades {
        dir: (0.43, 0.07),
        file: (0.53, 0.05),
        nested: 0.035,
        ink: (0.97, 0.01),
        strong: (0.72, 0.1),
        neutral: (0.33, 0.004),
        stripe: 0.08,
    };
}
