//! Duplicates: groups of byte-identical files, most wasted space first. The user picks the
//! copies to remove, by hand or with "keep oldest / newest / shortest path"; at least one
//! copy of every group always stays.

use std::collections::HashSet;
use std::ops::Range;
use std::time::Instant;

use gpui_kit::component::{ActiveTheme as _, IconName, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, ElementId, Entity, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, UniformListScrollHandle, Window, div, uniform_list,
};
use omc_proto::files::{DupReport, FileKind};
use omc_proto::jobs::{CleanReport, Denied, ItemId, JobOutput};

use super::large::{FileView, kind_icon};
use super::widgets::{self, CleanConfirm, FlowPhase, OnClick, Removal, Tone, tr};
use crate::format;
use crate::nav::Category;
use crate::scans::{self, Area, ScanEvent, Scans};
use crate::tokens::{badge, row, text};
use crate::ui;

/// Which copy of each group an automatic selection keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keep {
    /// The least recently modified copy (the usual original).
    Oldest,
    /// The most recently modified copy.
    Newest,
    /// The copy with the shortest path.
    ShortestPath,
}

/// Auto-selection segments, in order.
const KEEPS: [(Keep, &str); 3] = [
    (Keep::Oldest, "dupes.keep.oldest"),
    (Keep::Newest, "dupes.keep.newest"),
    (Keep::ShortestPath, "dupes.keep.shortest"),
];

#[derive(Debug, Clone)]
struct GroupView {
    files: Vec<FileView>,
    wasted: u64,
    /// "3 copies · 10 MB each".
    header: SharedString,
    /// Name of the first copy: the group's title.
    title: SharedString,
}

impl GroupView {
    /// Recomputes the display strings after the copies changed.
    fn relabel(&mut self) {
        let bytes = self.files.first().map_or(0, |f| f.entry.bytes);
        let copies = u64::try_from(self.files.len()).unwrap_or(u64::MAX);
        self.wasted = bytes.saturating_mul(copies.saturating_sub(1));
        self.header = header(self.files.len(), bytes);
        self.title = self
            .files
            .first()
            .map(|f| f.name.clone())
            .unwrap_or_default();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Group(usize),
    File(usize, usize),
}

/// Duplicate groups with the selection. Pure data. Invariant: no group has every copy
/// selected.
#[derive(Debug, Default)]
pub(crate) struct DupModel {
    groups: Vec<GroupView>,
    denied: Vec<Denied>,
    truncated: bool,
    selected: HashSet<ItemId>,
    /// Folded groups (indices).
    collapsed: HashSet<usize>,
    rows: Vec<Row>,
    selected_bytes: u64,
    wasted: u64,
    /// The last manual pick was refused because it would remove every copy.
    refused: bool,
    /// The automatic selection in force; a manual change ends it.
    preset: Option<Keep>,
}

fn header(files: usize, bytes: u64) -> SharedString {
    rust_i18n::t!(
        "dupes.group",
        count = format::count(u64::try_from(files).unwrap_or(u64::MAX)),
        bytes = format::bytes(bytes)
    )
    .to_string()
    .into()
}

impl DupModel {
    /// Builds the model (groups by wasted size); nothing is selected.
    pub(crate) fn new(report: DupReport) -> Self {
        let mut groups: Vec<GroupView> = report
            .groups
            .into_iter()
            .filter(|g| g.files.len() > 1)
            .map(|g| {
                let mut group = GroupView {
                    files: g.files.into_iter().map(FileView::new).collect(),
                    wasted: 0,
                    header: SharedString::default(),
                    title: SharedString::default(),
                };
                group.relabel();
                group
            })
            .collect();
        groups.sort_by_key(|g| std::cmp::Reverse(g.wasted));
        let mut model = Self {
            groups,
            denied: report.denied,
            truncated: report.truncated,
            ..Self::default()
        };
        model.refresh();
        model
    }

    fn refresh(&mut self) {
        self.rows.clear();
        self.selected_bytes = 0;
        self.wasted = 0;
        for (gi, group) in self.groups.iter().enumerate() {
            self.wasted = self.wasted.saturating_add(group.wasted);
            self.rows.push(Row::Group(gi));
            let open = !self.collapsed.contains(&gi);
            for (fi, file) in group.files.iter().enumerate() {
                if open {
                    self.rows.push(Row::File(gi, fi));
                }
                if self.selected.contains(&file.entry.id) {
                    self.selected_bytes = self.selected_bytes.saturating_add(file.entry.bytes);
                }
            }
        }
    }

    /// Flips one copy. Selecting the last unselected copy of a group is refused (returns
    /// `false` and raises [`Self::refused`]).
    pub(crate) fn toggle(&mut self, gi: usize, fi: usize) -> bool {
        let Some(group) = self.groups.get(gi) else {
            return false;
        };
        let Some(id) = group.files.get(fi).map(|f| f.entry.id) else {
            return false;
        };
        if self.selected.remove(&id) {
            self.refused = false;
        } else {
            let kept = group
                .files
                .iter()
                .filter(|f| !self.selected.contains(&f.entry.id))
                .count();
            if kept <= 1 {
                self.refused = true;
                return false;
            }
            self.refused = false;
            self.selected.insert(id);
        }
        self.preset = None;
        self.refresh();
        true
    }

    /// Selects every copy except the one `keep` chooses, in every group.
    pub(crate) fn auto_select(&mut self, keep: Keep) {
        self.selected.clear();
        for group in &self.groups {
            let keeper = match keep {
                Keep::Oldest => group
                    .files
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, f)| f.entry.modified.unwrap_or(i64::MAX))
                    .map(|(ix, _)| ix),
                Keep::Newest => group
                    .files
                    .iter()
                    .enumerate()
                    // `max_by_key` keeps the last maximum; reverse so ties keep the first.
                    .rev()
                    .max_by_key(|(_, f)| f.entry.modified.unwrap_or(i64::MIN))
                    .map(|(ix, _)| ix),
                Keep::ShortestPath => group
                    .files
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, f)| f.entry.path.chars().count())
                    .map(|(ix, _)| ix),
            };
            for (ix, file) in group.files.iter().enumerate() {
                if Some(ix) != keeper {
                    self.selected.insert(file.entry.id);
                }
            }
        }
        self.refused = false;
        self.preset = Some(keep);
        self.refresh();
    }

    /// Clears the selection.
    pub(crate) fn select_none(&mut self) {
        self.selected.clear();
        self.refused = false;
        self.preset = None;
        self.refresh();
    }

    /// The automatic selection in force, if the user has not changed it since.
    pub(crate) fn preset(&self) -> Option<Keep> {
        self.preset
    }

    /// Whether the last manual pick was refused.
    pub(crate) fn refused(&self) -> bool {
        self.refused
    }

    /// Whether every group keeps at least one copy.
    pub(crate) fn keeps_one_per_group(&self) -> bool {
        self.groups
            .iter()
            .all(|g| g.files.iter().any(|f| !self.selected.contains(&f.entry.id)))
    }

    /// Selected ids, ascending.
    pub(crate) fn selected_ids(&self) -> Vec<ItemId> {
        let mut ids: Vec<ItemId> = self.selected.iter().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Size of the selection.
    pub(crate) fn selected_bytes(&self) -> u64 {
        self.selected_bytes
    }

    /// Group ids in list order (the first file's id stands for the group).
    #[cfg(test)]
    pub(crate) fn group_order(&self) -> Vec<ItemId> {
        self.groups
            .iter()
            .filter_map(|g| g.files.first().map(|f| f.entry.id))
            .collect()
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

    /// Number of rows in the flattened list.
    #[cfg(test)]
    pub(crate) fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Drops removed copies (except failures) and groups left with a single file.
    pub(crate) fn remove_cleaned(&mut self, cleaned: &[ItemId], report: &CleanReport) {
        let failed: HashSet<String> = report
            .failures
            .iter()
            .map(|f| f.location.display())
            .collect();
        let cleaned: HashSet<ItemId> = cleaned.iter().copied().collect();
        for group in &mut self.groups {
            group
                .files
                .retain(|f| !cleaned.contains(&f.entry.id) || failed.contains(&f.entry.path));
            group.relabel();
        }
        // Group indices shift when groups vanish: forget the folding state.
        let before = self.groups.len();
        self.groups.retain(|g| g.files.len() > 1);
        if self.groups.len() != before {
            self.collapsed.clear();
        }
        self.selected.retain(|id| !cleaned.contains(id));
        self.refused = false;
        self.preset = None;
        self.refresh();
    }
}

/// The duplicates page: a view of the store's duplicates result.
pub(crate) struct DuplicatesPage {
    scans: Entity<Scans>,
    model: Option<DupModel>,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for DuplicatesPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DuplicatesPage")
            .field("model", &self.model.is_some())
            .finish_non_exhaustive()
    }
}

const AREA: Area = Area::Duplicates;

/// The leading grid of the results list: the header's kind icon and every copy's icon
/// share a column, and the copies' titles start where the header title starts (headers
/// have no checkbox: removing every copy is never offered).
const GRID: ui::RowGrid = ui::RowGrid::new().disclosure().check().icon();

/// A group's or copy's file-kind glyph, muted, for the grid's icon column.
fn kind_glyph(kind: FileKind, cx: &App) -> AnyElement {
    div()
        .flex_none()
        .text_color(cx.theme().muted_foreground)
        .child(kind_icon(kind).size(badge::FACT_ICON))
        .into_any_element()
}

impl DuplicatesPage {
    /// Creates the page.
    pub(crate) fn new(_window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let scans = scans::entity(cx);
        let subscriptions = super::follow(&scans, AREA, cx, Self::on_scan);
        let mut this = Self {
            scans,
            model: None,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        this.sync(cx);
        this
    }

    /// Rebuilds the model from the store's result.
    fn sync(&mut self, cx: &mut Context<'_, Self>) {
        self.model = match self.scans.read(cx).output(AREA) {
            Some(JobOutput::Duplicates(report)) => Some(DupModel::new(report.clone())),
            Some(other) => {
                tracing::warn!(?other, "unexpected duplicates output");
                None
            }
            None => None,
        };
        cx.notify();
    }

    fn on_scan(&mut self, event: &ScanEvent, _: &Entity<Scans>, cx: &mut Context<'_, Self>) {
        match event {
            ScanEvent::Scanned(_) | ScanEvent::Cleared(_) => self.sync(cx),
            ScanEvent::Cleaned { items, report, .. } => {
                self.update_model(cx, |m| m.remove_cleaned(items, report));
            }
        }
    }

    /// The page came on screen (`shown`) or left it.
    pub(crate) fn set_visible(&mut self, shown: bool, cx: &mut Context<'_, Self>) {
        self.scans.update(cx, |s, cx| {
            if shown {
                s.show(AREA, cx);
            } else {
                s.hide(AREA);
            }
        });
    }

    fn store(
        &mut self,
        cx: &mut Context<'_, Self>,
        f: impl FnOnce(&mut Scans, Area, &mut Context<'_, Scans>),
    ) {
        self.scans.update(cx, |s, cx| f(s, AREA, cx));
    }

    fn scan(&mut self, cx: &mut Context<'_, Self>) {
        self.store(cx, Scans::rescan);
    }

    fn update_model(&mut self, cx: &mut Context<'_, Self>, f: impl FnOnce(&mut DupModel)) {
        if let Some(model) = &mut self.model {
            f(model);
            cx.notify();
        }
    }

    fn ask_clean(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(model) = &self.model else { return };
        if !model.keeps_one_per_group() {
            return;
        }
        let items = model.selected_ids();
        if items.is_empty() {
            return;
        }
        let confirm = CleanConfirm {
            count: items.len(),
            bytes: model.selected_bytes(),
            removal: Removal::UserFiles,
        };
        let scans = self.scans.downgrade();
        widgets::confirm_clean(
            confirm,
            move |_, cx| {
                let items = items.clone();
                if let Err(err) = scans.update(cx, |s, cx| s.clean(AREA, items, cx)) {
                    tracing::debug!("scan store gone before cleaning: {err}");
                }
            },
            window,
            cx,
        );
    }

    fn listener(
        cx: &mut Context<'_, Self>,
        f: impl Fn(&mut Self, &mut Window, &mut Context<'_, Self>) + 'static,
    ) -> OnClick {
        Box::new(cx.listener(move |this, _, window, cx| f(this, window, cx)))
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let now = format::now();
        let rows: Vec<Row> = self
            .model
            .as_ref()
            .and_then(|m| m.rows.get(range).map(<[Row]>::to_vec))
            .unwrap_or_default();
        rows.into_iter()
            .filter_map(|row| match row {
                Row::Group(gi) => self.group_row(gi, cx),
                Row::File(gi, fi) => self.file_row(gi, fi, now, cx),
            })
            .collect()
    }

    fn group_row(&self, gi: usize, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        let model = self.model.as_ref()?;
        let group = model.groups.get(gi)?;
        let kind = group.files.first().map(|f| f.entry.kind)?;
        // The whole header folds the group once per click/Enter/Space.
        let header = ui::CollapsibleHeader::new(
            ("dupes-group", gi),
            group.title.clone(),
            !model.is_collapsed(gi),
        )
        .grid(GRID)
        .icon(kind_glyph(kind, cx))
        .summary(group.header.clone())
        // The size column of a group is what removing all but one copy frees.
        .size_label(format::bytes(group.wasted))
        .on_toggle(cx.listener(move |this, open: &bool, _, cx| {
            let open = *open;
            this.update_model(cx, |m| m.set_collapsed(gi, !open));
        }));
        // A uniform list measures one row: the 32 px header sits in a 40 px slot.
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .w_full()
                .h(row::HEIGHT_TWO_LINE)
                .child(header)
                .into_any_element(),
        )
    }

    fn file_row(
        &self,
        gi: usize,
        fi: usize,
        now: i64,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        let model = self.model.as_ref()?;
        let file = model.groups.get(gi)?.files.get(fi)?;
        let id = file.entry.id;
        let kind = file.entry.kind;
        let selected = model.selected.contains(&id);
        let age = file
            .entry
            .modified
            .and_then(|t| format::age(t, now))
            .unwrap_or_default();
        Some(
            ui::ListRow::new(
                ElementId::from(("dupes-file", u64::from(id))),
                file.name.clone(),
            )
            .detail(file.dir.clone())
            .grid(GRID)
            .icon(kind_glyph(kind, cx))
            .checkbox(widgets::check(
                ElementId::from(("dupes-check", u64::from(id))),
                selected,
                cx.listener(move |this, _, _, cx| {
                    this.update_model(cx, |m| {
                        m.toggle(gi, fi);
                    });
                }),
            ))
            .when(!selected, |this| {
                this.trailing(ui::Badge::new(tr("dupes.kept")))
            })
            .trailing(widgets::muted_cell(age, cx))
            .trailing(widgets::size_cell(file.size.clone(), cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.update_model(cx, |m| {
                    m.toggle(gi, fi);
                });
            }))
            .into_any_element(),
        )
    }

    fn render_results(&self, connected: bool, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let Some(model) = &self.model else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(denied) = widgets::denied_notice(&model.denied, cx) {
            out.push(denied);
        }
        if model.groups.is_empty() {
            out.push(widgets::empty_card(
                IconName::CircleCheck,
                tr("dupes.empty.title"),
                tr("dupes.empty.body"),
            ));
            return out;
        }
        if model.truncated {
            out.push(widgets::notice(
                Tone::Info,
                tr("scan.truncated"),
                None,
                Vec::new(),
                cx,
            ));
        }
        if model.refused() {
            out.push(widgets::notice(
                Tone::Warning,
                tr("dupes.keep_one.title"),
                Some(tr("dupes.keep_one.body")),
                Vec::new(),
                cx,
            ));
        }
        out.push(Self::render_toolbar(model, cx));
        let header = ui::CardHeader::new(
            rust_i18n::t!(
                "files2.groups",
                count = format::count(u64::try_from(model.groups.len()).unwrap_or(u64::MAX))
            )
            .to_string(),
        )
        .trailing(ui::Stat::new(
            tr("dupes.wasted_total"),
            format::bytes(model.wasted),
        ))
        .trailing(ui::Stat::new(
            tr("junk.selected_size"),
            format::bytes(model.selected_bytes()),
        ))
        .trailing(widgets::clean_button(
            "dupes-clean",
            model.selected.len(),
            connected && model.keeps_one_per_group(),
            Self::listener(cx, Self::ask_clean),
        ));
        let list = uniform_list(
            "dupes-list",
            model.rows.len(),
            cx.processor(|this, range, _, cx| this.render_rows(range, cx)),
        )
        .track_scroll(&self.scroll)
        .size_full();
        out.push(
            ui::Card::new()
                .flush()
                .flex_1()
                .min_h_0()
                .header(header)
                .child(
                    widgets::card_body()
                        .pt(row::INSET)
                        .child(widgets::list_frame(list)),
                )
                .into_any_element(),
        );
        out
    }

    /// "Select automatically" presets and "None".
    fn render_toolbar(model: &DupModel, cx: &mut Context<'_, Self>) -> AnyElement {
        // No segment is raised while no preset is in force (a manual pick ends it).
        let selected = model
            .preset()
            .and_then(|keep| KEEPS.iter().position(|(k, _)| *k == keep))
            .unwrap_or(KEEPS.len());
        let presets = KEEPS
            .iter()
            .fold(ui::Segmented::new("dupes-keep").small(), |seg, (_, key)| {
                seg.segment(tr(key))
            })
            .selected(selected)
            .on_select(cx.listener(|this, ix: &usize, _, cx| {
                if let Some((keep, _)) = KEEPS.get(*ix) {
                    let keep = *keep;
                    this.update_model(cx, |m| m.auto_select(keep));
                }
            }));
        ui::Toolbar::new()
            .w_full()
            .child(
                div()
                    .flex_none()
                    .text_size(text::CAPTION)
                    .line_height(text::CAPTION_LINE_HEIGHT)
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("dupes.auto")),
            )
            .child(presets)
            .child(div().flex_1())
            .child(
                ui::Button::new("dupes-select-none", tr("scan.select_none"))
                    .small()
                    .ghost()
                    .disabled(model.selected.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.update_model(cx, DupModel::select_none);
                    })),
            )
            .into_any_element()
    }
}

impl Render for DuplicatesPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let (flow, connected, stale) = {
            let scans = self.scans.read(cx);
            (
                scans.view(AREA),
                scans.is_connected(),
                scans.stale_age(AREA, Instant::now()),
            )
        };
        let phase = flow.phase;
        let actions = if matches!(phase, FlowPhase::Ready | FlowPhase::Cleaned) {
            vec![widgets::rescan_button(
                "dupes-rescan-header",
                connected,
                Self::listener(cx, |this, _, cx| this.scan(cx)),
            )]
        } else {
            Vec::new()
        };
        let mut column = widgets::page_column("dupes-page")
            .child(widgets::area_header(Category::Duplicates, actions, cx))
            .children(widgets::connection_notice(connected, cx));
        if let Some(error) = flow.error.clone() {
            column = column.child(widgets::error_notice(
                error,
                Self::listener(cx, |this, _, cx| this.store(cx, Scans::dismiss_error)),
                cx,
            ));
        }
        column = column.children(super::stale_notice(
            "dupes-rescan",
            stale,
            connected,
            Self::listener(cx, |this, _, cx| this.scan(cx)),
            cx,
        ));
        let body: Vec<AnyElement> = match phase {
            FlowPhase::Idle | FlowPhase::Failed => vec![widgets::idle_card(
                Category::Duplicates,
                Some(widgets::idle_scan_button(
                    "dupes-scan",
                    connected,
                    Self::listener(cx, |this, _, cx| this.scan(cx)),
                )),
            )],
            FlowPhase::Scanning | FlowPhase::Cleaning => vec![widgets::job_progress(
                "dupes-progress",
                &flow.progress,
                flow.phase.counters(),
                Self::listener(cx, |this, _, cx| this.store(cx, Scans::cancel)),
                cx,
            )],
            FlowPhase::Ready => self.render_results(connected, cx),
            FlowPhase::Cleaned => flow
                .report
                .as_ref()
                .map(|report| {
                    vec![widgets::clean_report(
                        report,
                        Self::listener(cx, |this, _, cx| this.store(cx, Scans::dismiss_report)),
                        Self::listener(cx, |this, _, cx| this.scan(cx)),
                        cx,
                    )]
                })
                .unwrap_or_default(),
        };
        let scrolls = phase != FlowPhase::Ready;
        v_flex().size_full().child(column.children(body).when(
            scrolls,
            gpui_kit::StatefulInteractiveElement::overflow_y_scroll,
        ))
    }
}

#[cfg(test)]
mod tests {
    use omc_proto::files::{DupGroup, DupReport, FileEntry, FileKind};
    use omc_proto::jobs::CleanReport;

    use super::{DupModel, Keep};

    fn file(id: u32, path: &str, modified: Option<i64>) -> FileEntry {
        FileEntry {
            id,
            path: path.to_owned(),
            bytes: 10,
            modified,
            accessed: None,
            kind: FileKind::Image,
            large: false,
            old: false,
        }
    }

    fn model() -> DupModel {
        DupModel::new(report())
    }

    fn report() -> DupReport {
        DupReport {
            groups: vec![
                DupGroup {
                    bytes: 10,
                    files: vec![
                        file(0, "/a/long/name.jpg", Some(5)),
                        file(1, "/b.jpg", Some(1)),
                    ],
                },
                DupGroup {
                    bytes: 10,
                    files: vec![
                        file(2, "/c/x.jpg", Some(7)),
                        file(3, "/c/yy.jpg", Some(9)),
                        file(4, "/d/zzz.jpg", None),
                    ],
                },
            ],
            truncated: false,
            denied: Vec::new(),
        }
    }

    #[test]
    fn groups_are_ordered_by_wasted_space() {
        assert_eq!(
            model().group_order(),
            vec![2, 0],
            "three copies waste more than two"
        );
    }

    #[test]
    fn manual_picks_always_keep_one_copy() {
        let mut model = model();
        // Group index 1 is the two-copy group after sorting.
        assert!(model.toggle(1, 0), "the first copy can go");
        assert!(!model.toggle(1, 1), "the last copy cannot");
        assert!(model.refused(), "the refusal is reported");
        assert!(model.keeps_one_per_group(), "invariant holds");
        assert!(model.toggle(1, 0), "deselecting is always allowed");
        assert!(!model.refused(), "and clears the warning");
    }

    #[test]
    fn auto_selection_keeps_the_chosen_copy() {
        let mut model = model();
        model.auto_select(Keep::Oldest);
        assert_eq!(model.selected_ids(), vec![0, 3, 4], "keep modified 1 and 7");
        model.auto_select(Keep::Newest);
        assert_eq!(model.selected_ids(), vec![1, 2, 4], "keep modified 5 and 9");
        model.auto_select(Keep::ShortestPath);
        assert_eq!(
            model.selected_ids(),
            vec![0, 3, 4],
            "keep /b.jpg and /c/x.jpg"
        );
        assert!(model.keeps_one_per_group(), "every rule keeps a copy");
        assert_eq!(model.selected_bytes(), 30, "three copies of 10 bytes");
        assert_eq!(model.preset(), Some(Keep::ShortestPath), "the preset shows");
        assert!(model.toggle(1, 0), "a manual pick");
        assert_eq!(model.preset(), None, "ends the preset");
    }

    #[test]
    fn cleaning_drops_groups_left_with_one_file() {
        let mut model = model();
        model.auto_select(Keep::Oldest);
        model.remove_cleaned(&[0, 3], &CleanReport::default());
        assert_eq!(
            model.group_order(),
            vec![2],
            "the two-copy group is resolved"
        );
        assert_eq!(
            model.selected_ids(),
            vec![4],
            "uncleaned picks stay selected"
        );
    }

    #[test]
    fn folding_hides_copies_but_keeps_their_selection() {
        let mut model = model();
        model.auto_select(Keep::Oldest);
        assert_eq!(model.row_count(), 7, "two headers and five copies");
        model.set_collapsed(0, true);
        assert_eq!(
            model.row_count(),
            4,
            "the folded group shows only its header"
        );
        assert_eq!(model.selected_bytes(), 30, "hidden picks still count");
        model.set_collapsed(0, false);
        assert_eq!(model.row_count(), 7, "unfolded");
    }

    use gpui_kit::TestAppContext;
    use gpui_kit::test::TestWindowExt as _;
    use omc_proto::jobs::JobOutput;

    use super::DuplicatesPage;
    use gpui_kit::component::WindowExt as _;

    use crate::pages::widgets::test_support::open_page;
    use crate::scans::{self, Area};

    #[gpui_kit::test]
    fn page_renders_groups_and_asks_before_cleaning(cx: &mut TestAppContext) {
        let Some((window, page)) = open_page(cx, DuplicatesPage::new) else {
            return;
        };
        let store = cx.update(scans::entity);
        store.update(cx, |s, cx| {
            s.force_scanned(Area::Duplicates, JobOutput::Duplicates(report()), cx);
        });
        cx.run_until_parked();
        let clicked = window.update(cx, |_, window, cx| {
            window.render_frame(cx);
            window.click(gpui_kit::ElementId::from(("dupes-group", 0_usize)), cx);
        });
        assert!(clicked.is_ok(), "the window is alive");
        cx.run_until_parked();
        let folded = cx.update(|cx| page.read(cx).model.as_ref().map(|m| m.is_collapsed(0)));
        assert_eq!(
            folded,
            Some(true),
            "one click on a group header folds it once"
        );
        page.update(cx, |page, cx| {
            page.update_model(cx, |m| m.auto_select(Keep::Oldest));
        });
        cx.run_until_parked();
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
}
