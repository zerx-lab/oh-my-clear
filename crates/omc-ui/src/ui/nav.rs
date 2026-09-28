//! [`NavItem`]: one entry of a sidebar navigation (main window and settings window).

use std::rc::Rc;

use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div,
};

use super::{focus_handle, focus_ring};
use crate::tokens::{chrome, layout, space, text};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

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
