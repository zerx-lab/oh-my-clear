//! [`ProgressBar`]: 4 px determinate bar or indeterminate shimmer.

use gpui_kit::component::{ActiveTheme as _, ThemeStyled as _};
use gpui_kit::{
    Animation, AnimationExt as _, App, ElementId, IntoElement, ParentElement as _, RenderOnce,
    Styled as _, Window, div, relative,
};

use super::Tone;
use crate::tokens::progress;

/// A thin progress track. `value(Some(0..=1))` fills it; `value(None)` runs a shimmer
/// (≤ 30 fps; a static partial bar under reduced motion).
#[derive(IntoElement)]
pub struct ProgressBar {
    id: ElementId,
    value: Option<f32>,
    tone: Tone,
}

impl ProgressBar {
    /// An indeterminate accent bar.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            value: None,
            tone: Tone::Accent,
        }
    }

    /// Fraction done (`None` = indeterminate); clamped to 0..=1.
    #[must_use]
    pub fn value(mut self, value: Option<f32>) -> Self {
        self.value = value.map(|v| v.clamp(0., 1.));
        self
    }

    /// Fill colour.
    #[must_use]
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
}

impl RenderOnce for ProgressBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let fill = self.tone.color(cx);
        let track = div()
            .relative()
            .w_full()
            .h(progress::BAR_HEIGHT)
            .rounded_full_style(cx)
            .bg(cx.theme().muted)
            .overflow_hidden();
        let bar = div()
            .absolute()
            .top_0()
            .bottom_0()
            .rounded_full_style(cx)
            .bg(fill);
        match self.value {
            Some(value) => track
                .child(bar.left_0().w(relative(value)))
                .into_any_element(),
            None if cx.reduce_motion() => track
                .child(
                    bar.left_0()
                        .w(relative(progress::SHIMMER_FRACTION))
                        .opacity(progress::STATIC_OPACITY),
                )
                .into_any_element(),
            None => track
                .child(
                    bar.w(relative(progress::SHIMMER_FRACTION)).with_animation(
                        self.id,
                        Animation::new(progress::SHIMMER_PERIOD)
                            .repeat()
                            .with_max_fps(progress::SHIMMER_FPS),
                        |bar, delta| {
                            // Travels from fully left of the track to fully right of it.
                            let start = -progress::SHIMMER_FRACTION;
                            bar.left(relative(start + delta * (1. - start)))
                        },
                    ),
                )
                .into_any_element(),
        }
    }
}
