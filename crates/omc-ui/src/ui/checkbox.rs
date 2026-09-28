//! [`Checkbox`]: a 16 px tri-state box on gpui-base's headless `Checkbox`.

use std::rc::Rc;

use gpui_kit::base::{Checkbox as BaseCheckbox, CheckboxState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, IntoElement, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};

#[cfg(test)]
use gpui_kit::InteractiveElement as _;

use super::{focus_handle, focus_ring};
use crate::tokens::{control, radius, selection, space, text};

type ToggleHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

/// Selection state of a checkbox; `Indeterminate` = some but not all children selected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CheckState {
    /// Nothing selected.
    #[default]
    Unchecked,
    /// Everything selected.
    Checked,
    /// Some selected.
    Indeterminate,
}

impl CheckState {
    /// State of a group with `selected` of `total` children chosen (empty = unchecked).
    pub const fn from_counts(selected: usize, total: usize) -> Self {
        if selected == 0 || total == 0 {
            Self::Unchecked
        } else if selected >= total {
            Self::Checked
        } else {
            Self::Indeterminate
        }
    }

    /// Checked value an activation produces: indeterminate and unchecked select all,
    /// checked clears.
    pub const fn next(self) -> bool {
        matches!(self, Self::Unchecked | Self::Indeterminate)
    }

    const fn base(self) -> CheckboxState {
        match self {
            Self::Unchecked => CheckboxState::Unchecked,
            Self::Checked => CheckboxState::Checked,
            Self::Indeterminate => CheckboxState::Indeterminate,
        }
    }
}

impl From<bool> for CheckState {
    fn from(checked: bool) -> Self {
        if checked {
            Self::Checked
        } else {
            Self::Unchecked
        }
    }
}

/// A selection checkbox with an optional label. Activation (click, Space, Enter) calls
/// `on_click` with the new checked value and never reaches the parent (a row or group
/// header containing the box does not also activate).
#[derive(IntoElement)]
pub struct Checkbox {
    id: ElementId,
    state: CheckState,
    label: Option<SharedString>,
    disabled: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ToggleHandler>,
}

impl Checkbox {
    /// An unchecked box.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            state: CheckState::Unchecked,
            label: None,
            disabled: false,
            tooltip: None,
            on_click: None,
        }
    }

    /// Sets the tri-state value.
    #[must_use]
    pub fn state(mut self, state: CheckState) -> Self {
        self.state = state;
        self
    }

    /// Sets a two-state value.
    #[must_use]
    pub fn checked(self, checked: bool) -> Self {
        self.state(checked.into())
    }

    /// Text after the box (clicking it toggles too).
    #[must_use]
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Ignores activation and dims the box.
    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Hover tooltip (e.g. why it is disabled).
    #[must_use]
    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// Called with the new checked value.
    #[must_use]
    pub fn on_click(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Checkbox {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = focus_handle(&self.id, window, cx);
        let theme = cx.theme();
        let on = self.state != CheckState::Unchecked;
        let (fill, border) = if on {
            (theme.primary, theme.primary)
        } else {
            (theme.background, theme.input)
        };
        let mark = theme.primary_foreground;
        let box_radius = radius::inner(theme.radius);
        let square = div()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(selection::CHECKBOX)
            .rounded(box_radius)
            .border(selection::CHECKBOX_BORDER)
            .border_color(border)
            .bg(fill)
            .text_color(mark)
            .map(|this| match self.state {
                CheckState::Checked => {
                    this.child(Icon::new(IconName::Check).size(selection::CHECK_ICON))
                }
                CheckState::Indeterminate => this.child(
                    div()
                        .w(selection::INDETERMINATE_BAR)
                        .h(selection::INDETERMINATE_BAR_HEIGHT)
                        .rounded(selection::INDETERMINATE_BAR_HEIGHT)
                        .bg(mark),
                ),
                CheckState::Unchecked => this,
            });
        let square = focus_ring(square, &focus, window, cx);
        let foreground = theme.foreground;
        let on_click = self.on_click;
        let tooltip = self.tooltip;

        #[cfg(test)]
        let selector = self.id.to_string();
        let checkbox = BaseCheckbox::new(self.id)
            .state(self.state.base())
            .disabled(self.disabled)
            .track_focus(&focus)
            .when_some(self.label.clone(), |this, label| {
                this.accessibility_label(label)
            })
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .gap(space::MD)
            .min_w(selection::HIT_TARGET)
            .min_h(selection::HIT_TARGET)
            .map(|this| {
                if self.disabled {
                    this.opacity(control::DISABLED_OPACITY)
                        .cursor_default()
                        // A disabled box inside a clickable row still must not activate it.
                        .on_click(|_, _, cx| cx.stop_propagation())
                } else {
                    this.cursor_pointer()
                }
            })
            .on_change(move |next, _, window, cx| {
                cx.stop_propagation();
                if let Some(handler) = &on_click {
                    handler(&(next == CheckboxState::Checked), window, cx);
                }
            })
            .child(square)
            .when_some(self.label, |this, label| {
                this.justify_start().child(
                    div()
                        .text_size(text::BODY)
                        .line_height(text::BODY_LINE_HEIGHT)
                        .text_color(foreground)
                        .child(label),
                )
            })
            .when_some(tooltip, |this, tooltip| {
                this.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            });
        #[cfg(test)]
        let checkbox = checkbox.debug_selector(|| selector);
        checkbox.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::CheckState;

    #[test]
    fn group_state_follows_the_selected_share() {
        assert_eq!(CheckState::from_counts(0, 3), CheckState::Unchecked, "none");
        assert_eq!(
            CheckState::from_counts(1, 3),
            CheckState::Indeterminate,
            "some"
        );
        assert_eq!(CheckState::from_counts(3, 3), CheckState::Checked, "all");
        assert_eq!(
            CheckState::from_counts(0, 0),
            CheckState::Unchecked,
            "an empty group is unchecked, not checked"
        );
        assert_eq!(
            CheckState::from_counts(5, 3),
            CheckState::Checked,
            "a stale over-count still reads as all"
        );
    }

    #[test]
    fn activation_selects_all_unless_everything_is_selected() {
        assert!(CheckState::Unchecked.next(), "unchecked → checked");
        assert!(
            CheckState::Indeterminate.next(),
            "partial → all, like every file manager"
        );
        assert!(!CheckState::Checked.next(), "checked → unchecked");
    }
}
