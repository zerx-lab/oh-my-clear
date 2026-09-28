//! [`Switch`]: a 28×16 on/off control on gpui-base's headless `Switch`.

use std::rc::Rc;

use gpui_kit::base::Switch as BaseSwitch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, ThemeStyled as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, IntoElement, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div,
};

use super::{focus_handle, focus_ring};
use crate::tokens::{control, selection, space, text};

type ToggleHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

/// An on/off switch for settings that apply immediately; optional trailing label.
#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    checked: bool,
    disabled: bool,
    label: Option<SharedString>,
    tooltip: Option<SharedString>,
    on_click: Option<ToggleHandler>,
}

impl Switch {
    /// An off switch.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            checked: false,
            disabled: false,
            label: None,
            tooltip: None,
            on_click: None,
        }
    }

    /// Sets the value.
    #[must_use]
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Ignores activation and dims the switch.
    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Text after the track (clicking it toggles too).
    #[must_use]
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Hover tooltip.
    #[must_use]
    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// Called with the new value; the click does not reach the parent.
    #[must_use]
    pub fn on_click(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = focus_handle(&self.id, window, cx);
        let theme = cx.theme();
        let track_color = if self.checked {
            theme.primary
        } else {
            theme.switch
        };
        let thumb_color = theme.switch_thumb;
        let foreground = theme.foreground;
        let track = div()
            .flex_none()
            .flex()
            .items_center()
            .w(selection::SWITCH_WIDTH)
            .h(selection::SWITCH_HEIGHT)
            .px(space::XXS)
            .rounded_full_style(cx)
            .bg(track_color)
            .when(self.checked, Styled::justify_end)
            .child(
                div()
                    .size(selection::SWITCH_THUMB)
                    .rounded_full_style(cx)
                    .bg(thumb_color)
                    .when(cx.theme().shadow, Styled::shadow_xs),
            );
        let track = focus_ring(track, &focus, window, cx);
        let on_click = self.on_click;
        let tooltip = self.tooltip;

        BaseSwitch::new(self.id)
            .checked(self.checked)
            .disabled(self.disabled)
            .track_focus(&focus)
            .when_some(self.label.clone(), |this, label| {
                this.accessibility_label(label)
            })
            .flex_none()
            .flex()
            .items_center()
            .gap(space::MD)
            .min_h(selection::HIT_TARGET)
            .map(|this| {
                if self.disabled {
                    this.opacity(control::DISABLED_OPACITY).cursor_default()
                } else {
                    this.cursor_pointer()
                }
            })
            .on_change(move |next, _, window, cx| {
                cx.stop_propagation();
                if let Some(handler) = &on_click {
                    handler(&next, window, cx);
                }
            })
            .child(track)
            .when_some(self.label, |this, label| {
                this.child(
                    div()
                        .text_size(text::BODY)
                        .line_height(text::BODY_LINE_HEIGHT)
                        .text_color(foreground)
                        .child(label),
                )
            })
            .when_some(tooltip, |this, tooltip| {
                this.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            })
    }
}
