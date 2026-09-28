//! The app's component library (UI v2, the shadcn model): controls built on gpui-base's
//! headless primitives (which own focus, keyboard and accessibility) and styled only from
//! [`crate::tokens`] and theme colours. Pages compose these; they never style a control
//! themselves. Restyles change this module and the tokens, not pages.
//!
//! Every component is a builder that implements `IntoElement` (via `RenderOnce`): create
//! it with `new(..)`, chain options, pass it to `.child(..)`. Focusable components take an
//! `id`, show a 2 px focus ring on keyboard focus, and have hit targets ≥ 24 px.
//! Callbacks receive `&mut Window, &mut App`; wrap view methods with `cx.listener(..)`.
//!
//! # Catalogue — what to use when
//!
//! **Actions**
//! - [`Button`] — every text button. Variants ([`ButtonVariant`]): `primary` (the ONE main
//!   action of a view), `secondary` (default for toolbar actions: "Rescan", "Select…"),
//!   `outline` (a secondary action next to another secondary), `ghost` (tertiary, inline),
//!   `danger` (only the destructive confirm inside a confirmation). Sizes ([`ControlSize`]):
//!   `sm` 24 px (row actions, chips), `md` 28 px (default), `lg` 32 px (at most one per
//!   screen: the empty-state primary). Options: `.icon(IconName)`, `.loading(bool)`,
//!   `.selected(bool)` (toggle/chip look), `.disabled(bool)`, `.tooltip(text)`.
//! - [`IconButton`] — square icon-only button (24/28 px, ghost by default); the tooltip is a
//!   required constructor argument.
//! - [`Kbd`] — key-binding hint for an action (renders nothing when unbound).
//!
//! **Inputs and selection**
//! - [`TextInput`] — gpui-component `Input` pinned to the control scale (28 px, 13 px
//!   text); `.search()` adds the leading search icon; clear button on by default. The page
//!   owns the `InputState` entity (create it in `new`, never in `render`).
//! - [`Checkbox`] — 16 px tri-state box ([`CheckState`], `CheckState::from_counts` for
//!   group checkboxes), optional `.label(..)`. Its click never reaches the parent row, so it
//!   is safe inside clickable rows and headers.
//! - [`Switch`] — 28×16 on/off for settings that apply immediately.
//! - [`Segmented`] — single choice among 2–5 short options (filters, sort keys, views);
//!   `sm` inside toolbars, `md` elsewhere. Prefer it over rows of toggle buttons.
//! - [`Select`] — single choice from a dropdown (more options than fit a [`Segmented`], or
//!   long labels; the settings controls): outline trigger with the selected label and a
//!   caret, `md` 28 px (default) or `.small()` 24 px, `.disabled(..)`, `.anchor(..)`; the
//!   menu scrolls past 8 options. `(value, label)` options, `.selected(value)`,
//!   `.on_change(..)` gets the chosen value; `.label(..)` is the trigger text when nothing
//!   matches.
//!
//! **Structure**
//! - [`PageHeader`] — top of every page: icon tile, 20 px title, description, and the page
//!   toolbar (secondary actions + the one primary, all `md`) on the right.
//! - [`Card`] + [`CardHeader`] — every content section (results, summaries, forms). Use
//!   `.flush()` for cards that hold list rows edge to edge.
//! - [`SectionHeader`] — a titled group inside a page or card without a card of its own.
//! - [`Toolbar`] — a horizontal run of controls with the standard gap.
//! - [`CollapsibleHeader`] — a 32 px group header of a result list: the whole row toggles
//!   (click, Enter, Space), the optional checkbox slot selects without toggling, the chevron
//!   turns on a spring (instant under reduced motion).
//! - [`ListRow`] — one- (32 px) or two-line (40 px) rows: `.checkbox(..)`/`.icon(..)` on the
//!   leading grid, free `.leading(..)`, name + detail (`.detail_lead(id)` puts a technical
//!   id before it, `.detail_mono()` for commands), trailing cells, optional click (then
//!   focusable). States (`tokens::row`): hover/press neutral fill on the inset box, focus
//!   ring, `.current(..)` for the one opened row, `.disabled(..)`; checked rows get NO fill
//!   (the checkbox shows selection). [`row_icon`] — a 16 px app icon with a glyph fallback.
//! - [`RowGrid`] — the leading columns `[disclosure][checkbox][icon]` a grouped list shares:
//!   give the header and its items the same grid and checkboxes/titles line up.
//! - [`NavItem`] — a 28 px sidebar entry (main sidebar, settings navigation): 16 px icon,
//!   13 px label, optional `.suffix(..)` (key hint); `.selected(..)` = neutral sidebar fill
//!   and 500 weight (never the accent); faint hover; focus ring.
//!
//! **Data and status**
//! - [`Badge`] — at most ONE per row: the most important fact, tinted, no border ([`Tone`]).
//! - [`Dot`] — 6 px status dot for secondary facts ("app running" = warning dot) and the
//!   connection state; [`FactIcon`] — 14 px muted icon + tooltip for facts like "needs
//!   admin" (lock).
//! - [`Stat`] — label over a tabular number (summary strips: 20 px; `.hero()` 28 px).
//! - [`ProgressBar`] — 4 px determinate bar, or an indeterminate shimmer (≤ 30 fps, static
//!   under reduced motion).
//! - [`EmptyState`] — centred icon circle, title, one line, and one `lg` primary action.
//! - [`Treemap`] — squarified disk map from precomputed [`MapTile`]s ([`squarify`] lays
//!   them out; [`hit`] finds a tile): one canvas paint pass, labels only on large tiles,
//!   accent outline on hover/keyboard cursor, accent overlay on selected tiles, one
//!   hover/click listener per map. [`TreemapPlaceholder`] — breathing muted blocks while
//!   the data is being measured (static under reduced motion).
//!
//! Helpers: [`tabular`] (tabular figures for sizes/counts), [`Tone::color`],
//! [`selected_of`] / [`item_count`] (localised group-header summaries).

/// `Debug` for builders that hold callbacks and child elements: prints the type name only.
macro_rules! opaque_debug {
    ($($ty:ty),+ $(,)?) => {
        $(impl std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($ty)).finish_non_exhaustive()
            }
        })+
    };
}

mod badge;
mod button;
mod card;
mod checkbox;
mod collapsible;
mod empty;
mod grid;
mod input;
mod kbd;
mod list;
mod nav;
mod progress;
mod section;
mod segmented;
mod select;
mod stat;
mod switch;
mod treemap;

use std::sync::{Arc, LazyLock};

use gpui_kit::component::{ActiveTheme as _, ThemeStyled as _};
use gpui_kit::{
    App, ElementId, FocusHandle, FontFeatures, FontWeight, Hsla, ParentElement, Pixels, Rems,
    SharedString, Styled, Window,
};

pub use badge::{Badge, Dot, FactIcon};
pub use button::{Button, ButtonVariant, IconButton};
pub use card::{Card, CardHeader};
pub use checkbox::{CheckState, Checkbox};
pub use collapsible::{CollapsibleHeader, item_count, selected_of};
pub use empty::EmptyState;
pub use grid::RowGrid;
pub use input::TextInput;
pub use kbd::Kbd;
pub use list::{ListRow, row_icon};
pub use nav::NavItem;
pub use progress::ProgressBar;
pub use section::{PageHeader, SectionHeader, Toolbar};
pub use segmented::Segmented;
pub use select::Select;
pub use stat::Stat;
pub use switch::Switch;
pub use treemap::{
    MapTile, Placed, Rect, Slot, TileLabel, Treemap, TreemapPlaceholder, hit, squarify,
};

use crate::tokens::{control, text};

opaque_debug!(
    Badge,
    Button,
    Card,
    CardHeader,
    Checkbox,
    CollapsibleHeader,
    Dot,
    EmptyState,
    FactIcon,
    IconButton,
    Kbd,
    ListRow,
    NavItem,
    PageHeader,
    ProgressBar,
    SectionHeader,
    Segmented,
    Select,
    Stat,
    Switch,
    TextInput,
    Toolbar,
);

/// Control height scale shared by buttons, inputs and segmented controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ControlSize {
    /// 24 px, 12/500 text: row actions, chips, toolbar segmented controls.
    Sm,
    /// 28 px, 13/500 text: the default.
    #[default]
    Md,
    /// 32 px, 13/600 text: at most one per screen.
    Lg,
}

impl ControlSize {
    /// Outer height.
    pub const fn height(self) -> Rems {
        match self {
            Self::Sm => control::HEIGHT_SM,
            Self::Md => control::HEIGHT_MD,
            Self::Lg => control::HEIGHT_LG,
        }
    }

    /// Label size.
    pub const fn text(self) -> Rems {
        match self {
            Self::Sm => text::SMALL,
            Self::Md | Self::Lg => text::BODY,
        }
    }

    /// Label weight.
    pub const fn weight(self) -> FontWeight {
        match self {
            Self::Sm | Self::Md => FontWeight::MEDIUM,
            Self::Lg => FontWeight::SEMIBOLD,
        }
    }

    /// Horizontal padding; text-only `md` controls get a little more air.
    pub const fn pad_x(self, text_only: bool) -> Pixels {
        match self {
            Self::Sm => control::PAD_X_SM,
            Self::Md if text_only => control::PAD_X_MD_TEXT,
            Self::Md => control::PAD_X_MD,
            Self::Lg => control::PAD_X_LG,
        }
    }

    /// Icon size.
    pub const fn icon(self) -> Pixels {
        match self {
            Self::Sm | Self::Md => control::ICON,
            Self::Lg => control::ICON_LG,
        }
    }

    /// Corner radius for the theme's control radius.
    pub fn radius(self, control_radius: Pixels) -> Pixels {
        match self {
            Self::Sm | Self::Md => control_radius,
            Self::Lg => crate::tokens::radius::outer(control_radius),
        }
    }
}

/// Semantic colour of badges, dots, notices and progress.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    /// Neutral fact.
    #[default]
    Neutral,
    /// Informational / accent.
    Accent,
    /// Succeeded / safe.
    Success,
    /// Needs attention.
    Warning,
    /// Failed / destructive.
    Danger,
}

impl Tone {
    /// Foreground colour of the tone (text, dots, icons).
    pub fn color(self, cx: &App) -> Hsla {
        let theme = cx.theme();
        match self {
            Self::Neutral => theme.muted_foreground,
            Self::Accent => theme.primary,
            Self::Success => theme.success,
            Self::Warning => theme.warning,
            Self::Danger => theme.danger,
        }
    }
}

/// OpenType features for tabular (fixed-width) figures: sizes and counts that update in
/// place don't jitter. Shared, so applying it costs an `Arc` clone.
pub fn tabular() -> FontFeatures {
    static TABULAR: LazyLock<FontFeatures> =
        LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".to_owned(), 1)])));
    TABULAR.clone()
}

/// A child id derived from `id` (`id/channel`), for state a component keeps per part.
pub(crate) fn child_id(id: &ElementId, channel: &'static str) -> ElementId {
    ElementId::NamedChild(Arc::new(id.clone()), SharedString::new_static(channel))
}

/// The focus handle a component keeps under `id` across frames.
pub(crate) fn focus_handle(id: &ElementId, window: &mut Window, cx: &mut App) -> FocusHandle {
    window
        .use_keyed_state(child_id(id, "focus"), cx, |_, cx| cx.focus_handle())
        .read(cx)
        .clone()
}

/// Draws the focus ring (accent border + 2 px ring outside) when `handle` has keyboard
/// focus. Call after the element's radius and border are set: the ring follows them.
pub(crate) fn focus_ring<E: Styled + ParentElement>(
    element: E,
    handle: &FocusHandle,
    window: &Window,
    cx: &App,
) -> E {
    if handle.is_focused(window) && window.last_input_was_keyboard() {
        element.focus_ring_style(window, cx)
    } else {
        element
    }
}
