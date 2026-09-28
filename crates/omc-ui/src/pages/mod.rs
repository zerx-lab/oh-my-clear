//! One view entity per sidebar area, created once with the main window. Scan results live
//! in the shared store ([`crate::scans::Scans`]); pages keep only their view state
//! (selection, folding, filters) and follow the store.
//!
//! - [`overview::OverviewPage`]: smart scan across the junk areas, disks, permissions.
//! - [`junk::JunkPage`]: system junk, browser data, developer junk, trash, installers,
//!   leftovers (grouped checklists).
//! - [`space::SpacePage`]: disk usage drill-down.
//! - [`large::LargeFilesPage`]: large and old files.
//! - [`dupes::DuplicatesPage`]: duplicate groups.
//! - [`uninstaller::UninstallerPage`]: app list, related files, uninstall.
//! - [`startup::StartupPage`]: login/boot items.
//!
//! [`Pages::set_visible`] tells a page it came on screen (`on_shown`) or left it
//! (`on_hidden`); that is where results are refreshed by the freshness policy.

use std::time::Duration;

use gpui_kit::component::IconName;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, EventEmitter, IntoElement,
    Subscription, Window,
};

use crate::main_view::MainView;
use crate::nav::Category;
use crate::scans::{self, Area, ScanEvent, Scans};
use crate::ui;
use widgets::{OnClick, Tone, tr};

pub(crate) mod dupes;
pub(crate) mod junk;
pub(crate) mod large;
pub(crate) mod overview;
pub(crate) mod space;
pub(crate) mod startup;
pub(crate) mod uninstaller;
pub(crate) mod widgets;

use dupes::DuplicatesPage;
use junk::JunkPage;
use large::LargeFilesPage;
use overview::OverviewPage;
use space::SpacePage;
use startup::StartupPage;
use uninstaller::UninstallerPage;

/// Asks the main window to show another area (emitted by pages, handled by [`MainView`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Navigate(pub(crate) Category);

/// Follows `area` in the store: re-renders on every store change (progress, connection)
/// and hands `area`'s [`ScanEvent`]s to `on_event`.
pub(crate) fn follow<V: 'static>(
    store: &Entity<Scans>,
    area: Area,
    cx: &mut Context<'_, V>,
    on_event: impl Fn(&mut V, &ScanEvent, &Entity<Scans>, &mut Context<'_, V>) + 'static,
) -> Vec<Subscription> {
    vec![
        cx.observe(store, |_, _, cx| cx.notify()),
        cx.subscribe(store, move |this, store, event: &ScanEvent, cx| {
            if event.area() == area {
                on_event(this, event, &store, cx);
            }
        }),
    ]
}

/// "Scanned N ago · Rescan" for a result past its freshness (`age` from
/// [`Scans::stale_age`] or [`scans::stale_age`]). The rescan is a `sm` secondary: the
/// page's one primary (Clean) stays its main action.
pub(crate) fn stale_notice(
    id: &'static str,
    age: Option<Duration>,
    connected: bool,
    on_rescan: OnClick,
    cx: &App,
) -> Option<AnyElement> {
    let age = age?;
    Some(widgets::notice(
        Tone::Info,
        scans::scanned_ago(age),
        Some(tr("scans.stale.body")),
        vec![
            ui::Button::new(id, tr("scans.rescan"))
                .small()
                .icon(IconName::RefreshCw)
                .disabled(!connected)
                .on_click(on_rescan)
                .into_any_element(),
        ],
        cx,
    ))
}

/// The junk-style areas, each served by a [`JunkPage`].
pub(crate) const JUNK_AREAS: [Category; 6] = [
    Category::SystemJunk,
    Category::BrowserData,
    Category::DeveloperJunk,
    Category::Trash,
    Category::Installers,
    Category::Leftovers,
];

/// Every page of the main window.
pub(crate) struct Pages {
    pub(crate) overview: Entity<OverviewPage>,
    junk: Vec<(Category, Entity<JunkPage>)>,
    space: Entity<SpacePage>,
    large: Entity<LargeFilesPage>,
    dupes: Entity<DuplicatesPage>,
    uninstaller: Entity<UninstallerPage>,
    startup: Entity<StartupPage>,
}

impl std::fmt::Debug for Pages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pages").finish_non_exhaustive()
    }
}

impl Pages {
    /// Creates every page.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, MainView>) -> Self {
        Self {
            overview: cx.new(|cx| OverviewPage::new(window, cx)),
            junk: JUNK_AREAS
                .into_iter()
                .map(|area| (area, cx.new(|cx| JunkPage::new(area, window, cx))))
                .collect(),
            space: cx.new(|cx| SpacePage::new(window, cx)),
            large: cx.new(|cx| LargeFilesPage::new(window, cx)),
            dupes: cx.new(|cx| DuplicatesPage::new(window, cx)),
            uninstaller: cx.new(|cx| UninstallerPage::new(window, cx)),
            startup: cx.new(|cx| StartupPage::new(window, cx)),
        }
    }

    /// The page of `category`.
    pub(crate) fn view(&self, category: Category) -> AnyView {
        match category {
            Category::Overview => self.overview.clone().into(),
            Category::SpaceLens => self.space.clone().into(),
            Category::LargeOldFiles => self.large.clone().into(),
            Category::Duplicates => self.dupes.clone().into(),
            Category::Uninstaller => self.uninstaller.clone().into(),
            Category::StartupItems => self.startup.clone().into(),
            junk => self
                .junk
                .iter()
                .find(|(area, _)| *area == junk)
                .map_or_else(
                    || self.overview.clone().into(),
                    |(_, page)| page.clone().into(),
                ),
        }
    }

    /// Tells the page of `category` that it came on screen (`shown`) or left it. Pages
    /// refresh stale results when shown (see [`scans::on_show`]).
    pub(crate) fn set_visible(&self, category: Category, shown: bool, cx: &mut App) {
        match category {
            Category::Overview => {}
            Category::SpaceLens => self.space.update(cx, |p, cx| p.set_visible(shown, cx)),
            Category::LargeOldFiles => self.large.update(cx, |p, cx| p.set_visible(shown, cx)),
            Category::Duplicates => self.dupes.update(cx, |p, cx| p.set_visible(shown, cx)),
            Category::Uninstaller => {
                self.uninstaller
                    .update(cx, |p, cx| p.set_visible(shown, cx));
            }
            Category::StartupItems => self.startup.update(cx, |p, cx| p.set_visible(shown, cx)),
            junk => {
                if let Some((_, page)) = self.junk.iter().find(|(area, _)| *area == junk) {
                    page.update(cx, |p, cx| p.set_visible(shown, cx));
                }
            }
        }
    }
}

impl EventEmitter<Navigate> for OverviewPage {}
