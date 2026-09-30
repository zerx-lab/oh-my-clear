//! [`NavItem`]: one entry of a sidebar navigation (main window and settings window), and
//! [`NavGroupHeader`]: the foldable heading of a run of entries.

use std::f32::consts::FRAC_PI_2;
use std::rc::Rc;

use gpui_kit::base::{Button as BaseButton, spring};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div, radians,
};

use super::{child_id, focus_handle, focus_ring};
use crate::motion;
use crate::tokens::{chrome, layout, row, space, text};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type ToggleHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

/// A 28 px sidebar entry: 16 px icon, 13 px label (ellipsised), optional trailing element
/// (key hint). The selected entry gets the neutral sidebar selection fill and 500 weight
/// (never the accent); others fill faintly on hover. Focusable, with the focus ring.
#[derive(IntoElement)]
pub struct NavItem {
    id: ElementId,
    icon: Icon,
    label: SharedString,
    selected: bool,
    suffix: Option<AnyElement>,
    on_click: Option<ClickHandler>,
}

impl NavItem {
    /// An unselected entry.
    pub fn new(
        id: impl Into<ElementId>,
        icon: impl Into<Icon>,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            icon: icon.into(),
            label: label.into(),
            selected: false,
            suffix: None,
            on_click: None,
        }
    }

    /// Selected (the shown page) look.
    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Element after the label (a key-binding hint).
    #[must_use]
    pub fn suffix(mut self, suffix: impl IntoElement) -> Self {
        self.suffix = Some(suffix.into_any_element());
        self
    }

    /// Called on click, Enter or Space.
    #[must_use]
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for NavItem {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = focus_handle(&self.id, window, cx);
        let theme = cx.theme();
        let selected = self.selected;
        let hover = theme
            .sidebar_accent
            .alpha(theme.sidebar_accent.a * layout::SIDEBAR_HOVER_ALPHA);
        let (icon_color, text_color) = if selected {
            (
                theme.sidebar_accent_foreground,
                theme.sidebar_accent_foreground,
            )
        } else {
            (theme.muted_foreground, theme.sidebar_foreground)
        };
        let item = BaseButton::new(self.id)
            .track_focus(&focus)
            .selected(selected)
            .accessibility_label(self.label.clone())
            .w_full()
            .h(layout::SIDEBAR_ITEM_HEIGHT)
            .justify_start()
            .px(space::MD)
            .gap(space::MD)
            .rounded(theme.radius)
            .text_size(text::BODY)
            .line_height(text::BODY_LINE_HEIGHT)
            .text_color(text_color)
            .cursor_pointer()
            .map(|this| {
                if selected {
                    this.bg(theme.sidebar_accent)
                        .font_weight(FontWeight::MEDIUM)
                } else {
                    this.hover(move |style| style.bg(hover))
                }
            })
            .child(
                div()
                    .flex_none()
                    .text_color(icon_color)
                    .child(self.icon.size(chrome::ICON)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(self.label),
            )
            .children(self.suffix)
            .when_some(self.on_click, |this, on_click| {
                this.on_click(move |event, window, cx| on_click(event, window, cx))
            });
        focus_ring(item, &focus, window, cx)
    }
}

/// The heading of a foldable run of sidebar entries: 11/500 muted label and a chevron that
/// points down while the group is open and right while it is folded (it turns on the
/// [`motion::UI`] spring, instantly under reduced motion). The whole row toggles (click,
/// Enter or Space) and calls `on_toggle` once with the new open state.
#[derive(IntoElement)]
pub struct NavGroupHeader {
    id: ElementId,
    label: SharedString,
    open: bool,
    on_toggle: Option<ToggleHandler>,
}

impl NavGroupHeader {
    /// A header of a group that is `open` (entries shown) or folded.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, open: bool) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            open,
            on_toggle: None,
        }
    }

    /// Called with the new open state.
    #[must_use]
    pub fn on_toggle(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for NavGroupHeader {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let open_amount = spring(
            child_id(&self.id, "chevron"),
            if self.open { 1. } else { 0. },
            motion::UI,
            window,
            cx,
        );
        let focus = focus_handle(&self.id, window, cx);
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let hover = theme
            .sidebar_accent
            .alpha(theme.sidebar_accent.a * layout::SIDEBAR_HOVER_ALPHA);
        let open = self.open;
        let on_toggle = self.on_toggle;
        let header = BaseButton::new(self.id)
            .track_focus(&focus)
            .accessibility_label(self.label.clone())
            .w_full()
            .h(layout::SIDEBAR_GROUP_HEIGHT)
            .justify_start()
            .mt(space::MD)
            .px(space::MD)
            .gap(space::XS)
            .rounded(theme.radius)
            .text_size(text::CAPTION)
            .line_height(text::CAPTION_LINE_HEIGHT)
            .font_weight(FontWeight::MEDIUM)
            .text_color(muted)
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .child(div().flex_none().child(self.label))
            .child(
                div().flex_none().child(
                    Icon::new(IconName::ChevronDown)
                        .size(row::CHEVRON)
                        .rotate(radians((open_amount - 1.) * FRAC_PI_2)),
                ),
            )
            .when_some(on_toggle, |this, on_toggle| {
                this.on_click(move |_, window, cx| on_toggle(&!open, window, cx))
            });
        focus_ring(header, &focus, window, cx)
    }
}
