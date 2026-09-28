//! [`Segmented`]: single choice among a few short options, on gpui-base's `ToggleGroup` and
//! headless `Button`s.

use std::rc::Rc;

use gpui_kit::base::{Button as BaseButton, ToggleGroup};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div,
};

use super::{ControlSize, focus_handle, focus_ring};
use crate::tokens::{control, radius};

type SelectHandler = Rc<dyn Fn(&usize, &mut Window, &mut App)>;

/// One option of a [`Segmented`] control.
struct Segment {
    label: SharedString,
    icon: Option<Icon>,
    icon_only: bool,
}

/// A segmented control: an inset track with one raised, selected segment.
///
/// ```ignore
/// ui::Segmented::new("large-sort")
///     .small()
///     .segment(tr("files.sort.size"))
///     .segment(tr("files.sort.age"))
///     .selected(model.sort as usize)
///     .on_select(cx.listener(|this, ix: &usize, _, cx| this.set_sort(*ix, cx)))
/// ```
#[derive(IntoElement)]
pub struct Segmented {
    id: ElementId,
    size: ControlSize,
    segments: Vec<Segment>,
    selected: usize,
    disabled: bool,
    on_select: Option<SelectHandler>,
}

impl Segmented {
    /// An empty `md` control; add options with [`Self::segment`].
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            size: ControlSize::Md,
            segments: Vec::new(),
            selected: 0,
            disabled: false,
            on_select: None,
        }
    }

    /// Appends a text option.
    #[must_use]
    pub fn segment(mut self, label: impl Into<SharedString>) -> Self {
        self.segments.push(Segment {
            label: label.into(),
            icon: None,
            icon_only: false,
        });
        self
    }

    /// Appends an option with a leading icon.
    #[must_use]
    pub fn segment_with_icon(
        mut self,
        icon: impl Into<Icon>,
        label: impl Into<SharedString>,
    ) -> Self {
        self.segments.push(Segment {
            label: label.into(),
            icon: Some(icon.into()),
            icon_only: false,
        });
        self
    }

    /// Appends an icon-only option; `label` becomes its tooltip and accessible name.
    #[must_use]
    pub fn icon_segment(mut self, icon: impl Into<Icon>, label: impl Into<SharedString>) -> Self {
        self.segments.push(Segment {
            label: label.into(),
            icon: Some(icon.into()),
            icon_only: true,
        });
        self
    }

    /// Index of the selected option.
    #[must_use]
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Sets the size (`Sm` in toolbars, `Md` elsewhere; `Lg` is treated as `Md`).
    #[must_use]
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = match size {
            ControlSize::Lg => ControlSize::Md,
            other => other,
        };
        self
    }

    /// Shorthand for [`ControlSize::Sm`].
    #[must_use]
    pub fn small(self) -> Self {
        self.size(ControlSize::Sm)
    }

    /// Ignores activation and dims the control.
    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Called with the index of a newly chosen option (not for the selected one).
    #[must_use]
    pub fn on_select(mut self, handler: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Segmented {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let track = theme.muted;
        let raised = if theme.mode.is_dark() {
            theme.secondary_active
        } else {
            theme.popover
        };
        let (fg, muted_fg) = (theme.foreground, theme.muted_foreground);
        let shadow = theme.shadow;
        let outer = theme.radius;
        let inner = radius::inner(outer);
        let size = self.size;
        let selected = self.selected;
        let disabled = self.disabled;
        let group_id = self.id.clone();

        let segments = self
            .segments
            .into_iter()
            .enumerate()
            .map(|(ix, segment)| {
                let id = ElementId::NamedChild(
                    std::sync::Arc::new(group_id.clone()),
                    SharedString::from(ix.to_string()),
                );
                let focus = focus_handle(&id, window, cx);
                #[cfg(test)]
                let id_text = id.to_string();
                let is_selected = ix == selected;
                let on_select = self.on_select.clone();
                let label = segment.label.clone();
                let button = BaseButton::new(id)
                    .track_focus(&focus)
                    .disabled(disabled)
                    .selected(is_selected)
                    .accessibility_label(segment.label.clone())
                    .h_full()
                    .px(size.pad_x(segment.icon.is_none()))
                    .gap(control::GAP)
                    .rounded(inner)
                    .text_size(size.text())
                    .font_weight(size.weight())
                    .whitespace_nowrap()
                    .map(|this| {
                        if is_selected {
                            this.bg(raised)
                                .text_color(fg)
                                .when(shadow, Styled::shadow_xs)
                        } else {
                            this.text_color(muted_fg).when(!disabled, |this| {
                                this.cursor_pointer().hover(move |s| s.text_color(fg))
                            })
                        }
                    })
                    .when_some(segment.icon, |this, icon| {
                        this.child(icon.size(size.icon()))
                    })
                    .map(|this| {
                        if segment.icon_only {
                            this.tooltip(move |window, cx| {
                                Tooltip::new(label.clone()).build(window, cx)
                            })
                        } else {
                            this.child(div().child(segment.label))
                        }
                    })
                    .when_some(on_select.filter(|_| !is_selected), |this, on_select| {
                        this.on_click(move |_, window, cx| on_select(&ix, window, cx))
                    });
                #[cfg(test)]
                let button = button.debug_selector(|| id_text);
                focus_ring(button, &focus, window, cx)
            })
            .collect::<Vec<_>>();

        ToggleGroup::new(self.id)
            .flex()
            .flex_none()
            .items_center()
            .h(size.height())
            .p(control::SEGMENT_INSET)
            .gap(control::SEGMENT_INSET)
            .rounded(outer)
            .bg(track)
            .when(disabled, |this| this.opacity(control::DISABLED_OPACITY))
            .children(segments)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui_kit::{
        Context, IntoElement, Modifiers, ParentElement as _, Render, Styled as _, TestAppContext,
        Window, div,
    };

    use super::Segmented;

    struct Harness {
        selected: usize,
        picks: Rc<RefCell<Vec<usize>>>,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
            let picks = self.picks.clone();
            div().size_full().child(
                Segmented::new("seg")
                    .segment("A")
                    .segment("B")
                    .segment("C")
                    .selected(self.selected)
                    .on_select(cx.listener(move |this, ix: &usize, _, cx| {
                        picks.borrow_mut().push(*ix);
                        this.selected = *ix;
                        cx.notify();
                    })),
            )
        }
    }

    #[gpui_kit::test]
    fn clicking_a_segment_selects_it_once(cx: &mut TestAppContext) {
        let init = cx.update(crate::init);
        assert!(init.is_ok(), "UI initialises headless: {init:?}");
        let picks = Rc::new(RefCell::new(Vec::new()));
        let shared = picks.clone();
        let (view, cx) = cx.add_window_view(move |_, _| Harness {
            selected: 0,
            picks: shared,
        });
        cx.run_until_parked();
        let bounds = cx.debug_bounds("seg-1");
        assert!(bounds.is_some(), "segment B is laid out");
        let Some(b) = bounds else { return };
        cx.simulate_click(b.center(), Modifiers::default());
        assert_eq!(picks.borrow().as_slice(), &[1], "B picked exactly once");
        let selected = cx.update(|_, cx| view.read(cx).selected);
        assert_eq!(selected, 1, "the page's selection follows");

        // The selected segment is inert: re-clicking it reports nothing.
        cx.simulate_click(b.center(), Modifiers::default());
        assert_eq!(
            picks.borrow().len(),
            1,
            "re-clicking the selection is a no-op"
        );
        let first = cx.debug_bounds("seg-0");
        assert!(first.is_some(), "segment A is laid out");
        if let Some(a) = first {
            cx.simulate_click(a.center(), Modifiers::default());
        }
        assert_eq!(picks.borrow().as_slice(), &[1, 0], "A picked after B");
    }
}
