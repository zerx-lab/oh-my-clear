//! The main window's sidebar: the cleaning areas of [`crate::nav::NAV`] and, at the bottom,
//! settings. It runs the full window height; its top strip lies under the transparent
//! titlebar (traffic lights and the sidebar toggle).
//!
//! Collapsing slides the panel out to the left. The clip width follows a spring (retargeted,
//! never restarted, when toggled mid-flight) while the panel keeps its full width and stays
//! pinned to the clip's right edge, so its text never re-wraps during the motion. Once
//! collapsed and settled the panel is not rendered at all.
//!
//! Every titled group folds (ADR 0024); the folded ids live in
//! [`UiSettings::collapsed_groups`], so they persist with the other preferences. A folded
//! group still shows the selected entry, so the user never loses where they are. The
//! Rules row carries the number of enabled rules and the Activity row the number of runs
//! waiting for an answer (one badge per row, ADR 0022).

use gpui_kit::base::spring;
use gpui_kit::component::{ActiveTheme as _, IconName, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div,
};

use crate::actions::OpenSettings;
use crate::motion;
use crate::nav::{Category, NAV, NavGroup};
use crate::rules::{self, Rules};
use crate::settings::UiSettings;
use crate::theme;
use crate::tokens::{chrome, layout, space};
use crate::ui;

/// Navigation state of the main window: selected area and whether the panel is shown.
pub(crate) struct Sidebar {
    collapsed: bool,
    selected: Category,
    rules: Entity<Rules>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for Sidebar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sidebar")
            .field("collapsed", &self.collapsed)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

/// The entries of `group` to draw: all of them while it is open, else only the selected one.
fn visible_items(group: &NavGroup, open: bool, selected: Category) -> Vec<Category> {
    group
        .items
        .iter()
        .copied()
        .filter(|&category| open || category == selected)
        .collect()
}

/// The badge of a sidebar row: `(count, tone)`, `None` when there is nothing to say.
fn badge_for(category: Category, enabled: usize, pending: usize) -> Option<(usize, ui::Tone)> {
    let (count, tone) = match category {
        Category::Rules => (enabled, ui::Tone::Neutral),
        Category::Activity => (pending, ui::Tone::Warning),
        _ => return None,
    };
    (count > 0).then_some((count, tone))
}

impl Sidebar {
    /// A sidebar showing the overview.
    pub(crate) fn new(cx: &mut Context<'_, Self>) -> Self {
        let rules = rules::entity(cx);
        // The badges follow the rules store.
        let subscriptions = vec![cx.observe(&rules, |_, _, cx| cx.notify())];
        Self {
            collapsed: false,
            selected: Category::default(),
            rules,
            _subscriptions: subscriptions,
        }
    }

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

    /// Shows `category`.
    pub(crate) fn select(&mut self, category: Category, cx: &mut Context<'_, Self>) {
        if self.selected != category {
            self.selected = category;
            cx.notify();
        }
    }

    fn row_badge(&self, category: Category, cx: &App) -> Option<AnyElement> {
        let model = self.rules.read(cx).model();
        let (count, tone) = badge_for(category, model.enabled_count(), model.pending_count())?;
        Some(
            ui::Badge::new(count.to_string())
                .tone(tone)
                .into_any_element(),
        )
    }

    fn render_nav(&self, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        NAV.iter()
            .map(|group| {
                let id = group.id();
                let open = id.is_none_or(|id| !UiSettings::get(cx).is_group_collapsed(id));
                let header = group.label.zip(id).map(|(label, id)| {
                    ui::NavGroupHeader::new(
                        ElementId::Name(SharedString::from(format!("sidebar-group-{id}"))),
                        tr(label),
                        open,
                    )
                    .on_toggle(move |_, _, cx| {
                        UiSettings::update(cx, |settings| settings.toggle_group(id));
                    })
                });
                let items = visible_items(group, open, self.selected)
                    .into_iter()
                    .map(|category| {
                        ui::NavItem::new(
                            ElementId::Name(SharedString::new_static(category.key())),
                            category.icon(),
                            category.title(),
                        )
                        .selected(category == self.selected)
                        .when_some(self.row_badge(category, cx), ui::NavItem::suffix)
                        .on_click(cx.listener(move |this, _, _, cx| this.select(category, cx)))
                    })
                    .collect::<Vec<_>>();
                v_flex()
                    .gap(space::XXS)
                    .children(header)
                    .children(items)
                    .into_any_element()
            })
            .collect()
    }

    fn render_footer(cx: &App) -> AnyElement {
        let settings = ui::NavItem::new(
            ElementId::Name(SharedString::new_static("sidebar-settings")),
            IconName::Settings,
            tr("nav.settings"),
        )
        .suffix(ui::Kbd::new(&OpenSettings))
        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx));
        div()
            .flex_none()
            .p(space::MD)
            .border_t_1()
            .border_color(theme::divider_color(cx))
            .child(settings)
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

        let nav = self.render_nav(cx);
        let footer = Self::render_footer(cx);
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
                        .gap(space::XS)
                        .children(nav),
                )
                .child(footer),
        )
    }
}

fn tr(key: &str) -> SharedString {
    rust_i18n::t!(key).to_string().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folded_group_still_shows_the_selected_entry() {
        let group = NavGroup {
            label: Some("nav.group.automation"),
            items: &[Category::Rules, Category::Activity],
        };
        assert_eq!(
            visible_items(&group, true, Category::Overview),
            [Category::Rules, Category::Activity],
            "an open group lists everything"
        );
        assert_eq!(
            visible_items(&group, false, Category::Activity),
            [Category::Activity],
            "a folded group keeps where the user is"
        );
        assert!(
            visible_items(&group, false, Category::Overview).is_empty(),
            "a folded group without the selection is empty"
        );
    }

    #[test]
    fn only_automation_rows_carry_a_badge_and_only_above_zero() {
        assert_eq!(
            badge_for(Category::Rules, 3, 9),
            Some((3, ui::Tone::Neutral)),
            "rules count the enabled ones"
        );
        assert_eq!(
            badge_for(Category::Activity, 3, 2),
            Some((2, ui::Tone::Warning)),
            "activity counts the runs waiting for the user"
        );
        assert_eq!(
            badge_for(Category::Activity, 3, 0),
            None,
            "nothing pending, no badge"
        );
        assert_eq!(
            badge_for(Category::Trash, 3, 2),
            None,
            "tool rows never carry one"
        );
    }
}
