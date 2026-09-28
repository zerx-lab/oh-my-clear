//! [`Button`] and [`IconButton`] on gpui-base's headless `Button`.

use std::rc::Rc;

use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, ElementId, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled, Window, div,
};

use super::{ControlSize, focus_handle, focus_ring};
use crate::tokens::control;

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Visual hierarchy of a button (one `Primary` per view).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonVariant {
    /// Accent fill: the view's main action.
    Primary,
    /// Neutral subtle fill, no border: default toolbar action.
    #[default]
    Secondary,
    /// Hairline border, transparent fill: a secondary action beside another.
    Outline,
    /// Transparent, muted fill on hover: tertiary and icon buttons.
    Ghost,
    /// Danger fill: the destructive confirm inside a confirmation only.
    Danger,
}

/// Resolved colours of one variant.
struct Palette {
    bg: Hsla,
    hover: Hsla,
    active: Hsla,
    fg: Hsla,
    border: Option<Hsla>,
}

impl ButtonVariant {
    fn palette(self, icon_only: bool, cx: &App) -> Palette {
        let t = cx.theme();
        match self {
            Self::Primary => Palette {
                bg: t.primary,
                hover: t.primary_hover,
                active: t.primary_active,
                fg: t.primary_foreground,
                border: None,
            },
            Self::Secondary => Palette {
                bg: t.secondary,
                hover: t.secondary_hover,
                active: t.secondary_active,
                fg: t.secondary_foreground,
                border: None,
            },
            Self::Outline => Palette {
                bg: t.transparent,
                hover: t.secondary,
                active: t.secondary_hover,
                fg: t.foreground,
                border: Some(t.border),
            },
            Self::Ghost => Palette {
                bg: t.transparent,
                hover: t.secondary,
                active: t.secondary_hover,
                fg: if icon_only {
                    t.muted_foreground
                } else {
                    t.foreground
                },
                border: None,
            },
            Self::Danger => Palette {
                bg: t.button_danger,
                hover: t.button_danger_hover,
                active: t.button_danger_active,
                fg: t.button_danger_foreground,
                border: None,
            },
        }
    }
}

/// A text button (optionally with a leading icon).
///
/// ```ignore
/// ui::Button::new("rescan", tr("scan.rescan"))
///     .icon(IconName::RefreshCw)
///     .on_click(cx.listener(|this, _, _, cx| this.rescan(cx)))
/// ```
#[derive(IntoElement)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent builder options, each a plain on/off"
)]
pub struct Button {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<Icon>,
    variant: ButtonVariant,
    size: ControlSize,
    disabled: bool,
    loading: bool,
    selected: bool,
    full_width: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl Button {
    /// A `secondary`, `md` button labelled `label`.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: Some(label.into()),
            icon: None,
            variant: ButtonVariant::default(),
            size: ControlSize::default(),
            disabled: false,
            loading: false,
            selected: false,
            full_width: false,
            tooltip: None,
            on_click: None,
        }
    }

    /// Sets the variant.
    #[must_use]
    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Shorthand for [`ButtonVariant::Primary`].
    #[must_use]
    pub fn primary(self) -> Self {
        self.variant(ButtonVariant::Primary)
    }

    /// Shorthand for [`ButtonVariant::Outline`].
    #[must_use]
    pub fn outline(self) -> Self {
        self.variant(ButtonVariant::Outline)
    }

    /// Shorthand for [`ButtonVariant::Ghost`].
    #[must_use]
    pub fn ghost(self) -> Self {
        self.variant(ButtonVariant::Ghost)
    }

    /// Shorthand for [`ButtonVariant::Danger`].
    #[must_use]
    pub fn danger(self) -> Self {
        self.variant(ButtonVariant::Danger)
    }

    /// Sets the size.
    #[must_use]
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Shorthand for [`ControlSize::Sm`].
    #[must_use]
    pub fn small(self) -> Self {
        self.size(ControlSize::Sm)
    }

    /// Shorthand for [`ControlSize::Lg`].
    #[must_use]
    pub fn large(self) -> Self {
        self.size(ControlSize::Lg)
    }

    /// Leading icon.
    #[must_use]
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Ignores activation and dims the button.
    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Replaces the icon with a spinner and ignores activation.
    #[must_use]
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    /// Pressed/active look for toggles and chips.
    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Stretches to the container width.
    #[must_use]
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// Hover tooltip.
    #[must_use]
    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// Activation handler (pointer, Enter, Space).
    #[must_use]
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let icon_only = self.label.is_none();
        let palette = self.variant.palette(icon_only, cx);
        let size = self.size;
        let inert = self.disabled || self.loading;
        let radius = size.radius(cx.theme().radius);
        let focus = focus_handle(&self.id, window, cx);
        let (bg, fg) = if self.selected {
            (palette.active, cx.theme().foreground)
        } else {
            (palette.bg, palette.fg)
        };
        let Palette { hover, active, .. } = palette;
        let icon = if self.loading {
            Some(
                Spinner::new()
                    .color(fg)
                    .with_size(size.icon())
                    .into_any_element(),
            )
        } else {
            self.icon
                .map(|icon| icon.size(size.icon()).into_any_element())
        };
        let tooltip = self.tooltip;
        let on_click = self.on_click;

        let button = BaseButton::new(self.id)
            .track_focus(&focus)
            .disabled(inert)
            .selected(self.selected)
            .when_some(self.label.clone(), |this, label| {
                this.accessibility_label(label)
            })
            .when_some(tooltip.clone().filter(|_| icon_only), |this, label| {
                this.accessibility_label(label)
            })
            .flex_none()
            .h(size.height())
            .gap(control::GAP)
            .rounded(radius)
            .text_size(size.text())
            .font_weight(size.weight())
            .whitespace_nowrap()
            .text_color(fg)
            .bg(bg)
            .map(|this| {
                if icon_only {
                    this.w(size.height())
                } else {
                    this.px(size.pad_x(icon.is_none()))
                }
            })
            .when(self.full_width, Styled::w_full)
            .when_some(palette.border, |this, border| {
                this.border_1().border_color(border)
            })
            .map(|this| {
                if inert {
                    this.opacity(if self.loading {
                        1.
                    } else {
                        control::DISABLED_OPACITY
                    })
                    .cursor_default()
                } else {
                    this.cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .active(move |style| style.bg(active))
                }
            })
            .children(icon)
            .when_some(self.label, |this, label| {
                this.child(div().overflow_hidden().text_ellipsis().child(label))
            })
            .when_some(tooltip, |this, tooltip| {
                this.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            })
            .when_some(on_click, |this, on_click| {
                this.on_click(move |event, window, cx| on_click(event, window, cx))
            });
        focus_ring(button, &focus, window, cx)
    }
}

/// A square icon-only button (ghost by default); the tooltip names the action.
#[derive(IntoElement)]
pub struct IconButton(Button);

impl IconButton {
    /// An `md` ghost icon button; `tooltip` doubles as its accessible label.
    pub fn new(
        id: impl Into<ElementId>,
        icon: impl Into<Icon>,
        tooltip: impl Into<SharedString>,
    ) -> Self {
        let mut button = Button::new(id, SharedString::default())
            .ghost()
            .icon(icon)
            .tooltip(tooltip);
        button.label = None;
        Self(button)
    }

    /// Sets the variant.
    #[must_use]
    pub fn variant(self, variant: ButtonVariant) -> Self {
        Self(self.0.variant(variant))
    }

    /// Sets the size (`Sm` = 24 px, `Md` = 28 px).
    #[must_use]
    pub fn size(self, size: ControlSize) -> Self {
        Self(self.0.size(size))
    }

    /// Shorthand for [`ControlSize::Sm`].
    #[must_use]
    pub fn small(self) -> Self {
        Self(self.0.small())
    }

    /// Ignores activation and dims the button.
    #[must_use]
    pub fn disabled(self, disabled: bool) -> Self {
        Self(self.0.disabled(disabled))
    }

    /// Pressed look for toggles.
    #[must_use]
    pub fn selected(self, selected: bool) -> Self {
        Self(self.0.selected(selected))
    }

    /// Activation handler (pointer, Enter, Space).
    #[must_use]
    pub fn on_click(self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        Self(self.0.on_click(handler))
    }
}

impl RenderOnce for IconButton {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.0
    }
}
