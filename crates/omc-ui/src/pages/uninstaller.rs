//! Uninstaller: the installed apps (search, sort, source filter), everything that belongs
//! to one app with per-item review, and single or batch uninstall with live phases and a
//! result report.
//!
//! Jobs: `list_apps` when the page is first shown or the daemon (re)connects; `app_files`
//! for the app opened in the detail panel; `uninstall` per confirmed app (a batch runs
//! `app_files` + `uninstall` app by app with the preselected items). Related items are
//! preselected at or above the confidence set in settings.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Subscription, Task, WeakEntity, Window, div, img, uniform_list,
};
use omc_ipc::client::ClientEvent;
use omc_proto::Event;
use omc_proto::apps::{
    AppFile, AppFileKind, AppFilesReport, AppInfo, AppSource, Confidence, UninstallReport,
    UninstallerOutcome,
};
use omc_proto::jobs::{
    CleanReport, DeleteMethod, ItemId, JobId, JobOutput, JobSpec, Location, Phase, Progress,
    SpecialAction, UninstallSpec,
};

use super::widgets::{
    self, ConnChange, Connection, OnClick, Tone, area_header, check, chip, clean_report,
    connection_notice, error_notice, job_progress, notice, page_column, phase_label, size_cell, tr,
};
use crate::clean_settings::{self, CleanPrefs};
use crate::format;
use crate::nav::Category;
use crate::scans::{self, Area, OnShow};
use crate::tokens::{card, chrome, control, empty, layout, page, radius, row, space, text};
use crate::ui;

use super::widgets::slots::{self, JobSlot, SlotHost};

// ---- Pure model -------------------------------------------------------------------------

/// Sort orders of the app list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SortKey {
    /// A → Z.
    #[default]
    Name,
    /// Largest first.
    Size,
    /// Least recently used first (the usual uninstall candidates); unknown last.
    LastUsed,
    /// Newest install first; unknown last.
    Installed,
}

impl SortKey {
    const ALL: [Self; 4] = [Self::Name, Self::Size, Self::LastUsed, Self::Installed];

    const fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Size => "size",
            Self::LastUsed => "last_used",
            Self::Installed => "installed",
        }
    }
}

/// What the list shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct AppFilter {
    /// Case-insensitive substring of name, publisher or identifier.
    query: String,
    sort: SortKey,
    /// Only apps from this source.
    source: Option<AppSource>,
    /// Include OS apps.
    show_system: bool,
}

/// Indices into `apps` of the apps `filter` shows, in display order.
fn visible(apps: &[AppInfo], filter: &AppFilter) -> Vec<usize> {
    let query = filter.query.trim().to_lowercase();
    let matches = |app: &AppInfo| {
        query.is_empty()
            || [Some(&app.name), app.publisher.as_ref(), app.ident.as_ref()]
                .into_iter()
                .flatten()
                .any(|field| field.to_lowercase().contains(&query))
    };
    let mut shown: Vec<usize> = apps
        .iter()
        .enumerate()
        .filter(|(_, app)| filter.show_system || !app.system)
        .filter(|(_, app)| filter.source.is_none_or(|s| app.source == s))
        .filter(|(_, app)| matches(app))
        .map(|(ix, _)| ix)
        .collect();
    let name = |ix: usize| {
        apps.get(ix)
            .map(|a| a.name.to_lowercase())
            .unwrap_or_default()
    };
    let field = |ix: usize, get: fn(&AppInfo) -> Option<i64>| apps.get(ix).and_then(get);
    shown.sort_by(|&a, &b| {
        let primary = match filter.sort {
            SortKey::Name => std::cmp::Ordering::Equal,
            SortKey::Size => {
                let size = |ix: usize| apps.get(ix).and_then(|a| a.bytes);
                size(b).cmp(&size(a))
            }
            // `None` sorts after every value in both orders.
            SortKey::LastUsed => {
                let (x, y) = (field(a, |a| a.last_used), field(b, |a| a.last_used));
                match (x, y) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (x, y) => y.is_none().cmp(&x.is_none()).reverse(),
                }
            }
            SortKey::Installed => {
                let (x, y) = (field(a, |a| a.installed), field(b, |a| a.installed));
                match (x, y) {
                    (Some(x), Some(y)) => y.cmp(&x),
                    (x, y) => y.is_none().cmp(&x.is_none()).reverse(),
                }
            }
        };
        primary.then_with(|| name(a).cmp(&name(b)))
    });
    shown
}

/// Every source present in `apps`, in a stable order.
fn sources(apps: &[AppInfo]) -> Vec<AppSource> {
    ALL_SOURCES
        .into_iter()
        .filter(|s| apps.iter().any(|a| a.source == *s))
        .collect()
}

/// Items preselected for removal: confidence at or above `min`.
fn preselect(items: &[AppFile], min: Confidence) -> BTreeSet<ItemId> {
    items
        .iter()
        .filter(|item| item.confidence >= min)
        .map(|item| item.id)
        .collect()
}

const ALL_SOURCES: [AppSource; 12] = [
    AppSource::MacBundle,
    AppSource::MacAppStore,
    AppSource::Homebrew,
    AppSource::WinRegistry,
    AppSource::WinStore,
    AppSource::Deb,
    AppSource::Rpm,
    AppSource::Pacman,
    AppSource::Flatpak,
    AppSource::Snap,
    AppSource::AppImage,
    AppSource::Desktop,
];

const fn source_key(source: AppSource) -> &'static str {
    match source {
        AppSource::MacBundle => "mac_bundle",
        AppSource::MacAppStore => "mac_app_store",
        AppSource::Homebrew => "homebrew",
        AppSource::WinRegistry => "win_registry",
        AppSource::WinStore => "win_store",
        AppSource::Deb => "deb",
        AppSource::Rpm => "rpm",
        AppSource::Pacman => "pacman",
        AppSource::Flatpak => "flatpak",
        AppSource::Snap => "snap",
        AppSource::AppImage => "app_image",
        AppSource::Desktop => "desktop",
    }
}

const fn kind_key(kind: AppFileKind) -> &'static str {
    match kind {
        AppFileKind::Bundle => "bundle",
        AppFileKind::Support => "support",
        AppFileKind::Cache => "cache",
        AppFileKind::Preferences => "preferences",
        AppFileKind::Logs => "logs",
        AppFileKind::Container => "container",
        AppFileKind::GroupContainer => "group_container",
        AppFileKind::SavedState => "saved_state",
        AppFileKind::LaunchItem => "launch_item",
        AppFileKind::LoginItem => "login_item",
        AppFileKind::Receipt => "receipt",
        AppFileKind::Extension => "extension",
        AppFileKind::Shortcut => "shortcut",
        AppFileKind::Registry => "registry",
        AppFileKind::Service => "service",
        AppFileKind::ScheduledTask => "scheduled_task",
        AppFileKind::WebData => "web_data",
        AppFileKind::Other => "other",
    }
}

fn confidence_label(level: Confidence) -> SharedString {
    tr(&format!(
        "apps.confidence.{}",
        crate::settings_view::confidence_key(level)
    ))
}

fn count(n: usize) -> SharedString {
    format::count(u64::try_from(n).unwrap_or(u64::MAX))
}

/// Sums uninstall reports into one clean report (for the result view).
fn merged_clean(reports: impl IntoIterator<Item = CleanReport>) -> CleanReport {
    reports
        .into_iter()
        .fold(CleanReport::default(), |mut all, report| {
            all.removed = all.removed.saturating_add(report.removed);
            all.freed = all.freed.saturating_add(report.freed);
            all.failures.extend(report.failures);
            all
        })
}

// ---- Page -------------------------------------------------------------------------------

/// Jobs the page follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    List,
    Files,
    Uninstall,
}

const SLOTS: [Slot; 3] = [Slot::List, Slot::Files, Slot::Uninstall];

/// Where one app of an uninstall run is.
#[derive(Clone, Debug)]
enum RunStatus {
    Pending,
    Finding,
    Uninstalling,
    Done(UninstallReport),
    Failed(SharedString),
}

/// One app of an uninstall run.
#[derive(Clone, Debug)]
struct RunApp {
    app: AppInfo,
    /// Items to remove; `None` = find them first and take the preselection.
    items: Option<Vec<ItemId>>,
    /// The `app_files` job the items belong to (owned by the run).
    files_job: Option<JobId>,
    status: RunStatus,
}

/// A confirmed single or batch uninstall.
#[derive(Debug)]
struct Run {
    apps: Vec<RunApp>,
    current: Option<usize>,
    /// Run each app's own uninstaller first.
    vendor: bool,
    /// The daemon restarted mid-run.
    interrupted: bool,
}

impl Run {
    fn finished(&self) -> bool {
        self.current.is_none()
    }

    fn done_count(&self) -> usize {
        self.apps
            .iter()
            .filter(|a| matches!(a.status, RunStatus::Done(_) | RunStatus::Failed(_)))
            .count()
    }
}

/// The app opened in the detail panel.
#[derive(Debug)]
struct Detail {
    app: AppInfo,
    report: Option<AppFilesReport>,
    /// The report's `app_files` job (owned by the detail until a run takes it).
    files_job: Option<JobId>,
    picked: BTreeSet<ItemId>,
    /// Folded kind groups.
    collapsed: BTreeSet<AppFileKind>,
    error: Option<SharedString>,
}

/// Page view.
pub(crate) struct UninstallerPage {
    conn: Connection,
    /// The page is on screen.
    shown: bool,
    search: Entity<InputState>,
    filter: AppFilter,
    apps: Vec<AppInfo>,
    /// The `list_apps` job the apps belong to.
    apps_job: Option<JobId>,
    loaded: bool,
    /// When the app list arrived (freshness, see [`scans::Area::AppList`]).
    loaded_at: Option<Instant>,
    list_error: Option<SharedString>,
    visible: Vec<usize>,
    checked: BTreeSet<ItemId>,
    detail: Option<Detail>,
    run: Option<Run>,
    /// State of the confirmation's "run the app's own uninstaller" checkbox.
    confirm_vendor: bool,
    slots: [JobSlot; 3],
    notify_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for UninstallerPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UninstallerPage")
            .field("apps", &self.apps.len())
            .field("filter", &self.filter)
            .finish_non_exhaustive()
    }
}

impl SlotHost for UninstallerPage {
    type Slot = Slot;

    fn slot(&mut self, slot: Slot) -> Option<&mut JobSlot> {
        let ix = match slot {
            Slot::List => 0,
            Slot::Files => 1,
            Slot::Uninstall => 2,
        };
        self.slots.get_mut(ix)
    }

    fn output(&mut self, slot: Slot, job: JobId, output: JobOutput, cx: &mut Context<'_, Self>) {
        // The page owns finished jobs from here on; the slot must not release them.
        if let Some(s) = self.slot(slot) {
            s.job = None;
        }
        match (slot, output) {
            (Slot::List, JobOutput::Apps(list)) => {
                if let Some(old) = self.apps_job.replace(job) {
                    widgets::release(old, cx);
                }
                self.apps = list.apps;
                self.loaded = true;
                self.loaded_at = Some(Instant::now());
                self.list_error = None;
                self.checked.clear();
                self.refilter(cx);
            }
            (Slot::Files, JobOutput::AppFiles(report)) => self.files_found(job, report, cx),
            (Slot::Uninstall, JobOutput::Uninstall(report)) => {
                widgets::release(job, cx);
                self.finish_current(RunStatus::Done(report), cx);
            }
            (slot, other) => {
                tracing::warn!(?slot, ?other, "unexpected job output");
                widgets::release(job, cx);
                self.failed(slot, "unexpected job output".into(), cx);
            }
        }
        cx.notify();
    }

    fn failed(&mut self, slot: Slot, message: SharedString, cx: &mut Context<'_, Self>) {
        match slot {
            Slot::List => self.list_error = Some(message),
            Slot::Files if self.run_active() => {
                self.finish_current(RunStatus::Failed(message), cx);
            }
            Slot::Files => {
                if let Some(detail) = &mut self.detail {
                    detail.error = Some(message);
                }
            }
            Slot::Uninstall => self.finish_current(RunStatus::Failed(message), cx),
        }
        cx.notify();
    }

    fn notify_task(&mut self) -> &mut Option<Task<()>> {
        &mut self.notify_task
    }
}

impl UninstallerPage {
    /// Creates the page.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        clean_settings::attach(cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr("apps.search")));
        let engine = crate::engine::entity(cx);
        let subscriptions = vec![
            cx.subscribe(&engine, |this, _, event: &ClientEvent, cx| {
                this.on_engine(event, cx);
            }),
            cx.subscribe(&search, |this, search, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filter.query = search.read(cx).value().to_string();
                    this.refilter(cx);
                }
            }),
            cx.observe_global::<CleanPrefs>(|this, cx| {
                this.refilter(cx);
                // The auto-scan preference arrives with the daemon's settings.
                this.auto_load(cx);
            }),
        ];
        Self {
            conn: Connection::new(cx),
            shown: false,
            search,
            filter: AppFilter::default(),
            apps: Vec::new(),
            apps_job: None,
            loaded: false,
            loaded_at: None,
            list_error: None,
            visible: Vec::new(),
            checked: BTreeSet::new(),
            detail: None,
            run: None,
            confirm_vendor: true,
            slots: Default::default(),
            notify_task: None,
            _subscriptions: subscriptions,
        }
    }

    fn on_engine(&mut self, event: &ClientEvent, cx: &mut Context<'_, Self>) {
        match self.conn.observe(event) {
            ConnChange::NewDaemon => self.new_daemon(cx),
            ConnChange::Reattached => {
                for slot in SLOTS {
                    slots::poll(self, slot, cx);
                }
            }
            ConnChange::None => {}
        }
        match event {
            ClientEvent::Daemon(Event::Job(update)) => slots::route(self, &SLOTS, update, cx),
            ClientEvent::State(_) => {
                self.auto_load(cx);
                cx.notify();
            }
            ClientEvent::Daemon(_) => {}
        }
    }

    /// Every job id belongs to the old daemon: forget them all.
    fn new_daemon(&mut self, cx: &mut Context<'_, Self>) {
        for slot in &mut self.slots {
            slot.reset();
        }
        self.apps_job = None;
        self.apps.clear();
        self.visible.clear();
        self.checked.clear();
        self.loaded = false;
        self.loaded_at = None;
        self.list_error = None;
        self.detail = None;
        if let Some(run) = &mut self.run {
            if !run.finished() {
                run.interrupted = true;
            }
            run.current = None;
            for app in &mut run.apps {
                app.files_job = None;
                if matches!(app.status, RunStatus::Finding | RunStatus::Uninstalling) {
                    app.status = RunStatus::Failed(tr("uninstall.restarted"));
                }
            }
        }
        cx.notify();
    }

    fn list_busy(&self) -> bool {
        self.slots.first().is_some_and(JobSlot::is_active)
    }

    fn run_active(&self) -> bool {
        self.run.as_ref().is_some_and(|r| !r.finished())
    }

    /// The page came on screen (`shown`) or left it; showing it refreshes a missing or
    /// stale app list by the freshness policy.
    pub(crate) fn set_visible(&mut self, shown: bool, cx: &mut Context<'_, Self>) {
        self.shown = shown;
        if shown {
            self.auto_load(cx);
        }
    }

    /// Loads the app list when the page is shown, connected, idle, and the list is missing
    /// or stale (and the user lets areas scan by themselves). Never while an uninstall runs,
    /// a detail panel is open (the reload would close it) or after a failed load.
    fn auto_load(&mut self, cx: &mut Context<'_, Self>) {
        if !self.shown
            || !self.conn.is_connected()
            || !CleanPrefs::is_loaded(cx)
            || self.list_busy()
            || self.run_active()
            || self.detail.is_some()
            || self.list_error.is_some()
        {
            return;
        }
        let age = self.loaded_at.map(|t| t.elapsed());
        if scans::on_show(Area::AppList.policy(), age, CleanPrefs::auto_scan(cx)) == OnShow::Scan {
            self.load(cx);
        }
    }

    /// (Re)loads the app list. The detail panel closes: app ids change with the job.
    fn load(&mut self, cx: &mut Context<'_, Self>) {
        if self.run_active() {
            return;
        }
        self.list_error = None;
        self.close_detail(cx);
        slots::start(self, Slot::List, JobSpec::ListApps, cx);
        cx.notify();
    }

    fn refilter(&mut self, cx: &mut Context<'_, Self>) {
        self.filter.show_system = CleanPrefs::settings(cx).show_system_apps;
        if self
            .filter
            .source
            .is_some_and(|s| !self.apps.iter().any(|a| a.source == s))
        {
            self.filter.source = None;
        }
        self.visible = visible(&self.apps, &self.filter);
        cx.notify();
    }

    fn close_detail(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(files) = self.slot(Slot::Files).and_then(JobSlot::reset) {
            widgets::release(files, cx);
        }
        if let Some(job) = self.detail.take().and_then(|d| d.files_job) {
            widgets::release(job, cx);
        }
    }

    fn open_detail(&mut self, id: ItemId, cx: &mut Context<'_, Self>) {
        if self.run_active() || self.detail.as_ref().is_some_and(|d| d.app.id == id) {
            return;
        }
        let (Some(app), Some(apps_job)) = (
            self.apps.iter().find(|a| a.id == id).cloned(),
            self.apps_job,
        ) else {
            return;
        };
        self.close_detail(cx);
        if self.run.as_ref().is_some_and(Run::finished) {
            self.run = None;
        }
        self.detail = Some(Detail {
            app,
            report: None,
            files_job: None,
            picked: BTreeSet::new(),
            collapsed: BTreeSet::new(),
            error: None,
        });
        slots::start(
            self,
            Slot::Files,
            JobSpec::AppFiles { apps_job, app: id },
            cx,
        );
        cx.notify();
    }

    fn files_found(&mut self, job: JobId, report: AppFilesReport, cx: &mut Context<'_, Self>) {
        let min = CleanPrefs::settings(cx).leftover_confidence;
        let picked = preselect(&report.items, min);
        if self.run_active() {
            let run_uninstaller = self.run.as_ref().is_some_and(|r| r.vendor);
            let Some(current) = self.current_app_mut() else {
                widgets::release(job, cx);
                return;
            };
            current.files_job = Some(job);
            current.items = Some(picked.into_iter().collect());
            let items = current.items.clone().unwrap_or_default();
            current.status = RunStatus::Uninstalling;
            let spec = UninstallSpec {
                files_job: job,
                items,
                run_uninstaller,
            };
            slots::start(self, Slot::Uninstall, JobSpec::Uninstall(spec), cx);
            return;
        }
        match &mut self.detail {
            Some(detail) if detail.app.id == report.app.id => {
                if let Some(old) = detail.files_job.replace(job) {
                    widgets::release(old, cx);
                }
                detail.picked = picked;
                detail.report = Some(report);
            }
            _ => widgets::release(job, cx),
        }
    }

    fn current_app_mut(&mut self) -> Option<&mut RunApp> {
        let run = self.run.as_mut()?;
        let ix = run.current?;
        run.apps.get_mut(ix)
    }

    /// Records the current app's outcome and moves to the next one.
    fn finish_current(&mut self, status: RunStatus, cx: &mut Context<'_, Self>) {
        if let Some(app) = self.current_app_mut() {
            app.status = status;
            if let Some(job) = app.files_job.take() {
                widgets::release(job, cx);
            }
        }
        self.advance(cx);
    }

    fn advance(&mut self, cx: &mut Context<'_, Self>) {
        let apps_job = self.apps_job;
        let Some(run) = &mut self.run else { return };
        let next = run
            .apps
            .iter()
            .position(|a| matches!(a.status, RunStatus::Pending));
        run.current = next;
        let run_uninstaller = run.vendor;
        let Some(app) = next.and_then(|ix| run.apps.get_mut(ix)) else {
            // Everything done: the list is stale now.
            self.load(cx);
            return;
        };
        let spec = match (app.files_job, app.items.clone(), apps_job) {
            (Some(files_job), Some(items), _) => {
                app.status = RunStatus::Uninstalling;
                (
                    Slot::Uninstall,
                    JobSpec::Uninstall(UninstallSpec {
                        files_job,
                        items,
                        run_uninstaller,
                    }),
                )
            }
            (_, _, Some(apps_job)) => {
                app.status = RunStatus::Finding;
                (
                    Slot::Files,
                    JobSpec::AppFiles {
                        apps_job,
                        app: app.app.id,
                    },
                )
            }
            (_, _, None) => {
                app.status = RunStatus::Failed(tr("uninstall.restarted"));
                self.advance(cx);
                return;
            }
        };
        slots::start(self, spec.0, spec.1, cx);
        cx.notify();
    }

    fn start_run(&mut self, apps: Vec<RunApp>, run_uninstaller: bool, cx: &mut Context<'_, Self>) {
        if self.run_active() || apps.is_empty() {
            return;
        }
        // A single uninstall takes over the detail's files job.
        if let Some(detail) = self.detail.take() {
            let taken = apps.iter().any(|a| a.files_job == detail.files_job);
            if !taken && let Some(job) = detail.files_job {
                widgets::release(job, cx);
            }
        }
        self.checked.clear();
        self.run = Some(Run {
            apps,
            current: None,
            vendor: run_uninstaller,
            interrupted: false,
        });
        self.advance(cx);
    }

    fn toggle_checked(&mut self, id: ItemId, on: bool, cx: &mut Context<'_, Self>) {
        if on {
            self.checked.insert(id);
        } else {
            self.checked.remove(&id);
        }
        cx.notify();
    }

    fn toggle_item(&mut self, id: ItemId, on: bool, cx: &mut Context<'_, Self>) {
        if let Some(detail) = &mut self.detail {
            if on {
                detail.picked.insert(id);
            } else {
                detail.picked.remove(&id);
            }
        }
        cx.notify();
    }

    /// Picks (`on`) or drops every item of `kind` in the detail panel.
    fn pick_kind(&mut self, kind: AppFileKind, on: bool, cx: &mut Context<'_, Self>) {
        if let Some(detail) = &mut self.detail
            && let Some(report) = &detail.report
        {
            for item in report.items.iter().filter(|i| i.kind == kind) {
                if on {
                    detail.picked.insert(item.id);
                } else {
                    detail.picked.remove(&item.id);
                }
            }
        }
        cx.notify();
    }

    /// Unfolds (`open`) or folds the items of `kind` in the detail panel.
    fn set_kind_open(&mut self, kind: AppFileKind, open: bool, cx: &mut Context<'_, Self>) {
        if let Some(detail) = &mut self.detail {
            if open {
                detail.collapsed.remove(&kind);
            } else {
                detail.collapsed.insert(kind);
            }
        }
        cx.notify();
    }

    /// Asks before uninstalling the detail's app (with its picked items) or the checked
    /// apps (with their preselections).
    fn confirm(&mut self, batch: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        let settings = CleanPrefs::settings(cx).clone();
        let Some(apps) = self.run_apps(batch).filter(|apps| !apps.is_empty()) else {
            return;
        };
        let title: SharedString = match apps.as_slice() {
            [one] => rust_i18n::t!("uninstall.confirm.title_one", name = one.app.name.as_str()),
            many => rust_i18n::t!("uninstall.confirm.title_many", n = count(many.len())),
        }
        .to_string()
        .into();
        let mut lines: Vec<SharedString> = Vec::new();
        let mut needs_admin = apps.iter().any(|a| a.app.needs_admin);
        if let (false, Some(detail)) = (batch, &self.detail)
            && let Some(report) = &detail.report
        {
            let chosen = report
                .items
                .iter()
                .filter(|i| detail.picked.contains(&i.id));
            let (n, bytes, admin) = chosen.fold((0_usize, 0_u64, false), |(n, b, a), i| {
                (
                    n.saturating_add(1),
                    b.saturating_add(i.bytes),
                    a || i.needs_admin,
                )
            });
            needs_admin |= admin;
            lines.push(
                rust_i18n::t!(
                    "uninstall.confirm.body_one",
                    count = count(n),
                    bytes = format::bytes(bytes)
                )
                .to_string()
                .into(),
            );
        } else {
            const LISTED: usize = 8;
            let mut names = apps
                .iter()
                .take(LISTED)
                .map(|a| a.app.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            let more = apps.len().saturating_sub(LISTED);
            if more > 0 {
                names.push_str(", ");
                names.push_str(&rust_i18n::t!("uninstall.confirm.more", n = count(more)));
            }
            lines.push(names.into());
            lines.push(
                rust_i18n::t!(
                    "uninstall.confirm.body_many",
                    level = confidence_label(settings.leftover_confidence)
                )
                .to_string()
                .into(),
            );
        }
        lines.push(tr(match settings.files_delete {
            DeleteMethod::Trash => "uninstall.confirm.trash",
            DeleteMethod::Permanent => "uninstall.confirm.permanent",
        }));
        if settings.quit_running_apps {
            lines.push(tr("uninstall.confirm.quit"));
        }
        if needs_admin && settings.elevate {
            lines.push(tr("uninstall.confirm.admin"));
        }
        self.confirm_vendor = settings.run_vendor_uninstaller;
        let apps: Rc<[RunApp]> = apps.into();
        let lines: Rc<[SharedString]> = lines.into();
        let page = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            confirm_dialog(dialog, &page, &title, &lines, &apps, cx)
        });
    }
}

impl UninstallerPage {
    /// What a confirmed uninstall would run: the detail's app with its picked items, or
    /// the checked (non-system) apps with their preselections.
    fn run_apps(&self, batch: bool) -> Option<Vec<RunApp>> {
        if batch {
            return Some(
                self.apps
                    .iter()
                    .filter(|a| self.checked.contains(&a.id) && !a.system)
                    .map(|app| RunApp {
                        app: app.clone(),
                        items: None,
                        files_job: None,
                        status: RunStatus::Pending,
                    })
                    .collect(),
            );
        }
        let detail = self.detail.as_ref()?;
        let job = detail.files_job.filter(|_| detail.report.is_some())?;
        if detail.app.system || detail.picked.is_empty() {
            return None;
        }
        Some(vec![RunApp {
            app: detail.app.clone(),
            items: Some(detail.picked.iter().copied().collect()),
            files_job: Some(job),
            status: RunStatus::Pending,
        }])
    }
}

fn confirm_dialog(
    dialog: gpui_kit::component::dialog::Dialog,
    page: &WeakEntity<UninstallerPage>,
    title: &SharedString,
    lines: &Rc<[SharedString]>,
    apps: &Rc<[RunApp]>,
    cx: &App,
) -> gpui_kit::component::dialog::Dialog {
    let theme = cx.theme();
    let vendor = page.upgrade().is_some_and(|p| p.read(cx).confirm_vendor);
    let toggle_page = page.clone();
    let ok_page = page.clone();
    let ok_apps = apps.clone();
    dialog
        .w(page::DIALOG_WIDTH)
        .title(title.clone())
        .child(
            v_flex()
                .gap(space::MD)
                .children(lines.iter().map(|line| {
                    div()
                        .text_size(text::BODY)
                        .line_height(text::BODY_LINE_HEIGHT)
                        .text_color(theme.muted_foreground)
                        .child(line.clone())
                }))
                .child(
                    check("uninstall-run-vendor", vendor, move |on, _, cx| {
                        let on = *on;
                        if let Some(page) = toggle_page.upgrade() {
                            page.update(cx, |page, cx| {
                                page.confirm_vendor = on;
                                cx.notify();
                            });
                        }
                    })
                    .label(tr("uninstall.confirm.run_uninstaller")),
                ),
        )
        .footer(
            h_flex()
                .w_full()
                .justify_end()
                .gap(space::MD)
                .child(
                    ui::Button::new("uninstall-cancel", tr("uninstall.confirm.cancel"))
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
                .child(
                    ui::Button::new("uninstall-ok", tr("uninstall.confirm.ok"))
                        .danger()
                        .on_click(move |_, window, cx| {
                            if let Some(page) = ok_page.upgrade() {
                                let apps = ok_apps.to_vec();
                                page.update(cx, |page, cx| {
                                    let vendor = page.confirm_vendor;
                                    page.start_run(apps, vendor, cx);
                                });
                            }
                            window.close_dialog(cx);
                        }),
                ),
        )
}

// ---- Rendering --------------------------------------------------------------------------

/// Width of the detail pane beside the app list.
const DETAIL_WIDTH: Pixels = chrome::EMPTY_STATE_WIDTH;
/// App icon in a list row (24 px).
const ROW_ICON: Pixels = space::XXL;
/// App icon in the detail header (40 px, the empty-state circle's size).
const HEADER_ICON: Pixels = empty::ICON_CIRCLE;

/// The leading grid of the detail's item list: kind headers and their items share the
/// checkbox and title columns.
const DETAIL_GRID: ui::RowGrid = ui::RowGrid::new().disclosure().check();

/// Last component of a path or registry key (the whole text when it has none).
fn leaf(display: &str) -> &str {
    display
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(display)
}

/// Leaf names that say nothing without their folder ("Cache", "Data", "config").
const GENERIC_LEAVES: [&str; 9] = [
    "cache",
    "caches",
    "config",
    "data",
    "default",
    "logs",
    "settings",
    "storage",
    "user data",
];

/// A reverse-DNS style identifier (`com.mitchellh.ghostty`, `group.com.x`): at least three
/// dot-separated runs of letters, digits, `-` or `_`.
fn is_identifier(name: &str) -> bool {
    let mut runs = 0_usize;
    for run in name.split('.') {
        if run.is_empty()
            || !run
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return false;
        }
        runs = runs.saturating_add(1);
    }
    runs >= 3
}

/// Title of a path in an app's item list: its last component, prefixed with its folder
/// (`Preferences › com.mitchellh.ghostty.plist`) when the name alone does not read as a
/// place — an identifier, a `.plist`, or a generic name like "Cache".
fn path_title(path: &str) -> String {
    let mut parts = path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .filter(|s| !s.is_empty());
    let Some(name) = parts.next() else {
        return path.to_owned();
    };
    let vague = std::path::Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("plist"))
        || is_identifier(name)
        || GENERIC_LEAVES.contains(&name.to_lowercase().as_str());
    match parts
        .next()
        .filter(|folder| vague && *folder != "~" && !folder.ends_with(':'))
    {
        Some(folder) => format!("{folder} › {name}"),
        None => name.to_owned(),
    }
}

/// Row title of an app's related item, readable for people who don't know the path
/// layout: see [`path_title`] for files and folders; registry values by their name
/// (`Run\Ghostty` → `Ghostty`), keys by their last component; actions by the thing
/// they act on (a service, task or login item name, a launch job's plist, a package id).
fn item_title(location: &Location) -> String {
    match location {
        Location::Path { path } => path_title(path),
        Location::RegistryKey { key } => leaf(key).to_owned(),
        Location::RegistryValue { key, name } if name.is_empty() => leaf(key).to_owned(),
        Location::RegistryValue { name, .. } => name.clone(),
        Location::Special { action } => match action {
            SpecialAction::UnloadLaunchJob { plist, .. } => path_title(plist),
            SpecialAction::DeleteScheduledTask { path } => leaf(path).to_owned(),
            SpecialAction::DeleteService { name } | SpecialAction::RemoveLoginItem { name } => {
                name.clone()
            }
            SpecialAction::ForgetPackage { id } => id.clone(),
            SpecialAction::EmptyTrash => location.display(),
        },
    }
}

/// Detail line of an app's related item: the path (`~`), a launch job's plist rather than
/// the action, the registry path.
fn item_detail(location: &Location) -> SharedString {
    match location {
        Location::Special {
            action: SpecialAction::UnloadLaunchJob { plist, .. },
        } => format::tilde(plist),
        _ => format::tilde(&location.display()),
    }
}

/// The app's icon (24 px in rows, 40 px in the detail header); a Lucide glyph in a rounded
/// muted tile when the app has none or it cannot be loaded.
fn app_icon(app: &AppInfo, large: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (size, glyph, corner) = if large {
        (HEADER_ICON, empty::ICON, radius::outer(theme.radius))
    } else {
        (ROW_ICON, control::ICON, theme.radius)
    };
    let (fill, fg) = (theme.muted, theme.muted_foreground);
    let tile = move || {
        div()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(size)
            .rounded(corner)
            .bg(fill)
            .text_color(fg)
            .child(Icon::new(AssetIcon::AppWindow).size(glyph))
            .into_any_element()
    };
    match &app.icon {
        Some(path) => img(PathBuf::from(path))
            .flex_none()
            .size(size)
            .with_fallback(tile)
            .into_any_element(),
        None => tile(),
    }
}

fn app_subtitle(app: &AppInfo) -> SharedString {
    [
        app.version.as_deref(),
        app.publisher.as_deref().or(app.ident.as_deref()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
    .into()
}

/// A row's secondary facts, quietest form: "System" as the one neutral badge, running as a
/// warning dot, needs-admin as a lock icon. `scope` keeps element ids unique per place.
fn app_facts(app: &AppInfo, scope: &'static str) -> Vec<AnyElement> {
    let id = |fact: &str| SharedString::from(format!("{scope}-{fact}-{}", app.id));
    let mut facts = Vec::new();
    if app.system {
        facts.push(ui::Badge::new(tr("apps.badge.system")).into_any_element());
    }
    if app.running {
        facts.push(
            ui::Dot::new(ui::Tone::Warning)
                .tooltip(id("running"), tr("apps.badge.running"))
                .into_any_element(),
        );
    }
    if app.needs_admin {
        facts.push(
            ui::FactIcon::new(
                id("admin"),
                Icon::new(AssetIcon::Lock),
                tr("apps.badge.admin"),
            )
            .into_any_element(),
        );
    }
    facts
}

/// Badge tone of a related item's confidence.
const fn confidence_tone(level: Confidence) -> ui::Tone {
    match level {
        Confidence::High => ui::Tone::Neutral,
        Confidence::Medium => ui::Tone::Warning,
        Confidence::Low => ui::Tone::Danger,
    }
}

fn phase_text(phase: Phase) -> SharedString {
    tr(match phase {
        Phase::Quitting => "uninstall.phase.quitting",
        Phase::Uninstalling => "uninstall.phase.uninstalling",
        Phase::Elevating => "uninstall.phase.elevating",
        Phase::Removing | Phase::Finishing => "uninstall.phase.removing",
        Phase::Starting | Phase::Scanning | Phase::Measuring | Phase::Hashing => {
            "uninstall.phase.starting"
        }
    })
}

/// The uninstall steps in order, as the stepper lists them.
const STEPS: [&str; 4] = [
    "uninstall.step.quit",
    "uninstall.step.uninstaller",
    "uninstall.step.admin",
    "uninstall.step.remove",
];

/// Index into [`STEPS`] of the step `phase` belongs to (`None` while preparing).
const fn step_of(phase: Phase) -> Option<usize> {
    match phase {
        Phase::Quitting => Some(0),
        Phase::Uninstalling => Some(1),
        Phase::Elevating => Some(2),
        Phase::Removing | Phase::Finishing => Some(3),
        Phase::Starting | Phase::Scanning | Phase::Measuring | Phase::Hashing => None,
    }
}

fn outcome_text(outcome: &UninstallerOutcome) -> SharedString {
    match outcome {
        UninstallerOutcome::NotRun => tr("uninstall.outcome.not_run"),
        UninstallerOutcome::Succeeded => tr("uninstall.outcome.succeeded"),
        UninstallerOutcome::Cancelled => tr("uninstall.outcome.cancelled"),
        UninstallerOutcome::Failed { code, message } => {
            let detail = match code {
                Some(code) if message.is_empty() => code.to_string(),
                Some(code) => format!("{message} ({code})"),
                None => message.clone(),
            };
            rust_i18n::t!("uninstall.outcome.failed", detail = detail)
                .to_string()
                .into()
        }
    }
}

/// Muted 11/14 text.
fn caption(text_value: impl Into<SharedString>, cx: &App) -> gpui_kit::Div {
    div()
        .text_size(text::CAPTION)
        .line_height(text::CAPTION_LINE_HEIGHT)
        .text_color(cx.theme().muted_foreground)
        .child(text_value.into())
}

/// A running job inside a card: phase and cancel, the 4 px bar, tabular counters.
fn inline_progress(
    id: &'static str,
    progress: &Progress,
    on_cancel: OnClick,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let determinate = progress.total > 0;
    let counters = if determinate {
        rust_i18n::t!(
            "scan.progress.counted",
            done = format::count(progress.done),
            total = format::count(progress.total),
            bytes = format::bytes(progress.bytes)
        )
    } else {
        rust_i18n::t!(
            "scan.progress.found",
            items = format::count(progress.items),
            bytes = format::bytes(progress.bytes)
        )
    };
    v_flex()
        .w_full()
        .gap(space::MD)
        .p(card::PADDING)
        .child(
            h_flex()
                .w_full()
                .gap(space::MD)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(text::BODY)
                        .line_height(text::BODY_LINE_HEIGHT)
                        .font_weight(FontWeight::MEDIUM)
                        .child(phase_label(progress.phase)),
                )
                .child(
                    ui::Button::new(
                        SharedString::from(format!("{id}-cancel")),
                        tr("scan.cancel"),
                    )
                    .small()
                    .on_click(on_cancel),
                ),
        )
        .child(
            ui::ProgressBar::new(SharedString::from(format!("{id}-bar")))
                .value(determinate.then(|| widgets::fraction(progress.done, progress.total))),
        )
        .child(
            div()
                .text_size(text::SMALL)
                .line_height(text::SMALL_LINE_HEIGHT)
                .font_features(ui::tabular())
                .text_color(theme.muted_foreground)
                .child(counters.to_string()),
        )
        .into_any_element()
}

/// The detail header's identity: 40 px icon, name with its facts, version · publisher,
/// and the bundle identifier or location (muted, `~`, middle-truncated).
fn detail_title(app: &AppInfo, cx: &App) -> gpui_kit::Div {
    let theme = cx.theme();
    let (fg, muted) = (theme.foreground, theme.muted_foreground);
    let ident = app
        .ident
        .clone()
        .or_else(|| app.location.clone())
        .map(|s| format::tilde(&s));
    h_flex()
        .w_full()
        .items_start()
        .gap(space::LG)
        .child(app_icon(app, true, cx))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    h_flex()
                        .w_full()
                        .gap(space::SM)
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(text::SECTION)
                                .line_height(text::SECTION_LINE_HEIGHT)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(fg)
                                .child(app.name.clone()),
                        )
                        .children(app_facts(app, "app-detail")),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(text::SMALL)
                        .line_height(text::SMALL_LINE_HEIGHT)
                        .text_color(muted)
                        .child(app_subtitle(app)),
                )
                .when_some(ident, |this, ident| {
                    this.child(
                        caption(ident, cx)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_middle(),
                    )
                }),
        )
}

/// The app's own uninstaller: its command (monospace, middle-truncated, full text on
/// hover) or why there is none.
fn vendor_line(report: &AppFilesReport, cx: &App) -> gpui_kit::Div {
    let theme = cx.theme();
    let (muted, mono) = (theme.muted_foreground, theme.mono_font_family.clone());
    v_flex()
        .w_full()
        .gap(space::XXS)
        .child(
            div()
                .text_size(text::SMALL)
                .line_height(text::SMALL_LINE_HEIGHT)
                .font_weight(FontWeight::MEDIUM)
                .child(tr("uninstall.uninstaller")),
        )
        .child(match report.uninstaller.clone() {
            Some(command) => {
                let tip: SharedString = command.clone().into();
                div()
                    .id("app-uninstaller-command")
                    .w_full()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis_middle()
                    .text_size(text::CAPTION)
                    .line_height(text::CAPTION_LINE_HEIGHT)
                    .font_family(mono)
                    .text_color(muted)
                    .child(format::tilde(&command))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .into_any_element()
            }
            None => caption(tr("uninstall.no_uninstaller"), cx).into_any_element(),
        })
}

impl UninstallerPage {
    /// The list row's detail: version · publisher, plus the date the sort orders by.
    fn row_detail(&self, app: &AppInfo, now: i64) -> SharedString {
        let when: Option<SharedString> = match self.filter.sort {
            SortKey::LastUsed => Some(app.last_used.and_then(|t| format::age(t, now)).map_or_else(
                || tr("apps.never_used"),
                |age| {
                    rust_i18n::t!("apps.used_ago", age = age.as_ref())
                        .to_string()
                        .into()
                },
            )),
            SortKey::Installed => app.installed.and_then(|t| format::age(t, now)).map(|age| {
                rust_i18n::t!("apps.installed_ago", age = age.as_ref())
                    .to_string()
                    .into()
            }),
            SortKey::Name | SortKey::Size => None,
        };
        let subtitle = app_subtitle(app);
        match when {
            Some(when) if subtitle.is_empty() => when,
            Some(when) => format!("{subtitle} · {when}").into(),
            None => subtitle,
        }
    }

    /// Search, sort and source filter at the top of the list card.
    fn render_toolbar(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let sort = SortKey::ALL
            .iter()
            .fold(ui::Segmented::new("apps-sort").small(), |seg, key| {
                seg.segment(tr(&format!("apps.sort.{}", key.key())))
            })
            .selected(
                SortKey::ALL
                    .iter()
                    .position(|k| *k == self.filter.sort)
                    .unwrap_or(0),
            )
            .on_select(cx.listener(|this, ix: &usize, _, cx| {
                if let Some(key) = SortKey::ALL.get(*ix) {
                    this.filter.sort = *key;
                    this.refilter(cx);
                }
            }));
        let present = sources(&self.apps);
        let chips = (present.len() > 1).then(|| {
            std::iter::once(
                chip(
                    "apps-source-all",
                    tr("apps.source.all"),
                    self.filter.source.is_none(),
                    Box::new(cx.listener(|this, _, _, cx| {
                        this.filter.source = None;
                        this.refilter(cx);
                    })),
                )
                .into_any_element(),
            )
            .chain(present.into_iter().map(|source| {
                chip(
                    ("apps-source", source as usize),
                    tr(&format!("apps.source.{}", source_key(source))),
                    self.filter.source == Some(source),
                    Box::new(cx.listener(move |this, _, _, cx| {
                        this.filter.source = Some(source);
                        this.refilter(cx);
                    })),
                )
                .into_any_element()
            }))
            .collect::<Vec<_>>()
        });
        v_flex()
            .flex_none()
            .w_full()
            .gap(space::MD)
            .px(card::PADDING)
            .pt(card::PADDING)
            .pb(space::MD)
            .child(
                h_flex()
                    .w_full()
                    .gap(space::MD)
                    .child(ui::TextInput::new(&self.search).search().flex_1().min_w_0())
                    .child(sort),
            )
            .when_some(chips, |this, chips| {
                this.child(h_flex().flex_wrap().gap(space::XS).children(chips))
            })
            .into_any_element()
    }

    fn render_rows(
        &self,
        range: std::ops::Range<usize>,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        let now = format::now();
        let busy = self.run_active();
        let opened = self.detail.as_ref().map(|d| d.app.id);
        range
            .filter_map(|row| self.visible.get(row).and_then(|&ix| self.apps.get(ix)))
            .map(|app| {
                let id = app.id;
                let checkbox = ui::Checkbox::new(("app-check", id))
                    .checked(self.checked.contains(&id))
                    .disabled(app.system || busy)
                    .tooltip(tr(if app.system {
                        "apps.system_locked"
                    } else {
                        "apps.select"
                    }))
                    .on_click(cx.listener(move |this, on: &bool, _, cx| {
                        this.toggle_checked(id, *on, cx);
                    }));
                let size = app.bytes.map_or_else(|| "—".into(), format::bytes);
                app_facts(app, "app-row")
                    .into_iter()
                    .fold(
                        ui::ListRow::new(("app-row", id), app.name.clone())
                            .detail(self.row_detail(app, now))
                            .current(opened == Some(id))
                            .checkbox(checkbox)
                            .leading(app_icon(app, false, cx)),
                        ui::ListRow::trailing,
                    )
                    .trailing(size_cell(size, cx))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_detail(id, cx)))
                    .into_any_element()
            })
            .collect()
    }

    /// The left pane: toolbar, the app list (or its progress / no-match state) and the
    /// batch selection line.
    fn render_apps_card(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let body = if let Some(slot) = self.slots.first().filter(|s| s.is_active()) {
            inline_progress(
                "apps-list",
                &slot.progress,
                Box::new(cx.listener(|this, _, _, cx| slots::cancel(this, Slot::List, cx))),
                cx,
            )
        } else if self.visible.is_empty() {
            ui::EmptyState::new(IconName::Search, tr("apps.empty.title"))
                .description(tr("apps.empty.body"))
                .into_any_element()
        } else {
            uniform_list(
                "apps-list",
                self.visible.len(),
                cx.processor(|this, range, _, cx| this.render_rows(range, cx)),
            )
            .flex_1()
            .min_h_0()
            .w_full()
            .px(row::INSET)
            .pb(row::INSET)
            .into_any_element()
        };
        let selection = (!self.checked.is_empty()).then(|| {
            h_flex()
                .flex_none()
                .w_full()
                .gap(space::MD)
                .px(card::PADDING)
                .py(space::MD)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(text::SMALL)
                        .line_height(text::SMALL_LINE_HEIGHT)
                        .font_features(ui::tabular())
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            rust_i18n::t!("apps.selected", n = count(self.checked.len()))
                                .to_string(),
                        ),
                )
                .child(
                    ui::Button::new("apps-clear-selection", tr("apps.clear_selection"))
                        .small()
                        .ghost()
                        .disabled(self.run_active())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.checked.clear();
                            cx.notify();
                        })),
                )
        });
        ui::Card::new()
            .flush()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_toolbar(cx))
            .child(body)
            .children(selection)
            .into_any_element()
    }

    /// Detail of the opened app: header facts and actions, then its related items by kind.
    fn render_detail(&self, detail: &Detail, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = cx.theme();
        let hairline = theme.border.alpha(theme.border.a * card::BORDER_ALPHA);
        let app = &detail.app;
        let title = detail_title(app, cx).child(
            ui::IconButton::new("app-detail-close", IconName::Close, tr("uninstall.close"))
                .small()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_detail(cx);
                    cx.notify();
                })),
        );

        let report = detail.report.as_ref();
        let (picked_n, picked_bytes) = report.map_or((0, 0), |report| {
            report
                .items
                .iter()
                .filter(|i| detail.picked.contains(&i.id))
                .fold((0_usize, 0_u64), |(n, b), i| {
                    (n.saturating_add(1), b.saturating_add(i.bytes))
                })
        });
        let stats = h_flex()
            .w_full()
            .gap(space::XXL)
            .child(ui::Stat::new(
                tr("apps.sort.size"),
                app.bytes.map_or_else(|| "—".into(), format::bytes),
            ))
            .when_some(report, |this, _| {
                this.child(ui::Stat::new(
                    tr("uninstall.selected_size"),
                    format::bytes(picked_bytes),
                ))
            });
        let vendor = report.map(|report| vendor_line(report, cx));
        let actions = self.detail_actions(app, report, (picked_n, picked_bytes), cx);
        let header = v_flex()
            .flex_none()
            .w_full()
            .gap(space::LG)
            .p(card::PADDING)
            .border_b_1()
            .border_color(hairline)
            .child(title)
            .child(stats)
            .children(vendor)
            .child(actions);

        let mut body = v_flex().w_full().gap(space::XXS).p(row::INSET);
        if let Some(error) = detail.error.clone() {
            body = body.child(div().p(space::XS).child(error_notice(
                error,
                Box::new(cx.listener(|this, _, _, cx| {
                    if let Some(detail) = &mut this.detail {
                        detail.error = None;
                    }
                    cx.notify();
                })),
                cx,
            )));
        } else if let Some(slot) = self.slots.get(1).filter(|s| s.is_active()) {
            body = body.child(inline_progress(
                "app-files",
                &slot.progress,
                Box::new(cx.listener(|this, _, _, cx| slots::cancel(this, Slot::Files, cx))),
                cx,
            ));
        }
        if let Some(report) = report {
            if report.items.iter().any(|i| i.confidence < Confidence::High) {
                body = body.child(
                    caption(tr("uninstall.review_hint"), cx)
                        .px(row::PAD_X)
                        .py(space::MD),
                );
            }
            let kinds: BTreeSet<AppFileKind> = report.items.iter().map(|i| i.kind).collect();
            for kind in kinds {
                body = body.child(Self::render_kind_group(detail, report, kind, cx));
            }
        }
        ui::Card::new()
            .flush()
            .h_full()
            .child(header)
            .child(
                div()
                    .id("app-detail-items")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .child(body),
            )
            .into_any_element()
    }

    /// Uninstall (primary), the batch button when apps are checked, and the selection
    /// totals.
    fn detail_actions(
        &self,
        app: &AppInfo,
        report: Option<&AppFilesReport>,
        (picked_n, picked_bytes): (usize, u64),
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let connected = self.conn.is_connected();
        let totals = report.map(|report| {
            rust_i18n::t!(
                "uninstall.totals",
                selected = count(picked_n),
                all = count(report.items.len()),
                bytes = format::bytes(picked_bytes)
            )
            .to_string()
        });
        h_flex()
            .w_full()
            .gap(space::MD)
            .child(
                ui::Button::new("app-uninstall", tr("uninstall.button"))
                    .primary()
                    .disabled(app.system || picked_n == 0 || !connected || report.is_none())
                    .when(app.system, |b| b.tooltip(tr("apps.system_locked")))
                    .on_click(cx.listener(|this, _, window, cx| this.confirm(false, window, cx))),
            )
            .when(!self.checked.is_empty(), |this| {
                this.child(self.batch_button(false, cx))
            })
            .child(div().flex_1())
            .when_some(totals, |this, totals| {
                this.child(caption(totals, cx).flex_none().font_features(ui::tabular()))
            })
            .into_any_element()
    }

    /// "Uninstall N selected": primary when it is the pane's only action.
    fn batch_button(&self, primary: bool, cx: &mut Context<'_, Self>) -> ui::Button {
        ui::Button::new(
            "apps-uninstall-selected",
            rust_i18n::t!("uninstall.batch", n = count(self.checked.len())).to_string(),
        )
        .variant(if primary {
            ui::ButtonVariant::Primary
        } else {
            ui::ButtonVariant::Outline
        })
        .disabled(!self.conn.is_connected())
        .on_click(cx.listener(|this, _, window, cx| this.confirm(true, window, cx)))
    }

    /// The items of one kind: a whole-row collapsible header with the group checkbox,
    /// "n of m selected" and size, then one 40 px row per item.
    fn render_kind_group(
        detail: &Detail,
        report: &AppFilesReport,
        kind: AppFileKind,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let items: Vec<&AppFile> = report.items.iter().filter(|i| i.kind == kind).collect();
        let bytes = items
            .iter()
            .fold(0_u64, |acc, i| acc.saturating_add(i.bytes));
        let total = items.len();
        let chosen = items
            .iter()
            .filter(|i| detail.picked.contains(&i.id))
            .count();
        let open = !detail.collapsed.contains(&kind);
        let rows = open.then(|| {
            items
                .into_iter()
                .map(|item| {
                    let id = item.id;
                    let picked = detail.picked.contains(&id);
                    ui::ListRow::new(("app-item-row", id), item_title(&item.location))
                        .detail(item_detail(&item.location))
                        .grid(DETAIL_GRID)
                        .checkbox(
                            ui::Checkbox::new(("app-item", id))
                                .checked(picked)
                                .on_click(cx.listener(move |this, on: &bool, _, cx| {
                                    this.toggle_item(id, *on, cx);
                                })),
                        )
                        .when(item.needs_admin, |row| {
                            row.trailing(ui::FactIcon::new(
                                ("app-item-admin", id),
                                Icon::new(AssetIcon::Lock),
                                tr("apps.badge.admin"),
                            ))
                        })
                        .trailing(
                            ui::Badge::new(confidence_label(item.confidence))
                                .tone(confidence_tone(item.confidence)),
                        )
                        .trailing(size_cell(format::bytes(item.bytes), cx))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_item(id, !picked, cx);
                        }))
                })
                .collect::<Vec<_>>()
        });
        let key = kind_key(kind);
        let header = ui::CollapsibleHeader::new(
            SharedString::from(format!("app-kind-{key}")),
            tr(&format!("apps.kind.{key}")),
            open,
        )
        .grid(DETAIL_GRID)
        .checkbox(
            ui::Checkbox::new(SharedString::from(format!("app-kind-check-{key}")))
                .state(ui::CheckState::from_counts(chosen, total))
                .on_click(cx.listener(move |this, on: &bool, _, cx| {
                    this.pick_kind(kind, *on, cx);
                })),
        )
        .summary(ui::selected_of(chosen, total))
        .size_label(format::bytes(bytes))
        .on_toggle(cx.listener(move |this, open: &bool, _, cx| {
            this.set_kind_open(kind, *open, cx);
        }));
        v_flex()
            .w_full()
            .child(header)
            .children(rows.into_iter().flatten())
            .into_any_element()
    }

    /// The right pane with nothing opened: the checked apps as a batch, or a prompt.
    fn render_batch(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        if self.checked.is_empty() {
            return ui::Card::new()
                .h_full()
                .child(
                    v_flex().flex_1().justify_center().child(
                        ui::EmptyState::new(Icon::new(AssetIcon::AppWindow), tr("apps.pick.title"))
                            .description(tr("apps.pick.body")),
                    ),
                )
                .into_any_element();
        }
        let rows = self
            .apps
            .iter()
            .filter(|a| self.checked.contains(&a.id))
            .map(|app| {
                ui::ListRow::new(("apps-batch-row", app.id), app.name.clone())
                    .detail(app_subtitle(app))
                    .leading(app_icon(app, false, cx))
                    .trailing(size_cell(
                        app.bytes.map_or_else(|| "—".into(), format::bytes),
                        cx,
                    ))
            })
            .collect::<Vec<_>>();
        ui::Card::new()
            .flush()
            .h_full()
            .header(
                ui::CardHeader::new(
                    rust_i18n::t!("apps.selected", n = count(self.checked.len())).to_string(),
                )
                .description(tr("apps.batch_body")),
            )
            .child(
                div()
                    .id("apps-batch-list")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .child(v_flex().w_full().p(space::XS).children(rows)),
            )
            .child(
                h_flex()
                    .flex_none()
                    .w_full()
                    .gap(space::MD)
                    .p(card::PADDING)
                    .child(self.batch_button(true, cx))
                    .child(
                        ui::Button::new("apps-batch-clear", tr("apps.clear_selection"))
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.checked.clear();
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    /// A run: title, the current app's steps, every app's status, and when finished the
    /// result report.
    fn render_run(&self, run: &Run, cx: &mut Context<'_, Self>) -> AnyElement {
        let title: SharedString = if run.finished() {
            tr("uninstall.finished_title")
        } else if let [one] = run.apps.as_slice() {
            rust_i18n::t!("uninstall.running_title", name = one.app.name.as_str())
                .to_string()
                .into()
        } else {
            rust_i18n::t!(
                "uninstall.batch_title",
                done = count(run.done_count()),
                n = count(run.apps.len())
            )
            .to_string()
            .into()
        };
        let current = run
            .current
            .and_then(|ix| run.apps.get(ix))
            .map(|current| self.render_steps(current, cx));
        let apps = (run.apps.len() > 1 || run.finished()).then(|| {
            v_flex()
                .w_full()
                .children(run.apps.iter().map(|app| run_app_row(app, cx)))
        });
        let card = ui::Card::new()
            .flex_none()
            .header(ui::CardHeader::new(title))
            .children(current)
            .children(apps);
        let report = run.finished().then(|| {
            let reports = run.apps.iter().filter_map(|a| match &a.status {
                RunStatus::Done(report) => Some(report.clean.clone()),
                _ => None,
            });
            let merged = merged_clean(reports);
            v_flex()
                .w_full()
                .gap(layout::BLOCK_GAP)
                .child(clean_report(
                    &merged,
                    Box::new(cx.listener(|this, _, _, cx| {
                        this.run = None;
                        cx.notify();
                    })),
                    Box::new(cx.listener(|this, _, _, cx| {
                        this.run = None;
                        this.load(cx);
                    })),
                    cx,
                ))
                .child(notice(
                    Tone::Info,
                    tr("uninstall.leftovers_hint"),
                    None,
                    Vec::new(),
                    cx,
                ))
        });
        v_flex()
            .id("uninstall-run")
            .size_full()
            .overflow_y_scroll()
            .gap(layout::BLOCK_GAP)
            .when(run.interrupted, |this| {
                this.child(notice(
                    Tone::Warning,
                    tr("uninstall.restarted"),
                    None,
                    Vec::new(),
                    cx,
                ))
            })
            .child(card)
            .children(report)
            .into_any_element()
    }

    /// The stepper of the app being uninstalled: Quit → Uninstaller → Admin → Remove, the
    /// current step highlighted with its live sentence and progress bar.
    fn render_steps(&self, current: &RunApp, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let finding = matches!(current.status, RunStatus::Finding);
        let slot = if finding {
            self.slots.get(1)
        } else {
            self.slots.get(2)
        };
        let progress = slot.map(|s| &s.progress);
        let phase = progress.map(|p| p.phase);
        let active = if finding {
            None
        } else {
            phase.and_then(step_of)
        };
        let sentence = if finding {
            tr("uninstall.phase.finding")
        } else {
            phase.map_or_else(|| tr("uninstall.phase.starting"), phase_text)
        };
        let bar = ui::ProgressBar::new("uninstall-progress").value(
            progress
                .filter(|p| p.total > 0)
                .map(|p| widgets::fraction(p.done, p.total)),
        );
        let live = v_flex()
            .w_full()
            .gap(space::SM)
            .child(caption(sentence, cx))
            .child(bar);
        let mut live = Some(live);
        let steps = STEPS.iter().enumerate().map(|(ix, key)| {
            let done = active.is_some_and(|a| ix < a);
            let now = active == Some(ix);
            let marker = div()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .size(control::ICON)
                .map(|this| {
                    if done {
                        this.text_color(ui::Tone::Success.color(cx))
                            .child(Icon::new(IconName::Check).size(control::ICON))
                    } else if now {
                        // Spins while the step runs; static under reduced motion.
                        this.child(
                            Spinner::new()
                                .color(ui::Tone::Accent.color(cx))
                                .with_size(control::ICON),
                        )
                    } else {
                        this.child(ui::Dot::new(ui::Tone::Neutral))
                    }
                });
            v_flex()
                .w_full()
                .gap(space::XS)
                .child(
                    h_flex().h(row::HEIGHT).gap(space::MD).child(marker).child(
                        div()
                            .text_size(text::BODY)
                            .line_height(text::BODY_LINE_HEIGHT)
                            .when(now, |this| this.font_weight(FontWeight::MEDIUM))
                            .text_color(if now { fg } else { muted })
                            .child(tr(key)),
                    ),
                )
                .when(now, |this| {
                    this.children(live.take().map(|live| live.pl(space::XXL)))
                })
        });
        let steps: Vec<_> = steps.collect();
        v_flex()
            .w_full()
            .gap(space::XS)
            .child(
                h_flex()
                    .w_full()
                    .gap(space::MD)
                    .child(app_icon(&current.app, false, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(text::BODY)
                            .line_height(text::BODY_LINE_HEIGHT)
                            .font_weight(FontWeight::MEDIUM)
                            .child(current.app.name.clone()),
                    ),
            )
            // Before a step is known (finding files, preparing) the live line leads.
            .children(live.take())
            .children(steps)
            .into_any_element()
    }
}

/// One app of a run: icon, name over its outcome, one status badge.
fn run_app_row(app: &RunApp, cx: &App) -> AnyElement {
    let (label, tone) = match &app.status {
        RunStatus::Pending => (tr("uninstall.status.pending"), ui::Tone::Neutral),
        RunStatus::Finding => (tr("uninstall.status.finding"), ui::Tone::Accent),
        RunStatus::Uninstalling => (tr("uninstall.status.running"), ui::Tone::Accent),
        RunStatus::Done(report) if report.clean.failures.is_empty() => {
            (tr("uninstall.status.done"), ui::Tone::Success)
        }
        RunStatus::Done(_) => (tr("uninstall.status.done"), ui::Tone::Warning),
        RunStatus::Failed(_) => (tr("uninstall.status.failed"), ui::Tone::Danger),
    };
    let detail: Option<SharedString> = match &app.status {
        RunStatus::Done(report) => Some(
            format!(
                "{} · {}",
                format::bytes(report.clean.freed),
                outcome_text(&report.uninstaller)
            )
            .into(),
        ),
        RunStatus::Failed(message) => Some(message.clone()),
        _ => None,
    };
    ui::ListRow::new(("uninstall-run-row", app.app.id), app.app.name.clone())
        .when_some(detail, ui::ListRow::detail)
        .leading(app_icon(&app.app, false, cx))
        .trailing(ui::Badge::new(label).tone(tone))
        .into_any_element()
}

impl Render for UninstallerPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let connected = self.conn.is_connected();
        let stale = if self.list_busy() {
            None
        } else {
            scans::stale_age(Area::AppList.policy(), self.loaded_at, Instant::now())
        };
        let stale = super::stale_notice(
            "apps-rescan",
            stale,
            connected && !self.run_active(),
            Box::new(cx.listener(|this, _, _, cx| this.load(cx))),
            cx,
        );
        let refresh = ui::Button::new("apps-refresh", tr("apps.refresh"))
            .icon(IconName::RefreshCw)
            .disabled(!connected || self.run_active() || self.list_busy())
            .on_click(cx.listener(|this, _, _, cx| this.load(cx)))
            .into_any_element();
        let error = self.list_error.clone().map(|error| {
            error_notice(
                error,
                Box::new(cx.listener(|this, _, _, cx| {
                    this.list_error = None;
                    cx.notify();
                })),
                cx,
            )
        });
        let content = if !self.loaded && self.run.is_none() {
            // First load: one full-width card (progress or the idle prompt).
            match self.slots.first().filter(|s| s.is_active()) {
                Some(slot) => job_progress(
                    "apps-list",
                    &slot.progress,
                    Box::new(cx.listener(|this, _, _, cx| slots::cancel(this, Slot::List, cx))),
                    cx,
                ),
                None => ui::Card::new()
                    .flex_none()
                    .child(
                        ui::EmptyState::new(IconName::Inbox, tr("apps.idle.title"))
                            .description(tr("apps.idle.body"))
                            .action(
                                ui::Button::new("apps-load", tr("apps.load"))
                                    .primary()
                                    .large()
                                    .icon(IconName::RefreshCw)
                                    .disabled(!connected)
                                    .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                            ),
                    )
                    .into_any_element(),
            }
        } else {
            let side = match (&self.run, &self.detail) {
                (Some(run), _) => self.render_run(run, cx),
                (None, Some(detail)) => self.render_detail(detail, cx),
                (None, None) => self.render_batch(cx),
            };
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .items_start()
                .gap(layout::BLOCK_GAP)
                .child(self.render_apps_card(cx))
                .child(
                    div()
                        .flex_none()
                        .w(DETAIL_WIDTH)
                        .h_full()
                        .flex()
                        .flex_col()
                        .child(side),
                )
                .into_any_element()
        };
        page_column("page-uninstaller")
            .child(area_header(Category::Uninstaller, vec![refresh], cx))
            .children(connection_notice(connected, cx))
            .children(stale)
            .children(error)
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use omc_proto::apps::{
        AppFile, AppFileKind, AppFilesReport, AppInfo, AppSource, Confidence, UninstallReport,
    };
    use omc_proto::jobs::{Location, Phase, SpecialAction};

    use gpui_kit::TestAppContext;
    use gpui_kit::component::WindowExt as _;

    use super::{
        AppFilter, Detail, Run, RunApp, RunStatus, SortKey, UninstallerPage, item_detail,
        item_title, leaf, preselect, sources, visible,
    };
    use crate::pages::widgets::test_support::open_page;

    fn app(id: u32, name: &str, source: AppSource) -> AppInfo {
        AppInfo {
            id,
            name: name.to_owned(),
            version: None,
            publisher: None,
            ident: None,
            location: None,
            bytes: None,
            source,
            system: false,
            running: false,
            last_used: None,
            installed: None,
            icon: None,
            needs_admin: false,
        }
    }

    fn fixture() -> Vec<AppInfo> {
        let mut zed = app(0, "Zed", AppSource::MacBundle);
        zed.bytes = Some(300);
        zed.last_used = Some(50);
        zed.installed = Some(10);
        zed.publisher = Some("Zed Industries".to_owned());
        let mut arc = app(1, "arc", AppSource::Homebrew);
        arc.bytes = Some(900);
        arc.last_used = Some(20);
        arc.ident = Some("company.thebrowser.Browser".to_owned());
        let mut safari = app(2, "Safari", AppSource::MacBundle);
        safari.system = true;
        safari.bytes = Some(100);
        let mut notes = app(3, "Notes", AppSource::MacAppStore);
        notes.installed = Some(99);
        vec![zed, arc, safari, notes]
    }

    fn names(apps: &[AppInfo], filter: &AppFilter) -> Vec<String> {
        visible(apps, filter)
            .into_iter()
            .filter_map(|ix| apps.get(ix).map(|a| a.name.clone()))
            .collect()
    }

    #[test]
    fn system_apps_show_only_when_enabled() {
        let apps = fixture();
        let mut filter = AppFilter::default();
        assert_eq!(
            names(&apps, &filter),
            ["arc", "Notes", "Zed"],
            "case-insensitive A→Z, no system"
        );
        filter.show_system = true;
        assert_eq!(
            names(&apps, &filter),
            ["arc", "Notes", "Safari", "Zed"],
            "system apps listed"
        );
    }

    #[test]
    fn search_matches_name_publisher_and_identifier() {
        let apps = fixture();
        let mut filter = AppFilter {
            query: "  INDUSTRIES ".to_owned(),
            ..AppFilter::default()
        };
        assert_eq!(
            names(&apps, &filter),
            ["Zed"],
            "publisher, trimmed, case-insensitive"
        );
        filter.query = "thebrowser".to_owned();
        assert_eq!(names(&apps, &filter), ["arc"], "identifier");
        filter.query = "nothing".to_owned();
        assert!(names(&apps, &filter).is_empty(), "no match");
    }

    #[test]
    fn sorts_put_unknown_values_last() {
        let apps = fixture();
        let mut filter = AppFilter {
            sort: SortKey::Size,
            ..AppFilter::default()
        };
        assert_eq!(
            names(&apps, &filter),
            ["arc", "Zed", "Notes"],
            "largest first, unknown last"
        );
        filter.sort = SortKey::LastUsed;
        assert_eq!(
            names(&apps, &filter),
            ["arc", "Zed", "Notes"],
            "least recently used first"
        );
        filter.sort = SortKey::Installed;
        assert_eq!(
            names(&apps, &filter),
            ["Notes", "Zed", "arc"],
            "newest install first"
        );
    }

    #[test]
    fn source_filter_and_present_sources() {
        let apps = fixture();
        let filter = AppFilter {
            source: Some(AppSource::MacBundle),
            show_system: true,
            ..AppFilter::default()
        };
        assert_eq!(names(&apps, &filter), ["Safari", "Zed"], "only that source");
        assert_eq!(
            sources(&apps),
            [
                AppSource::MacBundle,
                AppSource::MacAppStore,
                AppSource::Homebrew
            ],
            "present sources in stable order"
        );
    }

    #[test]
    fn preselection_follows_the_confidence_setting() {
        let item = |id: u32, confidence: Confidence| AppFile {
            id,
            location: Location::Path {
                path: format!("/x/{id}"),
            },
            kind: AppFileKind::Support,
            bytes: 1,
            confidence,
            needs_admin: false,
        };
        let items = [
            item(0, Confidence::High),
            item(1, Confidence::Medium),
            item(2, Confidence::Low),
        ];
        let ids = |min| preselect(&items, min).into_iter().collect::<Vec<_>>();
        assert_eq!(
            ids(Confidence::High),
            [0],
            "only certain matches by default"
        );
        assert_eq!(
            ids(Confidence::Medium),
            [0, 1],
            "medium adds vendor-folder matches"
        );
        assert_eq!(ids(Confidence::Low), [0, 1, 2], "low selects everything");
    }

    #[test]
    fn leaves_are_last_components() {
        assert_eq!(leaf("/a/b/c.plist"), "c.plist", "file name");
        assert_eq!(leaf(r"HKCU\Software\Vendor\"), "Vendor", "registry key");
    }

    #[test]
    fn item_titles_name_the_place_when_the_leaf_alone_does_not() {
        let path = |p: &str| item_title(&Location::Path { path: p.to_owned() });
        assert_eq!(
            path("/Users/me/.config/ghostty"),
            "ghostty",
            "a plain folder name stands alone"
        );
        assert_eq!(path("/Applications/Ghostty.app"), "Ghostty.app", "bundle");
        assert_eq!(path("/Users/me/.ghostty/"), ".ghostty", "home dot folder");
        assert_eq!(
            path("/Users/me/Library/Application Support/com.mitchellh.ghostty"),
            "Application Support › com.mitchellh.ghostty",
            "an identifier gets its folder"
        );
        assert_eq!(
            path("/Users/me/Library/Preferences/com.mitchellh.ghostty.plist"),
            "Preferences › com.mitchellh.ghostty.plist",
            "a plist gets its folder"
        );
        assert_eq!(
            path(r"C:\Users\me\AppData\Local\Vendor\User Data"),
            "Vendor › User Data",
            "generic names get their folder (Windows separators)"
        );
        assert_eq!(
            path(r"C:\com.vendor.app"),
            "com.vendor.app",
            "a drive root is no folder name"
        );
        assert_eq!(
            path("/Users/me/Library/Caches/Ghostty.v2"),
            "Ghostty.v2",
            "two runs are not an identifier"
        );
        assert_eq!(
            item_title(&Location::RegistryValue {
                key: r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run".to_owned(),
                name: "Ghostty".to_owned(),
            }),
            "Ghostty",
            "registry values by name"
        );
        assert_eq!(
            item_title(&Location::RegistryValue {
                key: r"HKCU\Software\Ghostty".to_owned(),
                name: String::new(),
            }),
            "Ghostty",
            "the default value by its key"
        );
        let job = Location::Special {
            action: SpecialAction::UnloadLaunchJob {
                label: "com.citrolabs.keystone.agent".to_owned(),
                plist: "/Library/LaunchAgents/com.citrolabs.keystone.agent.plist".to_owned(),
            },
        };
        assert_eq!(
            item_title(&job),
            "LaunchAgents › com.citrolabs.keystone.agent.plist",
            "a launch job by its plist"
        );
        assert_eq!(
            item_detail(&job).as_ref(),
            "/Library/LaunchAgents/com.citrolabs.keystone.agent.plist",
            "the detail shows the plist, not the action"
        );
        assert_eq!(
            item_title(&Location::Special {
                action: SpecialAction::DeleteService {
                    name: "GhosttySvc".to_owned(),
                },
            }),
            "GhosttySvc",
            "a service by name"
        );
    }

    #[gpui_kit::test]
    fn the_page_renders_its_disconnected_state_without_a_daemon(cx: &mut TestAppContext) {
        let opened = open_page(cx, UninstallerPage::new);
        let Some((_, page)) = opened else { return };
        let (connected, loaded, busy) = cx.update(|cx| {
            let page = page.read(cx);
            (page.conn.is_connected(), page.loaded, page.list_busy())
        });
        assert!(!connected, "no daemon in tests");
        assert!(!loaded && !busy, "nothing is requested while disconnected");
    }

    #[gpui_kit::test]
    fn the_page_renders_list_detail_and_run_states(cx: &mut TestAppContext) {
        let Some((window, page)) = open_page(cx, UninstallerPage::new) else {
            return;
        };
        let item = |id: u32, kind: AppFileKind, confidence: Confidence| AppFile {
            id,
            location: Location::Path {
                path: format!("/Users/x/Library/{id}"),
            },
            kind,
            bytes: 10,
            confidence,
            needs_admin: id == 1,
        };
        let mut apps = fixture();
        if let Some(zed) = apps.first_mut() {
            zed.running = true;
            zed.needs_admin = true;
            zed.icon = Some("/nonexistent/icon.png".to_owned());
        }
        let zed = apps.first().cloned();
        page.update(cx, |page, cx| {
            page.apps = apps;
            page.loaded = true;
            page.apps_job = Some(1);
            page.checked.insert(1);
            page.refilter(cx);
        });
        cx.run_until_parked();
        let Some(zed) = zed else { return };
        let report = AppFilesReport {
            app: zed.clone(),
            uninstaller: Some("/Applications/Zed.app/uninstall --quiet".to_owned()),
            items: vec![
                item(0, AppFileKind::Bundle, Confidence::High),
                item(1, AppFileKind::Support, Confidence::Medium),
                item(2, AppFileKind::Cache, Confidence::Low),
            ],
        };
        page.update(cx, |page, cx| {
            page.detail = Some(Detail {
                app: zed.clone(),
                picked: BTreeSet::from([0]),
                report: Some(report),
                files_job: Some(2),
                collapsed: BTreeSet::from([AppFileKind::Cache]),
                error: None,
            });
            cx.notify();
        });
        cx.run_until_parked();
        let asked = window.update(cx, |_, window, cx| {
            page.update(cx, |page, cx| page.confirm(false, window, cx));
            window.has_active_dialog(cx)
        });
        assert_eq!(asked.ok(), Some(true), "uninstalling asks first");

        page.update(cx, |page, cx| {
            page.detail = None;
            if let Some(slot) = page.slots.get_mut(2) {
                slot.progress.phase = Phase::Elevating;
            }
            let run_app = |status| RunApp {
                app: zed.clone(),
                items: None,
                files_job: None,
                status,
            };
            page.run = Some(Run {
                apps: vec![
                    run_app(RunStatus::Done(UninstallReport::default())),
                    run_app(RunStatus::Uninstalling),
                    run_app(RunStatus::Pending),
                ],
                current: Some(1),
                vendor: true,
                interrupted: true,
            });
            cx.notify();
        });
        cx.run_until_parked();
        page.update(cx, |page, cx| {
            if let Some(run) = &mut page.run {
                run.current = None;
            }
            cx.notify();
        });
        cx.run_until_parked();
        let finished = page.read_with(cx, |page, _| page.run.as_ref().is_some_and(Run::finished));
        assert!(finished, "the finished run stays on screen with its report");
    }
}
