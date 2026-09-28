//! dial's client-side titlebar on top of gpui-component's `TitleBar`, which owns the
//! per-platform window controls:
//! - macOS: native traffic lights (positioned by [`crate::window::window_options`]),
//!   double-click follows the system "zoom/minimize" preference;
//! - Windows: drawn min/max/close buttons mapped to `WindowControlArea`, so snap layouts
//!   and caption hit-testing stay native;
//! - Linux: drawn controls only under client-side decorations, limited to what the
//!   compositor supports; right-click opens the window menu.
//!
//! dial adds its mark, an optional context title and quick toggles for appearance,
//! language and settings.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, TitleBar, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div,
};

use crate::actions::{self, OpenSettings};
use crate::i18n::Language;
use crate::settings::UiSettings;
use crate::theme::{self, Appearance};
use crate::tokens::{chrome, space, text};

/// Window titlebar. `title` names the window's content (e.g. "Settings"); the main window
/// passes none.
#[derive(Debug, IntoElement)]
pub struct AppTitleBar {
    title: Option<SharedString>,
}

impl AppTitleBar {
    /// A titlebar with just the product mark.
    pub fn new() -> Self {
        Self { title: None }
    }

    /// Shows `title` after the product mark.
    #[must_use]
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }
}

impl Default for AppTitleBar {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for AppTitleBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let settings = UiSettings::get(cx);
        let appearance = settings.theme.appearance;
        let language = settings.language;
        let is_dark = theme.mode.is_dark();

        let leading = h_flex()
            .gap(space::MD)
            .child(
                div()
                    .text_size(text::BODY)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child("dial"),
            )
            .when_some(self.title, |this, title| {
                this.child(
                    div()
                        .text_size(text::BODY)
                        .text_color(theme.muted_foreground)
                        .child(title),
                )
            });

        let trailing = h_flex()
            .gap(space::XS)
            .pr(space::MD)
            .child(
                Button::new("title-bar-appearance")
                    .ghost()
                    .small()
                    .icon(if is_dark {
                        IconName::Moon
                    } else {
                        IconName::Sun
                    })
                    .tooltip(rust_i18n::t!("title_bar.appearance").to_string())
                    .dropdown_menu(move |menu, _, _| {
                        Appearance::ALL.into_iter().fold(menu, |menu, choice| {
                            menu.menu_with_check(
                                choice.label(),
                                choice == appearance,
                                actions::appearance_action(choice),
                            )
                        })
                    }),
            )
            .child(
                Button::new("title-bar-language")
                    .ghost()
                    .small()
                    .icon(IconName::Globe)
                    .tooltip(rust_i18n::t!("title_bar.language").to_string())
                    .dropdown_menu(move |menu, _, _| {
                        Language::ALL.into_iter().fold(menu, |menu, choice| {
                            menu.menu_with_check(
                                choice.label(),
                                choice == language,
                                actions::language_action(choice),
                            )
                        })
                    }),
            )
            .child(
                Button::new("title-bar-settings")
                    .ghost()
                    .small()
                    .icon(IconName::Settings)
                    .tooltip_with_action(
                        rust_i18n::t!("title_bar.settings").to_string(),
                        &OpenSettings,
                        None,
                    )
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(OpenSettings), cx);
                    }),
            );

        TitleBar::new()
            .h(chrome::TITLE_BAR_HEIGHT)
            .border_color(theme::divider_color(cx))
            .child(leading)
            .child(trailing)
    }
}
