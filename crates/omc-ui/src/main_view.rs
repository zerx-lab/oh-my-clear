//! Root view of the main window: a full-height sidebar beside the page of the selected area
//! and the status bar with the daemon connection. The transparent titlebar lies over the top
//! of both; it carries only the window controls and the sidebar toggle.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div,
};
use omc_ipc::client::ConnState;

use crate::actions::ToggleSidebar;
use crate::engine::{self, Engine};
use crate::page;
use crate::sidebar::Sidebar;
use crate::theme;
use crate::title_bar::AppTitleBar;
use crate::tokens::{chrome, layout, space, text};

/// Main window root view.
pub struct MainView {
    focus_handle: FocusHandle,
    engine: Entity<Engine>,
    sidebar: Entity<Sidebar>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for MainView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MainView").finish_non_exhaustive()
    }
}

fn tr(key: &str) -> SharedString {
    rust_i18n::t!(key).to_string().into()
}

impl MainView {
    /// Creates the view, focuses it, and follows OS appearance changes.
    pub fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let engine = engine::entity(cx);
        let sidebar = cx.new(|_| Sidebar::default());
        let subscriptions = vec![
            cx.observe_window_appearance(window, |_, window, cx| {
                theme::system_appearance_changed(window.appearance(), cx);
            }),
            // The status bar shows the connection state.
            cx.subscribe(&engine, |_, _, _, cx| cx.notify()),
            // The page and the toggle icon follow the sidebar.
            cx.observe(&sidebar, |_, _, cx| cx.notify()),
        ];
        Self {
            focus_handle,
            engine,
            sidebar,
            _subscriptions: subscriptions,
        }
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<'_, Self>) {
        self.sidebar.update(cx, Sidebar::toggle);
    }

    fn render_title_bar(collapsed: bool, cx: &mut Context<'_, Self>) -> AnyElement {
        let toggle = Button::new("sidebar-toggle")
            .ghost()
            .small()
            .icon(if collapsed {
                IconName::PanelLeftOpen
            } else {
                IconName::PanelLeftClose
            })
            .tooltip_with_action(tr("nav.toggle_sidebar").to_string(), &ToggleSidebar, None)
            .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)));
        v_flex()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .child(AppTitleBar::new().child(toggle))
            .into_any_element()
    }

    fn render_status_bar(&self, cx: &Context<'_, Self>) -> AnyElement {
        let theme = cx.theme();
        let (color, label, reason) = match self.engine.read(cx).state() {
            ConnState::Connected { .. } => (theme.success, tr("status.connected"), None),
            ConnState::Connecting => (theme.warning, tr("status.connecting"), None),
            ConnState::Reconnecting { reason, .. } => (
                theme.warning,
                tr("status.reconnecting"),
                Some(reason.clone()),
            ),
            ConnState::Failed { reason } => {
                (theme.danger, tr("status.unavailable"), Some(reason.clone()))
            }
        };
        let connection = h_flex()
            .id("status-connection")
            .gap(space::SM)
            .child(
                div()
                    .flex_none()
                    .size(layout::STATUS_DOT)
                    .rounded(layout::STATUS_DOT)
                    .bg(color),
            )
            .child(label)
            .when_some(reason, |this, reason| {
                this.tooltip(move |window, cx| Tooltip::new(reason.clone()).build(window, cx))
            });

        h_flex()
            .flex_none()
            .h(layout::STATUS_BAR_HEIGHT)
            .w_full()
            .px(space::LG)
            .border_t_1()
            .border_color(theme::divider_color(cx))
            .bg(theme.status_bar)
            .text_size(text::CAPTION)
            .text_color(theme.muted_foreground)
            .child(connection)
            .into_any_element()
    }
}

impl Render for MainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let (collapsed, selected) = {
            let sidebar = self.sidebar.read(cx);
            (sidebar.is_collapsed(), sidebar.selected())
        };
        let content = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(cx.theme().background)
            // Under the titlebar.
            .child(div().flex_none().h(chrome::TITLE_BAR_HEIGHT))
            .child(page::render(selected, window, cx))
            .child(self.render_status_bar(cx));

        div()
            .id("main")
            .key_context("MainWindow")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .relative()
            .size_full()
            .child(
                h_flex()
                    .size_full()
                    .child(self.sidebar.clone())
                    .child(content),
            )
            .child(Self::render_title_bar(collapsed, cx))
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::Root;
    use gpui_kit::{AnyWindowHandle, TestAppContext};

    use super::MainView;

    fn sidebar_collapsed(window: AnyWindowHandle, cx: &mut TestAppContext) -> Option<bool> {
        cx.update(|cx| {
            window
                .update(cx, |root, _, cx| {
                    let main = root
                        .downcast::<Root>()
                        .ok()?
                        .read(cx)
                        .view()
                        .clone()
                        .downcast::<MainView>()
                        .ok()?;
                    Some(main.read(cx).sidebar.read(cx).is_collapsed())
                })
                .ok()
                .flatten()
        })
    }

    #[gpui_kit::test]
    fn toggle_sidebar_shortcut_hides_and_shows_the_sidebar(cx: &mut TestAppContext) {
        let opened = cx.update(|cx| crate::init(cx).and_then(|()| crate::open_main_window(cx)));
        assert!(opened.is_ok(), "main window opens: {opened:?}");
        let Ok(window) = opened else { return };
        cx.run_until_parked();
        assert_eq!(
            sidebar_collapsed(window, cx),
            Some(false),
            "the sidebar starts shown"
        );

        cx.simulate_keystrokes(window, "secondary-b");
        assert_eq!(
            sidebar_collapsed(window, cx),
            Some(true),
            "secondary-b hides it"
        );

        cx.simulate_keystrokes(window, "secondary-b");
        assert_eq!(
            sidebar_collapsed(window, cx),
            Some(false),
            "secondary-b shows it again"
        );
    }
}
