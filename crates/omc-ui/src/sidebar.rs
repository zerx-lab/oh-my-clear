//! The main window's sidebar: the cleaning areas of [`crate::nav::NAV`] and, at the bottom,
//! settings. It runs the full window height; its top strip lies under the transparent
//! titlebar (traffic lights and the sidebar toggle).
//!
//! Collapsing slides the panel out to the left. The clip width follows a spring (retargeted,
//! never restarted, when toggled mid-flight) while the panel keeps its full width and stays
//! pinned to the clip's right edge, so its text never re-wraps during the motion. Once
//! collapsed and settled the panel is not rendered at all.

use gpui_kit::base::spring;
use gpui_kit::component::sidebar::{SidebarGroup, SidebarItem as _, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use crate::actions::{self, OpenSettings};
use crate::motion;
use crate::nav::{Category, NAV};
use crate::theme;
use crate::tokens::{chrome, layout, space};

/// Navigation state of the main window: selected area and whether the panel is shown.
#[derive(Debug, Default)]
pub(crate) struct Sidebar {
    collapsed: bool,
    selected: Category,
}

impl Sidebar {
    /// Whether the panel is hidden (or on its way out).
    pub(crate) fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    /// The area the main window shows.
    pub(crate) fn selected(&self) -> Category {
        self.selected
    }

    /// Shows or hides the panel.
    pub(crate) fn toggle(&mut self, cx: &mut Context<'_, Self>) {
        self.collapsed = !self.collapsed;
        cx.notify();
    }

    fn select(&mut self, category: Category, cx: &mut Context<'_, Self>) {
        if self.selected != category {
            self.selected = category;
            cx.notify();
        }
    }

    fn render_nav(&self, window: &mut Window, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        NAV.iter()
            .enumerate()
            .map(|(ix, group)| {
                let menu = SidebarMenu::new()
                    .gap(space::XXS)
                    .children(group.items.iter().map(|&category| {
                        SidebarMenuItem::new(category.title())
                            .icon(Icon::new(category.icon()))
                            .active(category == self.selected)
                            .on_click(cx.listener(move |this, _, _, cx| this.select(category, cx)))
                    }));
                let id = SharedString::from(format!("sidebar-group-{ix}"));
                match group.label {
                    Some(label) => SidebarGroup::new(tr(label))
                        .child(menu)
                        .render(id, window, cx)
                        .into_any_element(),
                    None => menu.render(id, window, cx).into_any_element(),
                }
            })
            .collect()
    }

    fn render_footer(window: &mut Window, cx: &mut App) -> AnyElement {
        let settings = SidebarMenuItem::new(tr("nav.settings"))
            .icon(Icon::new(IconName::Settings))
            .suffix(|window, _| {
                h_flex()
                    .gap(space::XXS)
                    .children(actions::key_hints(&OpenSettings, window))
            })
            .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx));
        div()
            .flex_none()
            .p(space::MD)
            .border_t_1()
            .border_color(theme::divider_color(cx))
            .child(
                SidebarMenu::new()
                    .child(settings)
                    .render("sidebar-footer", window, cx),
            )
            .into_any_element()
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let (target, policy) = if self.collapsed {
            (Pixels::ZERO, motion::PANEL_EXIT)
        } else {
            (layout::SIDEBAR_WIDTH, motion::PANEL)
        };
        let width = spring("sidebar-width", target, policy, window, cx);
        let clip = div()
            .id("sidebar")
            .relative()
            .flex_none()
            .h_full()
            .w(width)
            .overflow_hidden();
        if width <= Pixels::ZERO {
            return clip;
        }

        let nav = self.render_nav(window, cx);
        let footer = Self::render_footer(window, cx);
        let theme = cx.theme();
        clip.child(
            v_flex()
                .absolute()
                .top_0()
                .right_0()
                .h_full()
                .w(layout::SIDEBAR_WIDTH)
                .bg(theme.sidebar)
                .text_color(theme.sidebar_foreground)
                .border_r_1()
                .border_color(theme::divider_color(cx))
                // Under the titlebar: traffic lights and the sidebar toggle.
                .child(div().flex_none().h(chrome::TITLE_BAR_HEIGHT))
                .child(
                    v_flex()
                        .id("sidebar-nav")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px(space::MD)
                        .pb(space::MD)
                        .gap(space::MD)
                        .children(nav),
                )
                .child(footer),
        )
    }
}

fn tr(key: &str) -> SharedString {
    rust_i18n::t!(key).to_string().into()
}
