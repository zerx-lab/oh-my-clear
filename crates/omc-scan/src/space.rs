//! Space Lens: a disk-usage tree built from one walk.
//!
//! Memory follows the number of directories, not files: every directory is a node, but
//! per directory only its [`TOP_FILES`] largest files of at least [`FILE_NODE_MIN`]
//! bytes become nodes; the others are summed into one [`SpaceKind::Rest`] node. Nodes
//! live in one arena (ids = indices, stable for the tree's life) with parent indices,
//! children in one shared index array and names in one string arena; full paths are
//! rebuilt on demand.
//!
//! The walk keeps no paths: a directory is identified by the XXH3-128 of its path, and
//! the visitor records `(key, parent key, name)` per directory plus per-directory file
//! totals in per-thread buffers. The walker reads one directory in one task, so a
//! thread sees a directory's files back to back and only takes its own, uncontended
//! lock per file. Buffers are merged after the walk and sizes summed bottom-up.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use omc_proto::files::{SpaceKind, SpaceListing, SpaceNode};
use omc_proto::jobs::{DeleteMethod, Denied, ItemId, Phase};
use parking_lot::Mutex;
use twox_hash::XxHash3_128;

use crate::ctx::JobCtx;
use crate::large::{DeniedLog, PerThread};
use crate::target::Target;
use crate::walk::{FileMeta, Visitor, Walker};
use crate::{Error, Result, paths};

/// Files per directory kept as their own nodes (the largest ones).
const TOP_FILES: usize = 32;

/// Smaller files only count towards their directory's `Rest` node: a space lens cannot
/// show them meaningfully, and keeping a node per small file would make memory follow
/// the number of files.
const FILE_NODE_MIN: u64 = 64 * 1024;

/// Shards of the hard-link set.
const LINK_SHARDS: u64 = 64;

/// Node id of the root.
const ROOT: u32 = 0;

/// A scanned disk-usage tree.
#[derive(Debug)]
pub struct SpaceTree {
    root: PathBuf,
    nodes: Vec<Node>,
    /// Children of every directory, each run largest first.
    children: Vec<u32>,
    /// UTF-8 names of all nodes, back to back.
    names: String,
    /// Names that are not UTF-8 (rare), by node id.
    odd_names: HashMap<u32, Box<OsStr>>,
    denied: Vec<Denied>,
}

#[derive(Debug)]
struct Node {
    bytes: u64,
    files: u64,
    /// Parent index (the root points at itself).
    parent: u32,
    /// Name range in [`SpaceTree::names`] (empty for the root and `Rest` nodes).
    name_at: u32,
    name_len: u32,
    /// Children range in [`SpaceTree::children`].
    first_child: u32,
    child_count: u32,
    kind: SpaceKind,
}

/// Walks `root` (a directory) and builds its tree.
pub fn scan(root: &Path, walker: &Walker, ctx: &JobCtx) -> Result<SpaceTree> {
    let root = paths::normalize(root)
        .ok_or_else(|| Error::Invalid(format!("not an absolute path: {}", root.display())))?;
    if !fs::symlink_metadata(&root).is_ok_and(|m| m.is_dir()) {
        return Err(Error::Invalid(format!(
            "not a directory: {}",
            root.display()
        )));
    }
    ctx.set_phase(Phase::Scanning);
    let collector = Collector {
        bufs: PerThread::new(walker),
        linked: (0..LINK_SHARDS).map(|_| Mutex::default()).collect(),
        denied: DeniedLog::default(),
        ctx,
    };
    walker.walk(std::slice::from_ref(&root), ctx, &collector);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let denied = collector.denied.into_inner();
    let root_key = key(&root);
    let mut dirs = vec![vec![DirRec {
        key: root_key,
        parent: root_key,
        name: Box::default(),
    }]];
    let mut accs = Vec::new();
    for mut buf in collector.bufs.into_inner() {
        dirs.push(buf.dirs);
        if let Some(acc) = buf.cur {
            ctx.add_bytes(acc.bytes);
            buf.done.push(acc);
        }
        accs.push(buf.done);
    }
    ctx.set_phase(Phase::Measuring);
    let mut tree = Builder::default().build(dirs, accs)?;
    tree.root = root;
    tree.denied = denied;
    Ok(tree)
}

impl SpaceTree {
    /// One directory level: `node` 0 is the root (its listing carries the denied places).
    /// `None` for unknown ids and non-directories.
    pub fn listing(&self, node: ItemId) -> Option<SpaceListing> {
        let n = self.node(node)?;
        if n.kind != SpaceKind::Dir {
            return None;
        }
        let children = self
            .children_of(n)
            .iter()
            .filter_map(|&id| {
                let child = self.node(id)?;
                Some(SpaceNode {
                    id,
                    name: self.name(id)?.to_string_lossy().into_owned(),
                    bytes: child.bytes,
                    files: child.files,
                    kind: child.kind,
                })
            })
            .collect();
        Some(SpaceListing {
            node,
            path: self.path(node)?.display().to_string(),
            bytes: n.bytes,
            files: n.files,
            children,
            denied: if node == ROOT {
                self.denied.clone()
            } else {
                Vec::new()
            },
        })
    }

    /// Removal target of a directory or file node; never the root or a `Rest` node.
    pub fn target(&self, node: ItemId, method: DeleteMethod) -> Option<Target> {
        let n = self.node(node)?;
        if node == ROOT || n.kind == SpaceKind::Rest {
            return None;
        }
        let path = self.path(node)?;
        Some(Target::path(path.display().to_string(), n.bytes, method))
    }

    /// Absolute path of `node` (a `Rest` node yields its directory's path).
    pub fn path(&self, node: ItemId) -> Option<PathBuf> {
        let mut names = Vec::new();
        let mut at = node;
        // A well-formed chain is shorter than the arena; the bound guards against cycles.
        for _ in 0..self.nodes.len() {
            if at == ROOT {
                let mut path = self.root.clone();
                path.extend(names.iter().rev());
                return Some(path);
            }
            let name = self.name(at)?;
            if !name.is_empty() {
                names.push(name);
            }
            at = self.node(at)?.parent;
        }
        None
    }

    fn node(&self, id: ItemId) -> Option<&Node> {
        self.nodes.get(usize::try_from(id).ok()?)
    }

    fn name(&self, id: ItemId) -> Option<&OsStr> {
        if let Some(odd) = self.odd_names.get(&id) {
            return Some(odd);
        }
        let n = self.node(id)?;
        let start = usize::try_from(n.name_at).ok()?;
        let end = start.checked_add(usize::try_from(n.name_len).ok()?)?;
        Some(OsStr::new(self.names.get(start..end)?))
    }

    fn children_of(&self, n: &Node) -> &[u32] {
        let range = || {
            let start = usize::try_from(n.first_child).ok()?;
            let end = start.checked_add(usize::try_from(n.child_count).ok()?)?;
            self.children.get(start..end)
        };
        range().unwrap_or_default()
    }
}

/// Identity of a directory: XXH3-128 of its path bytes (the walker builds child paths
/// by joining names onto the parent's path, so a child's `parent()` hashes to its
/// parent's key).
type Key = u128;

fn key(path: &Path) -> Key {
    XxHash3_128::oneshot(path.as_os_str().as_encoded_bytes())
}

/// A directory the walk entered.
struct DirRec {
    key: Key,
    parent: Key,
    name: Box<OsStr>,
}

/// Files of one directory seen by one thread in one burst.
struct DirAcc {
    dir: Key,
    /// All files of the directory (hard links once).
    bytes: u64,
    files: u64,
    /// The largest files of at least [`FILE_NODE_MIN`] bytes, as a min-heap.
    top: BinaryHeap<Reverse<(u64, Box<OsStr>)>>,
}

impl DirAcc {
    fn new(dir: Key) -> Self {
        Self {
            dir,
            bytes: 0,
            files: 0,
            top: BinaryHeap::new(),
        }
    }

    fn add(&mut self, name: &OsStr, bytes: u64) {
        self.bytes = self.bytes.saturating_add(bytes);
        self.files = self.files.saturating_add(1);
        if bytes >= FILE_NODE_MIN {
            self.offer(bytes, || name.into());
        }
    }

    /// Keeps `(bytes, name)` if it is among the [`TOP_FILES`] largest.
    fn offer(&mut self, bytes: u64, name: impl FnOnce() -> Box<OsStr>) {
        if self.top.len() >= TOP_FILES
            && self
                .top
                .peek()
                .is_some_and(|Reverse((min, _))| *min >= bytes)
        {
            return;
        }
        self.top.push(Reverse((bytes, name())));
        if self.top.len() > TOP_FILES {
            self.top.pop();
        }
    }

    /// Adds `other` (the same directory, seen in another burst).
    fn merge(&mut self, other: Self) {
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.files = self.files.saturating_add(other.files);
        for Reverse((bytes, name)) in other.top {
            self.offer(bytes, || name);
        }
    }
}

#[derive(Default)]
struct Buf {
    /// Path of the directory this thread is reading (reused allocation).
    cur_dir: PathBuf,
    cur: Option<DirAcc>,
    done: Vec<DirAcc>,
    /// Every directory entered from this thread.
    dirs: Vec<DirRec>,
}

/// `(dev, ino)` of hard-linked files seen.
type LinkShard = Mutex<HashSet<(u64, u64)>>;

struct Collector<'a> {
    bufs: PerThread<Buf>,
    /// Hard-linked files seen, sharded by inode (a system volume has ~10^5 of them).
    linked: Box<[LinkShard]>,
    denied: DeniedLog,
    ctx: &'a JobCtx,
}

impl Visitor for Collector<'_> {
    fn enter_dir(&self, dir: &Path, _depth: u32) -> bool {
        let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
            return true;
        };
        let rec = DirRec {
            key: key(dir),
            parent: key(parent),
            name: name.into(),
        };
        self.bufs.with(|buf| buf.dirs.push(rec));
        true
    }

    fn file(&self, path: &Path, meta: &FileMeta) {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        if meta.nlink > 1 && !self.first_link(meta) {
            return;
        }
        self.bufs.with(|buf| {
            if buf.cur.is_none() || buf.cur_dir != dir {
                let fresh = DirAcc::new(key(dir));
                if let Some(mut done) = buf.cur.replace(fresh) {
                    self.ctx.add_bytes(done.bytes);
                    done.top.shrink_to_fit();
                    buf.done.push(done);
                }
                buf.cur_dir.as_mut_os_string().clear();
                buf.cur_dir.as_mut_os_string().push(dir.as_os_str());
            }
            if let Some(acc) = &mut buf.cur {
                acc.add(name, meta.size);
            }
        });
    }

    fn denied(&self, path: &Path, err: &io::Error) {
        self.denied.push(path, err);
    }
}

impl Collector<'_> {
    /// Whether this is the first path of a hard-linked inode.
    fn first_link(&self, meta: &FileMeta) -> bool {
        let shard = usize::try_from(meta.ino % LINK_SHARDS).unwrap_or(0);
        self.linked
            .get(shard)
            .is_none_or(|set| set.lock().insert((meta.dev, meta.ino)))
    }
}

/// Assembles a [`SpaceTree`] (root path and denied list are set by the caller).
#[derive(Default)]
struct Builder {
    nodes: Vec<Node>,
    names: String,
    odd_names: HashMap<u32, Box<OsStr>>,
}

fn too_many() -> Error {
    Error::Invalid("space tree too large".to_owned())
}

impl Builder {
    /// Directory nodes first (the first record is the root), then file and `Rest` nodes;
    /// sizes summed bottom-up; children sorted largest first. Takes the per-thread
    /// buffers as they are (no concatenated copies at peak memory).
    fn build(mut self, dirs: Vec<Vec<DirRec>>, accs: Vec<Vec<DirAcc>>) -> Result<SpaceTree> {
        let dir_count = dirs
            .iter()
            .map(Vec::len)
            .fold(0_usize, usize::saturating_add);
        let mut index: HashMap<Key, u32> = HashMap::with_capacity(dir_count);
        for (i, rec) in dirs.iter().flatten().enumerate() {
            let id = u32::try_from(i).map_err(|_| too_many())?;
            index.entry(rec.key).or_insert(id);
        }
        self.nodes.reserve(dir_count);
        for rec in dirs.into_iter().flatten() {
            let parent = index.get(&rec.parent).copied().unwrap_or(ROOT);
            self.push(parent, &rec.name, SpaceKind::Dir, 0, 0)?;
        }
        // A directory read in several bursts (possible, though the walker reads each in
        // one task) has its bursts merged before its nodes are made.
        let dir_of = |acc: &DirAcc| index.get(&acc.dir).and_then(|&d| usize::try_from(d).ok());
        let mut bursts = vec![0_u8; dir_count];
        for acc in accs.iter().flatten() {
            if let Some(n) = dir_of(acc).and_then(|d| bursts.get_mut(d)) {
                *n = n.saturating_add(1);
            }
        }
        let mut split: HashMap<usize, DirAcc> = HashMap::new();
        for acc in accs.into_iter().flatten() {
            let Some(dir) = dir_of(&acc) else { continue };
            if bursts.get(dir).is_some_and(|n| *n > 1) {
                match split.get_mut(&dir) {
                    Some(first) => first.merge(acc),
                    None => {
                        split.insert(dir, acc);
                    }
                }
            } else {
                self.add_files(dir, acc)?;
            }
        }
        drop(index);
        for (dir, acc) in split {
            self.add_files(dir, acc)?;
        }
        self.sum_up(dir_count);
        let children = self.link_children(dir_count)?;
        Ok(SpaceTree {
            root: PathBuf::new(),
            nodes: self.nodes,
            children,
            names: self.names,
            odd_names: self.odd_names,
            denied: Vec::new(),
        })
    }

    fn push(
        &mut self,
        parent: u32,
        name: &OsStr,
        kind: SpaceKind,
        bytes: u64,
        files: u64,
    ) -> Result<()> {
        let id = u32::try_from(self.nodes.len()).map_err(|_| too_many())?;
        let (name_at, name_len) = if let Some(utf8) = name.to_str() {
            let at = u32::try_from(self.names.len()).map_err(|_| too_many())?;
            let len = u32::try_from(utf8.len()).map_err(|_| too_many())?;
            self.names.push_str(utf8);
            (at, len)
        } else {
            self.odd_names.insert(id, name.into());
            (0, 0)
        };
        self.nodes.push(Node {
            bytes,
            files,
            parent,
            name_at,
            name_len,
            first_child: 0,
            child_count: 0,
            kind,
        });
        Ok(())
    }

    /// Sets directory `dir`'s own totals and appends its file and `Rest` nodes.
    fn add_files(&mut self, dir: usize, acc: DirAcc) -> Result<()> {
        let parent = u32::try_from(dir).map_err(|_| too_many())?;
        if let Some(node) = self.nodes.get_mut(dir) {
            node.bytes = acc.bytes;
            node.files = acc.files;
        }
        let mut kept_bytes = 0_u64;
        let mut kept_files = 0_u64;
        for Reverse((bytes, name)) in acc.top {
            kept_bytes = kept_bytes.saturating_add(bytes);
            kept_files = kept_files.saturating_add(1);
            self.push(parent, &name, SpaceKind::File, bytes, 1)?;
        }
        let rest_files = acc.files.saturating_sub(kept_files);
        if rest_files > 0 {
            let rest_bytes = acc.bytes.saturating_sub(kept_bytes);
            self.push(
                parent,
                OsStr::new(""),
                SpaceKind::Rest,
                rest_bytes,
                rest_files,
            )?;
        }
        Ok(())
    }

    /// Adds every directory's totals to its parent, leaves first (a directory is added
    /// once all its subdirectories were added to it).
    fn sum_up(&mut self, dir_count: usize) {
        let parent_of = |nodes: &[Node], i: usize| {
            nodes
                .get(i)
                .and_then(|n| usize::try_from(n.parent).ok())
                .filter(|&p| p != i)
        };
        let mut pending = vec![0_u32; dir_count];
        for i in 1..dir_count {
            if let Some(count) = parent_of(&self.nodes, i).and_then(|p| pending.get_mut(p)) {
                *count = count.saturating_add(1);
            }
        }
        let mut ready: Vec<usize> = (1..dir_count)
            .filter(|&i| pending.get(i) == Some(&0))
            .collect();
        while let Some(i) = ready.pop() {
            let Some(p) = parent_of(&self.nodes, i) else {
                continue;
            };
            let Some((bytes, files)) = self.nodes.get(i).map(|n| (n.bytes, n.files)) else {
                continue;
            };
            if let Some(parent) = self.nodes.get_mut(p) {
                parent.bytes = parent.bytes.saturating_add(bytes);
                parent.files = parent.files.saturating_add(files);
            }
            if let Some(count) = pending.get_mut(p) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    ready.push(p);
                }
            }
        }
    }

    /// One children array for all directories (each run largest first).
    fn link_children(&mut self, dir_count: usize) -> Result<Vec<u32>> {
        let mut counts = vec![0_u32; dir_count];
        for node in self.nodes.iter().skip(1) {
            if let Some(count) = usize::try_from(node.parent)
                .ok()
                .and_then(|p| counts.get_mut(p))
            {
                *count = count.saturating_add(1);
            }
        }
        let mut next = 0_u32;
        for (node, count) in self.nodes.iter_mut().zip(&counts) {
            node.first_child = next;
            next = next.checked_add(*count).ok_or_else(too_many)?;
        }
        let mut children = vec![0_u32; usize::try_from(next).map_err(|_| too_many())?];
        for i in 1..self.nodes.len() {
            let id = u32::try_from(i).map_err(|_| too_many())?;
            let Some(parent) = self
                .nodes
                .get(i)
                .and_then(|n| usize::try_from(n.parent).ok())
                .and_then(|p| self.nodes.get_mut(p))
            else {
                continue;
            };
            let at = parent.first_child.saturating_add(parent.child_count);
            parent.child_count = parent.child_count.saturating_add(1);
            if let Some(slot) = usize::try_from(at).ok().and_then(|a| children.get_mut(a)) {
                *slot = id;
            }
        }
        let bytes_of = |id: u32| {
            usize::try_from(id)
                .ok()
                .and_then(|i| self.nodes.get(i))
                .map_or(0, |n| n.bytes)
        };
        for node in self.nodes.iter().take(dir_count) {
            let start = usize::try_from(node.first_child).map_err(|_| too_many())?;
            let len = usize::try_from(node.child_count).map_err(|_| too_many())?;
            let end = start.checked_add(len).ok_or_else(too_many)?;
            if let Some(run) = children.get_mut(start..end) {
                run.sort_unstable_by_key(|&id| (Reverse(bytes_of(id)), id));
            }
        }
        Ok(children)
    }
}

#[cfg(test)]
mod tests {
    use crate::walk::WalkOptions;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omc-space-{name}-{}", std::process::id()));
        let _ignored = fs::remove_dir_all(&dir);
        assert!(fs::create_dir_all(&dir).is_ok(), "mkdir {}", dir.display());
        dir
    }

    fn write(dir: &Path, name: &str, len: usize) -> u64 {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            assert!(
                fs::create_dir_all(parent).is_ok(),
                "mkdir {}",
                parent.display()
            );
        }
        assert!(fs::write(&path, vec![1_u8; len]).is_ok(), "write {name}");
        let meta = fs::symlink_metadata(&path);
        assert!(meta.is_ok(), "stat {name}");
        meta.map_or(0, |m| FileMeta::from_metadata(&m).size)
    }

    fn scan_dir(dir: &Path) -> Option<SpaceTree> {
        let walker = Walker::new(WalkOptions::default());
        assert!(walker.is_ok(), "pool builds");
        let tree = scan(dir, &walker.ok()?, &JobCtx::new());
        assert!(tree.is_ok(), "scan runs: {tree:?}");
        tree.ok()
    }

    #[test]
    fn sizes_aggregate_bottom_up_and_sort_largest_first() {
        let dir = temp_dir("sizes");
        let top = write(&dir, "top.bin", 100_000);
        let deep = write(&dir, "big/x/deep.bin", 300_000);
        let y = write(&dir, "big/y.bin", 200_000);
        let z = write(&dir, "small/z.bin", 70_000);
        // Below FILE_NODE_MIN: only counted in the directory's rest node.
        let tiny = write(&dir, "small/tiny", 100);
        assert!(fs::create_dir_all(dir.join("empty")).is_ok(), "mkdir empty");
        let Some(tree) = scan_dir(&dir) else { return };

        let root = tree.listing(0);
        assert!(root.is_some(), "root listing");
        let Some(root) = root else { return };
        let total = [top, deep, y, z, tiny]
            .iter()
            .fold(0_u64, |t, s| t.saturating_add(*s));
        assert_eq!(root.bytes, total, "root sums everything");
        assert_eq!(root.files, 5, "root counts every file");
        assert_eq!(root.path, dir.display().to_string(), "root path");
        let names: Vec<&str> = root.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["big", "top.bin", "small", "empty"], "largest first");
        let small = root.children.get(2).and_then(|n| tree.listing(n.id));
        let kinds: Option<Vec<(SpaceKind, u64)>> =
            small.map(|l| l.children.iter().map(|c| (c.kind, c.files)).collect());
        assert_eq!(
            kinds,
            Some(vec![(SpaceKind::File, 1), (SpaceKind::Rest, 1)]),
            "small files go to the rest node"
        );

        let big = root.children.first();
        assert!(
            big.is_some_and(|n| n.kind == SpaceKind::Dir
                && n.bytes == deep.saturating_add(y)
                && n.files == 2),
            "directory totals: {big:?}"
        );
        let Some(big) = big else { return };
        let listing = tree.listing(big.id);
        assert!(
            listing
                .as_ref()
                .is_some_and(|l| l.path == dir.join("big").display().to_string()
                    && l.children.len() == 2
                    && l.denied.is_empty()),
            "child listing: {listing:?}"
        );
        let file = root.children.get(1);
        assert!(
            file.is_some_and(|f| f.kind == SpaceKind::File
                && tree.listing(f.id).is_none()
                && tree.path(f.id) == Some(dir.join("top.bin"))),
            "file node: {file:?}"
        );
        let _ignored = fs::remove_dir_all(&dir);
    }

    #[test]
    fn many_small_files_become_one_rest_node() {
        let dir = temp_dir("rest");
        let extra = 5_usize;
        let mut total = write(&dir, "tiny", 100);
        for i in 0..TOP_FILES + extra {
            // Distinct allocation sizes above the node floor.
            let len = usize::try_from(FILE_NODE_MIN).unwrap_or(0) + (i + 1) * 4_096;
            total = total.saturating_add(write(&dir, &format!("f{i:03}"), len));
        }
        let Some(tree) = scan_dir(&dir) else { return };
        let Some(root) = tree.listing(0) else {
            assert!(tree.listing(0).is_some(), "root listing");
            return;
        };
        assert_eq!(root.bytes, total, "rest included in totals");
        assert_eq!(
            root.children.len(),
            TOP_FILES + 1,
            "top files + one rest node"
        );
        let rest: Vec<&SpaceNode> = root
            .children
            .iter()
            .filter(|c| c.kind == SpaceKind::Rest)
            .collect();
        assert_eq!(rest.len(), 1, "exactly one rest node");
        let Some(rest) = rest.first() else { return };
        assert_eq!(rest.files, 6, "the smallest files are aggregated");
        let largest = root.children.iter().find(|c| c.kind == SpaceKind::File);
        assert!(
            largest.is_some_and(|c| c.name == format!("f{:03}", TOP_FILES + extra - 1)),
            "largest file first: {largest:?}"
        );
        assert!(
            tree.target(rest.id, DeleteMethod::Trash).is_none(),
            "rest not removable"
        );
        assert!(
            tree.target(0, DeleteMethod::Trash).is_none(),
            "root not removable"
        );
        assert!(
            tree.target(u32::MAX, DeleteMethod::Trash).is_none(),
            "unknown id"
        );
        let first = largest.map(|c| c.id).unwrap_or_default();
        let target = tree.target(first, DeleteMethod::Trash);
        assert!(
            target.is_some_and(|t| t.method == DeleteMethod::Trash
                && t.location
                    == omc_proto::jobs::Location::Path {
                        path: dir
                            .join(format!("f{:03}", TOP_FILES + extra - 1))
                            .display()
                            .to_string()
                    }),
            "file target"
        );
        let _ignored = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_count_once() {
        let dir = temp_dir("links");
        let size = write(&dir, "a/orig", 50_000);
        assert!(
            fs::hard_link(dir.join("a/orig"), dir.join("b-link")).is_ok(),
            "link"
        );
        let Some(tree) = scan_dir(&dir) else { return };
        let root = tree.listing(0);
        assert!(
            root.is_some_and(|r| r.bytes == size && r.files == 1),
            "one inode counted once"
        );
        let _ignored = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_non_directories() {
        let dir = temp_dir("reject");
        write(&dir, "file", 10);
        let walker = Walker::new(WalkOptions::default());
        assert!(walker.is_ok(), "pool builds");
        let Ok(walker) = walker else { return };
        let ctx = JobCtx::new();
        assert!(scan(&dir.join("file"), &walker, &ctx).is_err(), "file root");
        assert!(
            scan(&dir.join("nope"), &walker, &ctx).is_err(),
            "missing root"
        );
        assert!(
            scan(Path::new("relative"), &walker, &ctx).is_err(),
            "relative root"
        );
        let _ignored = fs::remove_dir_all(&dir);
    }
}
