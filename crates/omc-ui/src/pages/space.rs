//! Space lens: a squarified treemap of the measured folder, two levels deep (each child
//! folder subdivided by its own largest children, fetched with `space_children` after
//! every navigation), beside a ranked list of the folder's children. Click a folder tile
//! (either level) or row to drill in; breadcrumb, Up, Backspace and Escape go back. Files
//! and folders can be selected across levels and removed together.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, AppContext as _, AsyncApp, Context, ElementId,
    Entity, FocusHandle, FontWeight, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled,
    Subscription, Task, UniformListScrollHandle, WeakEntity, Window, div, uniform_list,
};
use omc_ipc::client::{ClientEvent, ConnState};
use omc_proto::files::{SpaceKind, SpaceListing, SpaceNode};
use omc_proto::jobs::{CleanReport, ItemId, JobOutput, JobSpec, ScanArea};
use omc_proto::settings::SystemInfo;

use super::widgets::{self, CleanConfirm, FlowPhase, OnClick, Removal, Tone, tr};
use crate::engine;
use crate::format;
use crate::jobs;
use crate::nav::Category;
use crate::scans::{self, Area, ScanEvent, Scans};
use crate::theme::color::oklch;
use crate::tokens::{control, row, space, text, treemap};
use crate::ui::{self, MapTile, Rect, Slot, TileLabel};

const AREA: Area = Area::SpaceLens;

/// A selected node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Picked {
    /// Absolute path.
    pub(crate) path: String,
    /// Its size.
    pub(crate) bytes: u64,
}

/// Nodes selected across levels. Never holds a node together with one of its ancestors,
/// so the total is not counted twice.
#[derive(Debug, Default)]
pub(crate) struct SpaceSelection {
    picked: BTreeMap<ItemId, Picked>,
}

/// `path` lies strictly below `ancestor`.
fn is_below(path: &str, ancestor: &str) -> bool {
    Path::new(path).starts_with(ancestor) && Path::new(path) != Path::new(ancestor)
}

impl SpaceSelection {
    /// Selects or deselects a node; selecting a folder drops its selected descendants.
    pub(crate) fn toggle(&mut self, id: ItemId, path: String, bytes: u64) {
        if self.picked.remove(&id).is_some() || self.covers(&path) {
            return;
        }
        self.picked.retain(|_, p| !is_below(&p.path, &path));
        self.picked.insert(id, Picked { path, bytes });
    }

    /// Whether `id` is selected itself.
    pub(crate) fn contains(&self, id: ItemId) -> bool {
        self.picked.contains_key(&id)
    }

    /// Whether a selected folder already contains `path`.
    pub(crate) fn covers(&self, path: &str) -> bool {
        self.picked.values().any(|p| is_below(path, &p.path))
    }

    /// Selected ids, ascending.
    pub(crate) fn ids(&self) -> Vec<ItemId> {
        self.picked.keys().copied().collect()
    }

    /// Number of selected nodes.
    pub(crate) fn len(&self) -> usize {
        self.picked.len()
    }

    /// Their total size.
    pub(crate) fn bytes(&self) -> u64 {
        self.picked
            .values()
            .fold(0_u64, |sum, p| sum.saturating_add(p.bytes))
    }

    /// Forgets everything.
    pub(crate) fn clear(&mut self) {
        self.picked.clear();
    }
}

/// Absolute path of `name` inside `parent`.
fn child_path(parent: &str, name: &str) -> String {
    Path::new(parent).join(name).display().to_string()
}

// ---- Navigation ----

/// What asking to open a folder did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    /// Shown at once from its prefetched listing.
    Shown,
    /// Must be fetched with `space_children(node)`; the answer carries `rev`.
    Fetch { rev: u64, node: ItemId },
    /// Not a folder of the current level or of its prefetched children.
    Ignored,
}

/// The navigation stack plus the level-2 listings of the current level's largest child
/// folders. `rev` changes on every navigation; answers carrying an older one are dropped.
#[derive(Debug, Default)]
struct Lens {
    /// Opened levels, root first.
    levels: Vec<SpaceListing>,
    /// Listings of the current level's child folders, by their node id.
    sub: BTreeMap<ItemId, SpaceListing>,
    rev: u64,
    /// The folder being fetched for opening.
    opening: Option<ItemId>,
}

impl Lens {
    /// Starts over at `root` (or nothing).
    fn reset(&mut self, root: Option<SpaceListing>) {
        self.levels = root.into_iter().collect();
        self.sub.clear();
        self.bump();
    }

    fn bump(&mut self) {
        self.rev = self.rev.wrapping_add(1);
        self.opening = None;
    }

    fn current(&self) -> Option<&SpaceListing> {
        self.levels.last()
    }

    fn has_dir(listing: &SpaceListing, node: ItemId) -> bool {
        listing
            .children
            .iter()
            .any(|c| c.id == node && c.kind == SpaceKind::Dir)
    }

    /// Opens `node`: a child folder of the current level (instant when prefetched), or a
    /// folder inside one of those (its parent becomes the current level first).
    fn open(&mut self, node: ItemId) -> Open {
        let Some(current) = self.levels.last() else {
            return Open::Ignored;
        };
        if Self::has_dir(current, node) {
            if let Some(listing) = self.sub.remove(&node) {
                self.levels.push(listing);
                self.sub.clear();
                self.bump();
                return Open::Shown;
            }
            self.bump();
            self.opening = Some(node);
            return Open::Fetch {
                rev: self.rev,
                node,
            };
        }
        let Some(parent) = self
            .sub
            .iter()
            .find(|(_, listing)| Self::has_dir(listing, node))
            .map(|(&id, _)| id)
        else {
            return Open::Ignored;
        };
        let Some(listing) = self.sub.remove(&parent) else {
            return Open::Ignored;
        };
        self.levels.push(listing);
        self.sub.clear();
        self.bump();
        self.opening = Some(node);
        Open::Fetch {
            rev: self.rev,
            node,
        }
    }

    /// The fetched level arrived; `false` (dropped) when it is stale.
    fn opened(&mut self, rev: u64, listing: SpaceListing) -> bool {
        if rev != self.rev || self.opening != Some(listing.node) {
            return false;
        }
        self.levels.push(listing);
        self.sub.clear();
        self.opening = None;
        true
    }

    /// Fetching the level failed; `false` when that request is stale anyway.
    fn open_failed(&mut self, rev: u64) -> bool {
        if rev != self.rev {
            return false;
        }
        self.opening = None;
        true
    }

    /// A child folder's listing arrived; `false` (dropped) when stale or unrelated.
    fn sub_arrived(&mut self, rev: u64, listing: SpaceListing) -> bool {
        if rev != self.rev || self.opening.is_some() {
            return false;
        }
        let Some(current) = self.levels.last() else {
            return false;
        };
        if !Self::has_dir(current, listing.node) {
            return false;
        }
        self.sub.insert(listing.node, listing);
        true
    }

    /// Shows level `depth` (0 = root) again, cancelling a pending open.
    fn go_to(&mut self, depth: usize) -> bool {
        let keep = depth.saturating_add(1).max(1);
        let shrinks = keep < self.levels.len();
        if !shrinks && self.opening.is_none() {
            return false;
        }
        if shrinks {
            self.levels.truncate(keep);
            self.sub.clear();
        }
        self.bump();
        true
    }

    /// One level up.
    fn up(&mut self) -> bool {
        match self.levels.len().checked_sub(2) {
            Some(depth) => self.go_to(depth),
            None => false,
        }
    }

    /// Child folders of the current level whose listings are still missing: the
    /// [`treemap::PREFETCH`] largest.
    fn prefetch(&self) -> Vec<ItemId> {
        if self.opening.is_some() {
            return Vec::new();
        }
        self.current()
            .map(|level| {
                level
                    .children
                    .iter()
                    .filter(|c| c.kind == SpaceKind::Dir && c.bytes > 0)
                    .take(treemap::PREFETCH)
                    .filter(|c| !self.sub.contains_key(&c.id))
                    .map(|c| c.id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Removed nodes leave every listing (paths in `failed` stay); sizes of their
    /// parents stay as measured until the next scan.
    fn remove(&mut self, items: &[ItemId], failed: &[String]) {
        let prune = |listing: &mut SpaceListing| {
            let parent = listing.path.clone();
            listing.children.retain(|node| {
                !items.contains(&node.id) || failed.contains(&child_path(&parent, &node.name))
            });
        };
        self.levels.iter_mut().for_each(prune);
        self.sub.values_mut().for_each(prune);
    }
}

// ---- Palette ----

/// Colours of one hue.
#[derive(Debug, Clone)]
struct Hue {
    dir: Hsla,
    file: Hsla,
    nested_dir: [Hsla; 2],
    nested_file: [Hsla; 2],
    strong: Hsla,
    ink: Hsla,
}

/// The map's colours: one restrained OKLCH hue per level-1 child, starting at the
/// accent's hue. Built once per theme (mode + accent), never per render.
#[derive(Debug, Clone)]
struct LensPalette {
    key: (bool, Hsla),
    hues: Vec<Hue>,
    neutral: Hsla,
    stripe: Hsla,
    neutral_ink: Hsla,
    neutral_strong: Hsla,
}

impl LensPalette {
    fn new(dark: bool, accent: Hsla) -> Self {
        let s = if dark { treemap::DARK } else { treemap::LIGHT };
        let mut hue = accent.h * 360.;
        let mut hues = Vec::with_capacity(treemap::HUES);
        for _ in 0..treemap::HUES {
            let h = hue.rem_euclid(360.);
            let shade = |(l, c): (f32, f32), dl: f32| oklch(l + dl, c, h);
            hues.push(Hue {
                dir: shade(s.dir, 0.),
                file: shade(s.file, 0.),
                nested_dir: [shade(s.dir, s.nested), shade(s.dir, 2. * s.nested)],
                nested_file: [shade(s.file, s.nested), shade(s.file, 2. * s.nested)],
                strong: shade(s.strong, 0.),
                ink: shade(s.ink, 0.),
            });
            hue += treemap::HUE_STEP;
        }
        Self {
            key: (dark, accent),
            hues,
            neutral: oklch(s.neutral.0, s.neutral.1, 0.),
            stripe: oklch(s.neutral.0 + s.stripe, s.neutral.1, 0.),
            neutral_ink: oklch(s.ink.0, 0., 0.),
            neutral_strong: oklch(s.strong.0, 0., 0.),
        }
    }

    fn hue(&self, row: usize) -> Option<&Hue> {
        row.checked_rem(self.hues.len())
            .and_then(|ix| self.hues.get(ix))
    }

    /// Fill of a folder/file tile of level-1 child `row`; `nested` = level-2 position.
    fn fill(&self, row: usize, kind: SpaceKind, nested: Option<usize>) -> Hsla {
        let Some(hue) = self.hue(row) else {
            return self.neutral;
        };
        let pick = |pair: &[Hsla; 2], j: usize| {
            pair.get(j.checked_rem(2).unwrap_or(0))
                .copied()
                .unwrap_or(hue.dir)
        };
        match (kind, nested) {
            (SpaceKind::Rest, _) => self.neutral,
            (SpaceKind::Dir, None) => hue.dir,
            (SpaceKind::File, None) => hue.file,
            (SpaceKind::Dir, Some(j)) => pick(&hue.nested_dir, j),
            (SpaceKind::File, Some(j)) => pick(&hue.nested_file, j),
        }
    }

    fn ink(&self, row: Option<usize>) -> Hsla {
        row.and_then(|r| self.hue(r))
            .map_or(self.neutral_ink, |h| h.ink)
    }

    fn strong(&self, row: usize) -> Hsla {
        self.hue(row).map_or(self.neutral_strong, |h| h.strong)
    }
}

// ---- Map model ----

/// What a tile stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileKind {
    Dir,
    File,
    /// A directory's small files, aggregated by the scan.
    Rest,
    /// Items too small to draw, merged by the layout.
    Other,
}

/// Display and action data of one map tile.
#[derive(Debug, Clone)]
struct TileInfo {
    node: Option<ItemId>,
    kind: TileKind,
    name: SharedString,
    path: Option<String>,
    bytes: u64,
    files: u64,
    /// The level-1 child this tile is or lies in.
    row: Option<usize>,
    nested: bool,
}

/// The laid-out map: tiles in paint order (level 1, then level 2), their data, and the
/// tile of each level-1 child (row of the ranked list).
#[derive(Debug, Clone, Default)]
struct MapModel {
    tiles: Arc<[MapTile]>,
    info: Vec<TileInfo>,
    row_tile: Vec<Option<usize>>,
}

/// Where the pointer is: over a map tile or a list row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hover {
    Tile(usize),
    Row(usize),
}

/// Direction of a keyboard step between level-1 tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Left,
    Right,
    Up,
    Down,
}

impl MapModel {
    /// The tile to outline for `hover` (a row outlines its tile).
    fn linked_tile(&self, hover: Option<Hover>) -> Option<usize> {
        match hover? {
            Hover::Tile(t) => Some(t),
            Hover::Row(r) => self.row_tile.get(r).copied().flatten(),
        }
    }

    /// The row to highlight for `hover` (a tile highlights its level-1 row).
    fn linked_row(&self, hover: Option<Hover>) -> Option<usize> {
        match hover? {
            Hover::Tile(t) => self.info.get(t)?.row,
            Hover::Row(r) => Some(r),
        }
    }

    /// Tile of node `node` (or the level-1 "other" tile for `None`).
    fn tile_of(&self, node: Option<ItemId>, nested: bool) -> Option<usize> {
        self.info
            .iter()
            .position(|i| i.node == node && (node.is_some() || !i.nested || nested))
    }

    /// The level-1 tile next to `from` in direction `step` (nearest centre, favouring
    /// alignment); the first level-1 tile when there is no cursor yet.
    fn neighbour(&self, from: Option<usize>, step: Step) -> Option<usize> {
        let level1 = self
            .info
            .iter()
            .zip(self.tiles.iter())
            .enumerate()
            .filter(|(_, (info, _))| !info.nested)
            .map(|(ix, (_, tile))| (ix, tile.rect));
        let Some(from) = from.and_then(|f| self.tiles.get(f).map(|t| (f, t.rect))) else {
            return self.info.iter().position(|i| !i.nested);
        };
        let centre = |r: Rect| (r.x + r.w / 2., r.y + r.h / 2.);
        let (fx, fy) = centre(from.1);
        level1
            .filter(|&(ix, _)| ix != from.0)
            .filter_map(|(ix, r)| {
                let (x, y) = centre(r);
                let (along, across) = match step {
                    Step::Left => (fx - x, y - fy),
                    Step::Right => (x - fx, y - fy),
                    Step::Up => (fy - y, x - fx),
                    Step::Down => (y - fy, x - fx),
                };
                (along > 0.).then_some((ix, along + 2. * across.abs()))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(ix, _)| ix)
    }
}

/// "N small files".
fn rest_name(files: u64) -> SharedString {
    rust_i18n::t!("space.rest", n = format::count(files))
        .to_string()
        .into()
}

/// "N smaller items".
fn other_name(count: u64) -> SharedString {
    rust_i18n::t!("files2.lens.other", n = format::count(count))
        .to_string()
        .into()
}

/// How a tile is labelled.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LabelFit {
    None,
    TwoLine,
    Inline,
}

/// Two-line label when the tile is large enough.
fn fit(rect: Rect) -> LabelFit {
    if rect.w >= treemap::LABEL_MIN_WIDTH && rect.h >= treemap::LABEL_MIN_HEIGHT {
        LabelFit::TwoLine
    } else {
        LabelFit::None
    }
}

fn tile(
    rect: Rect,
    info: &TileInfo,
    fill: Hsla,
    hatch: Option<Hsla>,
    ink: Hsla,
    label: LabelFit,
) -> MapTile {
    MapTile {
        rect,
        fill,
        hatch,
        radius: if info.nested {
            treemap::RADIUS_NESTED
        } else {
            treemap::RADIUS
        },
        label: (label != LabelFit::None).then(|| TileLabel {
            name: info.name.clone(),
            size: format::bytes(info.bytes),
            color: ink,
            inline: label == LabelFit::Inline,
        }),
    }
}

/// Info of child `node` of the listing at `parent`.
fn node_info(node: &SpaceNode, parent: &str, row: Option<usize>, nested: bool) -> TileInfo {
    let (kind, name, path) = match node.kind {
        SpaceKind::Dir => (
            TileKind::Dir,
            node.name.clone().into(),
            Some(child_path(parent, &node.name)),
        ),
        SpaceKind::File => (
            TileKind::File,
            node.name.clone().into(),
            Some(child_path(parent, &node.name)),
        ),
        SpaceKind::Rest => (TileKind::Rest, rest_name(node.files), None),
    };
    TileInfo {
        node: (kind != TileKind::Rest).then_some(node.id),
        kind,
        name,
        path,
        bytes: node.bytes,
        files: node.files,
        row,
        nested,
    }
}

/// Info of the merged tile of `children` whose items `placed` did not keep.
fn other_info(
    children: &[SpaceNode],
    placed: &[ui::Placed],
    row: Option<usize>,
    nested: bool,
) -> TileInfo {
    let kept: Vec<usize> = placed
        .iter()
        .filter_map(|p| match p.slot {
            Slot::Item(ix) => Some(ix),
            Slot::Other => None,
        })
        .collect();
    let (mut bytes, mut files, mut count) = (0_u64, 0_u64, 0_u64);
    for (ix, c) in children.iter().enumerate() {
        if c.bytes > 0 && !kept.contains(&ix) {
            bytes = bytes.saturating_add(c.bytes);
            files = files.saturating_add(c.files);
            count = count.saturating_add(1);
        }
    }
    TileInfo {
        node: None,
        kind: TileKind::Other,
        name: other_name(count),
        path: None,
        bytes,
        files,
        row,
        nested,
    }
}

/// Fill, hatch stripes and label ink of a tile of `kind` in level-1 child `row`'s hue
/// (`nested` = its position among level-2 tiles).
fn colors(
    palette: &LensPalette,
    kind: TileKind,
    row: usize,
    nested: Option<usize>,
) -> (Hsla, Option<Hsla>, Hsla) {
    match kind {
        TileKind::Dir => (
            palette.fill(row, SpaceKind::Dir, nested),
            None,
            palette.ink(Some(row)),
        ),
        TileKind::File => (
            palette.fill(row, SpaceKind::File, nested),
            None,
            palette.ink(Some(row)),
        ),
        TileKind::Rest => (palette.neutral, Some(palette.stripe), palette.neutral_ink),
        TileKind::Other => (palette.neutral, None, palette.neutral_ink),
    }
}

/// Lays `listing` (the children of level-1 tile `row`) out inside `rect`, appending the
/// level-2 tiles to `out`; returns how the parent tile is labelled (in its header strip,
/// or not at all when there is no room for one).
fn nest(
    listing: &SpaceListing,
    rect: Rect,
    row: usize,
    palette: &LensPalette,
    out: &mut Vec<(MapTile, TileInfo)>,
) -> LabelFit {
    let half = treemap::GAP / 2.;
    let header =
        rect.w >= treemap::LABEL_MIN_WIDTH && rect.h >= treemap::HEADER + treemap::NEST_MIN_SIDE;
    let top = if header {
        treemap::HEADER
    } else {
        treemap::NEST_PAD
    };
    let pad = treemap::NEST_PAD;
    if let Some(inner) = rect.inset(top, pad, pad, pad) {
        let sizes: Vec<u64> = listing.children.iter().map(|c| c.bytes).collect();
        let placed = ui::squarify(&sizes, inner, treemap::MIN_SIDE);
        for (position, item) in placed.iter().enumerate() {
            let Some(area) = item.rect.inset(half, half, half, half) else {
                continue;
            };
            let info = match item.slot {
                Slot::Item(ix) => match listing.children.get(ix) {
                    Some(child) => node_info(child, &listing.path, Some(row), true),
                    None => continue,
                },
                Slot::Other => other_info(&listing.children, &placed, Some(row), true),
            };
            let (fill, hatch, ink) = colors(palette, info.kind, row, Some(position));
            out.push((tile(area, &info, fill, hatch, ink, fit(area)), info));
        }
    }
    if header {
        LabelFit::Inline
    } else {
        LabelFit::None
    }
}

/// Lays out `level` in a `width × height` map: level-1 tiles for its children, and inside
/// each large enough folder tile whose listing is in `sub`, level-2 tiles for its
/// children.
fn build_map(
    level: &SpaceListing,
    sub: &BTreeMap<ItemId, SpaceListing>,
    (width, height): (f32, f32),
    palette: &LensPalette,
) -> MapModel {
    let half = treemap::GAP / 2.;
    let sizes: Vec<u64> = level.children.iter().map(|c| c.bytes).collect();
    let placed = ui::squarify(&sizes, Rect::new(0., 0., width, height), treemap::MIN_SIDE);
    let mut tiles = Vec::with_capacity(placed.len());
    let mut info = Vec::with_capacity(placed.len());
    let mut nested = Vec::new();
    let mut row_tile = vec![None; level.children.len()];
    for item in &placed {
        let Some(rect) = item.rect.inset(half, half, half, half) else {
            continue;
        };
        let Slot::Item(ix) = item.slot else {
            let merged = other_info(&level.children, &placed, None, false);
            let (fill, hatch, ink) = colors(palette, TileKind::Other, 0, None);
            tiles.push(tile(rect, &merged, fill, hatch, ink, fit(rect)));
            info.push(merged);
            continue;
        };
        let Some(node) = level.children.get(ix) else {
            continue;
        };
        let node_data = node_info(node, &level.path, Some(ix), false);
        let (fill, hatch, ink) = colors(palette, node_data.kind, ix, None);
        let label = match sub.get(&node.id) {
            Some(children)
                if rect.w >= treemap::NEST_MIN_SIDE && rect.h >= treemap::NEST_MIN_SIDE =>
            {
                nest(children, rect, ix, palette, &mut nested)
            }
            _ => fit(rect),
        };
        if let Some(slot) = row_tile.get_mut(ix) {
            *slot = Some(tiles.len());
        }
        tiles.push(tile(rect, &node_data, fill, hatch, ink, label));
        info.push(node_data);
    }
    for (shape, data) in nested {
        tiles.push(shape);
        info.push(data);
    }
    MapModel {
        tiles: tiles.into(),
        info,
        row_tile,
    }
}

// ---- Page ----

/// A child row of the shown level, with its display data.
#[derive(Debug, Clone)]
struct NodeRow {
    id: ItemId,
    name: SharedString,
    path: String,
    bytes: u64,
    kind: SpaceKind,
    share: f32,
    size: SharedString,
}

/// `part` as a whole percentage of `whole`.
fn share_label(part: u64, whole: u64) -> SharedString {
    let pct = part.saturating_mul(100).checked_div(whole).unwrap_or(0);
    if pct == 0 && part > 0 {
        "<1%".into()
    } else {
        format!("{pct}%").into()
    }
}

fn rows_of(listing: &SpaceListing) -> Vec<NodeRow> {
    listing
        .children
        .iter()
        .map(|node| NodeRow {
            id: node.id,
            name: if node.kind == SpaceKind::Rest {
                rest_name(node.files)
            } else {
                node.name.clone().into()
            },
            path: child_path(&listing.path, &node.name),
            bytes: node.bytes,
            kind: node.kind,
            share: widgets::fraction(node.bytes, listing.bytes),
            size: format::bytes(node.bytes),
        })
        .collect()
}

/// The space-lens page: a treemap and ranked list of the store's space-lens result,
/// browsed level by level.
pub(crate) struct SpacePage {
    scans: Entity<Scans>,
    input: Entity<InputState>,
    info: Option<SystemInfo>,
    lens: Lens,
    rows: Vec<NodeRow>,
    selection: SpaceSelection,
    nav_error: Option<SharedString>,
    nav_task: Option<Task<()>>,
    /// Level-2 fetches of the current level (dropping them cancels them).
    sub_tasks: Vec<Task<()>>,
    info_task: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    /// The map's measured size (px), once known.
    map_size: Option<(f32, f32)>,
    palette: Option<LensPalette>,
    map: MapModel,
    hover: Option<Hover>,
    /// Keyboard cursor (a level-1 tile).
    cursor: Option<usize>,
    /// Bumped on every navigation: keys the drill-down crossfade.
    fade: u64,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for SpacePage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpacePage")
            .field("levels", &self.lens.levels.len())
            .finish_non_exhaustive()
    }
}

impl SpacePage {
    /// Creates the page with an empty root (the home folder once the daemon tells it).
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr("space.root_placeholder")));
        let engine = engine::entity(cx);
        let scans = scans::entity(cx);
        let mut subscriptions = vec![
            cx.subscribe(&engine, |this, _, event: &ClientEvent, cx| {
                if matches!(event, ClientEvent::State(ConnState::Connected { .. }))
                    && this.info.is_none()
                {
                    this.fetch_info(cx);
                }
            }),
            cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.scan(cx);
                }
            }),
        ];
        subscriptions.extend(super::follow(&scans, AREA, cx, Self::on_scan));
        let connected = scans.read(cx).is_connected();
        let mut this = Self {
            scans,
            input,
            info: None,
            lens: Lens::default(),
            rows: Vec::new(),
            selection: SpaceSelection::default(),
            nav_error: None,
            nav_task: None,
            sub_tasks: Vec::new(),
            info_task: None,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            map_size: None,
            palette: None,
            map: MapModel::default(),
            hover: None,
            cursor: None,
            fade: 0,
            _subscriptions: subscriptions,
        };
        this.sync(cx);
        if connected {
            this.fetch_info(cx);
        }
        this
    }

    /// Shows the root level of the store's result (or nothing); navigation restarts.
    fn sync(&mut self, cx: &mut Context<'_, Self>) {
        let root = match self.scans.read(cx).output(AREA) {
            Some(JobOutput::Space(listing)) => Some(listing.clone()),
            Some(other) => {
                tracing::warn!(?other, "unexpected space-lens output");
                None
            }
            None => None,
        };
        self.lens.reset(root);
        self.selection.clear();
        self.nav_task = None;
        self.navigated(cx);
    }

    /// The shown level changed: rows, map, hover and level-2 fetches follow.
    fn navigated(&mut self, cx: &mut Context<'_, Self>) {
        self.nav_error = None;
        self.rows = self.lens.current().map(rows_of).unwrap_or_default();
        self.hover = None;
        self.cursor = None;
        self.fade = self.fade.wrapping_add(1);
        self.relayout();
        self.scroll.scroll_to_item(0, gpui_kit::ScrollStrategy::Top);
        self.prefetch(cx);
        cx.notify();
    }

    fn on_scan(&mut self, event: &ScanEvent, _: &Entity<Scans>, cx: &mut Context<'_, Self>) {
        match event {
            ScanEvent::Scanned(_) | ScanEvent::Cleared(_) => self.sync(cx),
            ScanEvent::Cleaned { items, report, .. } => self.cleaned(items, report, cx),
        }
    }

    /// Removed nodes leave the map and the list.
    fn cleaned(&mut self, items: &[ItemId], report: &CleanReport, cx: &mut Context<'_, Self>) {
        let failed: Vec<String> = report
            .failures
            .iter()
            .map(|f| f.location.display())
            .collect();
        self.lens.remove(items, &failed);
        self.selection.clear();
        self.rows = self.lens.current().map(rows_of).unwrap_or_default();
        self.hover = None;
        self.cursor = None;
        self.relayout();
        cx.notify();
    }

    /// Recomputes the map for the current size, data and palette; keeps hover and cursor
    /// on the same nodes.
    fn relayout(&mut self) {
        let keep = |ix: Option<usize>, map: &MapModel| {
            ix.and_then(|t| map.info.get(t)).map(|i| (i.node, i.nested))
        };
        let hover_row = match self.hover {
            Some(Hover::Row(r)) => Some(r),
            _ => None,
        };
        let hover_tile = match self.hover {
            Some(Hover::Tile(t)) => keep(Some(t), &self.map),
            _ => None,
        };
        let cursor = keep(self.cursor, &self.map);
        self.map = match (self.map_size, &self.palette, self.lens.current()) {
            (Some(size), Some(palette), Some(level)) => {
                build_map(level, &self.lens.sub, size, palette)
            }
            _ => MapModel::default(),
        };
        self.hover = hover_row.map(Hover::Row).or_else(|| {
            hover_tile
                .and_then(|(node, nested)| self.map.tile_of(node, nested))
                .map(Hover::Tile)
        });
        self.cursor = cursor.and_then(|(node, nested)| self.map.tile_of(node, nested));
    }

    /// Builds the palette when the theme's mode or accent changed.
    fn ensure_palette(&mut self, cx: &App) {
        let theme = cx.theme();
        let key = (theme.mode.is_dark(), theme.primary);
        if self.palette.as_ref().is_some_and(|p| p.key == key) {
            return;
        }
        self.palette = Some(LensPalette::new(key.0, key.1));
        self.relayout();
    }

    fn resize(&mut self, size: (f32, f32), cx: &mut Context<'_, Self>) {
        if self.map_size == Some(size) {
            return;
        }
        self.map_size = Some(size);
        self.relayout();
        cx.notify();
    }

    /// Fetches the level-2 listings of the current level's largest child folders in
    /// parallel; each shows as it arrives, stale ones are dropped.
    fn prefetch(&mut self, cx: &mut Context<'_, Self>) {
        let Some(job) = self.scans.read(cx).scan_job(AREA) else {
            self.sub_tasks.clear();
            return;
        };
        let rev = self.lens.rev;
        let nodes = self.lens.prefetch();
        self.sub_tasks = nodes
            .into_iter()
            .map(|node| {
                let answer = jobs::space_children(job, node, cx);
                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    let listing = answer.await;
                    if let Err(err) = this.update(cx, |this, cx| {
                        if this.scans.read(cx).scan_job(AREA) != Some(job) {
                            return;
                        }
                        match listing {
                            Ok(listing) => {
                                if this.lens.sub_arrived(rev, listing) {
                                    this.relayout();
                                    cx.notify();
                                }
                            }
                            Err(err) => tracing::debug!("space lens level 2 of {node}: {err}"),
                        }
                    }) {
                        tracing::debug!("space page closed before a level arrived: {err}");
                    }
                })
            })
            .collect();
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

    fn fetch_info(&mut self, cx: &mut Context<'_, Self>) {
        let answer = jobs::system_info(cx);
        self.info_task =
            Some(cx.spawn(
                async move |this: WeakEntity<Self>, cx: &mut AsyncApp| match answer.await {
                    Ok(info) => {
                        if let Err(err) = this.update(cx, |this, cx| {
                            this.info = Some(info);
                            cx.notify();
                        }) {
                            tracing::debug!("space page closed before system info: {err}");
                        }
                    }
                    Err(err) => tracing::debug!("system info for the space lens: {err}"),
                },
            ));
    }

    /// The folder to measure: the typed path, else the home folder.
    fn root(&self, cx: &Context<'_, Self>) -> Option<String> {
        let typed = self.input.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            self.info.as_ref().map(|info| info.home.clone())
        } else {
            Some(typed)
        }
    }

    fn scan(&mut self, cx: &mut Context<'_, Self>) {
        let (busy, connected) = {
            let scans = self.scans.read(cx);
            (scans.is_busy(AREA), scans.is_connected())
        };
        if busy || !connected {
            return;
        }
        let Some(root) = self.root(cx) else { return };
        self.store(cx, |s, area, cx| {
            s.scan(area, JobSpec::Scan(ScanArea::SpaceLens { root }), cx);
        });
    }

    /// Measures the shown result's root again (the typed root when there is none).
    fn rescan_retained(&mut self, cx: &mut Context<'_, Self>) {
        let Some(spec) = self.scans.read(cx).spec(AREA).cloned() else {
            self.scan(cx);
            return;
        };
        self.store(cx, |s, area, cx| s.scan(area, spec, cx));
    }

    fn scan_path(&mut self, path: String, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.input
            .update(cx, |input, cx| input.set_value(path, window, cx));
        self.scan(cx);
    }

    /// Drills into folder `node` (a child of the current level or of one of its folders).
    fn open(&mut self, node: ItemId, cx: &mut Context<'_, Self>) {
        let Some(job) = self.scans.read(cx).scan_job(AREA) else {
            return;
        };
        let depth = self.lens.levels.len();
        match self.lens.open(node) {
            Open::Ignored => {}
            Open::Shown => self.navigated(cx),
            Open::Fetch { rev, node } => {
                self.nav_error = None;
                let answer = jobs::space_children(job, node, cx);
                self.nav_task = Some(cx.spawn(
                    async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                        let listing = answer.await;
                        if let Err(err) = this.update(cx, |this, cx| {
                            if this.scans.read(cx).scan_job(AREA) != Some(job) {
                                return;
                            }
                            match listing {
                                Ok(listing) => {
                                    if this.lens.opened(rev, listing) {
                                        this.navigated(cx);
                                    }
                                }
                                Err(message) => {
                                    if this.lens.open_failed(rev) {
                                        this.nav_error = Some(message.into());
                                        this.prefetch(cx);
                                        cx.notify();
                                    }
                                }
                            }
                        }) {
                            tracing::debug!("space page closed before a level arrived: {err}");
                        }
                    },
                ));
                if self.lens.levels.len() == depth {
                    cx.notify();
                } else {
                    // A level-2 folder: its parent is shown while the folder loads.
                    self.navigated(cx);
                }
            }
        }
    }

    /// Shows level `depth` (0 = root) again.
    fn go_to(&mut self, depth: usize, cx: &mut Context<'_, Self>) {
        let shown = self.lens.levels.len();
        let moved = self.lens.go_to(depth);
        self.went_back(moved, shown, cx);
    }

    /// One level up.
    fn up(&mut self, cx: &mut Context<'_, Self>) {
        let shown = self.lens.levels.len();
        let moved = self.lens.up();
        self.went_back(moved, shown, cx);
    }

    /// After [`Lens::go_to`]/[`Lens::up`]: a pending open is dropped; a shorter stack is
    /// a navigation.
    fn went_back(&mut self, moved: bool, shown: usize, cx: &mut Context<'_, Self>) {
        if !moved {
            return;
        }
        self.nav_task = None;
        if self.lens.levels.len() == shown {
            cx.notify();
        } else {
            self.navigated(cx);
        }
    }

    fn toggle_row(&mut self, row: usize, cx: &mut Context<'_, Self>) {
        if let Some(node) = self.rows.get(row)
            && node.kind != SpaceKind::Rest
        {
            self.selection
                .toggle(node.id, node.path.clone(), node.bytes);
            cx.notify();
        }
    }

    /// A tile was clicked (or Enter/Space on the cursor): folders open (or, with
    /// `select`, toggle), files toggle; aggregates do nothing.
    fn activate(&mut self, tile: usize, select: bool, cx: &mut Context<'_, Self>) {
        let Some(info) = self.map.info.get(tile) else {
            return;
        };
        match (info.kind, info.node, info.path.clone()) {
            (TileKind::Dir, Some(node), _) if !select => self.open(node, cx),
            (TileKind::Dir | TileKind::File, Some(node), Some(path)) => {
                self.selection.toggle(node, path, info.bytes);
                cx.notify();
            }
            _ => {}
        }
    }

    fn set_hover(&mut self, hover: Option<Hover>, cx: &mut Context<'_, Self>) {
        if self.hover != hover {
            self.hover = hover;
            cx.notify();
        }
    }

    fn hover_row(&mut self, row: usize, inside: bool, cx: &mut Context<'_, Self>) {
        if inside {
            self.set_hover(Some(Hover::Row(row)), cx);
        } else if self.hover == Some(Hover::Row(row)) {
            self.set_hover(None, cx);
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<'_, Self>) {
        let step = |this: &mut Self, step| {
            this.cursor = this.map.neighbour(this.cursor, step).or(this.cursor);
        };
        match event.keystroke.key.as_str() {
            "left" => step(self, Step::Left),
            "right" => step(self, Step::Right),
            "up" => step(self, Step::Up),
            "down" => step(self, Step::Down),
            "enter" => {
                if let Some(tile) = self.cursor {
                    self.activate(tile, false, cx);
                }
            }
            "space" => {
                if let Some(tile) = self.cursor {
                    self.activate(tile, true, cx);
                }
            }
            "backspace" | "escape" => self.up(cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Tiles whose node is selected (a folder's overlay covers its nested tiles).
    fn marked_tiles(&self) -> Vec<usize> {
        if self.selection.len() == 0 {
            return Vec::new();
        }
        self.map
            .info
            .iter()
            .enumerate()
            .filter(|(_, i)| i.node.is_some_and(|n| self.selection.contains(n)))
            .map(|(ix, _)| ix)
            .collect()
    }

    fn ask_clean(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let items = self.selection.ids();
        if items.is_empty() {
            return;
        }
        let confirm = CleanConfirm {
            count: items.len(),
            bytes: self.selection.bytes(),
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

    /// Path field, Scan, and the Home / volume quick picks below. Scan is the page's primary
    /// until results are shown (then the results card's Clean is).
    fn render_picker(
        &self,
        busy: bool,
        connected: bool,
        showing_results: bool,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let can_scan = connected && !busy && self.root(cx).is_some();
        let typed = self.input.read(cx).value().trim().to_owned();
        let mut quick: Vec<(SharedString, String, IconName)> = Vec::new();
        if let Some(info) = &self.info {
            quick.push((tr("space.home"), info.home.clone(), IconName::Folder));
            for volume in &info.volumes {
                let label = if volume.name.is_empty() {
                    volume.mount.clone()
                } else {
                    volume.name.clone()
                };
                quick.push((label.into(), volume.mount.clone(), IconName::HardDrive));
            }
        }
        let home = self.info.as_ref().map(|info| info.home.as_str());
        let picks = quick
            .into_iter()
            .enumerate()
            .map(|(ix, (label, path, icon))| {
                let current = typed == path || (typed.is_empty() && home == Some(path.as_str()));
                ui::Button::new(("space-root", ix), label)
                    .small()
                    .icon(icon)
                    .selected(current)
                    .disabled(!connected || busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.scan_path(path.clone(), window, cx);
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let scan = ui::Button::new(
            "space-scan",
            tr(if self.lens.levels.is_empty() {
                "scan.scan"
            } else {
                "scan.rescan"
            }),
        )
        .icon(IconName::Search)
        .when(!showing_results, ui::Button::primary)
        .disabled(!can_scan)
        .on_click(cx.listener(|this, _, _, cx| this.scan(cx)));
        v_flex()
            .w_full()
            .gap(space::MD)
            .child(
                h_flex()
                    .w_full()
                    .gap(space::MD)
                    .child(
                        ui::TextInput::new(&self.input)
                            .icon(IconName::Folder)
                            .flex_1()
                            .min_w_0(),
                    )
                    .child(scan),
            )
            .when(!picks.is_empty(), |this| {
                this.child(ui::Toolbar::new().children(picks))
            })
            .into_any_element()
    }

    /// Up, then the opened levels as ghost crumbs; the current level is plain text.
    fn render_breadcrumb(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let last = self.lens.levels.len().saturating_sub(1);
        let crumbs = self
            .lens
            .levels
            .iter()
            .enumerate()
            .flat_map(|(depth, level)| {
                let name = level_name(level, depth);
                let separator = (depth > 0).then(|| {
                    div()
                        .flex_none()
                        .text_color(muted)
                        .child(Icon::new(IconName::ChevronRight).size(control::ICON))
                        .into_any_element()
                });
                let crumb = if depth == last {
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .px(control::PAD_X_SM)
                        .text_size(text::SMALL)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(fg)
                        .child(name)
                        .into_any_element()
                } else {
                    ui::Button::new(("space-crumb", depth), name)
                        .small()
                        .ghost()
                        .on_click(cx.listener(move |this, _, _, cx| this.go_to(depth, cx)))
                        .into_any_element()
                };
                separator.into_iter().chain(std::iter::once(crumb))
            })
            .collect::<Vec<_>>();
        h_flex()
            .w_full()
            .flex_none()
            .h(row::HEIGHT)
            .gap(space::XS)
            .child(
                ui::IconButton::new("space-up", IconName::ArrowUp, tr("space.back"))
                    .small()
                    .disabled(self.lens.levels.len() < 2)
                    .on_click(cx.listener(|this, _, _, cx| this.up(cx))),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap(space::XXS)
                    .children(crumbs),
            )
            .when(self.lens.opening.is_some(), |this| {
                this.child(
                    div()
                        .flex_none()
                        .px(space::MD)
                        .text_size(text::SMALL)
                        .text_color(muted)
                        .child(tr("space.loading")),
                )
            })
            .into_any_element()
    }

    /// Name, size, share and item count of the hovered (or keyboard) tile; a hint
    /// otherwise.
    fn render_info(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let tile = self.map.linked_tile(self.hover).or(self.cursor);
        let bar = h_flex()
            .w_full()
            .flex_none()
            .h(row::HEIGHT)
            .px(space::XS)
            .gap(space::MD)
            .text_size(text::SMALL)
            .line_height(text::SMALL_LINE_HEIGHT);
        let (Some(info), Some(shape), Some(level)) = (
            tile.and_then(|t| self.map.info.get(t)),
            tile.and_then(|t| self.map.tiles.get(t)),
            self.lens.current(),
        ) else {
            return bar
                .text_color(muted)
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(tr("files2.lens.hint")),
                )
                .into_any_element();
        };
        let mut facts = vec![
            format::bytes(info.bytes).to_string(),
            rust_i18n::t!(
                "files2.lens.share",
                share = share_label(info.bytes, level.bytes),
                folder = level_name(level, self.lens.levels.len().saturating_sub(1))
            )
            .to_string(),
        ];
        if info.kind != TileKind::File {
            facts.push(
                rust_i18n::t!("files2.lens.files", n = format::count(info.files)).to_string(),
            );
        }
        bar.child(
            div()
                .flex_none()
                .size(treemap::SWATCH)
                .rounded(treemap::RADIUS_NESTED)
                .bg(shape.fill),
        )
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(text::BODY)
                .font_weight(FontWeight::MEDIUM)
                .text_color(fg)
                .child(info.name.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .font_features(ui::tabular())
                .text_color(muted)
                .child(facts.join(" · ")),
        )
        .into_any_element()
    }

    /// Swatch (tile fill) and bar colour of list row `ix`.
    fn row_colors(&self, ix: usize) -> (Hsla, Hsla) {
        let Some(palette) = &self.palette else {
            return (Hsla::default(), Hsla::default());
        };
        let fill = self
            .map
            .row_tile
            .get(ix)
            .copied()
            .flatten()
            .and_then(|t| self.map.tiles.get(t))
            .map_or(palette.neutral, |t| t.fill);
        let kind = self.rows.get(ix).map(|r| r.kind);
        let strong = if kind == Some(SpaceKind::Rest) {
            palette.neutral_strong
        } else {
            palette.strong(ix)
        };
        (fill, strong)
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        range.filter_map(|ix| self.row(ix, cx)).collect()
    }

    fn row(&self, ix: usize, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        let node = self.rows.get(ix)?;
        let muted = cx.theme().muted_foreground;
        let linked = self.map.linked_row(self.hover) == Some(ix);
        let (fill, strong) = self.row_colors(ix);
        let swatch = div()
            .flex_none()
            .size(treemap::SWATCH)
            .rounded(treemap::RADIUS_NESTED)
            .bg(fill);
        let bar = div()
            .flex_none()
            .w(treemap::LIST_BAR_WIDTH)
            .child(widgets::size_bar(node.share, strong, cx));
        let id = u64::from(node.id);
        let row = ui::ListRow::new(ElementId::from(("space-row", id)), node.name.clone())
            .icon(swatch)
            .current(linked)
            .trailing(bar)
            .trailing(widgets::size_cell(node.size.clone(), cx));
        let row = if node.kind == SpaceKind::Rest {
            row.disabled(true)
                .trailing(div().flex_none().size(control::ICON))
        } else {
            let covered = self.selection.covers(&node.path);
            let checked = self.selection.contains(node.id) || covered;
            let is_dir = node.kind == SpaceKind::Dir;
            let row = row
                .checkbox(
                    widgets::check(
                        ElementId::from(("space-check", id)),
                        checked,
                        cx.listener(move |this, _, _, cx| this.toggle_row(ix, cx)),
                    )
                    .disabled(covered),
                )
                .trailing(
                    div()
                        .flex_none()
                        .size(control::ICON)
                        .text_color(muted)
                        .when(is_dir, |this| {
                            this.child(Icon::new(IconName::ChevronRight).size(control::ICON))
                        }),
                );
            if is_dir {
                let node = node.id;
                row.on_click(cx.listener(move |this, _, _, cx| this.open(node, cx)))
            } else {
                row.on_click(cx.listener(move |this, _, _, cx| this.toggle_row(ix, cx)))
            }
        };
        Some(
            div()
                .id(ElementId::from(("space-row-hover", id)))
                .w_full()
                .on_hover(cx.listener(move |this, inside: &bool, _, cx| {
                    this.hover_row(ix, *inside, cx);
                }))
                .child(row)
                .into_any_element(),
        )
    }

    /// The treemap, sized by its parent.
    fn render_map(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        ui::Treemap::new("space-map", self.map.tiles.clone())
            .laid_out_for(self.map_size)
            .hovered(self.map.linked_tile(self.hover))
            .cursor(self.cursor)
            .marked(self.marked_tiles())
            .focus(self.focus.clone())
            .on_resize(cx.listener(|this, size: &(f32, f32), _, cx| this.resize(*size, cx)))
            .on_hover(cx.listener(|this, tile: &Option<usize>, _, cx| {
                this.set_hover(tile.map(Hover::Tile), cx);
            }))
            .on_click(cx.listener(|this, tile: &usize, _, cx| this.activate(*tile, false, cx)))
            .on_key(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .into_any_element()
    }

    fn render_results(
        &mut self,
        connected: bool,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        self.ensure_palette(cx);
        let Some(level) = self.lens.current() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(root) = self.lens.levels.first()
            && let Some(denied) = widgets::denied_notice(&root.denied, cx)
        {
            out.push(denied);
        }
        if let Some(error) = self.nav_error.clone() {
            out.push(widgets::notice(
                Tone::Danger,
                tr("space.open_failed"),
                Some(error),
                Vec::new(),
                cx,
            ));
        }
        let count = self.selection.len();
        let header =
            ui::CardHeader::new(level_name(level, self.lens.levels.len().saturating_sub(1)))
                .trailing(ui::Stat::new(tr("space.size"), format::bytes(level.bytes)))
                .trailing(ui::Stat::new(tr("space.files"), format::count(level.files)))
                .trailing(ui::Stat::new(
                    tr("junk.selected_size"),
                    format::bytes(self.selection.bytes()),
                ))
                .trailing(
                    ui::Toolbar::new()
                        .child(
                            ui::Button::new("space-clear-selection", tr("scan.select_none"))
                                .small()
                                .ghost()
                                .disabled(count == 0)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.selection.clear();
                                    cx.notify();
                                })),
                        )
                        .child(widgets::clean_button(
                            "space-clean",
                            count,
                            connected,
                            Self::listener(cx, Self::ask_clean),
                        )),
                );
        let empty = self.rows.is_empty();
        let body = if empty {
            ui::EmptyState::new(IconName::FolderOpen, tr("space.empty")).into_any_element()
        } else {
            self.render_lens(window, cx)
        };
        out.push(
            ui::Card::new()
                .flush()
                .flex_1()
                .min_h_0()
                .header(header)
                .child(
                    widgets::card_body()
                        .pt(space::XS)
                        .child(self.render_breadcrumb(cx))
                        .when(!empty, |this| this.child(self.render_info(cx)))
                        .child(body),
                )
                .into_any_element(),
        );
        out
    }

    /// The map and the ranked list: side by side on wide windows, else stacked.
    fn render_lens(&self, window: &Window, cx: &mut Context<'_, Self>) -> AnyElement {
        let side = window.viewport_size().width >= treemap::SIDE_PANEL_MIN_WINDOW;
        let list = widgets::list_frame(
            uniform_list(
                "space-list",
                self.rows.len(),
                cx.processor(|this, range, _, cx| this.render_rows(range, cx)),
            )
            .track_scroll(&self.scroll)
            .size_full(),
        );
        let map = div()
            .relative()
            .flex_1()
            .min_w_0()
            .min_h(treemap::MAP_MIN_HEIGHT)
            .when(side, Styled::h_full)
            .when(!side, Styled::w_full)
            .child(self.render_map(cx))
            .with_animation(
                ElementId::from(("space-map-fade", self.fade)),
                Animation::new(treemap::FADE),
                Styled::opacity,
            );
        if side {
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .gap(space::MD)
                .child(map)
                .child(
                    v_flex()
                        .flex_none()
                        .w(treemap::SIDE_PANEL_WIDTH)
                        .h_full()
                        .child(list),
                )
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .gap(space::MD)
                .child(map)
                .child(
                    v_flex()
                        .flex_none()
                        .w_full()
                        .h(treemap::LIST_BELOW_HEIGHT)
                        .child(list),
                )
                .into_any_element()
        }
    }
}

/// Display name of `level` at `depth`: the root as its `~` path, deeper levels by name.
fn level_name(level: &SpaceListing, depth: usize) -> SharedString {
    if depth == 0 {
        format::tilde(&level.path)
    } else {
        Path::new(&level.path)
            .file_name()
            .map_or_else(|| level.path.clone(), |n| n.to_string_lossy().into_owned())
            .into()
    }
}

/// The map's stand-in while the scan runs.
fn scanning_placeholder() -> AnyElement {
    ui::Card::new()
        .flush()
        .flex_1()
        .min_h(treemap::MAP_MIN_HEIGHT)
        .child(
            div()
                .size_full()
                .p(row::INSET)
                .child(ui::TreemapPlaceholder::new("space-map-placeholder")),
        )
        .into_any_element()
}

impl Render for SpacePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let (flow, connected, stale) = {
            let scans = self.scans.read(cx);
            (
                scans.view(AREA),
                scans.is_connected(),
                scans.stale_age(AREA, Instant::now()),
            )
        };
        let phase = flow.phase;
        let showing_results = matches!(phase, FlowPhase::Ready | FlowPhase::Cleaned);
        let picker = self.render_picker(flow.is_busy(), connected, showing_results, cx);
        let mut column = widgets::page_column("space-page")
            .max_w(treemap::PAGE_MAX_WIDTH)
            .child(widgets::area_header(Category::SpaceLens, Vec::new(), cx))
            .children(widgets::connection_notice(connected, cx))
            .child(picker);
        if let Some(error) = flow.error.clone() {
            column = column.child(widgets::error_notice(
                error,
                Self::listener(cx, |this, _, cx| this.store(cx, Scans::dismiss_error)),
                cx,
            ));
        }
        column = column.children(super::stale_notice(
            "space-rescan",
            stale,
            connected,
            Self::listener(cx, |this, _, cx| this.rescan_retained(cx)),
            cx,
        ));
        let body: Vec<AnyElement> = match phase {
            // The toolbar's Scan is this page's primary: the idle card has no action.
            FlowPhase::Idle | FlowPhase::Failed => {
                vec![widgets::idle_card(Category::SpaceLens, None)]
            }
            FlowPhase::Scanning => vec![
                widgets::job_progress(
                    "space-progress",
                    &flow.progress,
                    Self::listener(cx, |this, _, cx| this.store(cx, Scans::cancel)),
                    cx,
                ),
                scanning_placeholder(),
            ],
            FlowPhase::Cleaning => vec![widgets::job_progress(
                "space-progress",
                &flow.progress,
                Self::listener(cx, |this, _, cx| this.store(cx, Scans::cancel)),
                cx,
            )],
            FlowPhase::Ready => self.render_results(connected, window, cx),
            FlowPhase::Cleaned => flow
                .report
                .as_ref()
                .map(|report| {
                    vec![widgets::clean_report(
                        report,
                        Self::listener(cx, |this, _, cx| this.store(cx, Scans::dismiss_report)),
                        Self::listener(cx, |this, _, cx| this.rescan_retained(cx)),
                        cx,
                    )]
                })
                .unwrap_or_default(),
        };
        let scrolls = !matches!(phase, FlowPhase::Ready | FlowPhase::Scanning);
        v_flex().size_full().child(column.children(body).when(
            scrolls,
            gpui_kit::StatefulInteractiveElement::overflow_y_scroll,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gpui_kit::component::WindowExt as _;
    use gpui_kit::{Hsla, TestAppContext};
    use omc_proto::files::{SpaceKind, SpaceListing, SpaceNode};
    use omc_proto::jobs::JobOutput;

    use super::{
        Hover, Lens, LensPalette, MapModel, Open, SpacePage, SpaceSelection, Step, TileKind,
        build_map,
    };
    use crate::pages::widgets::test_support::open_page;
    use crate::scans::{self, Area};

    fn node(id: u32, name: &str, bytes: u64, kind: SpaceKind) -> SpaceNode {
        SpaceNode {
            id,
            name: name.to_owned(),
            bytes,
            files: 1,
            kind,
        }
    }

    fn listing(node_id: u32, path: &str, children: Vec<SpaceNode>) -> SpaceListing {
        let bytes = children.iter().map(|c| c.bytes).sum();
        SpaceListing {
            node: node_id,
            path: path.to_owned(),
            bytes,
            files: 12,
            children,
            denied: Vec::new(),
        }
    }

    fn root() -> SpaceListing {
        listing(
            0,
            "/home",
            vec![
                node(1, "videos", 600, SpaceKind::Dir),
                node(2, "a.iso", 300, SpaceKind::File),
                node(4, "docs", 90, SpaceKind::Dir),
                node(3, "", 10, SpaceKind::Rest),
            ],
        )
    }

    fn videos() -> SpaceListing {
        listing(
            1,
            "/home/videos",
            vec![
                node(10, "trip", 400, SpaceKind::Dir),
                node(11, "a.mov", 200, SpaceKind::File),
            ],
        )
    }

    fn palette() -> LensPalette {
        LensPalette::new(false, Hsla::default())
    }

    #[test]
    fn selection_never_counts_a_folder_and_its_contents_twice() {
        let mut sel = SpaceSelection::default();
        sel.toggle(5, "/home/a/big.iso".to_owned(), 40);
        sel.toggle(6, "/home/b".to_owned(), 10);
        assert_eq!(sel.bytes(), 50, "two unrelated nodes add up");

        sel.toggle(1, "/home/a".to_owned(), 100);
        assert_eq!(
            sel.ids(),
            vec![1, 6],
            "selecting a folder drops its selected files"
        );
        assert_eq!(sel.bytes(), 110, "the folder's size replaces its files'");
        assert!(
            sel.covers("/home/a/other"),
            "the folder covers its contents"
        );
        assert!(
            !sel.covers("/home/ab"),
            "a sibling with a common prefix is not covered"
        );

        sel.toggle(7, "/home/a/x".to_owned(), 1);
        assert_eq!(sel.ids(), vec![1, 6], "a covered node cannot be added");

        sel.toggle(1, "/home/a".to_owned(), 100);
        assert_eq!(
            (sel.len(), sel.bytes()),
            (1, 10),
            "toggling again deselects"
        );
    }

    #[test]
    fn navigation_uses_prefetched_levels_and_drops_stale_answers() {
        let mut lens = Lens::default();
        lens.reset(Some(root()));
        assert_eq!(lens.prefetch(), vec![1, 4], "child folders are prefetched");
        let rev = lens.rev;
        assert!(
            lens.sub_arrived(rev, videos()),
            "a fresh level-2 answer is kept"
        );
        assert!(
            !lens.sub_arrived(rev.wrapping_sub(1), listing(4, "/home/docs", Vec::new())),
            "an answer from an older navigation is dropped"
        );
        assert_eq!(
            lens.prefetch(),
            vec![4],
            "only missing listings are fetched"
        );

        // Opening a level-2 folder shows its parent at once, then fetches it.
        let opened = lens.open(10);
        assert!(
            matches!(opened, Open::Fetch { .. }),
            "a nested folder is fetched"
        );
        let Open::Fetch { rev, node } = opened else {
            return;
        };
        assert_eq!(node, 10, "the clicked folder is fetched");
        assert_eq!(lens.levels.len(), 2, "its parent became the current level");
        assert!(
            lens.prefetch().is_empty(),
            "no level-2 fetches while opening"
        );
        assert!(
            !lens.opened(
                rev.wrapping_add(1),
                listing(10, "/home/videos/trip", Vec::new())
            ),
            "a mismatched answer is dropped"
        );
        assert!(
            lens.opened(rev, listing(10, "/home/videos/trip", Vec::new())),
            "the answer for the pending open is shown"
        );
        assert_eq!(lens.levels.len(), 3, "breadcrumb has root, videos, trip");

        assert!(lens.up(), "up goes back one level");
        assert_eq!(
            lens.current().map(|l| l.node),
            Some(1),
            "videos is shown again"
        );
        assert!(lens.go_to(0), "the root crumb goes home");
        assert!(!lens.up(), "nothing above the root");

        // A prefetched child opens without a request.
        let rev = lens.rev;
        assert!(lens.sub_arrived(rev, videos()), "prefetch arrives");
        assert_eq!(lens.open(1), Open::Shown, "prefetched folders open at once");
        assert_eq!(lens.open(2), Open::Ignored, "files do not open");
        let before = lens.rev;
        assert!(
            matches!(lens.open(10), Open::Fetch { .. }),
            "an unfetched child is fetched"
        );
        assert!(lens.go_to(1), "going to the current level cancels the open");
        assert!(
            !lens.opened(before.wrapping_add(1), listing(10, "/x", Vec::new())),
            "the cancelled open's answer is dropped"
        );
    }

    #[test]
    fn map_nests_prefetched_children_and_links_hover_both_ways() {
        let mut sub = BTreeMap::new();
        sub.insert(1, videos());
        let map = build_map(&root(), &sub, (600., 400.), &palette());
        let level1 = map.info.iter().filter(|i| !i.nested).count();
        assert_eq!(level1, 4, "one level-1 tile per child");
        let trip = map.info.iter().position(|i| i.node == Some(10));
        assert!(trip.is_some(), "the nested folder has a tile");
        let Some(trip) = trip else { return };
        assert!(
            map.info
                .get(trip)
                .is_some_and(|i| i.nested && i.row == Some(0)),
            "nested tiles belong to their parent's row"
        );
        let videos_tile = map.row_tile.first().copied().flatten();
        assert!(videos_tile.is_some(), "the first row has a tile");
        let Some(videos_tile) = videos_tile else {
            return;
        };
        let (outer, inner) = (
            map.tiles.get(videos_tile).map(|t| t.rect),
            map.tiles.get(trip).map(|t| t.rect),
        );
        assert!(
            matches!((outer, inner), (Some(o), Some(i))
                if i.x >= o.x && i.y >= o.y && i.x + i.w <= o.x + o.w && i.y + i.h <= o.y + o.h),
            "level-2 tiles lie inside their parent: {outer:?} {inner:?}"
        );
        assert_eq!(
            map.linked_row(Some(Hover::Tile(trip))),
            Some(0),
            "hovering a nested tile highlights its level-1 row"
        );
        assert_eq!(
            map.linked_tile(Some(Hover::Row(0))),
            Some(videos_tile),
            "hovering a row outlines its tile"
        );
        assert!(
            map.info
                .iter()
                .any(|i| i.kind == TileKind::Rest && i.node.is_none()),
            "the rest aggregate is drawn but not actionable"
        );
        assert_eq!(
            map.neighbour(None, Step::Right),
            Some(0),
            "the first key press lands on the largest tile"
        );
        let right = map.neighbour(Some(0), Step::Right);
        assert!(
            right.is_some_and(|t| map.info.get(t).is_some_and(|i| !i.nested)),
            "arrow keys move between level-1 tiles"
        );
        assert!(
            MapModel::default().neighbour(None, Step::Left).is_none(),
            "an empty map has no cursor"
        );
    }

    #[gpui_kit::test]
    fn page_selects_from_list_and_map_and_asks_before_cleaning(cx: &mut TestAppContext) {
        let Some((window, page)) = open_page(cx, SpacePage::new) else {
            return;
        };
        let store = cx.update(scans::entity);
        store.update(cx, |s, cx| {
            s.force_scanned(Area::SpaceLens, JobOutput::Space(root()), cx);
        });
        cx.run_until_parked();
        page.update(cx, |page, cx| {
            page.toggle_row(1, cx);
            page.toggle_row(3, cx);
            page.resize((600., 400.), cx);
        });
        cx.run_until_parked();
        let picked = cx.update(|cx| page.read(cx).selection.ids());
        assert_eq!(
            picked,
            vec![2],
            "files are selectable, the rest node is not"
        );
        let marked = cx.update(|cx| {
            let page = page.read(cx);
            page.marked_tiles()
                .into_iter()
                .filter_map(|t| page.map.info.get(t).and_then(|i| i.node))
                .collect::<Vec<_>>()
        });
        assert_eq!(marked, vec![2], "a row's selection marks its tile");
        page.update(cx, |page, cx| {
            let tile = page.map.row_tile.get(1).copied().flatten();
            if let Some(tile) = tile {
                page.activate(tile, false, cx);
            }
        });
        let picked = cx.update(|cx| page.read(cx).selection.ids());
        assert!(picked.is_empty(), "clicking a file tile toggles it");
        page.update(cx, |page, cx| page.toggle_row(1, cx));
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
