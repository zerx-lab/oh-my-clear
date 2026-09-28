//! Large & old files: one walk over the user's file roots, keeping only the
//! [`MAX_FILES`] largest matches in bounded per-thread min-heaps (never a list of every
//! file). Also hosts the small helpers the other file scans share (root de-duplication,
//! per-thread buffers, the capped denied list).

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use omc_proto::files::{FileEntry, FileKind, FileReport};
use omc_proto::jobs::{Denied, Phase};
use omc_proto::settings::CleanSettings;
use parking_lot::Mutex;

use crate::ctx::JobCtx;
use crate::target::{Scanned, Target, push_target};
use crate::walk::{FileMeta, Visitor, Walker};
use crate::{Error, Result, errors, paths};

/// Most files one report holds (the smallest matches beyond it are dropped).
pub const MAX_FILES: usize = 10_000;

/// Most unreadable places one report lists.
pub(crate) const MAX_DENIED: usize = 200;

/// Seconds per day.
const DAY_SECS: i64 = 86_400;

/// Extension → kind, compared case-insensitively.
const KINDS: &[(&str, FileKind)] = &[
    ("mp4", FileKind::Video),
    ("m4v", FileKind::Video),
    ("mov", FileKind::Video),
    ("avi", FileKind::Video),
    ("mkv", FileKind::Video),
    ("wmv", FileKind::Video),
    ("flv", FileKind::Video),
    ("webm", FileKind::Video),
    ("mpg", FileKind::Video),
    ("mpeg", FileKind::Video),
    ("3gp", FileKind::Video),
    ("mts", FileKind::Video),
    ("m2ts", FileKind::Video),
    ("vob", FileKind::Video),
    ("mp3", FileKind::Audio),
    ("m4a", FileKind::Audio),
    ("aac", FileKind::Audio),
    ("flac", FileKind::Audio),
    ("wav", FileKind::Audio),
    ("aif", FileKind::Audio),
    ("aiff", FileKind::Audio),
    ("ogg", FileKind::Audio),
    ("opus", FileKind::Audio),
    ("wma", FileKind::Audio),
    ("ape", FileKind::Audio),
    ("jpg", FileKind::Image),
    ("jpeg", FileKind::Image),
    ("png", FileKind::Image),
    ("gif", FileKind::Image),
    ("bmp", FileKind::Image),
    ("tif", FileKind::Image),
    ("tiff", FileKind::Image),
    ("heic", FileKind::Image),
    ("heif", FileKind::Image),
    ("webp", FileKind::Image),
    ("raw", FileKind::Image),
    ("cr2", FileKind::Image),
    ("cr3", FileKind::Image),
    ("nef", FileKind::Image),
    ("arw", FileKind::Image),
    ("dng", FileKind::Image),
    ("orf", FileKind::Image),
    ("rw2", FileKind::Image),
    ("raf", FileKind::Image),
    ("psd", FileKind::Image),
    ("zip", FileKind::Archive),
    ("tar", FileKind::Archive),
    ("gz", FileKind::Archive),
    ("tgz", FileKind::Archive),
    ("bz2", FileKind::Archive),
    ("tbz", FileKind::Archive),
    ("xz", FileKind::Archive),
    ("txz", FileKind::Archive),
    ("7z", FileKind::Archive),
    ("rar", FileKind::Archive),
    ("zst", FileKind::Archive),
    ("lz4", FileKind::Archive),
    ("lzma", FileKind::Archive),
    ("cab", FileKind::Archive),
    ("dmg", FileKind::DiskImage),
    ("iso", FileKind::DiskImage),
    ("img", FileKind::DiskImage),
    ("pkg", FileKind::DiskImage),
    ("mpkg", FileKind::DiskImage),
    ("msi", FileKind::DiskImage),
    ("msix", FileKind::DiskImage),
    ("appx", FileKind::DiskImage),
    ("exe", FileKind::DiskImage),
    ("deb", FileKind::DiskImage),
    ("rpm", FileKind::DiskImage),
    ("appimage", FileKind::DiskImage),
    ("snap", FileKind::DiskImage),
    ("sparseimage", FileKind::DiskImage),
    ("toast", FileKind::DiskImage),
    ("pdf", FileKind::Document),
    ("doc", FileKind::Document),
    ("docx", FileKind::Document),
    ("xls", FileKind::Document),
    ("xlsx", FileKind::Document),
    ("ppt", FileKind::Document),
    ("pptx", FileKind::Document),
    ("odt", FileKind::Document),
    ("ods", FileKind::Document),
    ("odp", FileKind::Document),
    ("rtf", FileKind::Document),
    ("txt", FileKind::Document),
    ("md", FileKind::Document),
    ("csv", FileKind::Document),
    ("pages", FileKind::Document),
    ("numbers", FileKind::Document),
    ("key", FileKind::Document),
    ("epub", FileKind::Document),
    ("vdi", FileKind::VirtualMachine),
    ("vmdk", FileKind::VirtualMachine),
    ("vhd", FileKind::VirtualMachine),
    ("vhdx", FileKind::VirtualMachine),
    ("qcow", FileKind::VirtualMachine),
    ("qcow2", FileKind::VirtualMachine),
    ("hdd", FileKind::VirtualMachine),
    ("ova", FileKind::VirtualMachine),
    ("vmem", FileKind::VirtualMachine),
];

/// Coarse type of `path` from its extension (case-insensitive).
pub fn file_kind(path: &Path) -> FileKind {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return FileKind::Other;
    };
    KINDS
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(ext))
        .map_or(FileKind::Other, |(_, kind)| *kind)
}

/// Finds files at least `large_min_bytes` large, or not modified for `old_days` and at
/// least `old_min_bytes` large, below `roots`. Keeps the [`MAX_FILES`] largest matches.
pub fn scan(
    roots: &[PathBuf],
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Scanned<FileReport>> {
    let roots = distinct_roots(roots);
    if roots.is_empty() {
        return Ok(Scanned {
            report: FileReport::default(),
            targets: Vec::new(),
        });
    }
    ctx.set_phase(Phase::Scanning);
    let old_before = if settings.old_days > 0 {
        let age = i64::from(settings.old_days).saturating_mul(DAY_SECS);
        Some(paths::now_secs().saturating_sub(age))
    } else {
        None
    };
    let finder = Finder {
        large_min: settings.large_min_bytes,
        old_min: settings.old_min_bytes,
        old_before,
        skip_dot: settings.skip_hidden && !walker.options().skip_hidden,
        heaps: PerThread::new(walker),
        matched: AtomicU64::new(0),
        denied: DeniedLog::default(),
        ctx,
    };
    walker.walk(&roots, ctx, &finder);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let matched = finder.matched.load(Ordering::Relaxed);
    let mut all: Vec<Match> = finder
        .heaps
        .into_inner()
        .flat_map(|heap| heap.into_iter().map(|Reverse(m)| m))
        .collect();
    all.sort_unstable_by(|a, b| b.cmp(a));
    // Hard links to one inode are one file: keep its first (largest-sorted) path.
    let mut inodes = HashSet::new();
    all.retain(|m| m.inode.is_none_or(|inode| inodes.insert(inode)));
    let truncated = usize::try_from(matched).map_or(true, |n| n > MAX_FILES);
    all.truncate(MAX_FILES);

    let mut targets = Vec::with_capacity(all.len());
    let mut files = Vec::with_capacity(all.len());
    for m in all {
        let path = m.path.display().to_string();
        let id = push_target(
            &mut targets,
            Target::path(path.clone(), m.bytes, settings.files_delete),
        );
        files.push(FileEntry {
            id,
            kind: file_kind(&m.path),
            path,
            bytes: m.bytes,
            modified: m.modified,
            accessed: m.accessed,
            large: m.large,
            old: m.old,
        });
    }
    Ok(Scanned {
        report: FileReport {
            files,
            truncated,
            denied: finder.denied.into_inner(),
        },
        targets,
    })
}

/// One matching file.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Match {
    bytes: u64,
    path: Box<Path>,
    modified: Option<i64>,
    accessed: Option<i64>,
    large: bool,
    old: bool,
    /// `(dev, ino)` of a hard-linked file.
    inode: Option<(u64, u64)>,
}

struct Finder<'a> {
    large_min: u64,
    old_min: u64,
    /// Files modified before this (Unix seconds) are old; `None` = age check off.
    old_before: Option<i64>,
    /// Filter dot-names here (the walker was built without `skip_hidden`).
    skip_dot: bool,
    heaps: PerThread<BinaryHeap<Reverse<Match>>>,
    matched: AtomicU64,
    denied: DeniedLog,
    ctx: &'a JobCtx,
}

impl Visitor for Finder<'_> {
    fn enter_dir(&self, dir: &Path, _depth: u32) -> bool {
        !(self.skip_dot && is_dot_name(dir))
    }

    fn file(&self, path: &Path, meta: &FileMeta) {
        if meta.placeholder || (self.skip_dot && is_dot_name(path)) {
            return;
        }
        let bytes = if meta.size == 0 { meta.len } else { meta.size };
        let large = bytes >= self.large_min;
        let old = bytes >= self.old_min
            && self
                .old_before
                .is_some_and(|before| meta.modified.is_some_and(|m| m < before));
        if !large && !old {
            return;
        }
        self.matched.fetch_add(1, Ordering::Relaxed);
        self.ctx.add_bytes(bytes);
        self.heaps.with(|heap| {
            if heap.len() >= MAX_FILES && heap.peek().is_some_and(|Reverse(min)| min.bytes >= bytes)
            {
                return;
            }
            heap.push(Reverse(Match {
                bytes,
                path: path.into(),
                modified: meta.modified,
                accessed: meta.accessed,
                large,
                old,
                inode: (meta.nlink > 1).then_some((meta.dev, meta.ino)),
            }));
            if heap.len() > MAX_FILES {
                heap.pop();
            }
        });
    }

    fn denied(&self, path: &Path, err: &io::Error) {
        self.denied.push(path, err);
    }
}

/// The last component of `path` starts with a dot.
pub(crate) fn is_dot_name(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.as_encoded_bytes().first() == Some(&b'.'))
}

/// `roots` normalized, without duplicates and without roots inside another root (so no
/// file is reported twice).
pub(crate) fn distinct_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = roots.iter().filter_map(|r| paths::normalize(r)).collect();
    // Shorter paths first: an ancestor is kept before its descendants are considered.
    out.sort_by_key(|p| p.components().count());
    let mut kept: Vec<PathBuf> = Vec::with_capacity(out.len());
    for root in out {
        if !kept.iter().any(|k| paths::is_within(&root, k)) {
            kept.push(root);
        }
    }
    kept
}

/// One slot per pool thread (plus one for callers outside the pool): visitors keep
/// thread-local state behind an uncontended lock and merge it after the walk.
#[derive(Debug)]
pub(crate) struct PerThread<T> {
    slots: Box<[Mutex<T>]>,
}

impl<T: Default + Send> PerThread<T> {
    /// Slots for every thread of `walker`'s pool.
    pub(crate) fn new(walker: &Walker) -> Self {
        let threads = walker.install(rayon::current_num_threads);
        Self {
            slots: (0..=threads).map(|_| Mutex::new(T::default())).collect(),
        }
    }

    /// Runs `f` on the calling thread's slot.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let last = self.slots.len().checked_sub(1)?;
        let index = rayon::current_thread_index()
            .filter(|i| *i < last)
            .unwrap_or(last);
        let slot = self.slots.get(index)?;
        Some(f(&mut slot.lock()))
    }

    /// Every slot's state.
    pub(crate) fn into_inner(self) -> impl Iterator<Item = T> {
        self.slots.into_iter().map(Mutex::into_inner)
    }
}

/// Unreadable directories of a scan, capped at [`MAX_DENIED`]. Vanished directories are
/// not denials.
#[derive(Debug, Default)]
pub(crate) struct DeniedLog {
    list: Mutex<Vec<Denied>>,
}

impl DeniedLog {
    /// Records `path`.
    pub(crate) fn push(&self, path: &Path, err: &io::Error) {
        if err.kind() == io::ErrorKind::NotFound {
            return;
        }
        let mut list = self.list.lock();
        if list.len() < MAX_DENIED {
            list.push(Denied {
                path: path.display().to_string(),
                reason: errors::classify(err, path),
            });
        }
    }

    /// The recorded places.
    pub(crate) fn into_inner(self) -> Vec<Denied> {
        self.list.into_inner()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, SystemTime};

    use crate::walk::WalkOptions;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omc-large-{name}-{}", std::process::id()));
        let _ignored = fs::remove_dir_all(&dir);
        assert!(fs::create_dir_all(&dir).is_ok(), "mkdir {}", dir.display());
        dir
    }

    fn write(dir: &Path, name: &str, len: usize) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            assert!(
                fs::create_dir_all(parent).is_ok(),
                "mkdir {}",
                parent.display()
            );
        }
        assert!(fs::write(&path, vec![1_u8; len]).is_ok(), "write {name}");
        path
    }

    fn walker() -> Option<Walker> {
        let walker = Walker::new(WalkOptions::default());
        assert!(walker.is_ok(), "pool builds");
        walker.ok()
    }

    fn settings(large: u64, old_days: u32, old_min: u64) -> CleanSettings {
        CleanSettings {
            large_min_bytes: large,
            old_days,
            old_min_bytes: old_min,
            ..CleanSettings::default()
        }
    }

    #[test]
    fn kinds_by_extension_ignore_case() {
        assert_eq!(
            file_kind(Path::new("/a/Movie.MKV")),
            FileKind::Video,
            "video"
        );
        assert_eq!(
            file_kind(Path::new("/a/x.tar.gz")),
            FileKind::Archive,
            "archive"
        );
        assert_eq!(
            file_kind(Path::new("/a/setup.Dmg")),
            FileKind::DiskImage,
            "dmg"
        );
        assert_eq!(
            file_kind(Path::new("/a/disk.vmdk")),
            FileKind::VirtualMachine,
            "vm"
        );
        assert_eq!(
            file_kind(Path::new("/a/noext")),
            FileKind::Other,
            "no extension"
        );
        assert_eq!(
            file_kind(Path::new("/a/x.weird")),
            FileKind::Other,
            "unknown"
        );
    }

    #[test]
    fn large_and_old_thresholds() {
        let dir = temp_dir("thresholds");
        let big = write(&dir, "sub/big.mp4", 200_000);
        let _small = write(&dir, "small.txt", 1_000);
        let aged = write(&dir, "aged.zip", 50_000);
        let _aged_tiny = write(&dir, "aged-tiny.zip", 100);
        let long_ago = SystemTime::now() - Duration::from_hours(400 * 24);
        for name in ["aged.zip", "aged-tiny.zip"] {
            let file = fs::File::options().write(true).open(dir.join(name));
            assert!(
                file.is_ok_and(|f| f.set_modified(long_ago).is_ok()),
                "backdate {name}"
            );
        }
        let Some(walker) = walker() else { return };
        let result = scan(
            std::slice::from_ref(&dir),
            &settings(100_000, 365, 10_000),
            &walker,
            &JobCtx::new(),
        );
        assert!(result.is_ok(), "scan runs: {result:?}");
        let Ok(scanned) = result else { return };
        let files = &scanned.report.files;
        assert_eq!(files.len(), 2, "big + aged only: {files:?}");
        let first = files.first();
        assert!(
            first.is_some_and(|f| f.path == big.display().to_string()
                && f.large
                && !f.old
                && f.kind == FileKind::Video
                && f.id == 0),
            "largest first, large: {first:?}"
        );
        let second = files.get(1);
        assert!(
            second.is_some_and(|f| f.path == aged.display().to_string() && f.old && !f.large),
            "old file: {second:?}"
        );
        assert!(!scanned.report.truncated, "not truncated");
        assert_eq!(scanned.targets.len(), 2, "one target per entry");
        assert!(
            scanned.targets.get(1).is_some_and(|t| t.location
                == omc_proto::jobs::Location::Path {
                    path: aged.display().to_string()
                }),
            "target index = id"
        );

        let off = scan(
            std::slice::from_ref(&dir),
            &settings(100_000, 0, 0),
            &walker,
            &JobCtx::new(),
        );
        assert!(
            off.is_ok_and(|s| s.report.files.len() == 1),
            "old_days 0 disables the age check"
        );
        let _ignored = fs::remove_dir_all(&dir);
    }

    #[test]
    fn caps_at_max_files_and_flags_truncated() {
        let dir = temp_dir("cap");
        let extra = 5_usize;
        for i in 0..MAX_FILES + extra {
            // Sizes 1..: the smallest `extra` files must be the ones dropped.
            write(&dir, &format!("d{}/f{i}", i % 50), i + 1);
        }
        let Some(walker) = walker() else { return };
        let result = scan(
            &[dir.clone(), dir.join("d1")],
            &settings(1, 0, 0),
            &walker,
            &JobCtx::new(),
        );
        assert!(result.is_ok(), "scan runs");
        let Ok(scanned) = result else { return };
        assert_eq!(scanned.report.files.len(), MAX_FILES, "capped");
        assert!(scanned.report.truncated, "truncated flagged");
        let sorted = scanned
            .report
            .files
            .windows(2)
            .all(|w| w.first().map(|f| f.bytes) >= w.get(1).map(|f| f.bytes));
        assert!(sorted, "largest first");
        let unique: HashSet<&str> = scanned
            .report
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(
            unique.len(),
            MAX_FILES,
            "overlapping roots report each file once"
        );
        let _ignored = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_roots_is_an_empty_report() {
        let Some(walker) = walker() else { return };
        let result = scan(&[], &CleanSettings::default(), &walker, &JobCtx::new());
        assert!(
            result.is_ok_and(|s| s.report.files.is_empty() && s.targets.is_empty()),
            "empty"
        );
    }
}
