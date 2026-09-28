//! The parallel file-system walker every scan is built on.
//!
//! Design (measured against `du`/`pdu`-style tools): one rayon pool per job, one task per
//! directory (`rayon::scope` + `spawn`), so wide trees saturate every core and deep trees
//! stay depth-first in memory. Per entry it costs one `readdir` record plus one `lstat` on
//! Unix (Windows gets metadata from `FindNextFile` for free). Nothing is collected: callers
//! aggregate in their [`Visitor`], so memory stays proportional to what they keep.
//!
//! Never follows symlinks (Windows junctions and mount points count as symlinks in std),
//! optionally stays on one file system, honours the exclusion list, skips hidden entries
//! on request, and treats cloud placeholders (iCloud "dataless", `OneDrive` on-demand) as
//! files that occupy no space.

use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use omc_proto::settings::CleanSettings;
use parking_lot::Mutex;
use rayon::prelude::*;

use crate::ctx::JobCtx;
use crate::paths;
use crate::{Error, Result};

/// What a walk honours.
#[derive(Debug, Clone, Default)]
pub struct WalkOptions {
    /// Normalized absolute paths never entered or reported.
    pub exclude: Vec<PathBuf>,
    /// Skip dot-files and entries flagged hidden.
    pub skip_hidden: bool,
    /// Do not descend into another file system than the root's.
    pub one_file_system: bool,
    /// Pool size; 0 = one thread per logical CPU.
    pub threads: usize,
}

impl WalkOptions {
    /// Options from the user's settings (hidden files are the caller's decision: junk
    /// scans always include them).
    pub fn from_settings(settings: &CleanSettings) -> Self {
        Self {
            exclude: settings
                .exclude
                .iter()
                .filter_map(|p| paths::normalize(&paths::expand(p)))
                .collect(),
            skip_hidden: false,
            one_file_system: settings.one_file_system,
            threads: usize::from(settings.scan_threads),
        }
    }

    /// `path` is excluded by the user.
    pub fn is_excluded(&self, path: &Path) -> bool {
        self.exclude.iter().any(|ex| paths::is_within(path, ex))
    }
}

/// Metadata of a visited file, reduced to what scans use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMeta {
    /// Bytes the file occupies on disk (allocated blocks on Unix; the logical length on
    /// Windows). This is what removing it frees.
    pub size: u64,
    /// Logical length.
    pub len: u64,
    /// Last modification, Unix seconds.
    pub modified: Option<i64>,
    /// Last access, Unix seconds.
    pub accessed: Option<i64>,
    /// Device id (0 on Windows).
    pub dev: u64,
    /// Inode / file index (0 on Windows).
    pub ino: u64,
    /// Hard-link count (1 on Windows).
    pub nlink: u64,
    /// Cloud placeholder whose content is not local (reading it would download it).
    pub placeholder: bool,
}

impl FileMeta {
    /// Reduces std metadata.
    pub fn from_metadata(meta: &Metadata) -> Self {
        let modified = meta.modified().ok().map(paths::unix_secs);
        let accessed = meta.accessed().ok().map(paths::unix_secs);
        let len = meta.len();
        let (size, dev, ino, nlink, placeholder) = platform_meta(meta, len);
        Self {
            size,
            len,
            modified,
            accessed,
            dev,
            ino,
            nlink,
            placeholder,
        }
    }
}

#[cfg(unix)]
fn platform_meta(meta: &Metadata, len: u64) -> (u64, u64, u64, u64, bool) {
    use std::os::unix::fs::MetadataExt as _;
    let allocated = meta.blocks().saturating_mul(512);
    #[cfg(target_os = "macos")]
    let placeholder = {
        use std::os::macos::fs::MetadataExt as _;
        /// `SF_DATALESS`: iCloud/File Provider file whose content lives in the cloud.
        const SF_DATALESS: u32 = 0x4000_0000;
        meta.st_flags() & SF_DATALESS != 0
    };
    #[cfg(not(target_os = "macos"))]
    let placeholder = false;
    // Sparse and compressed files occupy less than their length; a file whose blocks are
    // not reported (some FUSE/network file systems) falls back to its length.
    let size = if allocated == 0 && !placeholder && len > 0 && !cfg!(target_os = "macos") {
        len
    } else {
        allocated
    };
    (size, meta.dev(), meta.ino(), meta.nlink(), placeholder)
}

#[cfg(windows)]
fn platform_meta(meta: &Metadata, len: u64) -> (u64, u64, u64, u64, bool) {
    use std::os::windows::fs::MetadataExt as _;
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    let attrs = meta.file_attributes();
    let placeholder = attrs
        & (FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0;
    let size = if placeholder { 0 } else { len };
    (size, 0, 0, 1, placeholder)
}

/// Whether an entry counts as hidden: a dot-name everywhere, plus the platform's hidden
/// flag (macOS `UF_HIDDEN`, Windows `FILE_ATTRIBUTE_HIDDEN`).
pub fn is_hidden(name: &std::ffi::OsStr, meta: Option<&Metadata>) -> bool {
    if name.as_encoded_bytes().first() == Some(&b'.') {
        return true;
    }
    let Some(meta) = meta else { return false };
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt as _;
        const UF_HIDDEN: u32 = 0x8000;
        meta.st_flags() & UF_HIDDEN != 0
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = meta;
        false
    }
}

/// Callbacks of a walk. Called concurrently from pool threads.
pub trait Visitor: Sync {
    /// Called for every directory below a root before it is read (`depth` 1 = a root's
    /// child). Return `false` to skip it entirely.
    fn enter_dir(&self, _dir: &Path, _depth: u32) -> bool {
        true
    }

    /// Called for every regular file (and other non-directory, non-symlink entries).
    fn file(&self, path: &Path, meta: &FileMeta);

    /// A directory that could not be read.
    fn denied(&self, _path: &Path, _err: &io::Error) {}
}

/// Totals of [`Walker::measure`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Measure {
    /// Bytes on disk, hard links counted once.
    pub bytes: u64,
    /// Files.
    pub files: u64,
    /// Newest modification (Unix seconds).
    pub newest: Option<i64>,
    /// Some directory below could not be read (the totals are a lower bound).
    pub incomplete: bool,
    /// The path does not exist.
    pub missing: bool,
}

impl Measure {
    fn add_file(&mut self, meta: &FileMeta) {
        self.bytes = self.bytes.saturating_add(meta.size);
        self.files = self.files.saturating_add(1);
        self.newest = self.newest.max(meta.modified);
    }
}

/// A configured walker with its own thread pool.
#[derive(Debug)]
pub struct Walker {
    pool: rayon::ThreadPool,
    opts: WalkOptions,
}

impl Walker {
    /// Builds the pool.
    pub fn new(opts: WalkOptions) -> Result<Self> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(opts.threads)
            .thread_name(|i| format!("omc-scan-{i}"))
            .build()
            .map_err(|e| Error::Pool(e.to_string()))?;
        Ok(Self { pool, opts })
    }

    /// The options.
    pub fn options(&self) -> &WalkOptions {
        &self.opts
    }

    /// Runs `f` inside the pool, so rayon parallel iterators in it use these threads.
    pub fn install<R: Send>(&self, f: impl FnOnce() -> R + Send) -> R {
        self.pool.install(f)
    }

    /// Walks every root in parallel. Roots that are files are reported as files; missing
    /// roots are ignored; excluded roots are skipped.
    pub fn walk(&self, roots: &[PathBuf], ctx: &JobCtx, visitor: &dyn Visitor) {
        let mut dirs = Vec::with_capacity(roots.len());
        for root in roots {
            if self.opts.is_excluded(root) {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(root) else {
                continue;
            };
            if meta.is_dir() {
                dirs.push((root, FsFilter::for_root(root, &meta, &self.opts)));
            } else if meta.is_file() {
                ctx.add_items(1);
                visitor.file(root, &FileMeta::from_metadata(&meta));
            }
        }
        self.pool.install(|| {
            rayon::scope(|scope| {
                for (root, fs_filter) in &dirs {
                    scope.spawn(move |scope| {
                        self.walk_dir(scope, root, 1, fs_filter, ctx, visitor);
                    });
                }
            });
        });
    }

    fn walk_dir<'s>(
        &'s self,
        scope: &rayon::Scope<'s>,
        dir: &Path,
        depth: u32,
        fs_filter: &'s FsFilter,
        ctx: &'s JobCtx,
        visitor: &'s dyn Visitor,
    ) {
        if ctx.is_cancelled() {
            return;
        }
        ctx.entered_dir(dir);
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => {
                visitor.denied(dir, &err);
                return;
            }
        };
        let mut visited = 0_u64;
        for entry in entries {
            let Ok(entry) = entry else { continue };
            visited = visited.saturating_add(1);
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            let hidden_by_name = self.opts.skip_hidden
                && entry.file_name().as_encoded_bytes().first() == Some(&b'.');
            if hidden_by_name || self.opts.is_excluded(&path) {
                continue;
            }
            if file_type.is_dir() {
                let child_depth = depth.saturating_add(1);
                if self.opts.skip_hidden || fs_filter.active() {
                    let Ok(meta) = entry.metadata() else { continue };
                    if self.opts.skip_hidden && is_hidden(&entry.file_name(), Some(&meta)) {
                        continue;
                    }
                    if !fs_filter.allows(&path, &meta) {
                        continue;
                    }
                }
                if !visitor.enter_dir(&path, depth) {
                    continue;
                }
                scope.spawn(move |scope| {
                    self.walk_dir(scope, &path, child_depth, fs_filter, ctx, visitor);
                });
            } else {
                let Ok(meta) = entry.metadata() else { continue };
                if self.opts.skip_hidden && is_hidden(&entry.file_name(), Some(&meta)) {
                    continue;
                }
                visitor.file(&path, &FileMeta::from_metadata(&meta));
            }
        }
        ctx.add_items(visited);
    }

    /// Size of one file or directory tree (hard links counted once).
    pub fn measure(&self, path: &Path, ctx: &JobCtx) -> Measure {
        self.pool.install(|| self.measure_in_pool(path, ctx))
    }

    /// [`Self::measure`] for many paths at once, in parallel; results are in input order.
    pub fn measure_all(&self, paths: &[PathBuf], ctx: &JobCtx) -> Vec<Measure> {
        self.pool.install(|| {
            paths
                .par_iter()
                .map(|p| self.measure_in_pool(p, ctx))
                .collect()
        })
    }

    fn measure_in_pool(&self, path: &Path, ctx: &JobCtx) -> Measure {
        let Ok(meta) = fs::symlink_metadata(path) else {
            return Measure {
                missing: true,
                ..Measure::default()
            };
        };
        if !meta.is_dir() {
            let mut m = Measure::default();
            if meta.is_file() {
                let fm = FileMeta::from_metadata(&meta);
                m.add_file(&fm);
                ctx.add_items(1);
            }
            return m;
        }
        let summer = Summer::new();
        let fs_filter = FsFilter::for_root(path, &meta, &self.opts);
        rayon::scope(|scope| {
            self.walk_dir(scope, path, 1, &fs_filter, ctx, &summer);
        });
        summer.into_measure()
    }
}

/// Sums files with atomics (no lock per file: wide trees would serialize on it); only
/// hard-linked files (nlink > 1, rare) take a lock to be counted once per measurement.
#[derive(Default)]
struct Summer {
    bytes: AtomicU64,
    files: AtomicU64,
    /// Newest mtime; `i64::MIN` = none.
    newest: AtomicI64,
    incomplete: AtomicBool,
    linked: Mutex<HashSet<(u64, u64)>>,
}

impl Summer {
    fn new() -> Self {
        Self {
            newest: AtomicI64::new(i64::MIN),
            ..Self::default()
        }
    }

    fn into_measure(self) -> Measure {
        let newest = self.newest.into_inner();
        Measure {
            bytes: self.bytes.into_inner(),
            files: self.files.into_inner(),
            newest: (newest != i64::MIN).then_some(newest),
            incomplete: self.incomplete.into_inner(),
            missing: false,
        }
    }
}

impl Visitor for Summer {
    fn file(&self, _path: &Path, meta: &FileMeta) {
        if meta.nlink > 1 && !self.linked.lock().insert((meta.dev, meta.ino)) {
            return;
        }
        // Saturation is irrelevant at u64 scale; `fetch_add` wraps, which 2^64 bytes never
        // reaches.
        self.bytes.fetch_add(meta.size, Ordering::Relaxed);
        self.files.fetch_add(1, Ordering::Relaxed);
        if let Some(modified) = meta.modified {
            self.newest.fetch_max(modified, Ordering::Relaxed);
        }
    }

    fn denied(&self, _path: &Path, _err: &io::Error) {
        self.incomplete.store(true, Ordering::Relaxed);
    }
}

/// The one-file-system rule for one root.
#[derive(Debug)]
struct FsFilter {
    /// Devices a walk may enter; empty = unrestricted.
    devices: Vec<u64>,
    /// Directories never entered (the macOS data volume's second path when walking `/`).
    skip: Vec<PathBuf>,
}

impl FsFilter {
    fn for_root(root: &Path, meta: &Metadata, opts: &WalkOptions) -> Self {
        if !opts.one_file_system {
            return Self {
                devices: Vec::new(),
                skip: Vec::new(),
            };
        }
        let mut devices = vec![device(meta)];
        let mut skip = Vec::new();
        // macOS splits the startup disk into a read-only system volume and the data
        // volume, joined by firmlinks (/Users, /Applications, /Library…). Walking `/`
        // must enter both, but not the data volume's second mount point.
        if cfg!(target_os = "macos") && root == Path::new("/") {
            let data = Path::new("/System/Volumes/Data");
            if let Ok(data_meta) = fs::symlink_metadata(data) {
                devices.push(device(&data_meta));
                skip.push(data.to_path_buf());
            }
        }
        Self { devices, skip }
    }

    fn active(&self) -> bool {
        !self.devices.is_empty()
    }

    fn allows(&self, dir: &Path, meta: &Metadata) -> bool {
        if !self.active() {
            return true;
        }
        self.devices.contains(&device(meta)) && !self.skip.iter().any(|s| s == dir)
    }
}

#[cfg(unix)]
fn device(meta: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    meta.dev()
}

/// Windows: mount points are reparse points, which the walker never follows, so every
/// directory it reaches is on the root's volume.
#[cfg(windows)]
fn device(_meta: &Metadata) -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn temp_tree(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omc-walk-{name}-{}", std::process::id()));
        let _ignored = fs::remove_dir_all(&dir);
        for sub in ["a/b/c", "a/d", ".hidden", "skip/me"] {
            assert!(fs::create_dir_all(dir.join(sub)).is_ok(), "mkdir {sub}");
        }
        for (file, len) in [
            ("a/one", 10_usize),
            ("a/b/two", 20),
            ("a/b/c/three", 30),
            ("a/d/four", 40),
            (".hidden/five", 50),
            ("skip/me/six", 60),
        ] {
            assert!(
                fs::write(dir.join(file), vec![7_u8; len]).is_ok(),
                "write {file}"
            );
        }
        dir
    }

    struct Count {
        files: AtomicU64,
        len: AtomicU64,
    }

    impl Visitor for Count {
        fn file(&self, _path: &Path, meta: &FileMeta) {
            self.files.fetch_add(1, Ordering::Relaxed);
            self.len.fetch_add(meta.len, Ordering::Relaxed);
        }
    }

    #[test]
    fn walk_honours_hidden_and_exclusions() {
        let dir = temp_tree("opts");
        let opts = WalkOptions {
            exclude: vec![dir.join("skip")],
            skip_hidden: true,
            ..WalkOptions::default()
        };
        let walker = Walker::new(opts);
        assert!(walker.is_ok(), "pool builds");
        let Ok(walker) = walker else { return };
        let count = Count {
            files: AtomicU64::new(0),
            len: AtomicU64::new(0),
        };
        walker.walk(std::slice::from_ref(&dir), &JobCtx::new(), &count);
        assert_eq!(
            count.files.load(Ordering::Relaxed),
            4,
            "hidden + excluded skipped"
        );
        assert_eq!(
            count.len.load(Ordering::Relaxed),
            100,
            "sum of visible files"
        );
        let _ignored = fs::remove_dir_all(&dir);
    }

    #[test]
    fn measure_counts_hard_links_once_and_flags_missing() {
        let dir = temp_tree("measure");
        let linked = fs::hard_link(dir.join("a/one"), dir.join("a/one-link")).is_ok();
        let walker = Walker::new(WalkOptions::default());
        assert!(walker.is_ok(), "pool builds");
        let Ok(walker) = walker else { return };
        let ctx = JobCtx::new();
        let m = walker.measure(&dir, &ctx);
        assert_eq!(m.files, 6, "every file, hard link once (linked: {linked})");
        assert!(!m.missing && !m.incomplete, "complete: {m:?}");
        assert!(m.newest.is_some(), "newest mtime recorded");
        let all = walker.measure_all(&[dir.join("a"), dir.join("nope")], &ctx);
        assert_eq!(all.len(), 2, "one result per input");
        assert!(
            all.get(1).is_some_and(|m| m.missing),
            "missing path flagged"
        );
        assert!(all.first().is_some_and(|m| m.files == 4), "a/ has 4 files");
        let _ignored = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_walk_stops_early() {
        let dir = temp_tree("cancel");
        let walker = Walker::new(WalkOptions::default());
        assert!(walker.is_ok(), "pool builds");
        let Ok(walker) = walker else { return };
        let ctx = JobCtx::new();
        ctx.cancel();
        let m = walker.measure(&dir, &ctx);
        assert_eq!(m.files, 0, "nothing visited after cancel");
        let _ignored = fs::remove_dir_all(&dir);
    }
}
