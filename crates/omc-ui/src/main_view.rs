//! Root view of the main window: a full-height sidebar beside the page of the selected area
//! and the status bar with the daemon connection. The transparent titlebar lies over the top
//! of both; it carries only the window controls and the sidebar toggle.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FocusHandle, Global, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, WeakEntity, Window, div,
};
use omc_ipc::client::ConnState;

use crate::actions::{OpenCommandPalette, ToggleSidebar};
use crate::engine::{self, Engine};
use crate::nav::Category;
use crate::pages::{Automate, Navigate, Pages};
use crate::palette::{Palette, PaletteEvent, Target};
use crate::sidebar::Sidebar;
use crate::theme;
use crate::title_bar::AppTitleBar;
use crate::tokens::{chrome, layout, space, text};

/// Main window root view.
pub struct MainView {
    focus_handle: FocusHandle,
    engine: Entity<Engine>,
    sidebar: Entity<Sidebar>,
    pages: Pages,
    /// The area whose page was last told it is on screen.
    shown: Category,
    /// The open command palette.
    palette: Option<Entity<Palette>>,
    palette_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

/// The open main view, so other windows (the prompt window's "Open oh-my-clear") can show
/// an area in it.
struct MainViewRef(WeakEntity<MainView>);

impl Global for MainViewRef {}

/// Brings the main window forward (reopening it when only other windows are left) and shows
/// `category` in it. Call outside any window update (e.g. from `cx.defer`).
pub(crate) fn show_in_main_window(category: Category, cx: &mut gpui_kit::App) {
    crate::window::activate_main_window(cx);
    let main = cx
        .try_global::<MainViewRef>()
        .and_then(|global| global.0.upgrade());
    if let Some(main) = main {
        main.update(cx, |this, cx| this.show(category, cx));
    }
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
        crate::prompt::attach(cx);
        let sidebar = cx.new(Sidebar::new);
        let pages = Pages::new(window, cx);
        let shown = sidebar.read(cx).selected();
        pages.set_visible(shown, true, cx);
        let mut subscriptions = vec![
            cx.observe_window_appearance(window, |_, window, cx| {
                theme::system_appearance_changed(window.appearance(), cx);
            }),
            // Coming back to the window refreshes the shown page if its result went stale.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.pages.set_visible(this.shown, true, cx);
                }
            }),
            // The status bar shows the connection state.
            cx.subscribe(&engine, |_, _, _, cx| cx.notify()),
            // The page and the toggle icon follow the sidebar; a newly selected page is
            // told it is on screen (it may refresh its results).
            cx.observe(&sidebar, |this, sidebar, cx| {
                let selected = sidebar.read(cx).selected();
                if selected != this.shown {
                    let previous = std::mem::replace(&mut this.shown, selected);
                    this.pages.set_visible(previous, false, cx);
                    this.pages.set_visible(selected, true, cx);
                }
                cx.notify();
            }),
            // The overview links to the areas.
            cx.subscribe(&pages.overview, |this, _, Navigate(category), cx| {
                let category = *category;
                this.sidebar
                    .update(cx, |sidebar, cx| sidebar.select(category, cx));
            }),
        ];
        // An area page asks to automate itself ("Clean automatically…").
        for page in pages.junk_pages() {
            subscriptions.push(cx.subscribe_in(
                page,
                window,
                |this, _, Automate(category), window, cx| {
                    this.automate(*category, window, cx);
                },
            ));
        }
        let this = cx.entity().downgrade();
        cx.set_global(MainViewRef(this));
        Self {
            focus_handle,
            engine,
            sidebar,
            pages,
            shown,
            palette: None,
            palette_subscription: None,
            _subscriptions: subscriptions,
        }
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<'_, Self>) {
        self.sidebar.update(cx, Sidebar::toggle);
    }

    /// Shows the page of `category`.
    pub(crate) fn show(&mut self, category: Category, cx: &mut Context<'_, Self>) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.select(category, cx));
    }

    /// Opens the Rules page with an unsaved rule that automates `category`.
    fn automate(&mut self, category: Category, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.show(Category::Rules, cx);
        self.pages
            .rules
            .update(cx, |page, cx| page.start_from_area(category, window, cx));
    }

    /// Opens the command palette, or closes it when it is open.
    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.palette.is_some() {
            self.close_palette(window, cx);
            return;
        }
        let palette = cx.new(|cx| Palette::new(window, cx));
        self.palette_subscription = Some(cx.subscribe_in(
            &palette,
            window,
            |this, _, event: &PaletteEvent, window, cx| this.on_palette(*event, window, cx),
        ));
        self.palette = Some(palette);
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.palette = None;
        self.palette_subscription = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn on_palette(&mut self, event: PaletteEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.close_palette(window, cx);
        match event {
            PaletteEvent::Dismiss => {}
            PaletteEvent::Choose(Target::Page(category)) => self.show(category, cx),
            PaletteEvent::Choose(Target::Rule(id)) => {
                self.show(Category::Rules, cx);
                self.pages
                    .rules
                    .update(cx, |page, cx| page.select_rule(id, window, cx));
            }
        }
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
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
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.pages.view(selected)),
            )
            .child(self.render_status_bar(cx));

        div()
            .id("main")
            .key_context("MainWindow")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &OpenCommandPalette, window, cx| {
                this.toggle_palette(window, cx);
            }))
            .relative()
            .size_full()
            .child(
                h_flex()
                    .size_full()
                    .child(self.sidebar.clone())
                    .child(content),
            )
            .child(Self::render_title_bar(collapsed, cx))
            .children(self.palette.clone())
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::Root;
    use gpui_kit::{AnyWindowHandle, TestAppContext};

    use super::MainView;
    use crate::nav::Category;
    use crate::pages::Automate;
    use crate::pages::rules::Draft;

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

    /// (selected page, palette open) of the main view in `window`.
    fn state(window: AnyWindowHandle, cx: &mut TestAppContext) -> Option<(Category, bool)> {
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
                    let main = main.read(cx);
                    Some((main.sidebar.read(cx).selected(), main.palette.is_some()))
                })
                .ok()
                .flatten()
        })
    }

    #[gpui_kit::test]
    fn the_palette_opens_moves_with_the_arrows_and_jumps_on_enter(cx: &mut TestAppContext) {
        let opened = cx.update(|cx| crate::init(cx).and_then(|()| crate::open_main_window(cx)));
        assert!(opened.is_ok(), "main window opens: {opened:?}");
        let Ok(window) = opened else { return };
        cx.run_until_parked();
        assert_eq!(
            state(window, cx),
            Some((Category::Overview, false)),
            "the overview shows, no palette"
        );

        cx.simulate_keystrokes(window, "secondary-k");
        assert_eq!(
            state(window, cx),
            Some((Category::Overview, true)),
            "secondary-k opens the palette"
        );

        cx.simulate_keystrokes(window, "escape");
        assert_eq!(
            state(window, cx),
            Some((Category::Overview, false)),
            "escape closes it although the text input has the focus"
        );

        // Entries are in sidebar order: Overview, System Junk, Browser Data, …
        cx.simulate_keystrokes(window, "secondary-k down down enter");
        assert_eq!(
            state(window, cx),
            Some((Category::BrowserData, false)),
            "two arrows down and Enter go to the third page and close the palette"
        );

        cx.simulate_keystrokes(window, "secondary-k up enter");
        assert_eq!(
            state(window, cx),
            Some((Category::Activity, false)),
            "up from the first entry wraps to the last page"
        );
    }

    #[gpui_kit::test]
    fn clean_automatically_opens_the_rules_page_with_the_unsaved_template(cx: &mut TestAppContext) {
        let opened = cx.update(|cx| crate::init(cx).and_then(|()| crate::open_main_window(cx)));
        assert!(opened.is_ok(), "main window opens: {opened:?}");
        let Ok(window) = opened else { return };
        cx.run_until_parked();

        let asked = cx.update(|cx| {
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
                    let junk = main.read(cx).pages.junk_pages().nth(2)?.clone();
                    junk.update(cx, |_, cx| cx.emit(Automate(Category::DeveloperJunk)));
                    Some(main)
                })
                .ok()
                .flatten()
        });
        cx.run_until_parked();
        let Some(main) = asked else {
            panic!("the developer junk page is the third area page");
        };
        let (selected, draft) = cx.update(|cx| {
            let main = main.read(cx);
            (
                main.sidebar.read(cx).selected(),
                main.pages.rules.read(cx).draft().cloned(),
            )
        });
        assert_eq!(selected, Category::Rules, "the Rules page is shown");
        assert_eq!(
            draft,
            Some(Draft::template(
                rust_i18n::t!("rules.template.name").to_string()
            )),
            "the editor holds the unsaved 'clean old build output' template"
        );
    }
}
