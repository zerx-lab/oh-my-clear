//! [`EmptyState`]: the idle/empty content of a card.

use gpui_kit::component::{ActiveTheme as _, Icon, ThemeStyled as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div,
};

use crate::tokens::{empty, space, text};

/// Centred 40 px icon circle, 14/600 title, one 13 px muted line, and at most one action
/// (an `lg` primary [`super::Button`], e.g. "Scan").
#[derive(IntoElement)]
pub struct EmptyState {
    icon: Icon,
    title: SharedString,
    description: Option<SharedString>,
    action: Option<AnyElement>,
}

impl EmptyState {
    /// An empty state headed `title`.
    pub fn new(icon: impl Into<Icon>, title: impl Into<SharedString>) -> Self {
        Self {
            icon: icon.into(),
            title: title.into(),
            description: None,
            action: None,
        }
    }

    /// One explanatory line.
    #[must_use]
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// The one action.
    #[must_use]
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .w_full()
            .items_center()
            .gap(space::MD)
            .py(space::XXXL)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(empty::ICON_CIRCLE)
                    .rounded_full_style(cx)
                    .bg(theme.muted)
                    .text_color(theme.muted_foreground)
                    .child(self.icon.size(empty::ICON)),
            )
            .child(
                div()
                    .text_size(text::SECTION)
                    .line_height(text::SECTION_LINE_HEIGHT)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(self.title),
            )
            .when_some(self.description, |this, description| {
                this.child(
                    div()
                        .max_w(empty::TEXT_WIDTH)
                        .text_center()
                        .text_size(text::BODY)
                        .line_height(text::BODY_LINE_HEIGHT)
                        .text_color(theme.muted_foreground)
                        .child(description),
                )
            })
            .when_some(self.action, |this, action| {
                this.child(div().pt(space::XS).child(action))
            })
    }
}
