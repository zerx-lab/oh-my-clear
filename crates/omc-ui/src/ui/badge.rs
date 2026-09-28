//! [`Badge`], [`Dot`] and [`FactIcon`]: a row's facts, loudest to quietest.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, ThemeStyled as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::Tone;
use crate::tokens::{badge, radius, selection, text};

/// An 18 px tinted label without border. A row shows at most one — its most important fact.
#[derive(IntoElement)]
pub struct Badge {
    label: SharedString,
    tone: Tone,
}

impl Badge {
    /// A neutral badge.
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            tone: Tone::Neutral,
        }
    }

    /// Sets the tone.
    #[must_use]
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
}

impl RenderOnce for Badge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (fill, fg) = match self.tone {
            Tone::Neutral => (theme.muted, theme.muted_foreground),
            tone => {
                let color = tone.color(cx);
                (color.alpha(badge::TINT), color)
            }
        };
        div()
            .flex_none()
            .flex()
            .items_center()
            .h(badge::HEIGHT)
            .px(badge::PAD_X)
            .rounded(radius::inner(theme.radius))
            .bg(fill)
            .text_color(fg)
            .text_size(text::CAPTION)
            .line_height(text::CAPTION_LINE_HEIGHT)
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .whitespace_nowrap()
            .child(self.label)
    }
}

/// A 6 px status dot, optionally explained by a tooltip.
#[derive(IntoElement)]
pub struct Dot {
    tone: Tone,
    tooltip: Option<(ElementId, SharedString)>,
}

impl Dot {
    /// A dot in `tone`.
    pub fn new(tone: Tone) -> Self {
        Self {
            tone,
            tooltip: None,
        }
    }

    /// Explains the dot on hover; the hover area grows to the 24 px hit target.
    #[must_use]
    pub fn tooltip(mut self, id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some((id.into(), text.into()));
        self
    }
}

impl RenderOnce for Dot {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let dot = div()
            .flex_none()
            .size(badge::DOT)
            .rounded_full_style(cx)
            .bg(self.tone.color(cx));
        match self.tooltip {
            None => dot.into_any_element(),
            Some((id, text)) => div()
                .id(id)
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .size(selection::HIT_TARGET)
                .child(dot)
                .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
                .into_any_element(),
        }
    }
}

/// A 14 px muted icon for a secondary fact ("needs admin" = lock), explained by a tooltip.
#[derive(IntoElement)]
pub struct FactIcon {
    id: ElementId,
    icon: Icon,
    tooltip: SharedString,
    tone: Option<Tone>,
}

impl FactIcon {
    /// A muted `icon`; `tooltip` says what it means.
    pub fn new(
        id: impl Into<ElementId>,
        icon: impl Into<Icon>,
        tooltip: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            icon: icon.into(),
            tooltip: tooltip.into(),
            tone: None,
        }
    }

    /// Colours the icon in `tone` instead of muted.
    #[must_use]
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }
}

impl RenderOnce for FactIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.tone.unwrap_or(Tone::Neutral).color(cx);
        let tooltip = self.tooltip;
        div()
            .id(self.id)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(selection::HIT_TARGET)
            .text_color(color)
            .child(self.icon.size(badge::FACT_ICON))
            .when(!tooltip.is_empty(), |this| {
                this.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            })
    }
}
