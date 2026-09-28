//! Shared scan results: ONE [`Flow`] (running job, progress, retained output, finish time)
//! per scan area, owned by the [`Scans`] entity in a GPUI global. The overview and the area
//! pages are views onto it, so a smart scan started on the overview shows its live progress
//! on the area page and its results the moment they arrive; a clean from either place
//! updates the one result.
//!
//! - [`Area::policy`]: which areas scan by themselves when opened and how long a result
//!   stays fresh ([`on_show`] turns that into a decision).
//! - Views subscribe to [`ScanEvent`]s to (re)build their models and observe the entity to
//!   re-render on progress.
//! - A new daemon epoch drops every entry (job ids are meaningless there).

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, Global, SharedString, Subscription,
};
use omc_ipc::client::{ClientEvent, ConnState};
use omc_proto::jobs::{CleanReport, ItemId, JobOutput, JobSpec, ScanArea};
use omc_proto::junk::{JunkKind, JunkReport, Safety};

use crate::clean_settings::{self, CleanPrefs};
use crate::engine;
use crate::nav::Category;
use crate::pages::widgets::flow::FlowView;
use crate::pages::widgets::{Connection, Flow, FlowHost, FlowPhase, Throttle};

/// A place whose results can go stale: the store's scan areas plus the lists the
/// uninstaller and startup pages load themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Area {
    SystemJunk,
    BrowserData,
    DeveloperJunk,
    Trash,
    Installers,
    Leftovers,
    LargeOldFiles,
    Duplicates,
    /// Fresh per measured root.
    SpaceLens,
    /// The uninstaller's app list (owned by its page).
    AppList,
    /// The startup items list (owned by its page).
    StartupItems,
}

/// When an area scans by itself and how long its result stays fresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Policy {
    /// Opening the area starts a scan when the result is missing or stale (cheap scans).
    pub(crate) auto_scan: bool,
    /// Age below which a result is shown as is.
    pub(crate) fresh_for: Duration,
}

impl Area {
    /// Areas the store keeps, in slot order.
    pub(crate) const STORED: [Self; 9] = [
        Self::SystemJunk,
        Self::BrowserData,
        Self::DeveloperJunk,
        Self::Trash,
        Self::Installers,
        Self::Leftovers,
        Self::LargeOldFiles,
        Self::Duplicates,
        Self::SpaceLens,
    ];

    /// Freshness and auto-scan policy. Walking project folders, whole disks or hashing
    /// files is too heavy to start unasked.
    pub(crate) const fn policy(self) -> Policy {
        let (auto_scan, secs) = match self {
            Self::Trash => (true, 120),
            Self::BrowserData | Self::StartupItems => (true, 300),
            Self::SystemJunk | Self::AppList => (true, 600),
            Self::Installers | Self::Leftovers => (true, 1_800),
            Self::DeveloperJunk => (false, 1_800),
            Self::LargeOldFiles | Self::Duplicates | Self::SpaceLens => (false, 86_400),
        };
        Policy {
            auto_scan,
            fresh_for: Duration::from_secs(secs),
        }
    }

    /// The area a sidebar entry shows.
    pub(crate) const fn of(category: Category) -> Option<Self> {
        Some(match category {
            Category::SystemJunk => Self::SystemJunk,
            Category::BrowserData => Self::BrowserData,
            Category::DeveloperJunk => Self::DeveloperJunk,
            Category::Trash => Self::Trash,
            Category::Installers => Self::Installers,
            Category::Leftovers => Self::Leftovers,
            Category::LargeOldFiles => Self::LargeOldFiles,
            Category::Duplicates => Self::Duplicates,
            Category::SpaceLens => Self::SpaceLens,
            Category::Uninstaller => Self::AppList,
            Category::StartupItems => Self::StartupItems,
            Category::Overview => return None,
        })
    }

    /// The scan of an area that needs no parameters (the space lens needs a root).
    pub(crate) const fn default_scan(self) -> Option<ScanArea> {
        Some(match self {
            Self::SystemJunk => ScanArea::SystemJunk,
            Self::BrowserData => ScanArea::BrowserData,
            Self::DeveloperJunk => ScanArea::DeveloperJunk,
            Self::Trash => ScanArea::Trash,
            Self::Installers => ScanArea::Installers,
            Self::Leftovers => ScanArea::Leftovers,
            Self::LargeOldFiles => ScanArea::LargeOldFiles,
            Self::Duplicates => ScanArea::Duplicates,
            Self::SpaceLens | Self::AppList | Self::StartupItems => return None,
        })
    }

    /// Index into the store's entries.
    const fn slot(self) -> Option<usize> {
        Some(match self {
            Self::SystemJunk => 0,
            Self::BrowserData => 1,
            Self::DeveloperJunk => 2,
            Self::Trash => 3,
            Self::Installers => 4,
            Self::Leftovers => 5,
            Self::LargeOldFiles => 6,
            Self::Duplicates => 7,
            Self::SpaceLens => 8,
            Self::AppList | Self::StartupItems => return None,
        })
    }
}

/// What opening an area does with its current result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnShow {
    /// Fresh result: show it, no rescan.
    Fresh,
    /// Missing or stale, and the area scans by itself: start a scan.
    Scan,
    /// Stale, manual: keep showing it with a "scanned N ago · Rescan" notice.
    Stale,
    /// Nothing yet, manual: the idle state with the scan button.
    Idle,
}

/// Decides what opening an area does. `age` is the time since the result was produced
/// (a clean counts as producing it); `auto_enabled` is the user's "scan when opening"
/// preference.
pub(crate) fn on_show(policy: Policy, age: Option<Duration>, auto_enabled: bool) -> OnShow {
    match age {
        Some(age) if age < policy.fresh_for => OnShow::Fresh,
        _ if policy.auto_scan && auto_enabled => OnShow::Scan,
        Some(_) => OnShow::Stale,
        None => OnShow::Idle,
    }
}

/// Age of a result for a stale notice: `Some` only when it is past `policy.fresh_for`.
pub(crate) fn stale_age(
    policy: Policy,
    finished_at: Option<Instant>,
    now: Instant,
) -> Option<Duration> {
    let age = now.checked_duration_since(finished_at?)?;
    (age >= policy.fresh_for).then_some(age)
}

/// "Scanned 12 min ago"-style text.
pub(crate) fn scanned_ago(age: Duration) -> SharedString {
    let minutes = age.as_secs() / 60;
    let (hours, days) = (minutes / 60, minutes / 1_440);
    let text = if minutes < 1 {
        rust_i18n::t!("scans.age.just_now").to_string()
    } else if hours < 1 {
        rust_i18n::t!("scans.age.minutes", n = minutes).to_string()
    } else if days < 1 {
        rust_i18n::t!("scans.age.hours", n = hours).to_string()
    } else {
        rust_i18n::t!("scans.age.days", n = days).to_string()
    };
    text.into()
}

/// Names a [`SafeGroup`] keeps for the overview's breakdown.
pub(crate) const SAFE_NAMES: usize = 3;

/// The safe items of one junk group, for the overview's "what gets cleaned" breakdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SafeGroup {
    /// What the items are.
    pub(crate) kind: JunkKind,
    /// Safe items in the group.
    pub(crate) count: usize,
    /// Their size.
    pub(crate) bytes: u64,
    /// Distinct names of the largest safe items (apps, profiles, projects), at most
    /// [`SAFE_NAMES`].
    pub(crate) names: Vec<String>,
}

impl SafeGroup {
    /// Whether some safe items are not covered by [`Self::names`] (a name may also stand
    /// for several items, e.g. one app's caches in two places).
    pub(crate) fn has_more(&self) -> bool {
        self.count > self.names.len()
    }
}

/// What one finished junk scan offers the overview.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AreaSummary {
    /// Everything found.
    pub(crate) total: u64,
    /// Items found.
    pub(crate) items: usize,
    /// Safe items (preselected on the area page) with their sizes.
    pub(crate) safe: Vec<(ItemId, u64)>,
    /// The safe items per group, in report order (largest group first).
    pub(crate) safe_groups: Vec<SafeGroup>,
}

impl AreaSummary {
    /// Summarises a junk report: safe = [`Safety::Safe`] and the owning app not running.
    pub(crate) fn new(report: &JunkReport) -> Self {
        let mut summary = Self::default();
        for group in &report.groups {
            let mut safe = SafeGroup {
                kind: group.kind,
                count: 0,
                bytes: 0,
                names: Vec::new(),
            };
            for item in &group.items {
                summary.total = summary.total.saturating_add(item.bytes);
                summary.items = summary.items.saturating_add(1);
                if item.safety != Safety::Safe || item.app_running {
                    continue;
                }
                summary.safe.push((item.id, item.bytes));
                safe.count = safe.count.saturating_add(1);
                safe.bytes = safe.bytes.saturating_add(item.bytes);
                let name = item.name.trim();
                if safe.names.len() < SAFE_NAMES
                    && !name.is_empty()
                    && !safe.names.iter().any(|n| n == name)
                {
                    safe.names.push(name.to_owned());
                }
            }
            if safe.count > 0 {
                summary.safe_groups.push(safe);
            }
        }
        summary
    }

    /// Size of the safe items.
    pub(crate) fn safe_bytes(&self) -> u64 {
        self.safe
            .iter()
            .fold(0_u64, |sum, (_, bytes)| sum.saturating_add(*bytes))
    }
}

/// Drops the `cleaned` items from a retained scan output, except those whose location
/// failed (they are still there).
pub(crate) fn prune(output: &mut JobOutput, cleaned: &[ItemId], report: &CleanReport) {
    let failed: HashSet<String> = report
        .failures
        .iter()
        .map(|f| f.location.display())
        .collect();
    let cleaned: HashSet<ItemId> = cleaned.iter().copied().collect();
    let gone = |id: ItemId, path: &str| cleaned.contains(&id) && !failed.contains(path);
    match output {
        JobOutput::Junk(junk) => {
            for group in &mut junk.groups {
                group
                    .items
                    .retain(|item| !gone(item.id, &item.location.display()));
            }
            junk.groups.retain(|g| !g.items.is_empty());
        }
        JobOutput::Files(files) => files.files.retain(|f| !gone(f.id, &f.path)),
        JobOutput::Duplicates(dupes) => {
            for group in &mut dupes.groups {
                group.files.retain(|f| !gone(f.id, &f.path));
            }
            dupes.groups.retain(|g| g.files.len() > 1);
        }
        JobOutput::Space(listing) => {
            let parent = listing.path.clone();
            listing.children.retain(|node| {
                let path = Path::new(&parent).join(&node.name).display().to_string();
                !gone(node.id, &path)
            });
        }
        _ => {}
    }
}

/// A change of one area's result.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ScanEvent {
    /// A scan finished: rebuild from [`Scans::output`].
    Scanned(Area),
    /// The result is gone (rescan started, daemon restarted, scan failed).
    Cleared(Area),
    /// A clean of `items` finished with `report`; drop what was removed (the store's
    /// output is already pruned).
    Cleaned {
        /// The area.
        area: Area,
        /// Items sent to the clean.
        items: Vec<ItemId>,
        /// Its report.
        report: CleanReport,
    },
}

impl ScanEvent {
    /// The area whose result changed.
    pub(crate) fn area(&self) -> Area {
        match self {
            Self::Scanned(area) | Self::Cleared(area) | Self::Cleaned { area, .. } => *area,
        }
    }
}

/// One area's shared state.
#[derive(Debug, Default)]
struct Entry {
    flow: Flow,
    /// The scan that produced (or is producing) the result.
    spec: Option<JobSpec>,
    output: Option<JobOutput>,
    summary: Option<AreaSummary>,
    /// When the result was produced (a clean refreshes it).
    finished_at: Option<Instant>,
}

/// The shared store.
pub(crate) struct Scans {
    entries: Vec<Entry>,
    throttle: Throttle,
    conn: Connection,
    /// The area page on screen (auto-scans run for it once connected).
    visible: Option<Area>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for Scans {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scans")
            .field("entries", &self.entries)
            .field("visible", &self.visible)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<ScanEvent> for Scans {}

struct GlobalScans(Entity<Scans>);

impl Global for GlobalScans {}

/// The store, created on first use.
pub(crate) fn entity(cx: &mut App) -> Entity<Scans> {
    if let Some(global) = cx.try_global::<GlobalScans>() {
        return global.0.clone();
    }
    let scans = cx.new(Scans::new);
    cx.set_global(GlobalScans(scans.clone()));
    scans
}

impl FlowHost for Scans {
    fn flow(&mut self, key: usize) -> Option<&mut Flow> {
        self.entries.get_mut(key).map(|e| &mut e.flow)
    }

    fn throttle(&mut self) -> &mut Throttle {
        &mut self.throttle
    }

    fn scanned(&mut self, key: usize, output: JobOutput, cx: &mut Context<'_, Self>) {
        let (Some(area), Some(entry)) = (Area::STORED.get(key).copied(), self.entries.get_mut(key))
        else {
            return;
        };
        entry.summary = match &output {
            JobOutput::Junk(report) => Some(AreaSummary::new(report)),
            _ => None,
        };
        entry.output = Some(output);
        entry.finished_at = Some(Instant::now());
        cx.emit(ScanEvent::Scanned(area));
        cx.notify();
    }

    fn cleared(&mut self, key: usize, cx: &mut Context<'_, Self>) {
        let (Some(area), Some(entry)) = (Area::STORED.get(key).copied(), self.entries.get_mut(key))
        else {
            return;
        };
        entry.output = None;
        entry.summary = None;
        entry.finished_at = None;
        cx.emit(ScanEvent::Cleared(area));
        cx.notify();
    }

    fn cleaned(
        &mut self,
        key: usize,
        items: &[ItemId],
        report: &CleanReport,
        cx: &mut Context<'_, Self>,
    ) {
        let (Some(area), Some(entry)) = (Area::STORED.get(key).copied(), self.entries.get_mut(key))
        else {
            return;
        };
        if let Some(output) = &mut entry.output {
            prune(output, items, report);
            entry.summary = match &*output {
                JobOutput::Junk(junk) => Some(AreaSummary::new(junk)),
                _ => None,
            };
        }
        entry.finished_at = Some(Instant::now());
        cx.emit(ScanEvent::Cleaned {
            area,
            items: items.to_vec(),
            report: report.clone(),
        });
        cx.notify();
    }
}

impl Scans {
    fn new(cx: &mut Context<'_, Self>) -> Self {
        clean_settings::attach(cx);
        let engine = engine::entity(cx);
        let subscriptions = vec![
            cx.subscribe(&engine, |this, _, event: &ClientEvent, cx| {
                this.on_engine(event, cx);
            }),
            // The auto-scan preference arrives with the daemon's settings.
            cx.observe_global::<CleanPrefs>(Self::auto_scan_visible),
        ];
        Self {
            entries: Area::STORED.iter().map(|_| Entry::default()).collect(),
            throttle: Throttle::default(),
            conn: Connection::new(cx),
            visible: None,
            _subscriptions: subscriptions,
        }
    }

    fn on_engine(&mut self, event: &ClientEvent, cx: &mut Context<'_, Self>) {
        let change = self.conn.observe(event);
        for key in 0..self.entries.len() {
            Flow::on_event(self, key, event, change, cx);
        }
        if let ClientEvent::State(state) = event {
            if matches!(state, ConnState::Connected { .. }) {
                self.auto_scan_visible(cx);
            }
            cx.notify();
        }
    }

    fn entry(&self, area: Area) -> Option<&Entry> {
        self.entries.get(area.slot()?)
    }

    /// Whether requests can be sent now.
    pub(crate) fn is_connected(&self) -> bool {
        self.conn.is_connected()
    }

    /// What views render of `area`'s flow (idle for areas the store does not keep).
    pub(crate) fn view(&self, area: Area) -> FlowView {
        self.entry(area).map(|e| e.flow.view()).unwrap_or_default()
    }

    /// `area`'s scan or clean is running.
    pub(crate) fn is_busy(&self, area: Area) -> bool {
        self.entry(area).is_some_and(|e| e.flow.is_busy())
    }

    /// The finished scan job whose items can be cleaned or browsed.
    pub(crate) fn scan_job(&self, area: Area) -> Option<omc_proto::jobs::JobId> {
        self.entry(area)?.flow.scan_job()
    }

    /// The retained output of `area`'s last scan (cleaned items removed).
    pub(crate) fn output(&self, area: Area) -> Option<&JobOutput> {
        self.entry(area)?.output.as_ref()
    }

    /// The overview's digest of a junk area's result.
    pub(crate) fn summary(&self, area: Area) -> Option<&AreaSummary> {
        self.entry(area)?.summary.as_ref()
    }

    /// The scan the current result belongs to.
    pub(crate) fn spec(&self, area: Area) -> Option<&JobSpec> {
        self.entry(area)?.spec.as_ref()
    }

    /// Age of `area`'s result when it is past its freshness and nothing runs: the page
    /// shows "scanned N ago · Rescan".
    pub(crate) fn stale_age(&self, area: Area, now: Instant) -> Option<Duration> {
        let entry = self.entry(area)?;
        if !matches!(entry.flow.phase(), FlowPhase::Ready | FlowPhase::Cleaned) {
            return None;
        }
        stale_age(area.policy(), entry.finished_at, now)
    }

    /// Age of `area`'s result while it is shown and nothing runs (fresh or stale), for
    /// "Scanned N ago" labels.
    pub(crate) fn age(&self, area: Area, now: Instant) -> Option<Duration> {
        let entry = self.entry(area)?;
        if !matches!(entry.flow.phase(), FlowPhase::Ready | FlowPhase::Cleaned) {
            return None;
        }
        now.checked_duration_since(entry.finished_at?)
    }

    /// Whether `area` holds a result younger than its freshness window.
    pub(crate) fn is_fresh(&self, area: Area, now: Instant) -> bool {
        self.entry(area)
            .and_then(|e| e.finished_at)
            .and_then(|t| now.checked_duration_since(t))
            .is_some_and(|age| age < area.policy().fresh_for)
    }

    /// Starts `spec` for `area`, replacing (and releasing) its current result.
    pub(crate) fn scan(&mut self, area: Area, spec: JobSpec, cx: &mut Context<'_, Self>) {
        let Some(key) = area.slot() else { return };
        if let Some(entry) = self.entries.get_mut(key) {
            entry.spec = Some(spec.clone());
        }
        Flow::scan(self, key, spec, cx);
    }

    /// Starts `area`'s parameterless scan (no-op for the space lens).
    pub(crate) fn rescan(&mut self, area: Area, cx: &mut Context<'_, Self>) {
        if let Some(scan) = area.default_scan() {
            self.scan(area, JobSpec::Scan(scan), cx);
        }
    }

    /// Scans `area` unless it runs already or holds a fresh result (the smart scan).
    pub(crate) fn ensure(&mut self, area: Area, cx: &mut Context<'_, Self>) {
        if !self.is_busy(area) && !self.is_fresh(area, Instant::now()) {
            self.rescan(area, cx);
        }
    }

    /// Removes `items` of `area`'s finished scan.
    pub(crate) fn clean(&mut self, area: Area, items: Vec<ItemId>, cx: &mut Context<'_, Self>) {
        if let Some(key) = area.slot() {
            Flow::clean(self, key, items, cx);
        }
    }

    /// Stops `area`'s running scan or clean.
    pub(crate) fn cancel(&mut self, area: Area, cx: &mut Context<'_, Self>) {
        if let Some(key) = area.slot() {
            Flow::cancel(self, key, cx);
        }
    }

    /// Leaves `area`'s clean report.
    pub(crate) fn dismiss_report(&mut self, area: Area, cx: &mut Context<'_, Self>) {
        if let Some(key) = area.slot() {
            Flow::dismiss_report(self, key, cx);
        }
    }

    /// Clears `area`'s error banner.
    pub(crate) fn dismiss_error(&mut self, area: Area, cx: &mut Context<'_, Self>) {
        if let Some(key) = area.slot() {
            Flow::dismiss_error(self, key, cx);
        }
    }

    /// `area`'s page came on screen: show a fresh result, or scan when the policy and the
    /// user's preference allow (now, or once the daemon and its settings are there).
    pub(crate) fn show(&mut self, area: Area, cx: &mut Context<'_, Self>) {
        self.visible = Some(area);
        self.auto_scan(true, cx);
    }

    /// `area`'s page left the screen.
    pub(crate) fn hide(&mut self, area: Area) {
        if self.visible == Some(area) {
            self.visible = None;
        }
    }

    /// Connection or settings changed: the visible area may scan now (a failed scan is
    /// only retried when the user opens the area again).
    fn auto_scan_visible(&mut self, cx: &mut Context<'_, Self>) {
        self.auto_scan(false, cx);
    }

    fn auto_scan(&mut self, retry_failed: bool, cx: &mut Context<'_, Self>) {
        let Some(area) = self.visible else { return };
        let Some(entry) = self.entry(area) else {
            return;
        };
        let phase = entry.flow.phase();
        // The preference is only known once the daemon's settings arrived.
        if entry.flow.is_busy()
            || (phase == FlowPhase::Failed && !retry_failed)
            || !self.conn.is_connected()
            || !CleanPrefs::is_loaded(cx)
        {
            return;
        }
        let age = entry
            .finished_at
            .and_then(|t| Instant::now().checked_duration_since(t));
        if on_show(area.policy(), age, CleanPrefs::auto_scan(cx)) == OnShow::Scan {
            self.rescan(area, cx);
        }
    }

    /// Puts `output` in `area` as a finished scan of job 1 (no daemon).
    #[cfg(test)]
    pub(crate) fn force_scanned(
        &mut self,
        area: Area,
        output: JobOutput,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(key) = area.slot() else { return };
        if let Some(entry) = self.entries.get_mut(key) {
            entry.flow.force_ready(Some(1));
        }
        self.scanned(key, output, cx);
    }

    /// Hands a finished clean of `area` to the store as if the daemon answered.
    #[cfg(test)]
    pub(crate) fn force_cleaned(
        &mut self,
        area: Area,
        items: &[ItemId],
        report: &CleanReport,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(key) = area.slot() {
            self.cleaned(key, items, report, cx);
        }
    }

    /// Back-dates `area`'s result.
    #[cfg(test)]
    pub(crate) fn set_finished_at(&mut self, area: Area, at: Instant) {
        if let Some(entry) = area.slot().and_then(|key| self.entries.get_mut(key)) {
            entry.finished_at = Some(at);
        }
    }

    /// Feeds an engine event (tests simulate connections).
    #[cfg(test)]
    pub(crate) fn engine_event(&mut self, event: &ClientEvent, cx: &mut Context<'_, Self>) {
        self.on_engine(event, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use omc_proto::jobs::{CleanReport, FailReason, Failure, JobOutput, Location};
    use omc_proto::junk::{JunkGroup, JunkItem, JunkKind, JunkReport, Safety};

    use super::{Area, AreaSummary, OnShow, SafeGroup, on_show, prune};

    const MIN: u64 = 60;

    #[test]
    fn policy_table_matches_the_cost_of_each_scan() {
        let table = [
            (Area::Trash, true, 2 * MIN),
            (Area::BrowserData, true, 5 * MIN),
            (Area::SystemJunk, true, 10 * MIN),
            (Area::Installers, true, 30 * MIN),
            (Area::Leftovers, true, 30 * MIN),
            (Area::DeveloperJunk, false, 30 * MIN),
            (Area::LargeOldFiles, false, 24 * 60 * MIN),
            (Area::Duplicates, false, 24 * 60 * MIN),
            (Area::SpaceLens, false, 24 * 60 * MIN),
            (Area::AppList, true, 10 * MIN),
            (Area::StartupItems, true, 5 * MIN),
        ];
        for (area, auto, secs) in table {
            let policy = area.policy();
            assert_eq!(policy.auto_scan, auto, "{area:?} auto-scan");
            assert_eq!(
                policy.fresh_for,
                Duration::from_secs(secs),
                "{area:?} freshness"
            );
        }
    }

    #[test]
    fn opening_an_area_follows_freshness_policy_and_preference() {
        let trash = Area::Trash.policy();
        let dupes = Area::Duplicates.policy();
        let minute = Some(Duration::from_secs(MIN));
        let hour = Some(Duration::from_secs(60 * MIN));
        let days = Some(Duration::from_secs(48 * 60 * MIN));
        assert_eq!(
            on_show(trash, minute, true),
            OnShow::Fresh,
            "fresh: no rescan"
        );
        assert_eq!(
            on_show(trash, hour, true),
            OnShow::Scan,
            "stale + auto: rescan"
        );
        assert_eq!(
            on_show(trash, None, true),
            OnShow::Scan,
            "nothing + auto: scan"
        );
        assert_eq!(
            on_show(trash, hour, false),
            OnShow::Stale,
            "preference off: keep the old result with a notice"
        );
        assert_eq!(
            on_show(trash, None, false),
            OnShow::Idle,
            "preference off, nothing: idle"
        );
        assert_eq!(
            on_show(trash, minute, false),
            OnShow::Fresh,
            "fresh results still show"
        );
        assert_eq!(
            on_show(dupes, hour, true),
            OnShow::Fresh,
            "a day for duplicates"
        );
        assert_eq!(
            on_show(dupes, days, true),
            OnShow::Stale,
            "manual + stale: notice"
        );
        assert_eq!(
            on_show(dupes, None, true),
            OnShow::Idle,
            "manual + nothing: idle"
        );
        assert_eq!(
            on_show(trash, Some(trash.fresh_for), true),
            OnShow::Scan,
            "the window's end is stale"
        );
    }

    fn item(id: u32, bytes: u64, safety: Safety, running: bool) -> JunkItem {
        JunkItem {
            id,
            name: String::new(),
            location: Location::Path {
                path: format!("/x/{id}"),
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
                        item(0, 10, Safety::Safe, false),
                        item(1, 20, Safety::Safe, true),
                        item(2, 30, Safety::Review, false),
                    ],
                },
                JunkGroup {
                    kind: JunkKind::UserLog,
                    items: vec![item(3, 5, Safety::Safe, false)],
                },
            ],
            denied: Vec::new(),
        }
    }

    #[test]
    fn summary_offers_only_safe_items_of_idle_apps() {
        let summary = AreaSummary::new(&report());
        assert_eq!(summary.total, 65, "total found");
        assert_eq!(summary.items, 4, "items found");
        assert_eq!(
            summary.safe,
            vec![(0, 10), (3, 5)],
            "only the safe idle items"
        );
        assert_eq!(summary.safe_bytes(), 15, "safe size");
        let groups: Vec<(JunkKind, usize, u64)> = summary
            .safe_groups
            .iter()
            .map(|g| (g.kind, g.count, g.bytes))
            .collect();
        assert_eq!(
            groups,
            vec![(JunkKind::UserCache, 1, 10), (JunkKind::UserLog, 1, 5)],
            "the breakdown counts only the safe idle items, per group"
        );
    }

    #[test]
    fn safe_groups_name_the_largest_distinct_items_and_skip_unsafe_groups() {
        let named = |id, name: &str, safety| JunkItem {
            name: name.to_owned(),
            ..item(id, 1, safety, false)
        };
        let report = JunkReport {
            groups: vec![
                JunkGroup {
                    kind: JunkKind::UserCache,
                    items: vec![
                        named(0, "Chrome", Safety::Safe),
                        named(1, " Chrome ", Safety::Safe),
                        named(2, "", Safety::Safe),
                        named(3, "Slack", Safety::Review),
                        named(4, "Xcode", Safety::Safe),
                        named(5, "Zoom", Safety::Safe),
                        named(6, "Figma", Safety::Safe),
                    ],
                },
                JunkGroup {
                    kind: JunkKind::InstallerPackage,
                    items: vec![named(7, "setup.pkg", Safety::Review)],
                },
            ],
            denied: Vec::new(),
        };
        let summary = AreaSummary::new(&report);
        assert_eq!(
            summary.safe_groups,
            vec![SafeGroup {
                kind: JunkKind::UserCache,
                count: 6,
                bytes: 6,
                names: vec!["Chrome".to_owned(), "Xcode".to_owned(), "Zoom".to_owned()],
            }],
            "names are trimmed, distinct, non-empty and capped; groups without safe items drop"
        );
        assert!(
            summary.safe_groups.iter().all(SafeGroup::has_more),
            "six items behind three names"
        );
    }

    #[test]
    fn pruning_removes_cleaned_items_but_keeps_failures() {
        let mut output = JobOutput::Junk(report());
        let failed = CleanReport {
            removed: 1,
            freed: 10,
            failures: vec![Failure {
                location: Location::Path {
                    path: "/x/0".to_owned(),
                },
                reason: FailReason::InUse,
                message: String::new(),
            }],
        };
        prune(&mut output, &[0, 3], &failed);
        let JobOutput::Junk(junk) = output else {
            return;
        };
        let ids: Vec<u32> = junk
            .groups
            .iter()
            .flat_map(|g| g.items.iter().map(|i| i.id))
            .collect();
        assert_eq!(
            ids,
            vec![0, 1, 2],
            "the failed item stays, the removed one goes"
        );
        assert_eq!(junk.groups.len(), 1, "an emptied group disappears");
    }

    use gpui_kit::TestAppContext;
    use omc_ipc::client::{ClientEvent, ConnState};

    use crate::clean_settings::CleanPrefs;
    use crate::pages::widgets::FlowPhase;

    fn connected(epoch: &str) -> ClientEvent {
        ClientEvent::State(ConnState::Connected {
            epoch: epoch.to_owned(),
        })
    }

    #[gpui_kit::test]
    fn cleaning_updates_and_a_new_daemon_clears_the_shared_result(cx: &mut TestAppContext) {
        let init = cx.update(crate::init);
        assert!(init.is_ok(), "UI initialises headless: {init:?}");
        let scans = cx.update(super::entity);
        scans.update(cx, |s, cx| {
            s.engine_event(&connected("a"), cx);
            s.force_scanned(Area::SystemJunk, JobOutput::Junk(report()), cx);
        });
        let safe = cx.update(|cx| {
            scans
                .read(cx)
                .summary(Area::SystemJunk)
                .map(AreaSummary::safe_bytes)
        });
        assert_eq!(safe, Some(15), "the summary follows the scan");
        scans.update(cx, |s, cx| {
            s.force_cleaned(Area::SystemJunk, &[0], &CleanReport::default(), cx);
        });
        let (safe, fresh) = cx.update(|cx| {
            let s = scans.read(cx);
            (
                s.summary(Area::SystemJunk).map(AreaSummary::safe_bytes),
                s.is_fresh(Area::SystemJunk, std::time::Instant::now()),
            )
        });
        assert_eq!(safe, Some(5), "cleaned items leave the shared result");
        assert!(fresh, "a clean counts as a fresh result");

        scans.update(cx, |s, cx| s.engine_event(&connected("a"), cx));
        let kept = cx.update(|cx| scans.read(cx).output(Area::SystemJunk).is_some());
        assert!(kept, "the same daemon keeps the result");
        scans.update(cx, |s, cx| s.engine_event(&connected("b"), cx));
        let gone = cx.update(|cx| scans.read(cx).output(Area::SystemJunk).is_none());
        assert!(gone, "a new daemon epoch drops every result");
    }

    #[gpui_kit::test]
    fn a_stale_manual_result_stays_with_a_notice(cx: &mut TestAppContext) {
        let init = cx.update(crate::init);
        assert!(init.is_ok(), "UI initialises headless: {init:?}");
        let scans = cx.update(super::entity);
        scans.update(cx, |s, cx| {
            s.force_scanned(Area::DeveloperJunk, JobOutput::Junk(report()), cx);
        });
        let now = std::time::Instant::now();
        let fresh = cx.update(|cx| scans.read(cx).stale_age(Area::DeveloperJunk, now));
        assert_eq!(fresh, None, "a fresh result has no notice");
        let Some(past) = now.checked_sub(Duration::from_secs(31 * MIN)) else {
            return;
        };
        scans.update(cx, |s, cx| {
            s.engine_event(&connected("a"), cx);
            s.set_finished_at(Area::DeveloperJunk, past);
        });
        cx.update(|cx| CleanPrefs::force_loaded(cx, true));
        scans.update(cx, |s, cx| s.show(Area::DeveloperJunk, cx));
        let (age, kept) = cx.update(|cx| {
            let s = scans.read(cx);
            (
                s.stale_age(Area::DeveloperJunk, now),
                s.output(Area::DeveloperJunk).is_some(),
            )
        });
        assert!(
            age.is_some_and(|age| age >= Duration::from_secs(31 * MIN)),
            "the notice says how old the result is: {age:?}"
        );
        assert!(kept, "opening a manual area keeps its stale result");
        let phase = cx.update(|cx| scans.read(cx).view(Area::DeveloperJunk).phase);
        assert_eq!(phase, FlowPhase::Ready, "and does not rescan it");
    }

    #[gpui_kit::test]
    fn a_result_reports_its_age_fresh_or_stale_but_not_before_a_scan(cx: &mut TestAppContext) {
        let init = cx.update(crate::init);
        assert!(init.is_ok(), "UI initialises headless: {init:?}");
        let scans = cx.update(super::entity);
        let now = std::time::Instant::now();
        let before = cx.update(|cx| scans.read(cx).age(Area::DeveloperJunk, now));
        assert_eq!(before, None, "no result, no age");
        scans.update(cx, |s, cx| {
            s.force_scanned(Area::DeveloperJunk, JobOutput::Junk(report()), cx);
        });
        let Some(past) = now.checked_sub(Duration::from_secs(5 * MIN)) else {
            return;
        };
        scans.update(cx, |s, _| s.set_finished_at(Area::DeveloperJunk, past));
        let (age, stale) = cx.update(|cx| {
            let s = scans.read(cx);
            (
                s.age(Area::DeveloperJunk, now),
                s.stale_age(Area::DeveloperJunk, now),
            )
        });
        assert_eq!(
            age,
            Some(Duration::from_secs(5 * MIN)),
            "a fresh result still has an age"
        );
        assert_eq!(stale, None, "while it is not stale");
    }

    fn phase(
        scans: &gpui_kit::Entity<super::Scans>,
        area: Area,
        cx: &mut TestAppContext,
    ) -> FlowPhase {
        cx.update(|cx| scans.read(cx).view(area).phase)
    }

    #[gpui_kit::test]
    fn opening_an_area_scans_by_policy_once_connected(cx: &mut TestAppContext) {
        let init = cx.update(crate::init);
        assert!(init.is_ok(), "UI initialises headless: {init:?}");
        let scans = cx.update(super::entity);
        cx.update(|cx| CleanPrefs::force_loaded(cx, true));
        scans.update(cx, |s, cx| s.show(Area::Trash, cx));
        assert_eq!(
            phase(&scans, Area::Trash, cx),
            FlowPhase::Idle,
            "nothing while disconnected"
        );
        scans.update(cx, |s, cx| s.engine_event(&connected("a"), cx));
        assert_eq!(
            phase(&scans, Area::Trash, cx),
            FlowPhase::Scanning,
            "the shown cheap area scans as soon as the daemon connects"
        );
        scans.update(cx, |s, cx| s.show(Area::Duplicates, cx));
        assert_eq!(
            phase(&scans, Area::Duplicates, cx),
            FlowPhase::Idle,
            "an expensive area waits for the user"
        );
        cx.update(|cx| CleanPrefs::force_loaded(cx, false));
        scans.update(cx, |s, cx| s.show(Area::BrowserData, cx));
        assert_eq!(
            phase(&scans, Area::BrowserData, cx),
            FlowPhase::Idle,
            "with the preference off nothing scans by itself"
        );
        cx.update(|cx| CleanPrefs::force_loaded(cx, true));
        scans.update(cx, |s, cx| {
            s.force_scanned(Area::SystemJunk, JobOutput::Junk(report()), cx);
            s.show(Area::SystemJunk, cx);
        });
        assert_eq!(
            phase(&scans, Area::SystemJunk, cx),
            FlowPhase::Ready,
            "a fresh result is shown without a rescan"
        );
    }
}
