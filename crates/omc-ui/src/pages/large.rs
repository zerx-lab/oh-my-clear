//! Large and old files: one scan, then filter (all / large / old, file kinds), sort (size,
//! age, name) and pick files to remove. Filtering and sorting run when the user changes
//! them, never in `render`.

use std::collections::HashSet;
use std::ops::Range;
use std::path::Path;
use std::time::Instant;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, UniformListScrollHandle, Window, div, uniform_list,
};
use omc_proto::files::{FileEntry, FileKind, FileReport};
use omc_proto::jobs::{CleanReport, Denied, ItemId, JobOutput};

use super::widgets::{self, CleanConfirm, FlowPhase, OnClick, Removal, Tone, tr};
use crate::format;
use crate::nav::Category;
use crate::scans::{self, Area, ScanEvent, Scans};
use crate::tokens::{row, text};
use crate::ui;

/// A file with its display strings, computed once per scan.
#[derive(Debug, Clone)]
pub(crate) struct FileView {
    /// The file.
    pub(crate) entry: FileEntry,
    /// File name.
    pub(crate) name: SharedString,
    /// Containing folder, home shown as `~`.
    pub(crate) dir: SharedString,
    /// Formatted size.
    pub(crate) size: SharedString,
}

impl FileView {
    /// Wraps an entry.
    pub(crate) fn new(entry: FileEntry) -> Self {
        let path = Path::new(&entry.path);
        let name = path
            .file_name()
            .map_or_else(|| entry.path.clone(), |n| n.to_string_lossy().into_owned());
        let dir = path
            .parent()
            .map_or_else(|| entry.path.clone(), |d| d.display().to_string());
        Self {
            name: name.into(),
            dir: format::tilde(&dir),
            size: format::bytes(entry.bytes),
            entry,
        }
    }
}

/// Localised file kind.
pub(crate) fn kind_label(kind: FileKind) -> SharedString {
    tr(match kind {
        FileKind::Video => "files.kind.video",
        FileKind::Audio => "files.kind.audio",
        FileKind::Image => "files.kind.image",
        FileKind::Archive => "files.kind.archive",
        FileKind::DiskImage => "files.kind.disk_image",
        FileKind::Document => "files.kind.document",
        FileKind::VirtualMachine => "files.kind.virtual_machine",
        FileKind::Other => "files.kind.other",
    })
}

/// Icon of a file kind.
pub(crate) fn kind_icon(kind: FileKind) -> Icon {
    Icon::new(match kind {
        FileKind::Video => Lucide::Film,
        FileKind::Audio => Lucide::Music,
        FileKind::Image => Lucide::Image,
        FileKind::Archive => Lucide::FileArchive,
        FileKind::DiskImage => Lucide::Disc3,
        FileKind::Document => Lucide::FileText,
        FileKind::VirtualMachine => Lucide::AppWindow,
        FileKind::Other => Lucide::File,
    })
}

/// Every file kind, in chip order.
const KINDS: [FileKind; 8] = [
    FileKind::Video,
    FileKind::Audio,
    FileKind::Image,
    FileKind::Archive,
    FileKind::DiskImage,
    FileKind::Document,
    FileKind::VirtualMachine,
    FileKind::Other,
];

/// Which matches are listed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Filter {
    /// Everything the scan found.
    #[default]
    All,
    /// Above the size threshold.
    Large,
    /// Untouched for long.
    Old,
}

/// Filter segments, in order.
const FILTERS: [(Filter, &str); 3] = [
    (Filter::All, "files.filter.all"),
    (Filter::Large, "files.filter.large"),
    (Filter::Old, "files.filter.old"),
];

/// List order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Sort {
    /// Largest first.
    #[default]
    Size,
    /// Least recently modified first (unknown dates last).
    Age,
    /// By file name, case-insensitive.
    Name,
}

/// Sort segments, in order.
const SORTS: [(Sort, &str); 3] = [
    (Sort::Size, "files.sort.size"),
    (Sort::Age, "files.sort.age"),
    (Sort::Name, "files.sort.name"),
];

/// Scan results with filters, order and selection. Pure data.
#[derive(Debug, Default)]
pub(crate) struct LargeModel {
    files: Vec<FileView>,
    denied: Vec<Denied>,
    truncated: bool,
    filter: Filter,
    sort: Sort,
    /// Kinds shown; empty = all.
    kinds: HashSet<FileKind>,
    /// Kinds present in the results (chips).
    present: Vec<FileKind>,
    selected: HashSet<ItemId>,
    /// Indices into `files`, filtered and sorted.
    visible: Vec<usize>,
    /// How many listed files are selected.
    visible_selected: usize,
    selected_bytes: u64,
}

impl LargeModel {
    /// Builds the model; nothing is selected.
    pub(crate) fn new(report: FileReport) -> Self {
        let present: Vec<FileKind> = KINDS
            .into_iter()
            .filter(|k| report.files.iter().any(|f| f.kind == *k))
            .collect();
        let mut model = Self {
            files: report.files.into_iter().map(FileView::new).collect(),
            denied: report.denied,
            truncated: report.truncated,
            present,
            ..Self::default()
        };
        model.refresh();
        model
    }

    fn refresh(&mut self) {
        let mut visible: Vec<usize> = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| match self.filter {
                Filter::All => true,
                Filter::Large => f.entry.large,
                Filter::Old => f.entry.old,
            })
            .filter(|(_, f)| self.kinds.is_empty() || self.kinds.contains(&f.entry.kind))
            .map(|(ix, _)| ix)
            .collect();
        let files = &self.files;
        match self.sort {
            Sort::Size => visible
                .sort_by_key(|ix| std::cmp::Reverse(files.get(*ix).map_or(0, |f| f.entry.bytes))),
            Sort::Age => visible.sort_by_key(|ix| {
                files
                    .get(*ix)
                    .and_then(|f| f.entry.modified)
                    .unwrap_or(i64::MAX)
            }),
            Sort::Name => visible.sort_by_cached_key(|ix| {
                files
                    .get(*ix)
                    .map(|f| f.name.to_lowercase())
                    .unwrap_or_default()
            }),
        }
        self.visible_selected = visible
            .iter()
            .filter(|ix| {
                files
                    .get(**ix)
                    .is_some_and(|f| self.selected.contains(&f.entry.id))
            })
            .count();
        self.visible = visible;
        self.selected_bytes = self
            .files
            .iter()
            .filter(|f| self.selected.contains(&f.entry.id))
            .fold(0_u64, |sum, f| sum.saturating_add(f.entry.bytes));
    }

    /// Shows only `filter` matches.
    pub(crate) fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.refresh();
    }

    /// Orders by `sort`.
    pub(crate) fn set_sort(&mut self, sort: Sort) {
        self.sort = sort;
        self.refresh();
    }

    /// Shows or hides one file kind (none chosen = all kinds).
    pub(crate) fn toggle_kind(&mut self, kind: FileKind) {
        if !self.kinds.remove(&kind) {
            self.kinds.insert(kind);
        }
        self.refresh();
    }

    /// Flips one file.
    pub(crate) fn toggle(&mut self, id: ItemId) {
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
        self.refresh();
    }

    /// Selects every listed file (hidden ones keep their state).
    pub(crate) fn select_visible(&mut self) {
        let ids: Vec<ItemId> = self
            .visible
            .iter()
            .filter_map(|ix| self.files.get(*ix).map(|f| f.entry.id))
            .collect();
        self.selected.extend(ids);
        self.refresh();
    }

    /// Deselects every listed file (hidden ones keep their state).
    pub(crate) fn deselect_visible(&mut self) {
        for ix in &self.visible {
            if let Some(file) = self.files.get(*ix) {
                self.selected.remove(&file.entry.id);
            }
        }
        self.refresh();
    }

    /// Selection state of the listed files, for the column header's checkbox.
    fn visible_state(&self) -> ui::CheckState {
        ui::CheckState::from_counts(self.visible_selected, self.visible.len())
    }

    /// Ids of the listed files, in list order.
    #[cfg(test)]
    pub(crate) fn visible_ids(&self) -> Vec<ItemId> {
        self.visible
            .iter()
            .filter_map(|ix| self.files.get(*ix).map(|f| f.entry.id))
            .collect()
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

    /// Drops removed files, except those whose path failed.
    pub(crate) fn remove_cleaned(&mut self, cleaned: &[ItemId], report: &CleanReport) {
        let failed: HashSet<String> = report
            .failures
            .iter()
            .map(|f| f.location.display())
            .collect();
        let cleaned: HashSet<ItemId> = cleaned.iter().copied().collect();
        self.files
            .retain(|f| !cleaned.contains(&f.entry.id) || failed.contains(&f.entry.path));
        self.selected.retain(|id| !cleaned.contains(id));
        self.refresh();
    }
}

/// The large & old files page: a view of the store's large-files result.
pub(crate) struct LargeFilesPage {
    scans: Entity<Scans>,
    model: Option<LargeModel>,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for LargeFilesPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LargeFilesPage")
            .field("model", &self.model.is_some())
            .finish_non_exhaustive()
    }
}

const AREA: Area = Area::LargeOldFiles;

/// The leading grid of the file table: checkbox, then the file-kind icon; the column
/// header puts "select listed" in the same checkbox column.
const GRID: ui::RowGrid = ui::RowGrid::new().check().icon();

impl LargeFilesPage {
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
            Some(JobOutput::Files(report)) => Some(LargeModel::new(report.clone())),
            Some(other) => {
                tracing::warn!(?other, "unexpected large-files output");
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

    fn update_model(&mut self, cx: &mut Context<'_, Self>, f: impl FnOnce(&mut LargeModel)) {
        if let Some(model) = &mut self.model {
            f(model);
            cx.notify();
        }
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
        range.filter_map(|row| self.row(row, now, cx)).collect()
    }

    fn row(&self, row: usize, now: i64, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        let model = self.model.as_ref()?;
        let file = model.files.get(*model.visible.get(row)?)?;
        let id = file.entry.id;
        let kind = file.entry.kind;
        let selected = model.selected.contains(&id);
        let age = |t: Option<i64>| t.and_then(|t| format::age(t, now)).unwrap_or_default();
        Some(
            ui::ListRow::new(
                ElementId::from(("large-row", u64::from(id))),
                file.name.clone(),
            )
            .detail(file.dir.clone())
            .grid(GRID)
            .checkbox(widgets::check(
                ElementId::from(("large-check", u64::from(id))),
                selected,
                cx.listener(move |this, _, _, cx| {
                    this.update_model(cx, |m| m.toggle(id));
                }),
            ))
            .icon(ui::FactIcon::new(
                ElementId::from(("large-kind", u64::from(id))),
                kind_icon(kind),
                kind_label(kind),
            ))
            .trailing(widgets::muted_cell(age(file.entry.modified), cx))
            .trailing(widgets::muted_cell(age(file.entry.accessed), cx))
            .trailing(widgets::size_cell(file.size.clone(), cx))
            .on_click(cx.listener(move |this, _, _, cx| this.update_model(cx, |m| m.toggle(id))))
            .into_any_element(),
        )
    }

    /// Filter segments and kind chips on the left, the sort order on the right.
    fn render_controls(model: &LargeModel, cx: &mut Context<'_, Self>) -> AnyElement {
        let filter = FILTERS
            .iter()
            .fold(
                ui::Segmented::new("large-filter").small(),
                |seg, (_, key)| seg.segment(tr(key)),
            )
            .selected(
                FILTERS
                    .iter()
                    .position(|(f, _)| *f == model.filter)
                    .unwrap_or_default(),
            )
            .on_select(cx.listener(|this, ix: &usize, _, cx| {
                if let Some((filter, _)) = FILTERS.get(*ix) {
                    let filter = *filter;
                    this.update_model(cx, |m| m.set_filter(filter));
                }
            }));
        let sort = SORTS
            .iter()
            .fold(ui::Segmented::new("large-sort").small(), |seg, (_, key)| {
                seg.segment(tr(key))
            })
            .selected(
                SORTS
                    .iter()
                    .position(|(s, _)| *s == model.sort)
                    .unwrap_or_default(),
            )
            .on_select(cx.listener(|this, ix: &usize, _, cx| {
                if let Some((sort, _)) = SORTS.get(*ix) {
                    let sort = *sort;
                    this.update_model(cx, |m| m.set_sort(sort));
                }
            }));
        // One kind alone filters nothing: chips appear from two kinds on.
        let kinds = if model.present.len() > 1 {
            model
                .present
                .iter()
                .enumerate()
                .map(|(ix, kind)| {
                    let kind = *kind;
                    widgets::chip(
                        ("large-kind-chip", ix),
                        kind_label(kind),
                        model.kinds.contains(&kind),
                        Box::new(cx.listener(move |this, _, _, cx| {
                            this.update_model(cx, |m| m.toggle_kind(kind));
                        })),
                    )
                    .into_any_element()
                })
                .collect()
        } else {
            Vec::new()
        };
        ui::Toolbar::new()
            .w_full()
            .child(filter)
            .children(kinds)
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_size(text::CAPTION)
                    .line_height(text::CAPTION_LINE_HEIGHT)
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("files.sort.label")),
            )
            .child(sort)
            .into_any_element()
    }

    /// Checkbox for the listed files, then Name, Modified, Opened and Size.
    fn render_columns(model: &LargeModel, cx: &mut Context<'_, Self>) -> AnyElement {
        let check = ui::Checkbox::new("large-check-listed")
            .state(model.visible_state())
            .tooltip(tr("files.select_listed"))
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                let checked = *checked;
                this.update_model(cx, |m| {
                    if checked {
                        m.select_visible();
                    } else {
                        m.deselect_visible();
                    }
                });
            }));
        let column = |width, key: &str| div().flex_none().w(width).child(tr(key));
        widgets::column_header(
            GRID,
            Some(check.into_any_element()),
            tr("files.sort.name"),
            vec![
                column(row::AGE_COLUMN, "files.col.modified").into_any_element(),
                column(row::AGE_COLUMN, "files.col.accessed").into_any_element(),
                column(row::SIZE_COLUMN, "files.col.size")
                    .text_right()
                    .into_any_element(),
            ],
            cx,
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
        if model.files.is_empty() {
            out.push(widgets::empty_card(
                IconName::CircleCheck,
                tr("files.empty.title"),
                tr("files.empty.body"),
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
        out.push(Self::render_controls(model, cx));
        let header = ui::CardHeader::new(ui::item_count(model.visible.len()))
            .trailing(ui::Stat::new(
                tr("junk.selected_size"),
                format::bytes(model.selected_bytes()),
            ))
            .trailing(widgets::clean_button(
                "large-clean",
                model.selected.len(),
                connected,
                Self::listener(cx, Self::ask_clean),
            ));
        let list = uniform_list(
            "large-list",
            model.visible.len(),
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
                        .child(Self::render_columns(model, cx))
                        .child(widgets::list_frame(list)),
                )
                .into_any_element(),
        );
        out
    }
}

impl Render for LargeFilesPage {
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
                "large-rescan-header",
                connected,
                Self::listener(cx, |this, _, cx| this.scan(cx)),
            )]
        } else {
            Vec::new()
        };
        let mut column = widgets::page_column("large-page")
            .child(widgets::area_header(Category::LargeOldFiles, actions, cx))
            .children(widgets::connection_notice(connected, cx));
        if let Some(error) = flow.error.clone() {
            column = column.child(widgets::error_notice(
                error,
                Self::listener(cx, |this, _, cx| this.store(cx, Scans::dismiss_error)),
                cx,
            ));
        }
        column = column.children(super::stale_notice(
            "large-rescan",
            stale,
            connected,
            Self::listener(cx, |this, _, cx| this.scan(cx)),
            cx,
        ));
        let body: Vec<AnyElement> = match phase {
            FlowPhase::Idle | FlowPhase::Failed => vec![widgets::idle_card(
                Category::LargeOldFiles,
                Some(widgets::idle_scan_button(
                    "large-scan",
                    connected,
                    Self::listener(cx, |this, _, cx| this.scan(cx)),
                )),
            )],
            FlowPhase::Scanning | FlowPhase::Cleaning => vec![widgets::job_progress(
                "large-progress",
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
    use omc_proto::files::{FileEntry, FileKind, FileReport};
    use omc_proto::jobs::CleanReport;

    use super::{Filter, LargeModel, Sort};

    fn file(
        id: u32,
        name: &str,
        bytes: u64,
        modified: Option<i64>,
        kind: FileKind,
        large: bool,
        old: bool,
    ) -> FileEntry {
        FileEntry {
            id,
            path: format!("/data/{name}"),
            bytes,
            modified,
            accessed: None,
            kind,
            large,
            old,
        }
    }

    fn model() -> LargeModel {
        LargeModel::new(report())
    }

    fn report() -> FileReport {
        FileReport {
            files: vec![
                file(0, "b.mkv", 300, Some(50), FileKind::Video, true, false),
                file(1, "A.zip", 100, Some(10), FileKind::Archive, false, true),
                file(2, "c.iso", 200, None, FileKind::DiskImage, true, true),
            ],
            truncated: false,
            denied: Vec::new(),
        }
    }

    #[test]
    fn filters_and_sorts_reorder_the_listed_files() {
        let mut model = model();
        assert_eq!(model.visible_ids(), vec![0, 2, 1], "largest first");
        model.set_sort(Sort::Age);
        assert_eq!(
            model.visible_ids(),
            vec![1, 0, 2],
            "oldest first, unknown last"
        );
        model.set_sort(Sort::Name);
        assert_eq!(model.visible_ids(), vec![1, 0, 2], "case-insensitive names");
        model.set_filter(Filter::Old);
        assert_eq!(model.visible_ids(), vec![1, 2], "old only");
        model.set_filter(Filter::Large);
        model.toggle_kind(FileKind::Video);
        assert_eq!(model.visible_ids(), vec![0], "large videos");
        model.toggle_kind(FileKind::Video);
        assert_eq!(
            model.visible_ids(),
            vec![0, 2],
            "no kind chosen = all kinds"
        );
    }

    #[test]
    fn selection_survives_filters_and_cleaning() {
        let mut model = model();
        model.set_filter(Filter::Old);
        model.select_visible();
        assert_eq!(model.selected_ids(), vec![1, 2], "select what is listed");
        assert_eq!(model.selected_bytes(), 300, "selected size");
        model.set_filter(Filter::All);
        assert_eq!(
            model.selected_ids(),
            vec![1, 2],
            "hidden files keep their state"
        );
        model.remove_cleaned(&[1, 2], &CleanReport::default());
        assert_eq!(model.visible_ids(), vec![0], "cleaned files leave the list");
        assert!(model.selected_ids().is_empty(), "and the selection");
    }

    #[test]
    fn listed_selection_is_tri_state_and_leaves_hidden_files_alone() {
        let mut model = model();
        model.set_filter(Filter::Old);
        model.toggle(1);
        assert_eq!(
            model.visible_state(),
            crate::ui::CheckState::Indeterminate,
            "one of two listed files"
        );
        model.set_filter(Filter::All);
        model.toggle(0);
        model.set_filter(Filter::Old);
        model.deselect_visible();
        assert_eq!(
            model.selected_ids(),
            vec![0],
            "deselecting the listed files keeps the hidden pick"
        );
    }

    use gpui_kit::TestAppContext;
    use omc_proto::jobs::JobOutput;

    use super::LargeFilesPage;
    use gpui_kit::component::WindowExt as _;

    use crate::pages::widgets::test_support::open_page;
    use crate::scans::{self, Area};

    #[gpui_kit::test]
    fn page_renders_results_and_asks_before_cleaning(cx: &mut TestAppContext) {
        let Some((window, page)) = open_page(cx, LargeFilesPage::new) else {
            return;
        };
        let store = cx.update(scans::entity);
        store.update(cx, |s, cx| {
            s.force_scanned(Area::LargeOldFiles, JobOutput::Files(report()), cx);
        });
        cx.run_until_parked();
        page.update(cx, |page, cx| {
            page.update_model(cx, LargeModel::select_visible);
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
