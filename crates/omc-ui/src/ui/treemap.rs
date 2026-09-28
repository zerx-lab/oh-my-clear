//! [`Treemap`]: a disk map drawn from precomputed tiles, and [`squarify`], the
//! squarified treemap layout (Bruls, Huizing, van Wijk 2000) that computes them.
//!
//! The layout is a pure function in the map's local pixel space (`f32`), so pages lay out
//! once per size or data change and render many times. Rendering is one `canvas` paint
//! pass for the fills (thousands of quads cost no element tree), plus absolutely placed
//! labels for the few tiles large enough to carry one. Hit testing runs against the same
//! tiles, so there is one listener per map, not per tile.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, LazyLock};

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, BorderStyle, Bounds, ClickEvent, ElementId,
    FocusHandle, FontWeight, Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseMoveEvent,
    ParentElement as _, Pixels, RenderOnce, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, canvas, div, fill, pattern_slash, point, px, quad, relative, size,
    transparent_black,
};

use crate::tokens::{text, treemap};

/// A rectangle in a map's local pixel space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// A rectangle at `(x, y)` of `w × h`.
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Its area (0 for degenerate rectangles).
    pub fn area(self) -> f32 {
        self.w.max(0.) * self.h.max(0.)
    }

    /// Shrunk by the given edge insets; `None` when nothing is left.
    pub fn inset(self, top: f32, right: f32, bottom: f32, left: f32) -> Option<Self> {
        let r = Self::new(
            self.x + left,
            self.y + top,
            self.w - left - right,
            self.h - top - bottom,
        );
        (r.w > 0. && r.h > 0.).then_some(r)
    }

    /// Whether `(x, y)` lies inside (left/top edges inclusive).
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// What a laid-out tile stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// The input item with this index.
    Item(usize),
    /// Every item too small to draw on its own, merged.
    Other,
}

/// One laid-out tile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    /// What it stands for.
    pub slot: Slot,
    /// Where it goes (no gaps; callers inset for gaps).
    pub rect: Rect,
}

/// Lays out `sizes` in `rect` as a squarified treemap: every tile's area is proportional
/// to its size, tiles don't overlap and together fill `rect`, and rows are grown only
/// while that improves their worst aspect ratio.
///
/// Items are placed largest first; equal sizes keep their input order, so the output is
/// deterministic. Zero sizes get no tile. Items whose tile would be smaller than
/// `min_side × min_side` merge into one [`Slot::Other`] tile placed by its combined size.
pub fn squarify(sizes: &[u64], rect: Rect, min_side: f32) -> Vec<Placed> {
    let total: u64 = sizes.iter().fold(0, |sum, &s| sum.saturating_add(s));
    let area = f64::from(rect.area());
    if total == 0 || area <= 0. {
        return Vec::new();
    }
    let scale = area / as_f64(total);
    let min_area = f64::from(min_side.max(0.)).powi(2);

    let mut order: Vec<(Slot, u64)> = sizes
        .iter()
        .enumerate()
        .filter(|&(_, &s)| s > 0)
        .map(|(ix, &s)| (Slot::Item(ix), s))
        .collect();
    // Stable: equal sizes keep their input order.
    order.sort_by_key(|&(_, s)| std::cmp::Reverse(s));
    let keep = order
        .iter()
        .take_while(|&&(_, s)| as_f64(s) * scale >= min_area)
        .count();
    let merged: u64 = order
        .iter()
        .skip(keep)
        .fold(0, |sum, &(_, s)| sum.saturating_add(s));
    order.truncate(keep);
    if merged > 0 {
        let at = order.partition_point(|&(_, s)| s >= merged);
        order.insert(at, (Slot::Other, merged));
    }

    let areas: Vec<f64> = order.iter().map(|&(_, s)| as_f64(s) * scale).collect();
    let mut out = Vec::with_capacity(order.len());
    let mut free = Free {
        x: f64::from(rect.x),
        y: f64::from(rect.y),
        w: f64::from(rect.w),
        h: f64::from(rect.h),
    };
    let mut start = 0;
    while start < areas.len() {
        let side = free.w.min(free.h);
        let mut end = start.saturating_add(1);
        let mut row = RowStats::of(areas.get(start).copied().unwrap_or(0.));
        while let Some(&next) = areas.get(end) {
            let grown = row.with(next);
            if grown.worst(side) > row.worst(side) {
                break;
            }
            row = grown;
            end = end.saturating_add(1);
        }
        let last_row = end >= areas.len();
        let slots = order.get(start..end).unwrap_or_default();
        let row_areas = areas.get(start..end).unwrap_or_default();
        free.place_row(slots, row_areas, row.sum, last_row, &mut out);
        start = end;
    }
    out
}

/// `u64` → `f64` for area ratios.
fn as_f64(v: u64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "area ratios: 52 bits of mantissa are far beyond pixel precision"
    )]
    let f = v as f64;
    f
}

/// `f64` → `f32` for pixel coordinates.
fn as_f32(v: f64) -> f32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "pixel coordinates of a window fit f32 easily"
    )]
    let f = v as f32;
    f
}

/// The part of the rectangle not yet covered.
struct Free {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl Free {
    /// Places one row along the shorter side and removes it from the free space. The
    /// last tile of a row (and the last row) take the exact remainder, so rounding never
    /// leaves slivers.
    fn place_row(
        &mut self,
        slots: &[(Slot, u64)],
        areas: &[f64],
        sum: f64,
        last_row: bool,
        out: &mut Vec<Placed>,
    ) {
        let vertical = self.w >= self.h;
        let (length, extent) = if vertical {
            (self.h, self.w)
        } else {
            (self.w, self.h)
        };
        let thickness = if last_row || length <= 0. {
            extent
        } else {
            (sum / length).min(extent)
        };
        let mut offset = 0.;
        let count = slots.len();
        for (ix, (&(slot, _), &area)) in slots.iter().zip(areas).enumerate() {
            let along = if ix.saturating_add(1) == count {
                length - offset
            } else if thickness > 0. {
                area / thickness
            } else {
                0.
            };
            let rect = if vertical {
                Rect::new(
                    as_f32(self.x),
                    as_f32(self.y + offset),
                    as_f32(thickness),
                    as_f32(along),
                )
            } else {
                Rect::new(
                    as_f32(self.x + offset),
                    as_f32(self.y),
                    as_f32(along),
                    as_f32(thickness),
                )
            };
            out.push(Placed { slot, rect });
            offset += along;
        }
        if vertical {
            self.x += thickness;
            self.w -= thickness;
        } else {
            self.y += thickness;
            self.h -= thickness;
        }
    }
}

/// Sum, min and max of a candidate row.
#[derive(Clone, Copy)]
struct RowStats {
    sum: f64,
    min: f64,
    max: f64,
}

impl RowStats {
    fn of(area: f64) -> Self {
        Self {
            sum: area,
            min: area,
            max: area,
        }
    }

    fn with(self, area: f64) -> Self {
        Self {
            sum: self.sum + area,
            min: self.min.min(area),
            max: self.max.max(area),
        }
    }

    /// Worst aspect ratio of the row laid along a side of length `side`.
    fn worst(self, side: f64) -> f64 {
        let (s2, w2) = (self.sum * self.sum, side * side);
        if s2 <= 0. || w2 <= 0. || self.min <= 0. {
            return f64::INFINITY;
        }
        (w2 * self.max / s2).max(s2 / (w2 * self.min))
    }
}

/// Index of the topmost tile under `(x, y)` (later tiles are drawn over earlier ones).
pub fn hit(tiles: &[MapTile], x: f32, y: f32) -> Option<usize> {
    tiles.iter().rposition(|t| t.rect.contains(x, y))
}

/// Text drawn on a tile.
#[derive(Clone, Debug)]
pub struct TileLabel {
    /// Name (12/500, ellipsised).
    pub name: SharedString,
    /// Size (11, tabular).
    pub size: SharedString,
    /// Text colour (contrasting with the tile's fill).
    pub color: Hsla,
    /// One line (name and size side by side), for the header strip of a subdivided tile.
    pub inline: bool,
}

/// One tile of a [`Treemap`].
#[derive(Clone, Debug)]
pub struct MapTile {
    /// Where it is, gaps already applied.
    pub rect: Rect,
    /// Fill.
    pub fill: Hsla,
    /// Stripe colour of a hatched tile (aggregates that can't be opened or selected).
    pub hatch: Option<Hsla>,
    /// Corner radius.
    pub radius: Pixels,
    /// Label, when the tile is large enough.
    pub label: Option<TileLabel>,
}

type OnResize = Box<dyn FnOnce(&(f32, f32), &mut Window, &mut App)>;
type OnHover = Rc<dyn Fn(&Option<usize>, &mut Window, &mut App)>;
type OnTile = Box<dyn Fn(&usize, &mut Window, &mut App)>;
type OnKey = Box<dyn Fn(&KeyDownEvent, &mut Window, &mut App)>;

/// A treemap of precomputed [`MapTile`]s filling its parent. The owner lays tiles out for
/// the size `.on_resize(..)` reports (and passes that size back as `.laid_out_for(..)`),
/// tracks hover (`.hovered(..)` / `.on_hover(..)`), selection marks (`.marked(..)`) and
/// the keyboard cursor (`.cursor(..)`, outlined while the map has focus). Clicks report
/// the topmost tile; keys go to `.on_key(..)` while the map is focused.
#[derive(IntoElement)]
pub struct Treemap {
    id: ElementId,
    tiles: Arc<[MapTile]>,
    laid_out_for: Option<(f32, f32)>,
    hovered: Option<usize>,
    cursor: Option<usize>,
    marked: Vec<usize>,
    focus: Option<FocusHandle>,
    on_resize: Option<OnResize>,
    on_hover: Option<OnHover>,
    on_click: Option<OnTile>,
    on_key: Option<OnKey>,
}

impl Treemap {
    /// A map drawing `tiles` (in paint order: nested tiles after their parent).
    pub fn new(id: impl Into<ElementId>, tiles: Arc<[MapTile]>) -> Self {
        Self {
            id: id.into(),
            tiles,
            laid_out_for: None,
            hovered: None,
            cursor: None,
            marked: Vec::new(),
            focus: None,
            on_resize: None,
            on_hover: None,
            on_click: None,
            on_key: None,
        }
    }

    /// The size (px) the tiles were laid out for; a different measured size triggers
    /// `on_resize`.
    #[must_use]
    pub fn laid_out_for(mut self, size: Option<(f32, f32)>) -> Self {
        self.laid_out_for = size;
        self
    }

    /// The tile under the pointer (or linked from elsewhere): accent outline.
    #[must_use]
    pub fn hovered(mut self, tile: Option<usize>) -> Self {
        self.hovered = tile;
        self
    }

    /// The keyboard cursor: accent outline while the map has focus.
    #[must_use]
    pub fn cursor(mut self, tile: Option<usize>) -> Self {
        self.cursor = tile;
        self
    }

    /// Selected tiles: accent overlay.
    #[must_use]
    pub fn marked(mut self, tiles: Vec<usize>) -> Self {
        self.marked = tiles;
        self
    }

    /// Makes the map focusable (click focuses it); keys reach `on_key`.
    #[must_use]
    pub fn focus(mut self, handle: FocusHandle) -> Self {
        self.focus = Some(handle);
        self
    }

    /// Called (after the frame) with the measured size when it differs from
    /// `laid_out_for`.
    #[must_use]
    pub fn on_resize(
        mut self,
        f: impl FnOnce(&(f32, f32), &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_resize = Some(Box::new(f));
        self
    }

    /// Called when the tile under the pointer changes (`None` = off the tiles).
    #[must_use]
    pub fn on_hover(mut self, f: impl Fn(&Option<usize>, &mut Window, &mut App) + 'static) -> Self {
        self.on_hover = Some(Rc::new(f));
        self
    }

    /// Called with the clicked tile.
    #[must_use]
    pub fn on_click(mut self, f: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }

    /// Called for key presses while the map has focus.
    #[must_use]
    pub fn on_key(mut self, f: impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_key = Some(Box::new(f));
        self
    }
}

impl std::fmt::Debug for Treemap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Treemap")
            .field("tiles", &self.tiles.len())
            .finish_non_exhaustive()
    }
}

/// Converts a local tile rectangle to window bounds.
fn to_bounds(origin: gpui_kit::Point<Pixels>, r: Rect) -> Bounds<Pixels> {
    Bounds::new(
        point(px(f32::from(origin.x) + r.x), px(f32::from(origin.y) + r.y)),
        size(px(r.w), px(r.h)),
    )
}

/// A measured size differs from the laid-out one by more than rounding.
fn resized(laid_out_for: Option<(f32, f32)>, w: f32, h: f32) -> bool {
    laid_out_for.is_none_or(|(lw, lh)| (lw - w).abs() >= 0.5 || (lh - h).abs() >= 0.5)
}

/// Absolutely placed label of `tile`.
fn label(tile: &MapTile, label: &TileLabel) -> AnyElement {
    let r = tile.rect;
    let name = div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_size(text::SMALL)
        .line_height(text::SMALL_LINE_HEIGHT)
        .font_weight(FontWeight::MEDIUM)
        .child(label.name.clone());
    let size = div()
        .flex_none()
        .whitespace_nowrap()
        .text_size(text::CAPTION)
        .line_height(text::CAPTION_LINE_HEIGHT)
        .font_features(super::tabular())
        .opacity(treemap::SIZE_OPACITY)
        .child(label.size.clone());
    let body = if label.inline {
        h_flex()
            .gap(px(treemap::LABEL_PAD_X))
            .child(name.flex_1())
            .child(size)
            .into_any_element()
    } else {
        v_flex().child(name).child(size).into_any_element()
    };
    div()
        .absolute()
        .left(px(r.x + treemap::LABEL_PAD_X))
        .top(px(r.y + treemap::LABEL_PAD_Y))
        .w(px((r.w - 2. * treemap::LABEL_PAD_X).max(0.)))
        .overflow_hidden()
        .text_color(label.color)
        .child(body)
        .into_any_element()
}

/// The map's one paint pass: fills (and hatching), selection overlays, then outlines.
fn paint(
    origin: gpui_kit::Point<Pixels>,
    tiles: &[MapTile],
    marked: &[Shape],
    outlines: &[Shape],
    accent: Hsla,
    window: &mut Window,
) {
    for tile in tiles {
        let bounds = to_bounds(origin, tile.rect);
        window.paint_quad(fill(bounds, tile.fill).corner_radii(tile.radius));
        if let Some(stripe) = tile.hatch {
            let hatch = pattern_slash(stripe, treemap::HATCH_WIDTH, treemap::HATCH_INTERVAL);
            window.paint_quad(fill(bounds, hatch).corner_radii(tile.radius));
        }
    }
    for &(rect, radius) in marked {
        window.paint_quad(
            fill(
                to_bounds(origin, rect),
                accent.alpha(treemap::SELECTED_ALPHA),
            )
            .corner_radii(radius),
        );
    }
    for &(rect, radius) in outlines {
        window.paint_quad(quad(
            to_bounds(origin, rect),
            radius,
            transparent_black(),
            treemap::OUTLINE,
            accent,
            BorderStyle::Solid,
        ));
    }
}

/// A tile's rectangle and corner radius.
type Shape = (Rect, Pixels);

impl Treemap {
    /// Outlined tiles (hover, and the cursor while focused) and selected tiles.
    fn overlays(&self, window: &Window) -> (Vec<Shape>, Vec<Shape>) {
        let focused = self.focus.as_ref().is_some_and(|h| h.is_focused(window));
        let shape = |ix: usize| self.tiles.get(ix).map(|t| (t.rect, t.radius));
        let outlines = self
            .hovered
            .into_iter()
            .chain(self.cursor.filter(|_| focused))
            .filter_map(shape)
            .collect();
        let marked = self.marked.iter().filter_map(|&ix| shape(ix)).collect();
        (outlines, marked)
    }
}

impl RenderOnce for Treemap {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let accent = cx.theme().primary;
        let origin = Rc::new(Cell::new(Bounds::<Pixels>::default()));
        let (outlines, marked) = self.overlays(window);

        let paint_tiles = self.tiles.clone();
        let measured = origin.clone();
        let laid_out_for = self.laid_out_for;
        let on_resize = self.on_resize;
        let surface = canvas(
            move |bounds, window, cx| {
                measured.set(bounds);
                let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                if resized(laid_out_for, w, h)
                    && let Some(f) = on_resize
                {
                    window.defer(cx, move |window, cx| f(&(w, h), window, cx));
                }
            },
            move |bounds, (), window, _| {
                paint(
                    bounds.origin,
                    &paint_tiles,
                    &marked,
                    &outlines,
                    accent,
                    window,
                );
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let labels = self
            .tiles
            .iter()
            .filter_map(|t| t.label.as_ref().map(|l| label(t, l)))
            .take(treemap::MAX_LABELS)
            .collect::<Vec<_>>();

        let hover_tiles = self.tiles.clone();
        let hover_origin = origin.clone();
        let hovered = self.hovered;
        let on_move = self.on_hover.clone();
        let on_leave = self.on_hover;
        let click_tiles = self.tiles;
        let on_click = self.on_click;
        let on_key = self.on_key;
        let focus = self.focus;
        div()
            .id(self.id)
            .relative()
            .size_full()
            .overflow_hidden()
            .when_some(focus.as_ref(), InteractiveElement::track_focus)
            .child(surface)
            .children(labels)
            .when_some(on_move, |this, f| {
                this.on_mouse_move(move |event: &MouseMoveEvent, window, cx| {
                    let o = hover_origin.get().origin;
                    let tile = hit(
                        &hover_tiles,
                        f32::from(event.position.x) - f32::from(o.x),
                        f32::from(event.position.y) - f32::from(o.y),
                    );
                    if tile != hovered {
                        f(&tile, window, cx);
                    }
                })
            })
            .when_some(on_leave, |this, f| {
                this.on_hover(move |inside: &bool, window, cx| {
                    if !*inside && hovered.is_some() {
                        f(&None, window, cx);
                    }
                })
            })
            .on_click(move |event: &ClickEvent, window, cx| {
                if let Some(handle) = &focus {
                    window.focus(handle, cx);
                }
                let (Some(f), Some(pos)) = (&on_click, event.mouse_position()) else {
                    return;
                };
                let o = origin.get().origin;
                if let Some(tile) = hit(
                    &click_tiles,
                    f32::from(pos.x) - f32::from(o.x),
                    f32::from(pos.y) - f32::from(o.y),
                ) {
                    f(&tile, window, cx);
                }
            })
            .when_some(on_key, |this, f| {
                this.on_key_down(move |event: &KeyDownEvent, window, cx| f(event, window, cx))
            })
    }
}

/// Fixed placeholder layout (fractions of the map) shown while a scan runs.
static PLACEHOLDER: LazyLock<Vec<Rect>> = LazyLock::new(|| {
    squarify(
        &[34, 21, 13, 9, 8, 5, 4, 3, 2, 1],
        Rect::new(0., 0., 1., 1.),
        0.,
    )
    .into_iter()
    .map(|p| p.rect)
    .collect()
});

/// The map's stand-in while a scan runs: muted blocks breathing at ≤ 30 fps (static under
/// reduced motion).
#[derive(Debug, IntoElement)]
pub struct TreemapPlaceholder {
    id: ElementId,
}

impl TreemapPlaceholder {
    /// A placeholder filling its parent.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self { id: id.into() }
    }
}

impl RenderOnce for TreemapPlaceholder {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted;
        let half_gap = treemap::GAP / 2.;
        let blocks = PLACEHOLDER.iter().map(|r| {
            div()
                .absolute()
                .left(relative(r.x))
                .top(relative(r.y))
                .w(relative(r.w))
                .h(relative(r.h))
                .p(px(half_gap))
                .child(div().size_full().rounded(treemap::RADIUS).bg(muted))
        });
        let map = div().relative().size_full().children(blocks);
        if cx.reduce_motion() {
            map.opacity(treemap::PULSE_MIN_OPACITY).into_any_element()
        } else {
            map.with_animation(
                self.id,
                Animation::new(treemap::PULSE_PERIOD)
                    .repeat()
                    .with_max_fps(treemap::PULSE_FPS),
                |map, delta| {
                    // Triangle wave: dim → full → dim.
                    let wave = 1. - (2. * delta - 1.).abs();
                    map.opacity(
                        treemap::PULSE_MIN_OPACITY + (1. - treemap::PULSE_MIN_OPACITY) * wave,
                    )
                },
            )
            .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Placed, Rect, Slot, as_f32, as_f64, squarify};

    const RECT: Rect = Rect::new(10., 20., 800., 500.);

    fn overlap(a: Rect, b: Rect) -> f32 {
        let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
        let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
        w.max(0.) * h.max(0.)
    }

    fn aspect(r: Rect) -> f32 {
        r.w.max(r.h) / r.w.min(r.h)
    }

    fn sizes() -> Vec<u64> {
        // A realistic, heavy-tailed folder: a few big children, many medium ones.
        (1..=40_u64)
            .map(|i| {
                1_000_000_u64
                    .checked_div(i.saturating_mul(i))
                    .unwrap_or(0)
                    .saturating_add(5_000)
            })
            .collect()
    }

    fn area_of(placed: &[Placed], slot: Slot) -> f32 {
        placed
            .iter()
            .filter(|p| p.slot == slot)
            .map(|p| p.rect.area())
            .sum()
    }

    #[test]
    fn areas_are_proportional_to_sizes() {
        let sizes = sizes();
        let placed = squarify(&sizes, RECT, 0.);
        let total: u64 = sizes.iter().sum();
        assert_eq!(placed.len(), sizes.len(), "every item gets a tile");
        for (ix, &s) in sizes.iter().enumerate() {
            let want = RECT.area() * as_f32(as_f64(s) / as_f64(total));
            let got = area_of(&placed, Slot::Item(ix));
            assert!(
                (got - want).abs() <= want * 0.001 + 0.01,
                "item {ix}: area {got} should be {want}"
            );
        }
    }

    #[test]
    fn tiles_fill_the_rect_without_overlapping() {
        let placed = squarify(&sizes(), RECT, 0.);
        let covered: f32 = placed.iter().map(|p| p.rect.area()).sum();
        assert!(
            (covered - RECT.area()).abs() < 1.,
            "tiles cover the rect: {covered} vs {}",
            RECT.area()
        );
        for (i, a) in placed.iter().enumerate() {
            let r = a.rect;
            assert!(
                r.x >= RECT.x - 0.01
                    && r.y >= RECT.y - 0.01
                    && r.x + r.w <= RECT.x + RECT.w + 0.01
                    && r.y + r.h <= RECT.y + RECT.h + 0.01,
                "tile {i} {r:?} stays inside"
            );
            for b in placed.iter().skip(i.saturating_add(1)) {
                assert!(
                    overlap(r, b.rect) < 0.01,
                    "tiles {r:?} and {:?} must not overlap",
                    b.rect
                );
            }
        }
    }

    #[test]
    fn aspect_ratios_stay_reasonable() {
        let equal = squarify(&[1; 16], Rect::new(0., 0., 400., 400.), 0.);
        assert!(
            equal.iter().all(|p| aspect(p.rect) < 1.5),
            "16 equal items in a square make near-squares: {equal:?}"
        );
        let placed = squarify(&sizes(), RECT, 0.);
        let worst = placed.iter().map(|p| aspect(p.rect)).fold(0_f32, f32::max);
        assert!(
            worst < 5.,
            "heavy-tailed sizes stay squarish, worst {worst}"
        );
        let slice = squarify(&[1, 1], Rect::new(0., 0., 400., 100.), 0.);
        assert!(
            slice.iter().all(|p| (p.rect.w - 200.).abs() < 0.01),
            "a wide rect is split along its long side: {slice:?}"
        );
    }

    #[test]
    fn tiny_items_merge_into_one_other_tile() {
        let mut sizes = vec![1_000_000_u64, 500_000];
        sizes.extend(std::iter::repeat_n(1, 300));
        let rect = Rect::new(0., 0., 300., 200.);
        let placed = squarify(&sizes, rect, 3.);
        let others: Vec<_> = placed.iter().filter(|p| p.slot == Slot::Other).collect();
        assert_eq!(others.len(), 1, "one merged tile: {placed:?}");
        assert_eq!(placed.len(), 3, "two big tiles and the merged one");
        let total: u64 = sizes.iter().sum();
        let want = rect.area() * as_f32(300. / as_f64(total));
        let got = area_of(&placed, Slot::Other);
        assert!(
            (got - want).abs() < 0.01,
            "the merged tile keeps its combined area: {got} vs {want}"
        );
        assert!(
            placed.iter().all(|p| match p.slot {
                Slot::Item(ix) => ix < 2,
                Slot::Other => true,
            }),
            "only the big items keep their own tiles"
        );
    }

    #[test]
    fn order_is_stable_and_empty_inputs_yield_nothing() {
        let placed = squarify(&[5, 9, 5, 0, 5], RECT, 0.);
        let slots: Vec<Slot> = placed.iter().map(|p| p.slot).collect();
        assert_eq!(
            slots,
            vec![Slot::Item(1), Slot::Item(0), Slot::Item(2), Slot::Item(4)],
            "largest first, ties in input order, zero sizes skipped"
        );
        assert_eq!(
            squarify(&[5, 9, 5, 0, 5], RECT, 0.),
            placed,
            "deterministic"
        );
        assert!(squarify(&[], RECT, 0.).is_empty(), "no items, no tiles");
        assert!(
            squarify(&[0, 0], RECT, 0.).is_empty(),
            "zero sizes, no tiles"
        );
        assert!(
            squarify(&[3], Rect::new(0., 0., 0., 10.), 0.).is_empty(),
            "an empty rect has no room"
        );
    }
}
