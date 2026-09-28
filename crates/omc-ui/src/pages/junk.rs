//! The junk-style areas (system junk, browser data, developer junk, trash, installers,
//! leftovers): scan one area, review its groups and items, clean the selection.
//!
//! Only [`Safety::Safe`] items of apps that are not running start selected; everything
//! the user may want to keep (cookies, history, installers, old projects…) is left for the
//! user to pick, and its group summary says "Review".

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName as Glyph, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Div, ElementId, Entity, FontWeight, IntoElement,
    ListAlignment, ListState, ParentElement as _, Pixels, Render, SharedString, Styled as _,
    Subscription, Window, div, list, px,
};
use omc_proto::jobs::{CleanReport, Denied, FailReason, Failure, ItemId, JobOutput, Location};
use omc_proto::junk::{Browser, ItemTag, JunkItem, JunkKind, JunkReport, Safety};

use super::widgets::flow::FlowView;
use super::widgets::parts::{FAILURES_SHOWN, detail_ident, privacy_button, reason_stem};
use super::widgets::{self, CleanConfirm, FlowPhase, OnClick, Removal, tr};
use crate::format;
use crate::nav::Category;
use crate::scans::{self, Area, ScanEvent, Scans};
use crate::tokens::{card, control, row, space, text};
use crate::ui;

/// Selection state of a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Check {
    /// No item selected.
    None,
    /// Some items selected.
    Partial,
    /// Every item selected.
    All,
}

/// A one-click selection of the results card, in segment order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Preset {
    /// The safe items of apps that are not running (the initial selection).
    Safe,
    /// Every item.
    All,
    /// Nothing.
    None,
}

/// The presets in the order of the segmented control.
const PRESETS: [Preset; 3] = [Preset::Safe, Preset::All, Preset::None];

/// One item with its display strings, computed once per scan.
#[derive(Debug, Clone)]
struct ItemView {
    item: JunkItem,
    path: SharedString,
    /// The technical id shown before the path, when the path does not already end with it.
    ident: Option<SharedString>,
    size: SharedString,
}

#[derive(Debug, Clone)]
struct GroupView {
    kind: JunkKind,
    items: Vec<ItemView>,
    bytes: u64,
    size: SharedString,
    /// Some item is [`Safety::Review`]: the summary says so.
    review: bool,
}

/// A row of the flattened, virtualised list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Group(usize),
    Item(usize, usize),
}

/// A junk report with the user's selection and collapsed groups. Pure data: the page
/// renders it, tests drive it.
#[derive(Debug, Default)]
pub(crate) struct JunkModel {
    groups: Vec<GroupView>,
    denied: Vec<Denied>,
    selected: HashSet<ItemId>,
    collapsed: HashSet<usize>,
    rows: Vec<Row>,
    total_bytes: u64,
    total_items: usize,
    selected_bytes: u64,
    preset: Option<Preset>,
}

/// Whether an item starts selected: regenerated automatically and not in use.
fn preselected(item: &JunkItem) -> bool {
    item.safety == Safety::Safe && !item.app_running
}

impl JunkModel {
    /// Builds the model with the safe items selected.
    pub(crate) fn new(report: JunkReport) -> Self {
        let groups = report
            .groups
            .into_iter()
            .filter(|g| !g.items.is_empty())
            .map(|group| {
                let bytes = group.bytes();
                GroupView {
                    kind: group.kind,
                    review: group.items.iter().any(|i| i.safety == Safety::Review),
                    items: group
                        .items
                        .into_iter()
                        .map(|item| {
                            let path = format::tilde(&item.location.display());
                            ItemView {
                                ident: detail_ident(item.ident.as_deref(), &path),
                                path,
                                size: format::bytes(item.bytes),
                                item,
                            }
                        })
                        .collect(),
                    bytes,
                    size: format::bytes(bytes),
                }
            })
            .collect();
        let mut model = Self {
            groups,
            denied: report.denied,
            ..Self::default()
        };
        model.select_safe();
        model
    }

    fn items(&self) -> impl Iterator<Item = &JunkItem> {
        self.groups
            .iter()
            .flat_map(|g| g.items.iter().map(|v| &v.item))
    }

    fn refresh(&mut self) {
        self.selected_bytes = 0;
        self.total_bytes = 0;
        self.total_items = 0;
        let mut chosen = 0_usize;
        let mut safe_only = true;
        for item in self
            .groups
            .iter()
            .flat_map(|g| g.items.iter().map(|v| &v.item))
        {
            self.total_bytes = self.total_bytes.saturating_add(item.bytes);
            self.total_items = self.total_items.saturating_add(1);
            let selected = self.selected.contains(&item.id);
            if selected {
                self.selected_bytes = self.selected_bytes.saturating_add(item.bytes);
                chosen = chosen.saturating_add(1);
            }
            safe_only &= selected == preselected(item);
        }
        self.preset = if chosen == 0 {
            Some(Preset::None)
        } else if chosen == self.total_items {
            Some(Preset::All)
        } else if safe_only {
            Some(Preset::Safe)
        } else {
            None
        };
        self.rows.clear();
        for (gi, group) in self.groups.iter().enumerate() {
            self.rows.push(Row::Group(gi));
            if !self.collapsed.contains(&gi) {
                self.rows
                    .extend((0..group.items.len()).map(|ii| Row::Item(gi, ii)));
            }
        }
    }

    /// Flips one item.
    pub(crate) fn toggle_item(&mut self, id: ItemId) {
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
        self.refresh();
    }

    /// Selection state of group `gi`.
    pub(crate) fn group_check(&self, gi: usize) -> Check {
        let Some(group) = self.groups.get(gi) else {
            return Check::None;
        };
        let chosen = self.chosen_in(group);
        if chosen == 0 {
            Check::None
        } else if chosen == group.items.len() {
            Check::All
        } else {
            Check::Partial
        }
    }

    fn chosen_in(&self, group: &GroupView) -> usize {
        group
            .items
            .iter()
            .filter(|v| self.selected.contains(&v.item.id))
            .count()
    }

    /// A fully selected group becomes empty; a partial or empty one becomes full.
    pub(crate) fn toggle_group(&mut self, gi: usize) {
        let select = self.group_check(gi) != Check::All;
        if let Some(group) = self.groups.get(gi) {
            for view in &group.items {
                if select {
                    self.selected.insert(view.item.id);
                } else {
                    self.selected.remove(&view.item.id);
                }
            }
        }
        self.refresh();
    }

    /// Selects every item.
    pub(crate) fn select_all(&mut self) {
        self.selected = self.items().map(|i| i.id).collect();
        self.refresh();
    }

    /// Clears the selection.
    pub(crate) fn select_none(&mut self) {
        self.selected.clear();
        self.refresh();
    }

    /// Selects exactly the safe items of apps that are not running.
    pub(crate) fn select_safe(&mut self) {
        self.selected = self
            .items()
            .filter(|i| preselected(i))
            .map(|i| i.id)
            .collect();
        self.refresh();
    }

    /// Applies a one-click selection.
    pub(crate) fn apply(&mut self, preset: Preset) {
        match preset {
            Preset::Safe => self.select_safe(),
            Preset::All => self.select_all(),
            Preset::None => self.select_none(),
        }
    }

    /// The preset the selection equals (`None` once the user picked items by hand).
    pub(crate) fn preset(&self) -> Option<Preset> {
        self.preset
    }

    /// Folds (`collapsed`) or unfolds group `gi`.
    pub(crate) fn set_collapsed(&mut self, gi: usize, collapsed: bool) {
        if collapsed {
            self.collapsed.insert(gi);
        } else {
            self.collapsed.remove(&gi);
        }
        self.refresh();
    }

    /// Whether group `gi` is folded.
    pub(crate) fn is_collapsed(&self, gi: usize) -> bool {
        self.collapsed.contains(&gi)
    }

    /// Row index of group `gi`'s header and its item count.
    fn group_rows(&self, gi: usize) -> Option<(usize, usize)> {
        let header = self.rows.iter().position(|r| *r == Row::Group(gi))?;
        Some((header, self.groups.get(gi)?.items.len()))
    }

    /// Selected ids, ascending.
    pub(crate) fn selected_ids(&self) -> Vec<ItemId> {
        let mut ids: Vec<ItemId> = self
            .items()
            .map(|i| i.id)
            .filter(|id| self.selected.contains(id))
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Number of selected items.
    pub(crate) fn selected_count(&self) -> usize {
        self.selected.len()
    }

    /// Size of the selection.
    pub(crate) fn selected_bytes(&self) -> u64 {
        self.selected_bytes
    }

    /// Size of everything found.
    pub(crate) fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Number of rows in the flattened list.
    pub(crate) fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Drops the `cleaned` items, except those whose location failed.
    pub(crate) fn remove_cleaned(&mut self, cleaned: &[ItemId], report: &CleanReport) {
        let failed: HashSet<String> = report
            .failures
            .iter()
            .map(|f| f.location.display())
            .collect();
        let cleaned: HashSet<ItemId> = cleaned.iter().copied().collect();
        for group in &mut self.groups {
            group.items.retain(|v| {
                !cleaned.contains(&v.item.id) || failed.contains(&v.item.location.display())
            });
            group.bytes = group
                .items
                .iter()
                .fold(0_u64, |sum, v| sum.saturating_add(v.item.bytes));
            group.size = format::bytes(group.bytes);
            group.review = group.items.iter().any(|v| v.item.safety == Safety::Review);
        }
        // Group indices shift when groups vanish: forget the folding state.
        let before = self.groups.len();
        self.groups.retain(|g| !g.items.is_empty());
        if self.groups.len() != before {
            self.collapsed.clear();
        }
        self.selected.retain(|id| !cleaned.contains(id));
        self.refresh();
    }
}

/// How an area's items are removed: installers and leftovers are the user's files.
pub(crate) const fn removal(category: Category) -> Removal {
    match category {
        Category::Installers | Category::Leftovers => Removal::UserFiles,
        _ => Removal::Junk,
    }
}

/// The leading grid of the results list: headers and items share the checkbox and title
/// columns; items show their app's icon (or a glyph) in the icon column.
const GRID: ui::RowGrid = ui::RowGrid::new().disclosure().check().icon();

/// Glyph of an item without an app icon: files for installer-like groups, folders for
/// other paths, a settings glyph for registry entries and system actions.
fn item_glyph(kind: JunkKind, item: &JunkItem) -> Glyph {
    match item.location {
        Location::Path { .. } => match kind {
            JunkKind::DiskImage | JunkKind::InstallerPackage | JunkKind::SetupProgram => {
                Glyph::File
            }
            _ => Glyph::Folder,
        },
        _ => Glyph::Settings,
    }
}

/// Localised group title.
pub(crate) fn kind_label(kind: JunkKind) -> SharedString {
    let key = match kind {
        JunkKind::Browser(browser) => return browser_name(browser).into(),
        JunkKind::UserCache => "junk.kind.user_cache",
        JunkKind::SystemCache => "junk.kind.system_cache",
        JunkKind::UserLog => "junk.kind.user_log",
        JunkKind::SystemLog => "junk.kind.system_log",
        JunkKind::CrashReport => "junk.kind.crash_report",
        JunkKind::TempFiles => "junk.kind.temp_files",
        JunkKind::Thumbnails => "junk.kind.thumbnails",
        JunkKind::UpdateCache => "junk.kind.update_cache",
        JunkKind::PackageCache => "junk.kind.package_cache",
        JunkKind::ErrorReports => "junk.kind.error_reports",
        JunkKind::ShaderCache => "junk.kind.shader_cache",
        JunkKind::MailDownloads => "junk.kind.mail_downloads",
        JunkKind::DeviceBackups => "junk.kind.device_backups",
        JunkKind::OldOsInstall => "junk.kind.old_os_install",
        JunkKind::Xcode => "junk.kind.xcode",
        JunkKind::DevPackageCache => "junk.kind.dev_package_cache",
        JunkKind::ProjectArtifacts => "junk.kind.project_artifacts",
        JunkKind::IdeCache => "junk.kind.ide_cache",
        JunkKind::ToolCache => "junk.kind.tool_cache",
        JunkKind::Trash => "junk.kind.trash",
        JunkKind::DiskImage => "junk.kind.disk_image",
        JunkKind::InstallerPackage => "junk.kind.installer_package",
        JunkKind::SetupProgram => "junk.kind.setup_program",
        JunkKind::OrphanFiles => "junk.kind.orphan_files",
        JunkKind::OrphanLaunchItems => "junk.kind.orphan_launch_items",
        JunkKind::BrokenUninstallEntries => "junk.kind.broken_uninstall_entries",
        JunkKind::BrokenShortcuts => "junk.kind.broken_shortcuts",
        JunkKind::OrphanRegistry => "junk.kind.orphan_registry",
    };
    tr(key)
}

/// Product name of a browser (not translated).
const fn browser_name(browser: Browser) -> &'static str {
    match browser {
        Browser::Chrome => "Google Chrome",
        Browser::Chromium => "Chromium",
        Browser::Edge => "Microsoft Edge",
        Browser::Brave => "Brave",
        Browser::Vivaldi => "Vivaldi",
        Browser::Opera => "Opera",
        Browser::Arc => "Arc",
        Browser::Firefox => "Firefox",
        Browser::Safari => "Safari",
        Browser::Yandex => "Yandex Browser",
    }
}

/// Localised item tag, and whether removing it signs the user out or loses data.
fn tag_label(tag: ItemTag) -> (SharedString, bool) {
    let (key, sensitive) = match tag {
        ItemTag::Cache => ("junk.tag.cache", false),
        ItemTag::CodeCache => ("junk.tag.code_cache", false),
        ItemTag::GpuCache => ("junk.tag.gpu_cache", false),
        ItemTag::ServiceWorker => ("junk.tag.service_worker", false),
        ItemTag::Cookies => ("junk.tag.cookies", true),
        ItemTag::History => ("junk.tag.history", true),
        ItemTag::SiteData => ("junk.tag.site_data", true),
        ItemTag::Sessions => ("junk.tag.sessions", true),
        ItemTag::Logs => ("junk.tag.logs", false),
        ItemTag::Crashes => ("junk.tag.crashes", false),
        ItemTag::Temp => ("junk.tag.temp", false),
        ItemTag::NodeModules => ("junk.tag.node_modules", false),
        ItemTag::BuildOutput => ("junk.tag.build_output", false),
        ItemTag::PythonEnv => ("junk.tag.python_env", false),
        ItemTag::Packages => ("junk.tag.packages", false),
        ItemTag::Simulator => ("junk.tag.simulator", false),
        ItemTag::Archives => ("junk.tag.archives", false),
        ItemTag::DeviceSupport => ("junk.tag.device_support", false),
        ItemTag::Download => ("junk.tag.download", true),
    };
    (tr(key), sensitive)
}

/// Left inset of text under a 14 px icon and its gap (failure hints and paths).
fn icon_text_indent() -> Pixels {
    px(f32::from(control::ICON) + f32::from(space::MD))
}

/// `n` as a localisable count.
fn count(n: usize) -> SharedString {
    format::count(u64::try_from(n).unwrap_or(u64::MAX))
}

/// The header strip of a flush card: summary stats and controls on one line, a hairline
/// below.
fn card_strip(cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .w_full()
        .flex_none()
        .gap(space::XL)
        .px(card::PADDING)
        .py(card::HEADER_PAD_Y)
        .border_b_1()
        .border_color(theme.border.alpha(theme.border.a * card::BORDER_ALPHA))
}

/// "Scanned 40 min ago · These results may be out of date": a quiet line above results
/// past their freshness (the page toolbar holds the rescan).
fn stale_line(age: Duration, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .gap(space::SM)
        .text_size(text::SMALL)
        .line_height(text::SMALL_LINE_HEIGHT)
        .text_color(cx.theme().muted_foreground)
        .child(Icon::new(IconName::Clock).size(control::ICON))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(format!(
                    "{} · {}",
                    scans::scanned_ago(age),
                    tr("scans.stale.body")
                )),
        )
        .into_any_element()
}

/// The result of a clean as a card: freed / removed / failed stats with "Back to
/// results", then the failures grouped by reason with their explanation and fix.
pub(super) fn report_card(report: &CleanReport, on_back: OnClick, cx: &App) -> AnyElement {
    let failed = report.failures.len();
    let mut reasons: Vec<FailReason> = Vec::new();
    for failure in &report.failures {
        if !reasons.contains(&failure.reason) {
            reasons.push(failure.reason);
        }
    }
    let groups: Vec<AnyElement> = reasons
        .into_iter()
        .map(|reason| {
            let failures: Vec<&Failure> = report
                .failures
                .iter()
                .filter(|f| f.reason == reason)
                .collect();
            failure_group(reason, &failures, cx)
        })
        .collect();
    let strip = card_strip(cx)
        .when(groups.is_empty(), gpui_kit::Styled::border_b_0)
        .child(
            ui::Stat::new(tr("clean.stat.freed"), format::bytes(report.freed))
                .tone(ui::Tone::Success),
        )
        .child(ui::Stat::new(
            tr("clean.stat.removed"),
            format::count(report.removed),
        ))
        .when(failed > 0, |this| {
            this.child(
                ui::Stat::new(tr("clean.stat.failed"), count(failed)).tone(ui::Tone::Warning),
            )
        })
        .child(div().flex_1())
        .child(ui::Button::new("clean-back", tr("clean.back")).on_click(on_back));
    ui::Card::new()
        .flush()
        .child(strip)
        .when(!groups.is_empty(), |this| {
            this.child(
                v_flex()
                    .w_full()
                    .p(card::PADDING)
                    .gap(space::XL)
                    .children(groups),
            )
        })
        .into_any_element()
}

/// Failures of one reason: title and count, the fix, the explanation, the first paths.
fn failure_group(reason: FailReason, failures: &[&Failure], cx: &App) -> AnyElement {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let stem = reason_stem(reason);
    let more = failures.len().saturating_sub(FAILURES_SHOWN);
    let paths = failures.iter().take(FAILURES_SHOWN).map(|f| {
        div()
            .w_full()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis_middle()
            .text_size(text::CAPTION)
            .line_height(text::CAPTION_LINE_HEIGHT)
            .text_color(muted)
            .child(format::tilde(&f.location.display()))
    });
    v_flex()
        .w_full()
        .gap(space::XS)
        .child(
            h_flex()
                .w_full()
                .gap(space::MD)
                .child(
                    div()
                        .flex_none()
                        .text_color(ui::Tone::Warning.color(cx))
                        .child(Icon::new(IconName::TriangleAlert).size(control::ICON)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(text::BODY)
                        .line_height(text::BODY_LINE_HEIGHT)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!(
                            "{} · {}",
                            tr(&format!("clean.reason.{stem}")),
                            count(failures.len())
                        )),
                )
                .children(privacy_button(
                    ElementId::from(SharedString::from(format!("fix-{stem}"))),
                    reason,
                )),
        )
        .child(
            v_flex()
                .w_full()
                .pl(icon_text_indent())
                .gap(space::XXS)
                .child(
                    div()
                        .text_size(text::SMALL)
                        .line_height(text::SMALL_LINE_HEIGHT)
                        .text_color(muted)
                        .child(tr(&format!("clean.hint.{stem}"))),
                )
                .children(paths)
                .when(more > 0, |this| {
                    this.child(
                        div()
                            .text_size(text::CAPTION)
                            .line_height(text::CAPTION_LINE_HEIGHT)
                            .text_color(muted)
                            .child(rust_i18n::t!("clean.more", n = count(more)).to_string()),
                    )
                }),
        )
        .into_any_element()
}

/// One junk-style area: a view of its entry in the shared [`Scans`] store.
pub(crate) struct JunkPage {
    area: Category,
    store_area: Area,
    scans: Entity<Scans>,
    model: Option<JunkModel>,
    /// Variable-height rows: 32 px group headers, 40 px items.
    list: ListState,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for JunkPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JunkPage")
            .field("area", &self.area)
            .finish_non_exhaustive()
    }
}

impl JunkPage {
    /// Creates the page of `area` (one of [`super::JUNK_AREAS`]).
    pub(crate) fn new(area: Category, _window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let store_area = Area::of(area).unwrap_or(Area::SystemJunk);
        let scans = scans::entity(cx);
        let subscriptions = super::follow(&scans, store_area, cx, Self::on_scan);
        let mut this = Self {
            area,
            store_area,
            scans,
            model: None,
            list: ListState::new(0, ListAlignment::Top, row::HEIGHT_TWO_LINE),
            _subscriptions: subscriptions,
        };
        this.sync(cx);
        this
    }

    /// Rebuilds the model from the store's result (selection back to the safe items).
    fn sync(&mut self, cx: &mut Context<'_, Self>) {
        self.model = match self.scans.read(cx).output(self.store_area) {
            Some(JobOutput::Junk(report)) => Some(JunkModel::new(report.clone())),
            Some(other) => {
                tracing::warn!(area = ?self.area, ?other, "unexpected scan output");
                None
            }
            None => None,
        };
        self.reset_list();
        cx.notify();
    }

    /// Re-counts the list rows after the model was rebuilt or lost rows.
    fn reset_list(&self) {
        self.list
            .reset(self.model.as_ref().map_or(0, JunkModel::row_count));
    }

    fn on_scan(&mut self, event: &ScanEvent, _: &Entity<Scans>, cx: &mut Context<'_, Self>) {
        match event {
            ScanEvent::Scanned(_) | ScanEvent::Cleared(_) => self.sync(cx),
            ScanEvent::Cleaned { items, report, .. } => {
                self.update_model(cx, |m| m.remove_cleaned(items, report));
                self.reset_list();
            }
        }
    }

    /// The page came on screen (`shown`) or left it.
    pub(crate) fn set_visible(&mut self, shown: bool, cx: &mut Context<'_, Self>) {
        let area = self.store_area;
        self.scans.update(cx, |s, cx| {
            if shown {
                s.show(area, cx);
            } else {
                s.hide(area);
            }
        });
    }

    fn store(
        &mut self,
        cx: &mut Context<'_, Self>,
        f: impl FnOnce(&mut Scans, Area, &mut Context<'_, Scans>),
    ) {
        let area = self.store_area;
        self.scans.update(cx, |s, cx| f(s, area, cx));
    }

    fn scan(&mut self, cx: &mut Context<'_, Self>) {
        self.store(cx, Scans::rescan);
    }

    fn ask_clean(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(model) = &self.model else { return };
        let items = model.selected_ids();
        if items.is_empty() {
            return;
        }
        let confirm = CleanConfirm {
            count: items.len(),
            bytes: model.selected_bytes(),
            removal: removal(self.area),
        };
        let scans = self.scans.downgrade();
        let area = self.store_area;
        widgets::confirm_clean(
            confirm,
            move |_, cx| {
                let items = items.clone();
                if let Err(err) = scans.update(cx, |s, cx| s.clean(area, items, cx)) {
                    tracing::debug!("scan store gone before cleaning: {err}");
                }
            },
            window,
            cx,
        );
    }

    fn update_model(&mut self, cx: &mut Context<'_, Self>, f: impl FnOnce(&mut JunkModel)) {
        if let Some(model) = &mut self.model {
            f(model);
            cx.notify();
        }
    }

    /// Folds or unfolds group `gi`, telling the list which rows appeared or went.
    fn set_collapsed(&mut self, gi: usize, collapsed: bool, cx: &mut Context<'_, Self>) {
        let Some(model) = &mut self.model else { return };
        if model.is_collapsed(gi) == collapsed {
            return;
        }
        let Some((header, len)) = model.group_rows(gi) else {
            return;
        };
        model.set_collapsed(gi, collapsed);
        let start = header.saturating_add(1);
        if collapsed {
            self.list.splice(start..start.saturating_add(len), 0);
        } else {
            self.list.splice(start..start, len);
        }
        cx.notify();
    }

    fn listener(
        cx: &mut Context<'_, Self>,
        f: impl Fn(&mut Self, &mut Window, &mut Context<'_, Self>) + 'static,
    ) -> OnClick {
        Box::new(cx.listener(move |this, _, window, cx| f(this, window, cx)))
    }

    /// The page toolbar: "Scan again" once there is something to show, and the primary
    /// "Clean N items · size" while results are listed. Idle pages have their one large
    /// Scan button in the empty state instead; running jobs cancel from their card.
    fn header_actions(
        &self,
        flow: &FlowView,
        connected: bool,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        if flow.is_busy() || matches!(flow.phase, FlowPhase::Idle | FlowPhase::Failed) {
            return Vec::new();
        }
        let mut actions = vec![widgets::rescan_button(
            "junk-rescan",
            connected,
            Self::listener(cx, |this, _, cx| this.scan(cx)),
        )];
        if let Some(model) = self
            .model
            .as_ref()
            .filter(|m| flow.phase == FlowPhase::Ready && m.row_count() > 0)
        {
            let selected = model.selected_count();
            let label = if selected == 0 {
                rust_i18n::t!("junk.clean", count = count(0))
            } else {
                rust_i18n::t!(
                    "junk.clean_size",
                    count = count(selected),
                    bytes = format::bytes(model.selected_bytes())
                )
            };
            actions.push(
                ui::Button::new("junk-clean", label.to_string())
                    .primary()
                    .disabled(selected == 0 || !connected)
                    .on_click(Self::listener(cx, Self::ask_clean))
                    .into_any_element(),
            );
        }
        actions
    }

    fn render_row(&mut self, ix: usize, cx: &mut Context<'_, Self>) -> AnyElement {
        let row = self.model.as_ref().and_then(|m| m.rows.get(ix).copied());
        match row {
            Some(Row::Group(gi)) => self.group_row(gi, cx),
            Some(Row::Item(gi, ii)) => self.item_row(gi, ii, format::now(), cx),
            None => None,
        }
        .unwrap_or_else(|| div().into_any_element())
    }

    fn group_row(&self, gi: usize, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        let model = self.model.as_ref()?;
        let group = model.groups.get(gi)?;
        let chosen = model.chosen_in(group);
        let total = group.items.len();
        let summary = ui::selected_of(chosen, total);
        let summary: SharedString = if group.review {
            format!("{summary} · {}", tr("junk.badge.review")).into()
        } else {
            summary
        };
        // The whole header folds the group once per click/Enter/Space; its checkbox
        // selects without folding.
        Some(
            ui::CollapsibleHeader::new(
                ("junk-group", gi),
                kind_label(group.kind),
                !model.is_collapsed(gi),
            )
            .grid(GRID)
            .checkbox(
                ui::Checkbox::new(("junk-group-check", gi))
                    .state(ui::CheckState::from_counts(chosen, total))
                    .on_click(cx.listener(move |this, _: &bool, _, cx| {
                        this.update_model(cx, |m| m.toggle_group(gi));
                    })),
            )
            .summary(summary)
            .size_label(group.size.clone())
            .on_toggle(cx.listener(move |this, open: &bool, _, cx| {
                this.set_collapsed(gi, !*open, cx);
            }))
            .into_any_element(),
        )
    }

    fn item_row(
        &self,
        gi: usize,
        ii: usize,
        now: i64,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        let model = self.model.as_ref()?;
        let group = model.groups.get(gi)?;
        let view = group.items.get(ii)?;
        let item = &view.item;
        let id = item.id;
        let key = u64::from(id);
        let selected = model.selected.contains(&id);
        let age = item
            .modified
            .and_then(|m| format::age(m, now))
            .unwrap_or_default();
        // One badge at most (the data tag); running app and admin rights are quieter
        // facts with tooltips.
        let badge = item.tag.map(|tag| {
            let (label, sensitive) = tag_label(tag);
            ui::Badge::new(label).tone(if sensitive {
                ui::Tone::Warning
            } else {
                ui::Tone::Neutral
            })
        });
        Some(
            ui::ListRow::new(("junk-item", key), SharedString::from(item.name.clone()))
                .grid(GRID)
                .when_some(view.ident.clone(), ui::ListRow::detail_lead)
                .detail(view.path.clone())
                .checkbox(
                    ui::Checkbox::new(("junk-item-check", key))
                        .checked(selected)
                        .on_click(cx.listener(move |this, _: &bool, _, cx| {
                            this.update_model(cx, |m| m.toggle_item(id));
                        })),
                )
                .icon(ui::row_icon(
                    item.icon.as_deref(),
                    item_glyph(group.kind, item),
                    cx,
                ))
                .when(item.app_running, |this| {
                    this.trailing(
                        ui::Dot::new(ui::Tone::Warning)
                            .tooltip(("junk-running", key), tr("junk.badge.running")),
                    )
                })
                .when(item.needs_admin, |this| {
                    this.trailing(ui::FactIcon::new(
                        ("junk-admin", key),
                        IconName::Lock,
                        tr("junk.badge.admin"),
                    ))
                })
                .when_some(badge, ui::ListRow::trailing)
                .trailing(widgets::muted_cell(age, cx))
                .trailing(widgets::size_cell(view.size.clone(), cx))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.update_model(cx, |m| m.toggle_item(id));
                }))
                .into_any_element(),
        )
    }

    fn render_results(&self, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let Some(model) = &self.model else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(denied) = widgets::denied_notice(&model.denied, cx) {
            out.push(denied);
        }
        if model.row_count() == 0 {
            out.push(
                ui::Card::new()
                    .child(
                        ui::EmptyState::new(IconName::CircleCheck, tr("junk.empty.title"))
                            .description(tr("junk.empty.body")),
                    )
                    .into_any_element(),
            );
            return out;
        }
        let preset = model
            .preset()
            .and_then(|p| PRESETS.iter().position(|q| *q == p))
            .unwrap_or(usize::MAX);
        let strip = card_strip(cx)
            .child(ui::Stat::new(
                tr("junk.found"),
                format::bytes(model.total_bytes()),
            ))
            .child(
                ui::Stat::new(
                    tr("junk.selected_size"),
                    format::bytes(model.selected_bytes()),
                )
                .tone(ui::Tone::Accent),
            )
            .child(div().flex_1())
            .child(
                ui::Segmented::new("junk-preset")
                    .small()
                    .segment(tr("junk.select_safe"))
                    .segment(tr("scan.select_all"))
                    .segment(tr("scan.select_none"))
                    .selected(preset)
                    .on_select(cx.listener(|this, ix: &usize, _, cx| {
                        if let Some(preset) = PRESETS.get(*ix).copied() {
                            this.update_model(cx, |m| m.apply(preset));
                        }
                    })),
            );
        out.push(
            ui::Card::new()
                .flush()
                .flex_1()
                .min_h_0()
                .child(strip)
                .child(
                    list(
                        self.list.clone(),
                        cx.processor(|this, ix: usize, _, cx| this.render_row(ix, cx)),
                    )
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .p(row::INSET),
                )
                .into_any_element(),
        );
        out
    }

    /// Nothing scanned yet (or the scan failed): what the scan covers and the one large
    /// Scan button.
    fn render_idle(&self, connected: bool, cx: &mut Context<'_, Self>) -> AnyElement {
        ui::Card::new()
            .child(
                ui::EmptyState::new(self.area.icon(), tr("junk.idle.title"))
                    .when_some(self.area.covers(), ui::EmptyState::description)
                    .action(widgets::idle_scan_button(
                        "junk-scan",
                        connected,
                        Self::listener(cx, |this, _, cx| this.scan(cx)),
                    )),
            )
            .into_any_element()
    }
}

impl Render for JunkPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let (flow, connected, stale) = {
            let scans = self.scans.read(cx);
            (
                scans.view(self.store_area),
                scans.is_connected(),
                scans.stale_age(self.store_area, Instant::now()),
            )
        };
        let actions = self.header_actions(&flow, connected, cx);
        let mut column = widgets::page_column("junk-page")
            .child(widgets::area_header(self.area, actions, cx))
            .children(widgets::connection_notice(connected, cx));
        if let Some(error) = flow.error.clone() {
            column = column.child(widgets::error_notice(
                error,
                Self::listener(cx, |this, _, cx| this.store(cx, Scans::dismiss_error)),
                cx,
            ));
        }
        column = column.children(stale.map(|age| stale_line(age, cx)));
        let scrolls = flow.phase != FlowPhase::Ready;
        let body: Vec<AnyElement> = match flow.phase {
            FlowPhase::Idle | FlowPhase::Failed => vec![self.render_idle(connected, cx)],
            FlowPhase::Scanning | FlowPhase::Cleaning => vec![widgets::job_progress(
                "junk-progress",
                &flow.progress,
                flow.phase.counters(),
                Self::listener(cx, |this, _, cx| this.store(cx, Scans::cancel)),
                cx,
            )],
            FlowPhase::Ready => self.render_results(cx),
            FlowPhase::Cleaned => flow
                .report
                .as_ref()
                .map(|report| {
                    vec![report_card(
                        report,
                        Self::listener(cx, |this, _, cx| this.store(cx, Scans::dismiss_report)),
                        cx,
                    )]
                })
                .unwrap_or_default(),
        };
        v_flex().size_full().child(column.children(body).when(
            scrolls,
            gpui_kit::StatefulInteractiveElement::overflow_y_scroll,
        ))
    }
}

#[cfg(test)]
mod tests {
    use omc_proto::jobs::{CleanReport, FailReason, Failure, Location};
    use omc_proto::junk::{JunkGroup, JunkItem, JunkKind, JunkReport, Safety};

    use super::{Check, JunkModel, Preset};

    fn item(id: u32, bytes: u64, safety: Safety, running: bool) -> JunkItem {
        JunkItem {
            id,
            name: format!("item {id}"),
            location: Location::Path {
                path: format!("/junk/{id}"),
            },
            tag: None,
            bytes,
            files: 1,
            modified: None,
            safety,
            needs_admin: false,
            app_running: running,
            ident: None,
            icon: None,
        }
    }

    fn report() -> JunkReport {
        JunkReport {
            groups: vec![
                JunkGroup {
                    kind: JunkKind::UserCache,
                    items: vec![
                        item(0, 100, Safety::Safe, false),
                        item(1, 50, Safety::Safe, true),
                        item(2, 25, Safety::Review, false),
                    ],
                },
                JunkGroup {
                    kind: JunkKind::UserLog,
                    items: vec![item(3, 10, Safety::Safe, false)],
                },
                JunkGroup {
                    kind: JunkKind::TempFiles,
                    items: Vec::new(),
                },
            ],
            denied: Vec::new(),
        }
    }

    #[test]
    fn only_safe_items_of_idle_apps_start_selected() {
        let model = JunkModel::new(report());
        assert_eq!(model.selected_ids(), vec![0, 3], "safe, not running");
        assert_eq!(model.selected_bytes(), 110, "selected size");
        assert_eq!(model.total_bytes(), 185, "found size");
        assert_eq!(
            model.row_count(),
            6,
            "empty groups are dropped, groups are expanded"
        );
    }

    #[test]
    fn group_checkbox_is_tri_state() {
        let mut model = JunkModel::new(report());
        assert_eq!(model.group_check(0), Check::Partial, "one of three");
        assert_eq!(model.group_check(1), Check::All, "the only item");
        model.toggle_group(0);
        assert_eq!(model.group_check(0), Check::All, "partial becomes all");
        model.toggle_group(0);
        assert_eq!(model.group_check(0), Check::None, "all becomes none");
        model.toggle_item(2);
        assert_eq!(
            model.group_check(0),
            Check::Partial,
            "an item makes it partial"
        );
        assert_eq!(model.group_check(9), Check::None, "unknown group");
    }

    #[test]
    fn bulk_selection_and_folding() {
        let mut model = JunkModel::new(report());
        model.select_all();
        assert_eq!(model.selected_ids(), vec![0, 1, 2, 3], "all");
        model.select_none();
        assert!(model.selected_ids().is_empty(), "none");
        model.select_safe();
        assert_eq!(model.selected_ids(), vec![0, 3], "safe again");
        model.set_collapsed(0, true);
        assert_eq!(model.row_count(), 3, "folded group hides its items");
        assert!(model.is_collapsed(0), "folded");
        model.set_collapsed(0, false);
        assert_eq!(model.row_count(), 6, "unfolded");
    }

    #[test]
    fn the_preset_control_follows_the_selection() {
        let mut model = JunkModel::new(report());
        assert_eq!(
            model.preset(),
            Some(Preset::Safe),
            "starts on the safe items"
        );
        model.apply(Preset::All);
        assert_eq!(model.preset(), Some(Preset::All), "all");
        model.toggle_item(0);
        assert_eq!(
            model.preset(),
            None,
            "a hand-picked selection matches no preset"
        );
        model.apply(Preset::None);
        assert_eq!(model.preset(), Some(Preset::None), "none");
        model.toggle_item(0);
        model.toggle_item(3);
        assert_eq!(
            model.preset(),
            Some(Preset::Safe),
            "picking exactly the safe items is the safe preset"
        );
    }

    #[test]
    fn cleaning_drops_removed_items_but_keeps_failures() {
        let mut model = JunkModel::new(report());
        let report = CleanReport {
            removed: 1,
            freed: 100,
            failures: vec![Failure {
                location: Location::Path {
                    path: "/junk/3".to_owned(),
                },
                reason: FailReason::InUse,
                message: "busy".to_owned(),
            }],
        };
        model.remove_cleaned(&[0, 3], &report);
        assert_eq!(model.total_bytes(), 85, "the removed item is gone");
        assert_eq!(
            model.selected_ids(),
            Vec::<u32>::new(),
            "cleaned ids leave the selection"
        );
        assert_eq!(model.row_count(), 5, "the failed item stays listed");
    }

    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AnyWindowHandle, AppContext as _, ElementId, Entity, TestAppContext};
    use omc_proto::jobs::JobOutput;

    use super::JunkPage;
    use crate::nav::Category;
    use crate::pages::widgets::test_support::open_page;
    use crate::scans::{self, Area};

    fn open_junk(cx: &mut TestAppContext) -> Option<(AnyWindowHandle, Entity<JunkPage>)> {
        open_page(cx, |window, cx| {
            JunkPage::new(Category::SystemJunk, window, cx)
        })
    }

    /// Puts a finished system-junk scan in the store, as the overview's smart scan does.
    fn store_scan(cx: &mut TestAppContext) {
        let store = cx.update(scans::entity);
        store.update(cx, |s, cx| {
            s.force_scanned(Area::SystemJunk, JobOutput::Junk(report()), cx);
        });
        cx.run_until_parked();
    }

    fn rows(page: &Entity<JunkPage>, cx: &mut TestAppContext) -> Option<usize> {
        cx.update(|cx| page.read(cx).model.as_ref().map(JunkModel::row_count))
    }

    #[gpui_kit::test]
    fn page_renders_disconnected_then_results_and_asks_before_cleaning(cx: &mut TestAppContext) {
        let Some((window, page)) = open_junk(cx) else {
            return;
        };
        let connected = cx.update(|cx| page.read(cx).scans.read(cx).is_connected());
        assert!(
            !connected,
            "without a daemon the page shows the disconnected state"
        );
        store_scan(cx);
        assert_eq!(rows(&page, cx), Some(6), "the results list is built");
        let opened = window.update(cx, |_, window, cx| {
            page.update(cx, |page, cx| page.ask_clean(window, cx));
        });
        assert!(opened.is_ok(), "the window is alive");
        cx.run_until_parked();
        let dialog = window.update(cx, |_, window, cx| window.has_active_dialog(cx));
        assert_eq!(
            dialog.ok(),
            Some(true),
            "cleaning asks for confirmation first"
        );
    }

    #[gpui_kit::test]
    fn a_scan_elsewhere_and_its_clean_show_up_on_the_area_page(cx: &mut TestAppContext) {
        let Some((window, page)) = open_junk(cx) else {
            return;
        };
        assert_eq!(rows(&page, cx), None, "nothing scanned yet");
        store_scan(cx);
        assert_eq!(
            rows(&page, cx),
            Some(6),
            "the overview's result appears on the page"
        );
        let store = cx.update(scans::entity);
        store.update(cx, |s, cx| {
            s.force_cleaned(Area::SystemJunk, &[3], &CleanReport::default(), cx);
        });
        cx.run_until_parked();
        assert_eq!(
            rows(&page, cx),
            Some(4),
            "a clean from another view drops the item and its emptied group"
        );
        let again = window.update(cx, |_, window, cx| {
            cx.new(|cx| JunkPage::new(Category::SystemJunk, window, cx))
        });
        let Ok(again) = again else {
            return;
        };
        assert_eq!(
            rows(&again, cx),
            Some(4),
            "a page created later starts from the pruned shared result"
        );
    }

    #[gpui_kit::test]
    fn one_click_on_a_group_header_folds_once_and_its_checkbox_does_not(cx: &mut TestAppContext) {
        let Some((window, page)) = open_junk(cx) else {
            return;
        };
        store_scan(cx);
        let state = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                page.read(cx)
                    .model
                    .as_ref()
                    .map(|m| (m.is_collapsed(0), m.group_check(0)))
            })
        };
        let click = |id: ElementId, cx: &mut TestAppContext| {
            let clicked = window.update(cx, |_, window, cx| {
                window.render_frame(cx);
                window.click(id, cx);
            });
            assert!(clicked.is_ok(), "the window is alive");
            cx.run_until_parked();
        };
        assert_eq!(state(cx), Some((false, Check::Partial)), "starts unfolded");
        click(ElementId::from(("junk-group", 0_usize)), cx);
        assert_eq!(
            state(cx),
            Some((true, Check::Partial)),
            "one click folds the group exactly once and leaves the selection"
        );
        click(ElementId::from(("junk-group", 0_usize)), cx);
        assert_eq!(
            state(cx),
            Some((false, Check::Partial)),
            "a second click unfolds it"
        );
        click(ElementId::from(("junk-group-check", 0_usize)), cx);
        assert_eq!(
            state(cx),
            Some((false, Check::All)),
            "the checkbox selects the group without folding it"
        );
    }
}
