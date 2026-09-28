//! [`Stat`]: a label over a tabular number.

use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div,
};

use super::{Tone, tabular};
use crate::tokens::{space, text};

/// An 11 px muted label over a 20/600 tabular value (summary strips in card headers);
/// `.hero()` makes the value 28 px (overview totals).
#[derive(IntoElement)]
pub struct Stat {
    label: SharedString,
    value: SharedString,
    hero: bool,
    tone: Option<Tone>,
}

impl Stat {
    /// `value` captioned by `label`.
    pub fn new(label: impl Into<SharedString>, value: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            hero: false,
            tone: None,
        }
    }

    /// 28 px value.
    #[must_use]
    pub fn hero(mut self) -> Self {
        self.hero = true;
        self
    }

    /// Colours the value (e.g. accent for "selected").
    #[must_use]
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }
}

impl RenderOnce for Stat {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let value_color = self.tone.map_or(theme.foreground, |tone| tone.color(cx));
        let (size, line) = if self.hero {
            (text::HERO, text::HERO_LINE_HEIGHT)
        } else {
            (text::PAGE_TITLE, text::PAGE_TITLE_LINE_HEIGHT)
        };
        v_flex()
            .flex_none()
            .gap(space::XXS)
            .child(
                div()
                    .text_size(text::CAPTION)
                    .line_height(text::CAPTION_LINE_HEIGHT)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.muted_foreground)
                    .child(self.label),
            )
            .child(
                div()
                    .text_size(size)
                    .line_height(line)
                    .font_weight(FontWeight::SEMIBOLD)
                    .font_features(tabular())
                    .text_color(value_color)
                    .whitespace_nowrap()
                    .child(self.value),
            )
    }
}
