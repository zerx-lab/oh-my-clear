//! [`TextInput`]: gpui-component's `Input` pinned to the app's control scale.

use gpui_kit::base::StyledExt as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Entity, IntoElement, RenderOnce, SharedString, StyleRefinement, Styled, Window, px,
};

use super::ControlSize;
use crate::tokens::control;

/// A single-line text field: 28 px (or 24 px `sm`), 13 px text, 1 px input border,
/// accent focus ring; `.search()` adds the leading search icon (`.icon(..)` another
/// glyph); a ghost clear button appears while it holds text. Styled, so pages set its
/// width (`.w(..)`, `.flex_1()`).
///
/// The page owns the `InputState` (create it in `new`, subscribe to its events there).
#[derive(IntoElement)]
pub struct TextInput {
    state: Entity<InputState>,
    size: ControlSize,
    icon: Option<IconName>,
    cleanable: bool,
    disabled: bool,
    label: Option<SharedString>,
    style: StyleRefinement,
}

impl TextInput {
    /// An `md` field editing `state`.
    pub fn new(state: &Entity<InputState>) -> Self {
        Self {
            state: state.clone(),
            size: ControlSize::Md,
            icon: None,
            cleanable: true,
            disabled: false,
            label: None,
            style: StyleRefinement::default(),
        }
    }

    /// Leading search icon.
    #[must_use]
    pub fn search(self) -> Self {
        self.icon(IconName::Search)
    }

    /// Leading muted icon (a folder for path fields).
    #[must_use]
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Sets the size (`Lg` is treated as `Md`: inputs never exceed 28 px).
    #[must_use]
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = match size {
            ControlSize::Lg => ControlSize::Md,
            other => other,
        };
        self
    }

    /// Shows the clear button while the field holds text (default on).
    #[must_use]
    pub fn cleanable(mut self, cleanable: bool) -> Self {
        self.cleanable = cleanable;
        self
    }

    /// Read-only and dimmed.
    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Accessible name when no visible label sits next to the field (the placeholder is
    /// used otherwise).
    #[must_use]
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl Styled for TextInput {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for TextInput {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let size = self.size;
        let input = Input::new(&self.state)
            .small()
            .cleanable(self.cleanable)
            .disabled(self.disabled)
            .when_some(self.icon, |this, icon| {
                this.prefix(Icon::new(icon).size(control::ICON).text_color(muted))
            })
            .when_some(self.label, Input::aria_label);
        // `Input::h` is its multi-line height; the control height is a style refinement,
        // which gpui-component applies after its own size.
        Styled::h(input, size.height())
            .px(size.pad_x(false))
            .py(px(0.))
            .text_size(size.text())
            .refine_style(&self.style)
    }
}
