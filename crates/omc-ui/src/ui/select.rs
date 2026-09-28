//! [`Select`]: a single choice from a dropdown list, on gpui-component's `Button` +
//! `DropdownMenu` pinned to the control scale.

use std::rc::Rc;

use gpui_kit::component::button::Button as KitButton;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Sizable as _};
use gpui_kit::{
    Anchor, App, ElementId, IntoElement, RenderOnce, SharedString, Styled as _, Window,
};

use super::ControlSize;

type ChangeHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// Menus with more options than this scroll instead of growing past the window.
const SCROLL_AFTER: usize = 8;

/// An outline trigger showing the selected option's label and a caret; the menu lists the
/// options with a check on the selected one. For choices among more than ~5 options or
/// long labels (settings); 2–4 short, always-visible options use [`super::Segmented`].
/// `sm` (24 px) or `md` (28 px, default).
#[derive(IntoElement)]
pub struct Select {
    id: ElementId,
    options: Rc<[(SharedString, SharedString)]>,
    selected: Option<SharedString>,
    label: Option<SharedString>,
    size: ControlSize,
    disabled: bool,
    anchor: Anchor,
    on_change: Option<ChangeHandler>,
}

impl Select {
    /// A select over `(value, label)` options, nothing selected, menu below-left.
    pub fn new(
        id: impl Into<ElementId>,
        options: impl IntoIterator<Item = (SharedString, SharedString)>,
    ) -> Self {
        Self {
            id: id.into(),
            options: options.into_iter().collect(),
            selected: None,
            label: None,
            size: ControlSize::Md,
            disabled: false,
            anchor: Anchor::TopLeft,
            on_change: None,
        }
    }

    /// The selected value (its option's label shows on the trigger).
    #[must_use]
    pub fn selected(mut self, value: impl Into<SharedString>) -> Self {
        self.selected = Some(value.into());
        self
    }

    /// Trigger text when no option is selected; without it the raw selected value shows,
    /// so a stored value outside the options still reads truthfully.
    #[must_use]
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// 24 px trigger (toolbars).
    #[must_use]
    pub fn small(mut self) -> Self {
        self.size = ControlSize::Sm;
        self
    }

    /// Ignores activation and dims the trigger.
    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Corner of the trigger the menu attaches to (`TopRight` for right-aligned controls).
    #[must_use]
    pub fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = anchor;
        self
    }

    /// Called with the chosen value (also when it is the selected one).
    #[must_use]
    pub fn on_change(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }

    /// Text on the trigger: the selected option's label, else the label, else the value.
    fn trigger_text(&self) -> SharedString {
        let selected = self.selected.as_ref();
        self.options
            .iter()
            .find(|(value, _)| Some(value) == selected)
            .map(|(_, label)| label.clone())
            .or_else(|| self.label.clone())
            .or_else(|| selected.cloned())
            .unwrap_or_default()
    }
}

impl RenderOnce for Select {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let text = self.trigger_text();
        let size = self.size;
        let (options, selected, on_change) = (self.options, self.selected, self.on_change);
        let scrollable = options.len() > SCROLL_AFTER;
        KitButton::new(self.id)
            .outline()
            .small()
            .label(text)
            .dropdown_caret(true)
            .disabled(self.disabled)
            .h(size.height())
            .px(size.pad_x(false))
            .text_size(size.text())
            .font_weight(size.weight())
            .dropdown_menu_with_anchor(self.anchor, move |menu, _, _| {
                options
                    .iter()
                    .fold(menu, |menu, (value, label)| {
                        let on_change = on_change.clone();
                        let value = value.clone();
                        menu.item(
                            PopupMenuItem::new(label.clone())
                                .checked(selected.as_ref() == Some(&value))
                                .on_click(move |_, window, cx| {
                                    if let Some(on_change) = &on_change {
                                        on_change(&value, window, cx);
                                    }
                                }),
                        )
                    })
                    .scrollable(scrollable)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::Select;
    use gpui_kit::SharedString;

    fn options() -> Vec<(SharedString, SharedString)> {
        vec![("a".into(), "Alpha".into()), ("b".into(), "Beta".into())]
    }

    #[test]
    fn the_trigger_names_the_selection_and_falls_back_to_the_raw_value() {
        let text = |select: Select| select.trigger_text().to_string();
        assert_eq!(
            text(Select::new("s", options()).selected("b")),
            "Beta",
            "option label"
        );
        assert_eq!(
            text(Select::new("s", options()).selected("zzz")),
            "zzz",
            "a value outside the options shows as is"
        );
        assert_eq!(
            text(Select::new("s", options()).selected("zzz").label("Pick")),
            "Pick",
            "the label stands in when nothing matches"
        );
        assert_eq!(
            text(Select::new("s", options())),
            "",
            "nothing selected, no label"
        );
    }
}
