//! Root view of the main window: titlebar over the workspace body. The body is the
//! empty state until a layout for Runs, agents and surfaces is chosen; it lists the
//! commands that exist, with their live key bindings.

use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    Action, AnyElement, AsKeystroke as _, Context, FocusHandle, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, Subscription,
    Window, div,
};

use crate::actions::{OpenSettings, Quit, ToggleAppearance};
use crate::theme;
use crate::title_bar::AppTitleBar;
use crate::tokens::{chrome, space, text};

/// Main window root view.
pub struct Workspace {
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace").finish_non_exhaustive()
    }
}

impl Workspace {
    /// Creates the view, focuses it, and follows OS appearance changes.
    pub fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let subscriptions = vec![cx.observe_window_appearance(window, |_, window, cx| {
            theme::system_appearance_changed(window.appearance(), cx);
        })];
        Self {
            focus_handle,
            _subscriptions: subscriptions,
        }
    }

    fn shortcut_row(
        label: String,
        action: &dyn Action,
        window: &Window,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let keys = window
            .highest_precedence_binding_for_action(action)
            .map(|binding| {
                binding
                    .keystrokes()
                    .iter()
                    .map(|stroke| Kbd::new(stroke.as_keystroke().clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        h_flex()
            .justify_between()
            .py(space::XS)
            .child(
                div()
                    .text_size(text::BODY)
                    .text_color(cx.theme().foreground)
                    .child(label),
            )
            .child(h_flex().gap(space::XS).children(keys))
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let shortcuts = [
            (
                rust_i18n::t!("title_bar.settings"),
                &OpenSettings as &dyn Action,
            ),
            (rust_i18n::t!("appearance.toggle"), &ToggleAppearance),
            (rust_i18n::t!("workspace.quit"), &Quit),
        ]
        .into_iter()
        .map(|(label, action)| Self::shortcut_row(label.into_owned(), action, window, cx))
        .collect::<Vec<_>>();

        v_flex()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .size_full()
            .child(AppTitleBar::new())
            .child(
                v_flex().flex_1().items_center().justify_center().child(
                    v_flex()
                        .w(chrome::EMPTY_STATE_WIDTH)
                        .gap(space::XXL)
                        .child(
                            v_flex()
                                .gap(space::SM)
                                .child(
                                    div()
                                        .text_size(text::DISPLAY)
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("dial"),
                                )
                                .child(
                                    div()
                                        .text_size(text::BODY)
                                        .line_height(text::BODY_LINE_HEIGHT)
                                        .text_color(muted)
                                        .child(rust_i18n::t!("app.tagline").into_owned()),
                                ),
                        )
                        .child(
                            v_flex()
                                .gap(space::XS)
                                .child(
                                    div()
                                        .text_size(text::CAPTION)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(muted)
                                        .child(rust_i18n::t!("workspace.shortcuts").into_owned()),
                                )
                                .children(shortcuts),
                        ),
                ),
            )
    }
}
