//! Automation → Rules (ADR 0024): the daemon's rules as a list beside an editor that reads as
//! one sentence — *Every 14 days at 10:00 · in Developer Junk (Project build output) · if
//! unused for 14 days · clean it · ask first*. The list holds each rule's enable switch,
//! name, next run and "Run now"; the editor saves with `put_rule`, deletes with
//! `delete_rule`. Nothing is stored until Save: templates and new rules are drafts.
//!
//! [`Draft`] is the editor's pure model (validation, kind selection, dirty detection).

use std::collections::BTreeSet;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FontWeight, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div,
};
use omc_proto::jobs::{DeleteMethod, ScanArea};
use omc_proto::junk::{Browser, JunkKind};
use omc_proto::rules::{
    Confirm, Rule, RuleAction, RuleFilter, RuleId, RuleInfo, RuleScope, Trigger,
};

use super::junk::{kind_label, removal};
use super::widgets::runs::isolated;
use super::widgets::{self, Removal, Tone, tr};
use crate::clean_settings::CleanPrefs;
use crate::format;
use crate::nav::Category;
use crate::rules::{self, Rules, RulesEvent};
use crate::tokens::{page, row, space, text};
use crate::ui;

/// The junk areas a rule can look at, with the page each belongs to.
pub(crate) const RULE_AREAS: [(ScanArea, Category); 6] = [
    (ScanArea::SystemJunk, Category::SystemJunk),
    (ScanArea::BrowserData, Category::BrowserData),
    (ScanArea::DeveloperJunk, Category::DeveloperJunk),
    (ScanArea::Trash, Category::Trash),
    (ScanArea::Installers, Category::Installers),
    (ScanArea::Leftovers, Category::Leftovers),
];

const SYSTEM_KINDS: [JunkKind; 14] = [
    JunkKind::UserCache,
    JunkKind::SystemCache,
    JunkKind::UserLog,
    JunkKind::SystemLog,
    JunkKind::CrashReport,
    JunkKind::TempFiles,
    JunkKind::Thumbnails,
    JunkKind::UpdateCache,
    JunkKind::PackageCache,
    JunkKind::ErrorReports,
    JunkKind::ShaderCache,
    JunkKind::MailDownloads,
    JunkKind::DeviceBackups,
    JunkKind::OldOsInstall,
];

const BROWSER_KINDS: [JunkKind; 10] = [
    JunkKind::Browser(Browser::Chrome),
    JunkKind::Browser(Browser::Chromium),
    JunkKind::Browser(Browser::Edge),
    JunkKind::Browser(Browser::Brave),
    JunkKind::Browser(Browser::Vivaldi),
    JunkKind::Browser(Browser::Opera),
    JunkKind::Browser(Browser::Arc),
    JunkKind::Browser(Browser::Firefox),
    JunkKind::Browser(Browser::Safari),
    JunkKind::Browser(Browser::Yandex),
];

const DEVELOPER_KINDS: [JunkKind; 5] = [
    JunkKind::Xcode,
    JunkKind::DevPackageCache,
    JunkKind::ProjectArtifacts,
    JunkKind::IdeCache,
    JunkKind::ToolCache,
];

const TRASH_KINDS: [JunkKind; 1] = [JunkKind::Trash];

const INSTALLER_KINDS: [JunkKind; 3] = [
    JunkKind::DiskImage,
    JunkKind::InstallerPackage,
    JunkKind::SetupProgram,
];

const LEFTOVER_KINDS: [JunkKind; 5] = [
    JunkKind::OrphanFiles,
    JunkKind::OrphanLaunchItems,
    JunkKind::BrokenUninstallEntries,
    JunkKind::BrokenShortcuts,
    JunkKind::OrphanRegistry,
];

/// The group kinds the scan of `area` can report; empty for areas a rule cannot use.
pub(crate) fn area_kinds(area: &ScanArea) -> &'static [JunkKind] {
    match area {
        ScanArea::SystemJunk => &SYSTEM_KINDS,
        ScanArea::BrowserData => &BROWSER_KINDS,
        ScanArea::DeveloperJunk => &DEVELOPER_KINDS,
        ScanArea::Trash => &TRASH_KINDS,
        ScanArea::Installers => &INSTALLER_KINDS,
        ScanArea::Leftovers => &LEFTOVER_KINDS,
        _ => &[],
    }
}

/// The page of a rule area.
fn area_category(area: &ScanArea) -> Option<Category> {
    RULE_AREAS
        .iter()
        .find(|(a, _)| a == area)
        .map(|(_, category)| *category)
}

/// The rule area an area page automates.
fn category_area(category: Category) -> Option<ScanArea> {
    RULE_AREAS
        .iter()
        .find(|(_, c)| *c == category)
        .map(|(area, _)| area.clone())
}

/// Interval choices in days.
const DAY_STEPS: [u64; 11] = [1, 2, 3, 5, 7, 10, 14, 21, 30, 60, 90];
/// Idle-age choices in days (0 = any age).
const IDLE_STEPS: [u64; 10] = [0, 1, 3, 7, 14, 30, 60, 90, 180, 365];
/// Size choices in MB (0 = any size).
const SIZE_STEPS_MB: [u64; 7] = [0, 10, 50, 100, 500, 1_000, 5_000];

/// `steps` plus `current` when it is not one of them, ascending: a stored value the editor
/// has no step for still reads truthfully.
fn with_current(steps: &[u64], current: u64) -> Vec<u64> {
    let mut all = steps.to_vec();
    if !all.contains(&current) {
        all.push(current);
    }
    all.sort_unstable();
    all
}

/// What the editor edits: a rule in the shapes the controls speak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Draft {
    /// 0 for a rule that was never stored.
    pub(crate) id: RuleId,
    /// Typed name; blank falls back to a generated one on save.
    pub(crate) name: String,
    pub(crate) enabled: bool,
    /// Interval in days.
    pub(crate) days: u32,
    /// Local hour of day.
    pub(crate) hour: u8,
    pub(crate) area: ScanArea,
    /// Group kinds to keep; empty = every kind of the area.
    pub(crate) kinds: BTreeSet<JunkKind>,
    pub(crate) idle_days: u32,
    pub(crate) min_bytes: u64,
    pub(crate) include_review: bool,
    pub(crate) confirm: Confirm,
}

impl Draft {
    /// An empty new rule: weekly at 10:00 over system junk, asking first.
    pub(crate) fn blank(area: ScanArea) -> Self {
        Self {
            id: 0,
            name: String::new(),
            enabled: true,
            days: 7,
            hour: 10,
            area,
            kinds: BTreeSet::new(),
            idle_days: 0,
            min_bytes: 0,
            include_review: false,
            confirm: Confirm::Ask,
        }
    }

    /// "Clean old build output every 2 weeks": developer build output not rebuilt for 14
    /// days, every 14 days at 10:00, asking first.
    pub(crate) fn template(name: String) -> Self {
        Self {
            name,
            days: 14,
            area: ScanArea::DeveloperJunk,
            kinds: BTreeSet::from([JunkKind::ProjectArtifacts]),
            idle_days: 14,
            ..Self::blank(ScanArea::DeveloperJunk)
        }
    }

    /// The draft of a stored rule.
    pub(crate) fn from_info(info: &RuleInfo) -> Self {
        let rule = &info.rule;
        let Trigger::Every { days, hour } = rule.trigger;
        let RuleScope::Junk { area, kinds } = &rule.scope;
        Self {
            id: rule.id,
            name: rule.name.clone(),
            enabled: rule.enabled,
            days,
            hour,
            area: area.clone(),
            kinds: kinds.iter().copied().collect(),
            idle_days: rule.filter.idle_days,
            min_bytes: rule.filter.min_bytes,
            include_review: rule.filter.include_review,
            confirm: rule.confirm,
        }
    }

    /// The rule to send; a blank name becomes `fallback_name`.
    pub(crate) fn to_rule(&self, fallback_name: &str) -> Rule {
        let name = self.name.trim();
        Rule {
            id: self.id,
            name: if name.is_empty() { fallback_name } else { name }.to_owned(),
            enabled: self.enabled,
            trigger: Trigger::Every {
                days: self.days,
                hour: self.hour,
            },
            scope: RuleScope::Junk {
                area: self.area.clone(),
                kinds: self.kinds.iter().copied().collect(),
            },
            filter: RuleFilter {
                idle_days: self.idle_days,
                min_bytes: self.min_bytes,
                include_review: self.include_review,
            },
            action: RuleAction::Clean,
            confirm: self.confirm,
        }
    }

    /// Whether the daemon would accept it: 1–90 days, an hour of the day, a junk area, kinds
    /// of that area.
    pub(crate) fn is_valid(&self) -> bool {
        let kinds = area_kinds(&self.area);
        (1..=90).contains(&self.days)
            && self.hour <= 23
            && !kinds.is_empty()
            && self.kinds.iter().all(|kind| kinds.contains(kind))
    }

    /// Whether `kind` is checked (an empty selection means every kind).
    pub(crate) fn kind_checked(&self, kind: JunkKind) -> bool {
        self.kinds.is_empty() || self.kinds.contains(&kind)
    }

    /// Checks or unchecks `kind`. At least one kind stays checked, and checking them all
    /// is stored as the empty selection ("every kind", also those a later version adds).
    pub(crate) fn toggle_kind(&mut self, kind: JunkKind) {
        let all = area_kinds(&self.area);
        if self.kinds.is_empty() {
            self.kinds = all.iter().copied().collect();
        }
        if !self.kinds.remove(&kind) {
            self.kinds.insert(kind);
        }
        if self.kinds.is_empty() {
            self.kinds.insert(kind);
        }
        if self.kinds.len() >= all.len() {
            self.kinds.clear();
        }
    }

    /// Switches the area; the kinds of the old one no longer apply.
    pub(crate) fn set_area(&mut self, area: ScanArea) {
        if self.area != area {
            self.area = area;
            self.kinds.clear();
        }
    }

    /// Whether the edits differ from `saved` (the switch in the list is not an edit).
    pub(crate) fn edits_differ(&self, saved: &Self) -> bool {
        Self {
            enabled: saved.enabled,
            ..self.clone()
        } != *saved
    }
}

/// What the editor shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selection {
    /// Nothing opened.
    None,
    /// A draft that is not stored yet.
    New,
    /// A stored rule.
    Rule(RuleId),
}

/// The Rules page.
pub(crate) struct RulesPage {
    rules: Entity<Rules>,
    name: Entity<InputState>,
    selection: Selection,
    draft: Option<Draft>,
    /// The stored state of the opened rule, for dirty detection.
    base: Option<Draft>,
    /// A save from the editor is in flight.
    saving: bool,
    /// The first click on Delete asked; the second one deletes.
    confirm_delete: bool,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for RulesPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RulesPage")
            .field("selection", &self.selection)
            .finish_non_exhaustive()
    }
}

impl RulesPage {
    /// Creates the page over the shared store.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let rules = rules::entity(cx);
        let name = cx
            .new(|cx| InputState::new(window, cx).placeholder(tr("rules.editor.name_placeholder")));
        let subscriptions = vec![
            cx.observe(&rules, |this, rules, cx| {
                if rules.read(cx).error().is_some() {
                    this.saving = false;
                }
                this.drop_vanished(cx);
                cx.notify();
            }),
            cx.subscribe_in(&rules, window, |this, _, event: &RulesEvent, window, cx| {
                this.on_event(*event, window, cx);
            }),
            cx.subscribe(&name, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    if let Some(draft) = this.draft.as_mut() {
                        draft.name = value;
                        cx.notify();
                    }
                }
            }),
        ];
        Self {
            rules,
            name,
            selection: Selection::None,
            draft: None,
            base: None,
            saving: false,
            confirm_delete: false,
            _subscriptions: subscriptions,
        }
    }

    /// The draft in the editor, `None` while nothing is opened.
    #[cfg(test)]
    pub(crate) fn draft(&self) -> Option<&Draft> {
        self.draft.as_ref()
    }

    /// Opens the stored rule `id` in the editor.
    pub(crate) fn select_rule(
        &mut self,
        id: RuleId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let draft = self.rules.read(cx).model().rule(id).map(Draft::from_info);
        if let Some(draft) = draft {
            self.load(Selection::Rule(id), Some(draft), window, cx);
        }
    }

    /// Opens an empty new rule.
    fn new_rule(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.load(
            Selection::New,
            Some(Draft::blank(ScanArea::SystemJunk)),
            window,
            cx,
        );
    }

    /// Opens an unsaved draft that automates `category`: the "clean old build output"
    /// template for developer junk, else a new rule over that area.
    pub(crate) fn start_from_area(
        &mut self,
        category: Category,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let draft = if category == Category::DeveloperJunk {
            Draft::template(tr("rules.template.name").to_string())
        } else {
            Draft::blank(category_area(category).unwrap_or(ScanArea::SystemJunk))
        };
        self.load(Selection::New, Some(draft), window, cx);
    }

    fn load(
        &mut self,
        selection: Selection,
        draft: Option<Draft>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.selection = selection;
        self.base = draft
            .clone()
            .filter(|_| matches!(selection, Selection::Rule(_)));
        if let Some(draft) = &draft {
            let name = draft.name.clone();
            self.name
                .update(cx, |input, cx| input.set_value(name, window, cx));
        }
        self.draft = draft;
        self.confirm_delete = false;
        cx.notify();
    }

    fn close_editor(&mut self, cx: &mut Context<'_, Self>) {
        self.selection = Selection::None;
        self.draft = None;
        self.base = None;
        self.confirm_delete = false;
        cx.notify();
    }

    /// Closes the editor when its stored rule disappeared (deleted elsewhere).
    fn drop_vanished(&mut self, cx: &mut Context<'_, Self>) {
        if let Selection::Rule(id) = self.selection
            && self.rules.read(cx).model().rule(id).is_none()
            && self.rules.read(cx).is_loaded()
        {
            self.close_editor(cx);
        }
    }

    fn on_event(&mut self, event: RulesEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        match event {
            RulesEvent::Saved(id) if self.saving => {
                self.saving = false;
                self.select_rule(id, window, cx);
            }
            RulesEvent::Saved(id) if self.selection == Selection::Rule(id) => {
                let stored = self.rules.read(cx).model().rule(id).map(Draft::from_info);
                if let Some(stored) = stored {
                    let dirty = self.is_dirty();
                    match (&mut self.draft, dirty) {
                        (Some(draft), true) => draft.enabled = stored.enabled,
                        (draft, _) => *draft = Some(stored.clone()),
                    }
                    self.base = Some(stored);
                    cx.notify();
                }
            }
            RulesEvent::Deleted(id) if self.selection == Selection::Rule(id) => {
                self.close_editor(cx);
            }
            _ => {}
        }
    }

    fn is_dirty(&self) -> bool {
        match (&self.draft, &self.base) {
            (Some(draft), Some(base)) => draft.edits_differ(base),
            (Some(_), None) => true,
            _ => false,
        }
    }

    fn edit(&mut self, cx: &mut Context<'_, Self>, change: impl FnOnce(&mut Draft)) {
        if let Some(draft) = self.draft.as_mut() {
            change(draft);
            cx.notify();
        }
    }

    fn save(&mut self, cx: &mut Context<'_, Self>) {
        let Some(draft) = self.draft.as_ref().filter(|d| d.is_valid()) else {
            return;
        };
        let title = area_category(&draft.area).map_or_else(SharedString::default, Category::title);
        let fallback = rust_i18n::t!("rules.default_name", area = title).to_string();
        let rule = draft.to_rule(&fallback);
        self.saving = true;
        self.rules.update(cx, |_, cx| Rules::save(rule, cx));
    }

    fn delete(&mut self, cx: &mut Context<'_, Self>) {
        if !self.confirm_delete {
            self.confirm_delete = true;
            cx.notify();
            return;
        }
        if let Selection::Rule(id) = self.selection {
            self.rules.update(cx, |_, cx| Rules::delete(id, cx));
        }
        self.confirm_delete = false;
    }

    fn set_enabled(&mut self, id: RuleId, enabled: bool, cx: &mut Context<'_, Self>) {
        let rule = self.rules.read(cx).model().rule(id).map(|info| Rule {
            enabled,
            ..info.rule.clone()
        });
        if let Some(rule) = rule {
            self.rules.update(cx, |_, cx| Rules::save(rule, cx));
        }
    }

    fn run_now(&mut self, id: RuleId, window: &mut Window, cx: &mut Context<'_, Self>) {
        let name = self
            .rules
            .read(cx)
            .model()
            .rule(id)
            .map(|info| info.rule.name.clone())
            .unwrap_or_default();
        self.rules.update(cx, |_, cx| Rules::run_now(id, cx));
        let message = rust_i18n::t!("rules.list.run_started", name = name).to_string();
        window.push_notification(Notification::info(message), cx);
    }

    // ---- rendering ----

    fn rule_row(
        &self,
        info: &RuleInfo,
        now: i64,
        connected: bool,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let id = info.rule.id;
        let enabled = info.rule.enabled;
        let detail: SharedString = if !enabled {
            tr("rules.list.paused")
        } else if let Some(at) = info.next_run {
            rust_i18n::t!("rules.list.next", when = format::relative(at, now))
                .to_string()
                .into()
        } else {
            tr("rules.list.unscheduled")
        };
        ui::ListRow::new(
            ElementId::Name(SharedString::from(format!("rule-row-{id}"))),
            info.rule.name.clone(),
        )
        .detail(detail)
        .current(self.selection == Selection::Rule(id))
        .leading(
            ui::Switch::new(ElementId::Name(SharedString::from(format!(
                "rule-enabled-{id}"
            ))))
            .checked(enabled)
            .disabled(!connected)
            .tooltip(tr("rules.list.enable"))
            .on_click(cx.listener(move |this, on: &bool, _, cx| this.set_enabled(id, *on, cx))),
        )
        .trailing(isolated(
            ElementId::Name(SharedString::from(format!("rule-run-wrap-{id}"))),
            ui::Button::new(
                ElementId::Name(SharedString::from(format!("rule-run-{id}"))),
                tr("rules.list.run_now"),
            )
            .small()
            .icon(IconName::Play)
            .disabled(!connected)
            .on_click(cx.listener(move |this, _, window, cx| this.run_now(id, window, cx))),
        ))
        .on_click(cx.listener(move |this, _, window, cx| this.select_rule(id, window, cx)))
        .into_any_element()
    }

    fn render_list(&self, connected: bool, cx: &mut Context<'_, Self>) -> AnyElement {
        let now = format::now();
        let infos: Vec<RuleInfo> = self.rules.read(cx).model().rules().to_vec();
        let mut rows: Vec<AnyElement> = infos
            .iter()
            .map(|info| self.rule_row(info, now, connected, cx))
            .collect();
        if self.selection == Selection::New {
            rows.push(
                ui::ListRow::new("rule-row-new", tr("rules.list.new_row"))
                    .detail(tr("rules.list.unsaved"))
                    .current(true)
                    .into_any_element(),
            );
        }
        ui::Card::new()
            .flush()
            .w(page::RULES_LIST_WIDTH)
            .flex_none()
            .child(widgets::card_body().pt(row::INSET).children(rows))
            .into_any_element()
    }

    fn word(text: SharedString, cx: &App) -> AnyElement {
        div()
            .flex_none()
            .text_size(text::BODY)
            .line_height(text::BODY_LINE_HEIGHT)
            .text_color(cx.theme().foreground)
            .child(text)
            .into_any_element()
    }

    /// One labelled line of the sentence: the label column, then wrapping controls.
    fn field(label: &'static str, parts: Vec<AnyElement>, cx: &App) -> AnyElement {
        h_flex()
            .w_full()
            .items_start()
            .gap(space::LG)
            .child(
                div()
                    .flex_none()
                    .w(page::RULES_LABEL_WIDTH)
                    .pt(space::XS)
                    .text_size(text::CAPTION)
                    .line_height(text::CAPTION_LINE_HEIGHT)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(cx.theme().muted_foreground)
                    .child(tr(label)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .gap(space::MD)
                    .children(parts),
            )
            .into_any_element()
    }

    fn days_label(n: u64) -> SharedString {
        if n == 1 {
            rust_i18n::t!("rules.unit.day", n = n).to_string().into()
        } else {
            rust_i18n::t!("rules.unit.days", n = n).to_string().into()
        }
    }

    /// A select over numeric `steps` (plus the current value).
    fn number_select(
        id: &'static str,
        steps: &[u64],
        current: u64,
        label: impl Fn(u64) -> SharedString,
        on_pick: impl Fn(&u64, &mut Window, &mut App) + 'static,
    ) -> ui::Select {
        ui::Select::new(
            id,
            with_current(steps, current)
                .into_iter()
                .map(|value| (SharedString::from(value.to_string()), label(value))),
        )
        .selected(current.to_string())
        .on_change(move |value, window, cx| match value.parse::<u64>() {
            Ok(value) => on_pick(&value, window, cx),
            Err(err) => tracing::warn!(%value, "bad step value: {err}"),
        })
    }

    /// "Clean it — moved to the Trash / deleted permanently", from the settings.
    fn then_line(area: &ScanArea, cx: &App) -> SharedString {
        let removal = area_category(area).map_or(Removal::Junk, removal);
        let key = if CleanPrefs::is_loaded(cx) {
            let settings = CleanPrefs::settings(cx);
            let method = match removal {
                Removal::Junk => settings.junk_delete,
                Removal::UserFiles => settings.files_delete,
            };
            match method {
                DeleteMethod::Permanent => "rules.then.permanent",
                DeleteMethod::Trash => "rules.then.trash",
            }
        } else {
            "rules.then.default"
        };
        tr(key)
    }

    fn when_parts(draft: &Draft, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        vec![
            Self::word(tr("rules.editor.every"), cx),
            Self::number_select(
                "rule-days",
                &DAY_STEPS,
                u64::from(draft.days),
                Self::days_label,
                cx.listener(|this, value: &u64, _, cx| {
                    this.edit(cx, |d| d.days = u32::try_from(*value).unwrap_or(d.days));
                }),
            )
            .into_any_element(),
            Self::word(tr("rules.editor.at"), cx),
            Self::number_select(
                "rule-hour",
                &(0..24).collect::<Vec<u64>>(),
                u64::from(draft.hour),
                |hour| SharedString::from(format!("{hour:02}:00")),
                cx.listener(|this, value: &u64, _, cx| {
                    this.edit(cx, |d| d.hour = u8::try_from(*value).unwrap_or(d.hour));
                }),
            )
            .into_any_element(),
        ]
    }

    fn where_parts(draft: &Draft, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let areas = RULE_AREAS
            .iter()
            .map(|(_, category)| (SharedString::new_static(category.key()), category.title()));
        let area_select = ui::Select::new("rule-area", areas)
            .selected(
                area_category(&draft.area).map_or(SharedString::default(), |c| c.key().into()),
            )
            .on_change(cx.listener(|this, key: &SharedString, _, cx| {
                let area = RULE_AREAS
                    .iter()
                    .find(|(_, category)| category.key() == key.as_ref())
                    .map(|(area, _)| area.clone());
                if let Some(area) = area {
                    this.edit(cx, |d| d.set_area(area));
                }
            }));
        let kinds = area_kinds(&draft.area);
        let kind_boxes = (kinds.len() > 1).then(|| {
            h_flex()
                .w_full()
                .flex_wrap()
                .gap_x(space::XL)
                .gap_y(space::SM)
                .children(kinds.iter().copied().enumerate().map(|(ix, kind)| {
                    ui::Checkbox::new(ElementId::Name(SharedString::from(format!(
                        "rule-kind-{ix}"
                    ))))
                    .checked(draft.kind_checked(kind))
                    .label(kind_label(kind))
                    .on_click(cx.listener(move |this, _: &bool, _, cx| {
                        this.edit(cx, |d| d.toggle_kind(kind));
                    }))
                }))
                .into_any_element()
        });
        let mut where_parts = vec![
            Self::word(tr("rules.editor.in"), cx),
            area_select.into_any_element(),
        ];
        where_parts.extend(kind_boxes);
        where_parts
    }

    fn if_parts(draft: &Draft, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        vec![
            Self::word(tr("rules.editor.idle"), cx),
            Self::number_select(
                "rule-idle",
                &IDLE_STEPS,
                u64::from(draft.idle_days),
                |days| {
                    if days == 0 {
                        tr("rules.editor.any_age")
                    } else {
                        Self::days_label(days)
                    }
                },
                cx.listener(|this, value: &u64, _, cx| {
                    this.edit(cx, |d| {
                        d.idle_days = u32::try_from(*value).unwrap_or(d.idle_days);
                    });
                }),
            )
            .into_any_element(),
            Self::word(tr("rules.editor.size"), cx),
            Self::number_select(
                "rule-size",
                &SIZE_STEPS_MB.map(|mb| mb.saturating_mul(format::MEGABYTE)),
                draft.min_bytes,
                |bytes| {
                    if bytes == 0 {
                        tr("rules.editor.any_size")
                    } else {
                        format::bytes(bytes)
                    }
                },
                cx.listener(|this, value: &u64, _, cx| this.edit(cx, |d| d.min_bytes = *value)),
            )
            .into_any_element(),
            ui::Checkbox::new("rule-review")
                .checked(draft.include_review)
                .label(tr("rules.editor.include_review"))
                .on_click(cx.listener(|this, on: &bool, _, cx| {
                    this.edit(cx, |d| d.include_review = *on);
                }))
                .into_any_element(),
        ]
    }

    /// The confirmation segment and, for automatic cleaning, its warning.
    fn before_parts(
        draft: &Draft,
        cx: &mut Context<'_, Self>,
    ) -> (Vec<AnyElement>, Option<AnyElement>) {
        let before_parts = vec![
            ui::Segmented::new("rule-confirm")
                .segment(tr("rules.editor.ask"))
                .segment(tr("rules.editor.auto"))
                .selected(usize::from(draft.confirm == Confirm::Auto))
                .on_select(cx.listener(|this, ix: &usize, _, cx| {
                    this.edit(cx, |d| {
                        d.confirm = if *ix == 1 {
                            Confirm::Auto
                        } else {
                            Confirm::Ask
                        };
                    });
                }))
                .into_any_element(),
        ];
        let auto_warning = (draft.confirm == Confirm::Auto).then(|| {
            widgets::notice(
                Tone::Warning,
                tr("rules.editor.auto_warning.title"),
                Some(tr("rules.editor.auto_warning.body")),
                Vec::new(),
                cx,
            )
        });
        (before_parts, auto_warning)
    }

    /// Delete (asks twice) on the left, Save on the right.
    fn footer(&self, connected: bool, cx: &mut Context<'_, Self>) -> AnyElement {
        let saveable = self.draft.as_ref().is_some_and(Draft::is_valid)
            && connected
            && self.is_dirty()
            && !self.saving;
        let is_stored = matches!(self.selection, Selection::Rule(_));
        let confirming = self.confirm_delete;
        h_flex()
            .w_full()
            .gap(space::MD)
            .child(
                h_flex()
                    .flex_1()
                    .gap(space::MD)
                    .when(is_stored && !confirming, |this| {
                        this.child(
                            ui::Button::new("rule-delete", tr("rules.editor.delete"))
                                .ghost()
                                .disabled(!connected)
                                .on_click(cx.listener(|this, _, _, cx| this.delete(cx))),
                        )
                    })
                    .when(is_stored && confirming, |this| {
                        this.child(
                            ui::Button::new(
                                "rule-delete-confirm",
                                tr("rules.editor.delete_confirm"),
                            )
                            .danger()
                            .disabled(!connected)
                            .on_click(cx.listener(|this, _, _, cx| this.delete(cx))),
                        )
                        .child(
                            ui::Button::new("rule-delete-cancel", tr("rules.editor.cancel"))
                                .ghost()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_delete = false;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(
                ui::Button::new("rule-save", tr("rules.editor.save"))
                    .primary()
                    .disabled(!saveable)
                    .loading(self.saving)
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            )
            .into_any_element()
    }

    fn render_editor(
        &self,
        draft: &Draft,
        connected: bool,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let when = Self::when_parts(draft, cx);
        let where_parts = Self::where_parts(draft, cx);
        let if_parts = Self::if_parts(draft, cx);
        let then_parts = vec![Self::word(Self::then_line(&draft.area, cx), cx)];
        let (before_parts, auto_warning) = Self::before_parts(draft, cx);
        let footer = self.footer(connected, cx);
        let is_stored = matches!(self.selection, Selection::Rule(_));
        ui::Card::new()
            .flex_1()
            .min_w(page::RULES_EDITOR_MIN_WIDTH)
            .header(ui::CardHeader::new(if is_stored {
                tr("rules.editor.title_edit")
            } else {
                tr("rules.editor.title_new")
            }))
            .child(
                v_flex()
                    .w_full()
                    .gap(space::XL)
                    .child(ui::TextInput::new(&self.name).label(tr("rules.editor.name")))
                    .child(Self::field("rules.editor.when", when, cx))
                    .child(Self::field("rules.editor.where", where_parts, cx))
                    .child(Self::field("rules.editor.if", if_parts, cx))
                    .child(Self::field("rules.editor.then", then_parts, cx))
                    .child(Self::field("rules.editor.before", before_parts, cx))
                    .children(auto_warning)
                    .child(footer),
            )
            .into_any_element()
    }

    fn render_empty(connected: bool, cx: &mut Context<'_, Self>) -> AnyElement {
        ui::Card::new()
            .child(
                ui::EmptyState::new(Category::Rules.icon(), tr("rules.empty.title"))
                    .description(tr("rules.empty.body"))
                    .action(
                        h_flex()
                            .gap(space::LG)
                            .child(
                                ui::Button::new("rules-template", tr("rules.template.button"))
                                    .primary()
                                    .large()
                                    .disabled(!connected)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.start_from_area(Category::DeveloperJunk, window, cx);
                                    })),
                            )
                            .child(
                                ui::Button::new("rules-empty-new", tr("rules.new"))
                                    .large()
                                    .disabled(!connected)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.new_rule(window, cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for RulesPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let (connected, empty, error) = {
            let store = self.rules.read(cx);
            (
                store.is_connected(),
                store.model().rules().is_empty(),
                store.error().cloned(),
            )
        };
        let actions = vec![
            ui::Button::new("rules-new", tr("rules.new"))
                .icon(IconName::Plus)
                .disabled(!connected)
                .on_click(cx.listener(|this, _, window, cx| this.new_rule(window, cx)))
                .into_any_element(),
        ];
        let mut column = widgets::page_column("rules-page")
            .overflow_y_scroll()
            .child(widgets::area_header(Category::Rules, actions, cx))
            .children(widgets::connection_notice(connected, cx));
        if let Some(error) = error {
            column = column.child(widgets::error_notice(
                error,
                Box::new(cx.listener(|this, _, _, cx| {
                    this.rules.update(cx, Rules::dismiss_error);
                })),
                cx,
            ));
        }
        let draft = self.draft.clone();
        let body = if empty && draft.is_none() {
            Self::render_empty(connected, cx)
        } else {
            let editor = match &draft {
                Some(draft) => self.render_editor(draft, connected, cx),
                None => ui::Card::new()
                    .flex_1()
                    .min_w(page::RULES_EDITOR_MIN_WIDTH)
                    .child(
                        ui::EmptyState::new(Category::Rules.icon(), tr("rules.pick.title"))
                            .description(tr("rules.pick.body")),
                    )
                    .into_any_element(),
            };
            h_flex()
                .w_full()
                .flex_wrap()
                .items_start()
                .gap(space::XL)
                .child(self.render_list(connected, cx))
                .child(editor)
                .into_any_element()
        };
        v_flex().size_full().child(column.child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(draft: &Draft) -> RuleInfo {
        RuleInfo {
            rule: draft.to_rule("fallback"),
            last_run: None,
            next_run: None,
        }
    }

    #[test]
    fn the_template_is_the_build_output_rule_from_the_spec() {
        let rule = Draft::template("Clean old build output every 2 weeks".to_owned()).to_rule("x");
        assert_eq!(rule.id, 0, "a template is not stored yet");
        assert_eq!(
            rule.trigger,
            Trigger::Every { days: 14, hour: 10 },
            "every 14 days at 10:00"
        );
        assert_eq!(
            rule.scope,
            RuleScope::Junk {
                area: ScanArea::DeveloperJunk,
                kinds: vec![JunkKind::ProjectArtifacts],
            },
            "project build output of developer junk"
        );
        assert_eq!(rule.filter.idle_days, 14, "not rebuilt for 14 days");
        assert!(!rule.filter.include_review, "only safe items");
        assert_eq!(rule.confirm, Confirm::Ask, "asks first");
        assert!(rule.enabled, "on once saved");
    }

    #[test]
    fn a_stored_rule_survives_the_editor_unchanged() {
        let mut draft = Draft::template("Nightly".to_owned());
        draft.id = 7;
        draft.min_bytes = format::MEGABYTE.saturating_mul(5);
        draft.confirm = Confirm::Auto;
        let info = stored(&draft);
        assert_eq!(
            Draft::from_info(&info),
            draft,
            "opening a rule and saving it without edits changes nothing"
        );
        assert!(
            !Draft::from_info(&info).edits_differ(&draft),
            "no edits, not dirty"
        );
    }

    #[test]
    fn a_blank_name_falls_back_and_a_typed_one_is_trimmed() {
        let mut draft = Draft::blank(ScanArea::Trash);
        assert_eq!(draft.to_rule("Clean Trash").name, "Clean Trash", "fallback");
        draft.name = "  Weekly  ".to_owned();
        assert_eq!(draft.to_rule("Clean Trash").name, "Weekly", "trimmed");
    }

    #[test]
    fn kinds_keep_one_checked_and_all_checked_means_every_kind() {
        let mut draft = Draft::blank(ScanArea::DeveloperJunk);
        assert!(
            DEVELOPER_KINDS.iter().all(|k| draft.kind_checked(*k)),
            "an empty selection shows every kind checked"
        );
        draft.toggle_kind(JunkKind::Xcode);
        assert!(!draft.kind_checked(JunkKind::Xcode), "unchecked");
        assert!(
            draft.kind_checked(JunkKind::ProjectArtifacts),
            "others stay"
        );
        assert_eq!(draft.kinds.len(), 4, "the other four are now explicit");
        draft.toggle_kind(JunkKind::Xcode);
        assert!(
            draft.kinds.is_empty(),
            "checking them all again is 'every kind'"
        );

        let mut single = Draft::template("t".to_owned());
        single.toggle_kind(JunkKind::ProjectArtifacts);
        assert!(
            single.kind_checked(JunkKind::ProjectArtifacts) && single.is_valid(),
            "the last checked kind cannot be unchecked into an empty (= all) selection"
        );
        let mut trash = Draft::blank(ScanArea::Trash);
        trash.toggle_kind(JunkKind::Trash);
        assert!(trash.kinds.is_empty(), "a one-kind area never narrows");
    }

    #[test]
    fn changing_the_area_forgets_kinds_of_the_old_one() {
        let mut draft = Draft::template("t".to_owned());
        draft.set_area(ScanArea::DeveloperJunk);
        assert_eq!(draft.kinds.len(), 1, "the same area keeps its selection");
        draft.set_area(ScanArea::Installers);
        assert!(
            draft.kinds.is_empty(),
            "another area starts with every kind"
        );
        assert!(draft.is_valid(), "and is valid");
    }

    #[test]
    fn validation_follows_the_daemon_limits() {
        let mut draft = Draft::blank(ScanArea::SystemJunk);
        assert!(draft.is_valid(), "a blank rule is valid");
        draft.days = 0;
        assert!(!draft.is_valid(), "0 days");
        draft.days = 91;
        assert!(!draft.is_valid(), "91 days");
        draft.days = 90;
        draft.hour = 24;
        assert!(!draft.is_valid(), "hour 24");
        draft.hour = 23;
        assert!(draft.is_valid(), "the limits themselves are valid");
        draft.kinds.insert(JunkKind::Xcode);
        assert!(!draft.is_valid(), "a kind of another area");
        assert!(
            !Draft::blank(ScanArea::LargeOldFiles).is_valid(),
            "a non-junk area"
        );
    }

    #[test]
    fn the_enable_switch_is_not_an_edit() {
        let base = Draft::template("t".to_owned());
        let flipped = Draft {
            enabled: false,
            ..base.clone()
        };
        assert!(
            !flipped.edits_differ(&base),
            "the list's switch is separate"
        );
        let renamed = Draft {
            name: "u".to_owned(),
            ..base.clone()
        };
        assert!(renamed.edits_differ(&base), "a rename is an edit");
    }

    #[test]
    fn steps_include_a_stored_value_between_them() {
        assert_eq!(
            with_current(&[1, 7, 14], 10),
            [1, 7, 10, 14],
            "an odd stored value is listed in order"
        );
        assert_eq!(
            with_current(&[1, 7], 7),
            [1, 7],
            "a known value is not duplicated"
        );
    }

    #[test]
    fn every_rule_area_has_kinds_and_none_shares_them() {
        let mut seen = BTreeSet::new();
        for (area, _) in &RULE_AREAS {
            let kinds = area_kinds(area);
            assert!(!kinds.is_empty(), "{area:?} lists kinds");
            for kind in kinds {
                assert!(seen.insert(*kind), "{kind:?} belongs to one area only");
            }
        }
    }
}
