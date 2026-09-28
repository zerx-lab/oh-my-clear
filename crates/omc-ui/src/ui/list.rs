//! [`ListRow`]: one- and two-line rows of result lists and tables, and [`row_icon`].

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::{ActiveTheme as _, Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, ElementId, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Refineable as _, RenderOnce, SharedString, StatefulInteractiveElement as _,
    StyleRefinement, Styled, StyledImage as _, Window, div, img, relative,
};

use super::{Checkbox, RowGrid, focus_handle, focus_ring};
use crate::tokens::{control, row, text};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A list row: the leading grid ([`RowGrid`]: checkbox, 16 px icon, extra leading
/// elements), a name with an optional muted detail line (path, explanation;
/// middle-ellipsised; `.detail_mono()` for commands; `.detail_lead(..)` puts a technical id
/// before it), then trailing cells (badge, age, size). 32 px single-line, 40 px with a
/// detail. No separators.
///
/// States follow `tokens::row`: the row box is the list's inset box with a 6 px radius;
/// clickable rows are focusable (Enter/Space activate) and hover/press with a neutral
/// fill and show the focus ring on keyboard focus; a checked row gets NO fill (the
/// checkbox shows it); `.current(true)` marks the one opened row of a master-detail list;
/// `.disabled(true)` dims the row and drops its click. Styled: refinements apply to the
/// row box.
#[derive(IntoElement)]
pub struct ListRow {
    id: ElementId,
    title: SharedString,
    detail: Option<SharedString>,
    detail_lead: Option<SharedString>,
    detail_mono: bool,
    grid: RowGrid,
    checkbox: Option<Checkbox>,
    icon: Option<AnyElement>,
    leading: Vec<AnyElement>,
    trailing: Vec<AnyElement>,
    current: bool,
    disabled: bool,
    on_click: Option<ClickHandler>,
    style: StyleRefinement,
}

impl ListRow {
    /// A single-line row named `title`.
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            detail: None,
            detail_lead: None,
            detail_mono: false,
            grid: RowGrid::new(),
            checkbox: None,
            icon: None,
            leading: Vec::new(),
            trailing: Vec::new(),
            current: false,
            disabled: false,
            on_click: None,
            style: StyleRefinement::default(),
        }
    }

    /// Muted second line (makes the row 40 px).
    #[must_use]
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// A short technical id (bundle id, package or launchd label) shown before the detail
    /// as `id · detail`; the detail is shortened first. Makes the row two-line.
    #[must_use]
    pub fn detail_lead(mut self, lead: impl Into<SharedString>) -> Self {
        self.detail_lead = Some(lead.into());
        self
    }

    /// Sets the detail line in the theme's monospace font (commands, identifiers).
    #[must_use]
    pub fn detail_mono(mut self) -> Self {
        self.detail_mono = true;
        self
    }

    /// The list's leading grid; columns this row has nothing for stay empty so it lines up
    /// with its group header and siblings. The checkbox and icon slots add their columns
    /// on their own.
    #[must_use]
    pub fn grid(mut self, grid: RowGrid) -> Self {
        self.grid = grid;
        self
    }

    /// The selection checkbox, in the grid's checkbox column.
    #[must_use]
    pub fn checkbox(mut self, checkbox: Checkbox) -> Self {
        self.checkbox = Some(checkbox);
        self
    }

    /// A 16 px icon in the grid's icon column (see [`row_icon`]).
    #[must_use]
    pub fn icon(mut self, icon: impl IntoElement) -> Self {
        self.icon = Some(icon.into_any_element());
        self
    }

    /// Appends a free-form element after the grid columns, before the name (e.g. a 24 px
    /// app tile).
    #[must_use]
    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading.push(element.into_any_element());
        self
    }

    /// Appends a cell after the name.
    #[must_use]
    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing.push(element.into_any_element());
        self
    }

    /// Marks the one opened row of a master-detail list (neutral fill). Never use it for
    /// checked rows: the checkbox shows those.
    #[must_use]
    pub fn current(mut self, current: bool) -> Self {
        self.current = current;
        self
    }

    /// Dims the row (45 %) and drops its click and hover.
    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Makes the whole row clickable and focusable (pointer, Enter, Space).
    #[must_use]
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl Styled for ListRow {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for ListRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let fill = |alpha: f32| theme.muted.alpha(theme.muted.a * alpha);
        let (hover, press, current_fill) = (
            fill(row::HOVER_ALPHA),
            fill(row::PRESS_ALPHA),
            fill(row::CURRENT_ALPHA),
        );
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let radius = row::radius(theme.radius);
        let mono = self.detail_mono.then(|| theme.mono_font_family.clone());

        let mut grid = self.grid;
        if self.checkbox.is_some() {
            grid = grid.union(RowGrid::new().check());
        }
        if self.icon.is_some() {
            grid = grid.union(RowGrid::new().icon());
        }
        let lead = (!grid.is_empty() || !self.leading.is_empty()).then(|| {
            grid.cells(
                None,
                self.checkbox.map(IntoElement::into_any_element),
                self.icon,
            )
            .mr(row::SLOT_GAP)
            .children(self.leading)
        });
        let detail_line = detail_line(self.detail_lead, self.detail, mono, muted);
        #[cfg(test)]
        let title_selector = format!("{}/title", self.id);
        let title = div()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_size(text::BODY)
            .line_height(text::BODY_LINE_HEIGHT)
            .text_color(fg)
            .child(self.title.clone());
        #[cfg(test)]
        let title = title.debug_selector(|| title_selector);
        let height = if detail_line.is_some() {
            row::HEIGHT_TWO_LINE
        } else {
            row::HEIGHT
        };
        let body = v_flex()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .child(title)
            .children(detail_line);
        let trailing = (!self.trailing.is_empty()).then(|| {
            h_flex()
                .flex_none()
                .gap(row::GAP)
                .ml(row::GAP)
                .children(self.trailing)
        });
        let current = self.current;
        let disabled = self.disabled;

        if let Some(on_click) = self.on_click.filter(|_| !disabled) {
            let focus = focus_handle(&self.id, window, cx);
            let mut root = BaseButton::new(self.id)
                .track_focus(&focus)
                .accessibility_label(self.title)
                .w_full()
                .flex_none()
                .h(height)
                .justify_start()
                .px(row::PAD_X)
                .rounded(radius)
                .cursor_pointer()
                .when(current, |this| this.bg(current_fill))
                .when(!current, |this| this.hover(move |style| style.bg(hover)))
                .active(move |style| style.bg(press))
                .children(lead)
                .child(body)
                .children(trailing)
                .on_click(move |event, window, cx| on_click(event, window, cx));
            root.style().refine(&self.style);
            focus_ring(root, &focus, window, cx).into_any_element()
        } else {
            let mut root = h_flex()
                .id(self.id)
                .w_full()
                .flex_none()
                .h(height)
                .px(row::PAD_X)
                .rounded(radius)
                .when(current, |this| this.bg(current_fill))
                .when(disabled, |this| this.opacity(control::DISABLED_OPACITY))
                .children(lead)
                .child(body)
                .children(trailing);
            root.style().refine(&self.style);
            root.into_any_element()
        }
    }
}

/// The muted second line: `lead · detail` (the lead is kept, the detail shortened in the
/// middle first); `None` without either.
fn detail_line(
    lead: Option<SharedString>,
    detail: Option<SharedString>,
    mono: Option<SharedString>,
    color: Hsla,
) -> Option<Div> {
    if lead.is_none() && detail.is_none() {
        return None;
    }
    let separated = lead.is_some() && detail.is_some();
    Some(
        h_flex()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(text::CAPTION)
            .line_height(text::CAPTION_LINE_HEIGHT)
            .text_color(color)
            .when_some(mono, Styled::font_family)
            .when_some(lead, |this, lead| {
                this.child(
                    div()
                        .flex_none()
                        .max_w(relative(0.6))
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(lead),
                )
            })
            .when(separated, |this| this.child(div().flex_none().child(" · ")))
            .when_some(detail, |this, detail| {
                this.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis_middle()
                        .child(detail),
                )
            }),
    )
}

/// A row's 16 px leading icon: the PNG at `path` (an app icon the daemon cached), or the
/// muted `fallback` glyph when there is none or it cannot be loaded.
pub fn row_icon(path: Option<&str>, fallback: impl Into<Icon>, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let glyph: Icon = fallback.into();
    let fallback = move || {
        div()
            .flex_none()
            .text_color(muted)
            .child(glyph.clone().size(row::SLOT))
            .into_any_element()
    };
    match path {
        Some(path) => img(PathBuf::from(path))
            .flex_none()
            .size(row::SLOT)
            .with_fallback(fallback)
            .into_any_element(),
        None => fallback(),
    }
}
