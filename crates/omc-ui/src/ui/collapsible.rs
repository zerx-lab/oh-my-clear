//! [`CollapsibleHeader`]: the group header of a result list.

use std::f32::consts::FRAC_PI_2;
use std::rc::Rc;

use gpui_kit::base::{Button as BaseButton, spring};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Div, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, radians, relative,
};

use super::{Checkbox, RowGrid, child_id, focus_handle, focus_ring, tabular};
use crate::motion;
use crate::tokens::{row, space, text};

type ToggleHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

/// A 32 px group header: chevron, optional tri-state checkbox and icon on the list's
/// [`RowGrid`], title, muted summary ("3 of 12 selected") and a right-aligned tabular size.
///
/// The WHOLE row toggles collapse (click, Enter or Space while focused) and calls
/// `on_toggle` once with the new open state; the chevron is part of the row, not a second
/// button. The checkbox slot selects without toggling (its click stops at the box). The
/// chevron turns on the [`motion::UI`] spring; under reduced motion it snaps. States match
/// [`super::ListRow`]: hover/press fill on the inset row box, focus ring, never a
/// persistent fill.
///
/// ```ignore
/// const GRID: ui::RowGrid = ui::RowGrid::new().disclosure().check();
/// ui::CollapsibleHeader::new(("junk-group", gi), group.title.clone(), !collapsed)
///     .grid(GRID)
///     .checkbox(ui::Checkbox::new(("junk-group-check", gi)).state(state).on_click(..))
///     .summary(summary)
///     .size_label(format::bytes(group.bytes))
///     .on_toggle(cx.listener(move |this, open: &bool, _, cx| this.set_open(gi, *open, cx)))
/// ```
#[derive(IntoElement)]
pub struct CollapsibleHeader {
    id: ElementId,
    title: SharedString,
    open: bool,
    grid: RowGrid,
    checkbox: Option<Checkbox>,
    icon: Option<AnyElement>,
    summary: Option<SharedString>,
    size_label: Option<SharedString>,
    trailing: Vec<AnyElement>,
    on_toggle: Option<ToggleHandler>,
}

impl CollapsibleHeader {
    /// A header for a group that is `open` (expanded) or collapsed.
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>, open: bool) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            open,
            grid: RowGrid::new(),
            checkbox: None,
            icon: None,
            summary: None,
            size_label: None,
            trailing: Vec::new(),
            on_toggle: None,
        }
    }

    /// The list's leading grid (the header always has the disclosure column); give the
    /// group's items the same grid so checkboxes and titles line up.
    #[must_use]
    pub fn grid(mut self, grid: RowGrid) -> Self {
        self.grid = grid;
        self
    }

    /// Tri-state checkbox that (de)selects the whole group.
    #[must_use]
    pub fn checkbox(mut self, checkbox: Checkbox) -> Self {
        self.checkbox = Some(checkbox);
        self
    }

    /// A 16 px group icon in the grid's icon column.
    #[must_use]
    pub fn icon(mut self, element: impl IntoElement) -> Self {
        self.icon = Some(element.into_any_element());
        self
    }

    /// Muted 12 px text after the title (count, "n of m selected").
    #[must_use]
    pub fn summary(mut self, summary: impl Into<SharedString>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    /// Right-aligned size in the 88 px tabular size column.
    #[must_use]
    pub fn size_label(mut self, size: impl Into<SharedString>) -> Self {
        self.size_label = Some(size.into());
        self
    }

    /// Appends an element before the size column (e.g. a badge or a row action).
    #[must_use]
    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing.push(element.into_any_element());
        self
    }

    /// Called once per activation with the new open state.
    #[must_use]
    pub fn on_toggle(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for CollapsibleHeader {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let turn = spring(
            child_id(&self.id, "chevron"),
            if self.open { 1. } else { 0. },
            motion::UI,
            window,
            cx,
        );
        let focus = focus_handle(&self.id, window, cx);
        let theme = cx.theme();
        let hover = theme.muted.alpha(theme.muted.a * row::HOVER_ALPHA);
        let press = theme.muted.alpha(theme.muted.a * row::PRESS_ALPHA);
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let radius = row::radius(theme.radius);
        let open = self.open;
        let on_toggle = self.on_toggle;

        let mut grid = self.grid.union(RowGrid::new().disclosure());
        if self.checkbox.is_some() {
            grid = grid.union(RowGrid::new().check());
        }
        if self.icon.is_some() {
            grid = grid.union(RowGrid::new().icon());
        }
        let chevron = div().flex_none().text_color(muted).child(
            Icon::new(IconName::ChevronRight)
                .size(row::CHEVRON)
                .rotate(radians(turn * FRAC_PI_2)),
        );
        let lead = grid
            .cells(
                Some(chevron.into_any_element()),
                self.checkbox.map(IntoElement::into_any_element),
                self.icon,
            )
            .mr(row::SLOT_GAP);

        #[cfg(test)]
        let selector = self.id.to_string();
        #[cfg(test)]
        let title_selector = format!("{selector}/title");
        let title = div()
            .flex_none()
            .max_w(relative(0.6))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .font_weight(FontWeight::SEMIBOLD)
            .child(self.title.clone());
        #[cfg(test)]
        let title = title.debug_selector(|| title_selector);
        let header = BaseButton::new(self.id)
            .track_focus(&focus)
            .accessibility_label(self.title)
            .w_full()
            .h(row::GROUP_HEIGHT)
            .justify_start()
            .px(row::PAD_X)
            .rounded(radius)
            .line_height(text::BODY_LINE_HEIGHT)
            .text_size(text::BODY)
            .text_color(fg)
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .active(move |style| style.bg(press))
            .child(lead)
            .child(title)
            .when_some(self.summary, |this, summary| {
                this.child(
                    div()
                        .flex_none()
                        .ml(space::MD)
                        .whitespace_nowrap()
                        .text_size(text::SMALL)
                        .text_color(muted)
                        .font_features(tabular())
                        .child(summary),
                )
            })
            .child(div().flex_1())
            .child(trailing_cells(self.trailing, self.size_label))
            .when_some(on_toggle, |this, on_toggle| {
                this.on_click(move |_, window, cx| on_toggle(&!open, window, cx))
            });
        #[cfg(test)]
        let header = header.debug_selector(|| selector);
        focus_ring(header, &focus, window, cx)
    }
}

/// The header's trailing cells and its size column, spaced like a row's so the size
/// column ends where the items' does.
fn trailing_cells(trailing: Vec<AnyElement>, size_label: Option<SharedString>) -> Div {
    h_flex()
        .flex_none()
        .gap(row::GAP)
        .ml(row::GAP)
        .children(trailing)
        .when_some(size_label, |this, size| {
            this.child(
                div()
                    .flex_none()
                    .w(row::SIZE_COLUMN)
                    .text_right()
                    .whitespace_nowrap()
                    .font_weight(FontWeight::MEDIUM)
                    .font_features(tabular())
                    .child(size),
            )
        })
}

/// "3 of 12 selected" (tabular counts) for a group header's summary.
pub fn selected_of(selected: usize, total: usize) -> SharedString {
    rust_i18n::t!(
        "ui.selected_of",
        selected = crate::format::count(u64::try_from(selected).unwrap_or(u64::MAX)),
        total = crate::format::count(u64::try_from(total).unwrap_or(u64::MAX))
    )
    .to_string()
    .into()
}

/// "12 items" for a group header's summary.
pub fn item_count(count: usize) -> SharedString {
    rust_i18n::t!(
        "ui.items",
        count = crate::format::count(u64::try_from(count).unwrap_or(u64::MAX))
    )
    .to_string()
    .into()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui_kit::component::v_flex;
    use gpui_kit::{
        Context, IntoElement, Modifiers, ParentElement as _, Render, Styled as _, TestAppContext,
        Window, div, px,
    };

    use super::CollapsibleHeader;
    use crate::tokens::row;
    use crate::ui::{CheckState, Checkbox, ListRow, RowGrid};

    #[derive(Default)]
    struct Log {
        toggles: Vec<bool>,
        checks: Vec<bool>,
    }

    struct Harness {
        open: bool,
        log: Rc<RefCell<Log>>,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
            let checks = self.log.clone();
            let toggles = self.log.clone();
            div().w(px(600.)).child(
                CollapsibleHeader::new("group", "Caches", self.open)
                    .checkbox(
                        Checkbox::new("group-check")
                            .state(CheckState::Indeterminate)
                            .on_click(move |on, _, _| checks.borrow_mut().checks.push(*on)),
                    )
                    .summary("1 of 3")
                    .size_label("12 MB")
                    .on_toggle(cx.listener(move |this, open: &bool, _, cx| {
                        toggles.borrow_mut().toggles.push(*open);
                        this.open = *open;
                        cx.notify();
                    })),
            )
        }
    }

    #[gpui_kit::test]
    fn row_toggles_once_and_checkbox_never_toggles(cx: &mut TestAppContext) {
        let init = cx.update(crate::init);
        assert!(init.is_ok(), "UI initialises headless: {init:?}");
        let log = Rc::new(RefCell::new(Log::default()));
        let shared = log.clone();
        let (view, cx) = cx.add_window_view(move |_, _| Harness {
            open: false,
            log: shared,
        });
        cx.run_until_parked();
        let (header, check) = (cx.debug_bounds("group"), cx.debug_bounds("group-check"));
        assert!(
            header.is_some() && check.is_some(),
            "header and checkbox are laid out"
        );
        let (Some(header), Some(check)) = (header, check) else {
            return;
        };

        // A click on the row, right of the title: exactly one toggle, no selection.
        let row_point = gpui_kit::point(px(f32::from(header.right()) - 120.), header.center().y);
        cx.simulate_click(row_point, Modifiers::default());
        assert_eq!(log.borrow().toggles, vec![true], "one click, one toggle");
        assert!(log.borrow().checks.is_empty(), "the row does not select");
        assert!(cx.update(|_, cx| view.read(cx).open), "the group opened");

        // A click on the checkbox: selection only, the group stays open.
        cx.simulate_click(check.center(), Modifiers::default());
        assert_eq!(log.borrow().checks, vec![true], "indeterminate selects all");
        assert_eq!(
            log.borrow().toggles,
            vec![true],
            "the checkbox click does not toggle collapse"
        );

        // Clicking the row again collapses it: toggles are not double-counted.
        cx.simulate_click(row_point, Modifiers::default());
        assert_eq!(
            log.borrow().toggles,
            vec![true, false],
            "second toggle closes"
        );
    }

    /// A header and two items (one with an icon, one without) on one grid.
    struct Grouped;

    impl Render for Grouped {
        fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
            const GRID: RowGrid = RowGrid::new().disclosure().check().icon();
            v_flex()
                .w(px(600.))
                .child(
                    CollapsibleHeader::new("g", "Caches", true)
                        .grid(GRID)
                        .checkbox(Checkbox::new("g-check"))
                        .summary("1 of 2")
                        .size_label("12 MB"),
                )
                .child(
                    ListRow::new("a", "Ghostty")
                        .grid(GRID)
                        .detail("~/Library/Caches/com.mitchellh.ghostty")
                        .detail_lead("com.mitchellh.ghostty")
                        .checkbox(Checkbox::new("a-check"))
                        .icon(div().size(px(16.)))
                        .on_click(|_, _, _| {}),
                )
                .child(
                    ListRow::new("b", "node_modules")
                        .grid(GRID)
                        .checkbox(Checkbox::new("b-check")),
                )
        }
    }

    #[gpui_kit::test]
    fn header_and_items_share_checkbox_and_title_columns(cx: &mut TestAppContext) {
        let init = cx.update(crate::init);
        assert!(init.is_ok(), "UI initialises headless: {init:?}");
        let (_view, cx) = cx.add_window_view(|_, _| Grouped);
        cx.run_until_parked();
        // Checkboxes by their centre (the 16 px box sits in a 24 px hit target), titles by
        // their left edge.
        let mut bounds = |selector: &'static str| cx.debug_bounds(selector);
        let checks = ["g-check", "a-check", "b-check"].map(|s| bounds(s).map(|b| b.center().x));
        let titles = ["g/title", "a/title", "b/title"].map(|s| bounds(s).map(|b| b.origin.x));
        assert!(
            checks.iter().chain(&titles).all(Option::is_some),
            "every checkbox and title is laid out: {checks:?} {titles:?}"
        );
        assert!(
            checks.windows(2).all(|w| w[0] == w[1]),
            "header and item checkboxes share one x: {checks:?}"
        );
        assert!(
            titles.windows(2).all(|w| w[0] == w[1]),
            "item titles start where the header title starts: {titles:?}"
        );
        let (Some(check), Some(title)) = (checks[0], titles[0]) else {
            return;
        };
        let grid = RowGrid::new().disclosure().check().icon();
        let laid_out = f32::from(title) - f32::from(check);
        let expected = f32::from(grid.title_offset())
            - f32::from(grid.check_offset().unwrap_or_default())
            - f32::from(row::SLOT) / 2.;
        assert!(
            (laid_out - expected).abs() < 0.5,
            "the laid-out gap {laid_out} matches the grid's offsets {expected}"
        );
    }
}
