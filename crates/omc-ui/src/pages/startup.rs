//! Startup items: programs launched at login or boot, with an enable switch (optimistic,
//! reverted when the daemon refuses) and removal after a confirmation.
//!
//! Jobs: `list_startup` when the page is shown and the list is missing or stale (see
//! [`crate::scans::Area::StartupItems`]), including once the daemon (re)connects;
//! `change_startup` per switch flip or removal, addressed by the list job's item ids.

use std::collections::BTreeMap;
use std::time::Instant;

use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyWindowHandle, Context, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div,
};
use omc_ipc::client::ClientEvent;
use omc_proto::Event;
use omc_proto::apps::{Scope, StartupChange, StartupItem, StartupKind};
use omc_proto::jobs::{ItemId, JobId, JobOutput, JobSpec};

use super::widgets::parts::detail_ident;
use super::widgets::slots::{self, JobSlot, SlotHost};
use super::widgets::{
    self, ConnChange, Connection, Counters, area_header, connection_notice, error_notice,
    job_progress, muted_cell, page_column, tr,
};
use crate::clean_settings::{self, CleanPrefs};
use crate::format;
use crate::nav::Category;
use crate::scans::{self, Area, OnShow};
use crate::tokens::{page, row, space, text};
use crate::ui;

// ---- Pure model -------------------------------------------------------------------------

/// A change sent to the daemon and not answered yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    /// The switch flipped optimistically; `was` restores it on failure.
    Toggle { was: bool },
    /// Removal requested; the row stays until the daemon confirms.
    Remove,
}

/// The listed items and their in-flight changes.
#[derive(Debug, Default)]
struct StartupModel {
    items: Vec<StartupItem>,
    pending: BTreeMap<ItemId, Pending>,
}

impl StartupModel {
    fn replace(&mut self, items: Vec<StartupItem>) {
        self.items = items;
        self.pending.clear();
    }

    fn get(&self, id: ItemId) -> Option<&StartupItem> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Flips the switch at once; returns the change to send, or `None` when the item is
    /// unknown, busy, or already in that state.
    fn begin_toggle(&mut self, id: ItemId, enabled: bool) -> Option<StartupChange> {
        if self.pending.contains_key(&id) {
            return None;
        }
        let item = self.items.iter_mut().find(|i| i.id == id)?;
        if item.enabled == enabled {
            return None;
        }
        self.pending
            .insert(id, Pending::Toggle { was: item.enabled });
        item.enabled = enabled;
        Some(if enabled {
            StartupChange::Enable
        } else {
            StartupChange::Disable
        })
    }

    /// Marks a removal as sent; `false` when the item is unknown or busy.
    fn begin_remove(&mut self, id: ItemId) -> bool {
        if self.pending.contains_key(&id) || self.get(id).is_none() {
            return false;
        }
        self.pending.insert(id, Pending::Remove);
        true
    }

    /// Applies the daemon's answer: `Ok(Some)` = the changed item, `Ok(None)` = removed,
    /// `Err` = refused (a flipped switch goes back).
    fn finish(&mut self, id: ItemId, result: Result<Option<StartupItem>, ()>) {
        let pending = self.pending.remove(&id);
        match result {
            Ok(Some(mut changed)) => {
                changed.id = id;
                if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
                    *item = changed;
                }
            }
            Ok(None) => self.items.retain(|i| i.id != id),
            Err(()) => {
                if let (Some(Pending::Toggle { was }), Some(item)) =
                    (pending, self.items.iter_mut().find(|i| i.id == id))
                {
                    item.enabled = was;
                }
            }
        }
    }

    fn enabled_count(&self) -> usize {
        self.items.iter().filter(|i| i.enabled).count()
    }
}

/// Which items the list shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    Enabled,
    Disabled,
}

impl Filter {
    const ALL: [Self; 3] = [Self::All, Self::Enabled, Self::Disabled];

    const fn key(self) -> &'static str {
        match self {
            Self::All => "startup.filter.all",
            Self::Enabled => "startup.filter.enabled",
            Self::Disabled => "startup.filter.disabled",
        }
    }

    const fn shows(self, item: &StartupItem) -> bool {
        match self {
            Self::All => true,
            Self::Enabled => item.enabled,
            Self::Disabled => !item.enabled,
        }
    }
}

/// Leading icon of a startup mechanism.
const fn kind_icon(kind: StartupKind) -> AssetIcon {
    match kind {
        StartupKind::LoginItem => AssetIcon::User,
        StartupKind::LaunchAgent | StartupKind::XdgAutostart => AssetIcon::Rocket,
        StartupKind::LaunchDaemon | StartupKind::Service => AssetIcon::Power,
        StartupKind::RunKey => AssetIcon::Settings,
        StartupKind::StartupFolder => AssetIcon::Folder,
        StartupKind::ScheduledTask => AssetIcon::Clock,
        StartupKind::SystemdUnit => AssetIcon::SquareTerminal,
    }
}

const fn kind_key(kind: StartupKind) -> &'static str {
    match kind {
        StartupKind::LoginItem => "login_item",
        StartupKind::LaunchAgent => "launch_agent",
        StartupKind::LaunchDaemon => "launch_daemon",
        StartupKind::RunKey => "run_key",
        StartupKind::StartupFolder => "startup_folder",
        StartupKind::ScheduledTask => "scheduled_task",
        StartupKind::Service => "service",
        StartupKind::XdgAutostart => "xdg_autostart",
        StartupKind::SystemdUnit => "systemd_unit",
    }
}

fn count(n: usize) -> SharedString {
    format::count(u64::try_from(n).unwrap_or(u64::MAX))
}

// ---- Page -------------------------------------------------------------------------------

/// Jobs the page follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    /// The `list_startup` job.
    List,
    /// A `change_startup` job for one item.
    Change(ItemId),
}

/// Page view.
pub(crate) struct StartupPage {
    conn: Connection,
    /// The page is on screen.
    shown: bool,
    window: AnyWindowHandle,
    model: StartupModel,
    /// The `list_startup` job the item ids belong to.
    list_job: Option<JobId>,
    loaded: bool,
    /// When the list arrived (freshness, see [`Area::StartupItems`]).
    loaded_at: Option<Instant>,
    error: Option<SharedString>,
    filter: Filter,
    list: JobSlot,
    changes: BTreeMap<ItemId, JobSlot>,
    notify_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for StartupPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartupPage")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl SlotHost for StartupPage {
    type Slot = Slot;

    fn slot(&mut self, slot: Slot) -> Option<&mut JobSlot> {
        match slot {
            Slot::List => Some(&mut self.list),
            Slot::Change(id) => self.changes.get_mut(&id),
        }
    }

    fn output(&mut self, slot: Slot, job: JobId, output: JobOutput, cx: &mut Context<'_, Self>) {
        match (slot, output) {
            (Slot::List, JobOutput::Startup(list)) => {
                self.list.job = None;
                if let Some(old) = self.list_job.replace(job) {
                    widgets::release(old, cx);
                }
                self.model.replace(list.items);
                self.loaded = true;
                self.loaded_at = Some(Instant::now());
                self.error = None;
            }
            (Slot::Change(id), JobOutput::StartupChanged(item)) => {
                self.changes.remove(&id);
                widgets::release(job, cx);
                self.model.finish(id, Ok(item));
            }
            (slot, other) => {
                tracing::warn!(?slot, ?other, "unexpected job output");
                if slot != Slot::List {
                    widgets::release(job, cx);
                }
                self.failed(slot, "unexpected job output".into(), cx);
            }
        }
        cx.notify();
    }

    fn failed(&mut self, slot: Slot, message: SharedString, cx: &mut Context<'_, Self>) {
        match slot {
            Slot::List => self.error = Some(message),
            Slot::Change(id) => {
                if let Some(job) = self.changes.remove(&id).and_then(|mut s| s.reset()) {
                    widgets::release(job, cx);
                }
                let name = self
                    .model
                    .get(id)
                    .map(|i| i.name.clone())
                    .unwrap_or_default();
                self.model.finish(id, Err(()));
                let text: SharedString = rust_i18n::t!(
                    "startup.change_failed",
                    name = name,
                    error = message.as_ref()
                )
                .to_string()
                .into();
                let window = self.window;
                cx.defer(move |cx| {
                    if let Err(err) = window.update(cx, |_, window, cx| {
                        window.push_notification(Notification::error(text), cx);
                    }) {
                        tracing::debug!("cannot show a notification: {err:#}");
                    }
                });
            }
        }
        cx.notify();
    }

    fn notify_task(&mut self) -> &mut Option<Task<()>> {
        &mut self.notify_task
    }
}

impl StartupPage {
    /// Creates the page.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        clean_settings::attach(cx);
        let engine = crate::engine::entity(cx);
        let subscriptions = vec![
            cx.subscribe(&engine, |this, _, event: &ClientEvent, cx| {
                this.on_engine(event, cx);
            }),
            // The auto-scan preference arrives with the daemon's settings.
            cx.observe_global::<CleanPrefs>(Self::auto_load),
        ];
        Self {
            conn: Connection::new(cx),
            shown: false,
            window: window.window_handle(),
            model: StartupModel::default(),
            list_job: None,
            loaded: false,
            loaded_at: None,
            error: None,
            filter: Filter::default(),
            list: JobSlot::default(),
            changes: BTreeMap::new(),
            notify_task: None,
            _subscriptions: subscriptions,
        }
    }

    /// The page came on screen (`shown`) or left it; showing it refreshes a missing or
    /// stale list by the freshness policy.
    pub(crate) fn set_visible(&mut self, shown: bool, cx: &mut Context<'_, Self>) {
        self.shown = shown;
        if shown {
            self.auto_load(cx);
        }
    }

    /// Loads the list when the page is shown, connected, idle, and the list is missing or
    /// stale (and the user lets areas scan by themselves); never after a failed load.
    fn auto_load(&mut self, cx: &mut Context<'_, Self>) {
        if !self.shown
            || !self.conn.is_connected()
            || !CleanPrefs::is_loaded(cx)
            || self.list.is_active()
            || !self.changes.is_empty()
            || self.error.is_some()
        {
            return;
        }
        let age = self.loaded_at.map(|t| t.elapsed());
        if scans::on_show(Area::StartupItems.policy(), age, CleanPrefs::auto_scan(cx))
            == OnShow::Scan
        {
            self.load(cx);
        }
    }

    fn slot_ids(&self) -> Vec<Slot> {
        std::iter::once(Slot::List)
            .chain(self.changes.keys().map(|&id| Slot::Change(id)))
            .collect()
    }

    fn on_engine(&mut self, event: &ClientEvent, cx: &mut Context<'_, Self>) {
        match self.conn.observe(event) {
            ConnChange::NewDaemon => self.new_daemon(cx),
            ConnChange::Reattached => {
                for slot in self.slot_ids() {
                    slots::poll(self, slot, cx);
                }
            }
            ConnChange::None => {}
        }
        match event {
            ClientEvent::Daemon(Event::Job(update)) => {
                let ids = self.slot_ids();
                slots::route(self, &ids, update, cx);
            }
            ClientEvent::State(_) => {
                self.auto_load(cx);
                cx.notify();
            }
            ClientEvent::Daemon(_) => {}
        }
    }

    /// Every job id belongs to the old daemon: forget them, revert optimistic switches.
    fn new_daemon(&mut self, cx: &mut Context<'_, Self>) {
        self.list.reset();
        for (id, mut slot) in std::mem::take(&mut self.changes) {
            slot.reset();
            self.model.finish(id, Err(()));
        }
        self.list_job = None;
        self.model.replace(Vec::new());
        self.loaded = false;
        self.loaded_at = None;
        self.error = None;
        cx.notify();
    }

    fn load(&mut self, cx: &mut Context<'_, Self>) {
        if !self.changes.is_empty() {
            return;
        }
        self.error = None;
        slots::start(self, Slot::List, JobSpec::ListStartup, cx);
        cx.notify();
    }

    fn change(&mut self, id: ItemId, change: StartupChange, cx: &mut Context<'_, Self>) {
        let Some(list_job) = self.list_job else {
            return;
        };
        self.changes.insert(id, JobSlot::default());
        slots::start(
            self,
            Slot::Change(id),
            JobSpec::ChangeStartup {
                list_job,
                item: id,
                change,
            },
            cx,
        );
        cx.notify();
    }

    fn toggle(&mut self, id: ItemId, enabled: bool, cx: &mut Context<'_, Self>) {
        if let Some(change) = self.model.begin_toggle(id, enabled) {
            self.change(id, change, cx);
        }
    }

    fn remove(&mut self, id: ItemId, cx: &mut Context<'_, Self>) {
        if self.model.begin_remove(id) {
            self.change(id, StartupChange::Remove, cx);
        }
    }

    fn confirm_remove(&mut self, id: ItemId, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(item) = self.model.get(id) else {
            return;
        };
        let title: SharedString =
            rust_i18n::t!("startup.remove_confirm.title", name = item.name.as_str())
                .to_string()
                .into();
        let page: WeakEntity<Self> = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let page = page.clone();
            dialog
                .w(page::DIALOG_WIDTH)
                .title(title.clone())
                .child(
                    div()
                        .text_size(text::BODY)
                        .line_height(text::BODY_LINE_HEIGHT)
                        .text_color(cx.theme().muted_foreground)
                        .child(tr("startup.remove_confirm.body")),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap(space::MD)
                        .child(
                            ui::Button::new(
                                "startup-remove-cancel",
                                tr("startup.remove_confirm.cancel"),
                            )
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            ui::Button::new("startup-remove-ok", tr("startup.remove_confirm.ok"))
                                .danger()
                                .on_click(move |_, window, cx| {
                                    if let Some(page) = page.upgrade() {
                                        page.update(cx, |page, cx| page.remove(id, cx));
                                    }
                                    window.close_dialog(cx);
                                }),
                        ),
                )
        });
    }

    /// One 40 px row: kind icon, name over the command (`~`, monospace, middle-truncated),
    /// the kind as the one badge, facts as a dot / lock icon, publisher, switch, remove.
    fn render_row(&self, item: &StartupItem, cx: &mut Context<'_, Self>) -> AnyElement {
        let id = item.id;
        let busy = self.model.pending.contains_key(&id) || !self.conn.is_connected();
        let command = format::tilde(
            &item
                .command
                .clone()
                .unwrap_or_else(|| item.location.display()),
        );
        let kind = tr(&format!("startup.kind.{}", kind_key(item.kind)));
        // User scope is the default; only a machine-wide item says so.
        let badge: SharedString = match item.scope {
            Scope::User => kind,
            Scope::System => format!("{kind} · {}", tr("startup.scope.system")).into(),
        };
        ui::ListRow::new(("startup-row", id), item.name.clone())
            .when_some(
                detail_ident(item.ident.as_deref(), &command),
                ui::ListRow::detail_lead,
            )
            .detail(command)
            .detail_mono()
            .icon(ui::row_icon(
                item.icon.as_deref(),
                Icon::new(kind_icon(item.kind)),
                cx,
            ))
            .disabled(self.model.pending.contains_key(&id))
            .when(item.missing_target, |this| {
                this.trailing(
                    ui::Dot::new(ui::Tone::Warning)
                        .tooltip(("startup-missing", id), tr("startup.missing")),
                )
            })
            .when(item.needs_admin, |this| {
                this.trailing(ui::FactIcon::new(
                    ("startup-admin", id),
                    Icon::new(AssetIcon::Lock),
                    tr("startup.admin"),
                ))
            })
            .trailing(ui::Badge::new(badge))
            .trailing(muted_cell(
                item.publisher.clone().unwrap_or_default().into(),
                cx,
            ))
            .trailing(
                ui::Switch::new(("startup-toggle", id))
                    .checked(item.enabled)
                    .tooltip(tr("startup.toggle"))
                    .disabled(busy)
                    .on_click(cx.listener(move |this, on: &bool, _, cx| this.toggle(id, *on, cx))),
            )
            .trailing(
                ui::IconButton::new(
                    ("startup-remove", id),
                    Icon::new(AssetIcon::Trash),
                    tr("startup.remove"),
                )
                .small()
                .disabled(busy)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.confirm_remove(id, window, cx)),
                ),
            )
            .into_any_element()
    }

    fn render_body(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        if self.list.is_active() {
            return job_progress(
                "startup-list",
                &self.list.progress,
                Counters::Found,
                Box::new(cx.listener(|this, _, _, cx| slots::cancel(this, Slot::List, cx))),
                cx,
            );
        }
        if !self.loaded {
            return ui::Card::new()
                .child(
                    ui::EmptyState::new(IconName::Inbox, tr("startup.idle.title"))
                        .description(tr("startup.idle.body"))
                        .action(
                            ui::Button::new("startup-load", tr("startup.load"))
                                .primary()
                                .large()
                                .icon(IconName::RefreshCw)
                                .disabled(!self.conn.is_connected())
                                .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                        ),
                )
                .into_any_element();
        }
        if self.model.items.is_empty() {
            return ui::Card::new()
                .child(
                    ui::EmptyState::new(IconName::CircleCheck, tr("startup.empty.title"))
                        .description(tr("startup.empty.body")),
                )
                .into_any_element();
        }
        let summary = rust_i18n::t!(
            "startup.summary",
            enabled = count(self.model.enabled_count()),
            total = count(self.model.items.len())
        )
        .to_string();
        let filter = Filter::ALL
            .iter()
            .fold(ui::Segmented::new("startup-filter").small(), |seg, f| {
                seg.segment(tr(f.key()))
            })
            .selected(
                Filter::ALL
                    .iter()
                    .position(|f| *f == self.filter)
                    .unwrap_or(0),
            )
            .on_select(cx.listener(|this, ix: &usize, _, cx| {
                if let Some(filter) = Filter::ALL.get(*ix) {
                    this.filter = *filter;
                    cx.notify();
                }
            }));
        let rows = self
            .model
            .items
            .iter()
            .filter(|item| self.filter.shows(item))
            .map(|item| self.render_row(item, cx))
            .collect::<Vec<_>>();
        let list = if rows.is_empty() {
            ui::EmptyState::new(IconName::Search, tr("startup.filtered_empty.title"))
                .description(tr("startup.filtered_empty.body"))
                .into_any_element()
        } else {
            v_flex()
                .w_full()
                .p(row::INSET)
                .children(rows)
                .into_any_element()
        };
        ui::Card::new()
            .flush()
            .header(ui::CardHeader::new(summary).trailing(filter))
            .child(list)
            .into_any_element()
    }
}

impl Render for StartupPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let connected = self.conn.is_connected();
        let stale = if self.list.is_active() {
            None
        } else {
            scans::stale_age(Area::StartupItems.policy(), self.loaded_at, Instant::now())
        };
        let stale = super::stale_notice(
            "startup-rescan",
            stale,
            connected && self.changes.is_empty(),
            Box::new(cx.listener(|this, _, _, cx| this.load(cx))),
            cx,
        );
        let refresh = ui::Button::new("startup-refresh", tr("startup.refresh"))
            .icon(IconName::RefreshCw)
            .disabled(!connected || self.list.is_active() || !self.changes.is_empty())
            .on_click(cx.listener(|this, _, _, cx| this.load(cx)))
            .into_any_element();
        let error = self.error.clone().map(|error| {
            error_notice(
                error,
                Box::new(cx.listener(|this, _, _, cx| {
                    this.error = None;
                    cx.notify();
                })),
                cx,
            )
        });
        let body = self.render_body(cx);
        page_column("page-startup")
            .overflow_y_scroll()
            .child(area_header(Category::StartupItems, vec![refresh], cx))
            .children(connection_notice(connected, cx))
            .children(stale)
            .children(error)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use omc_proto::apps::{Scope, StartupChange, StartupItem, StartupKind};
    use omc_proto::jobs::Location;

    use gpui_kit::TestAppContext;

    use gpui_kit::component::WindowExt as _;

    use super::{Filter, StartupModel, StartupPage};
    use crate::pages::widgets::test_support::open_page;

    fn item(id: u32, enabled: bool) -> StartupItem {
        StartupItem {
            id,
            name: format!("item {id}"),
            command: None,
            location: Location::Path {
                path: format!("/x/{id}.plist"),
            },
            kind: StartupKind::LaunchAgent,
            scope: Scope::User,
            enabled,
            needs_admin: false,
            publisher: None,
            missing_target: false,
            ident: None,
            icon: None,
        }
    }

    fn enabled(model: &StartupModel, id: u32) -> Option<bool> {
        model.get(id).map(|i| i.enabled)
    }

    #[test]
    fn a_refused_toggle_reverts_the_switch() {
        let mut model = StartupModel::default();
        model.replace(vec![item(0, true), item(1, false)]);
        assert_eq!(
            model.begin_toggle(0, false),
            Some(StartupChange::Disable),
            "flipping sends a disable"
        );
        assert_eq!(enabled(&model, 0), Some(false), "the switch flips at once");
        assert_eq!(
            model.begin_toggle(0, true),
            None,
            "no second change while one is pending"
        );
        model.finish(0, Err(()));
        assert_eq!(
            enabled(&model, 0),
            Some(true),
            "refusal restores the switch"
        );
        assert!(model.pending.is_empty(), "nothing pending after the answer");
        assert_eq!(model.begin_toggle(1, false), None, "already in that state");
    }

    #[test]
    fn the_daemon_answer_replaces_or_removes_the_row() {
        let mut model = StartupModel::default();
        model.replace(vec![item(0, false), item(1, true)]);
        assert_eq!(
            model.begin_toggle(0, true),
            Some(StartupChange::Enable),
            "enable sent"
        );
        let mut changed = item(99, true);
        changed.name = "renamed".to_owned();
        model.finish(0, Ok(Some(changed)));
        assert_eq!(
            model.get(0).map(|i| (i.enabled, i.name.as_str())),
            Some((true, "renamed")),
            "the answered item replaces the row and keeps its id"
        );

        assert!(model.begin_remove(1), "removal sent");
        assert!(!model.begin_remove(1), "not twice");
        model.finish(1, Err(()));
        assert!(model.get(1).is_some(), "a refused removal keeps the row");
        assert!(model.begin_remove(1), "can retry");
        model.finish(1, Ok(None));
        assert!(model.get(1).is_none(), "a confirmed removal drops the row");
        assert_eq!(model.enabled_count(), 1, "one enabled item left");
    }

    #[gpui_kit::test]
    fn the_page_renders_its_disconnected_state_without_a_daemon(cx: &mut TestAppContext) {
        let opened = open_page(cx, StartupPage::new);
        let Some((_, page)) = opened else { return };
        let (connected, loaded, busy) = cx.update(|cx| {
            let page = page.read(cx);
            (page.conn.is_connected(), page.loaded, page.list.is_active())
        });
        assert!(!connected, "no daemon in tests");
        assert!(!loaded && !busy, "nothing is requested while disconnected");
    }

    #[gpui_kit::test]
    fn the_page_renders_filtered_rows_and_asks_before_removing(cx: &mut TestAppContext) {
        let Some((window, page)) = open_page(cx, StartupPage::new) else {
            return;
        };
        let mut odd = item(1, false);
        odd.scope = Scope::System;
        odd.needs_admin = true;
        odd.missing_target = true;
        odd.command = Some("/Users/x/bin/agent --serve".to_owned());
        page.update(cx, |page, cx| {
            page.model.replace(vec![item(0, true), odd]);
            page.list_job = Some(1);
            page.loaded = true;
            page.filter = Filter::Disabled;
            cx.notify();
        });
        cx.run_until_parked();
        let shown = page.read_with(cx, |page, _| {
            page.model
                .items
                .iter()
                .filter(|i| page.filter.shows(i))
                .map(|i| i.id)
                .collect::<Vec<_>>()
        });
        assert_eq!(
            shown,
            [1],
            "the Disabled filter shows only switched-off items"
        );
        let asked = window.update(cx, |_, window, cx| {
            page.update(cx, |page, cx| page.confirm_remove(1, window, cx));
            window.has_active_dialog(cx)
        });
        assert_eq!(asked.ok(), Some(true), "removal asks first");
    }
}
