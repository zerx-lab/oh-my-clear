//! Duplicate files: size → partial hash → full hash, every stage parallel on the walker's
//! pool.
//!
//! 1. One walk collects files by exact length (hard links to one inode collapse to one
//!    entry; placeholders and zero-length files are skipped). Only lengths shared by two
//!    or more files go on.
//! 2. XXH3-128 of the first 16 KiB (plus the last 16 KiB of files over 1 MiB, which
//!    separates media/VM images with identical headers) splits the buckets cheaply.
//! 3. XXH3-128 of the whole content, streamed through one reusable 256 KiB buffer per
//!    worker, confirms the survivors (files of at most 16 KiB were fully hashed in 2).
//!
//! Memory: a file is kept as its name plus the index of its directory, whose path is
//! stored once per directory, and the whole pipeline works in place on one `Vec`.
//! Files that vanish, change length or cannot be read mid-way are dropped from their
//! group; groups left with one file disappear.

use std::cmp::{Ordering, Reverse};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use omc_proto::files::{DupGroup, DupReport, FileEntry};
use omc_proto::jobs::{Denied, Phase};
use omc_proto::settings::CleanSettings;
use rayon::prelude::*;
use twox_hash::XxHash3_128;

use crate::ctx::JobCtx;
use crate::large::{DeniedLog, PerThread, distinct_roots, file_kind, is_dot_name};
use crate::target::{Scanned, Target, push_target};
use crate::walk::{FileMeta, Visitor, Walker};
use crate::{Error, Result};

/// Most groups one report holds (those wasting the least space are dropped).
pub const MAX_GROUPS: usize = 5_000;

/// Bytes hashed at each end of a file in the partial-hash stage.
const EDGE: usize = 16 * 1024;
/// [`EDGE`] as a length.
const EDGE_LEN: u64 = 16 * 1024;
/// Files longer than this also get their tail hashed in the partial stage.
const TAIL_FROM: u64 = 1024 * 1024;
/// Read buffer of the full-hash stage.
const FULL_BUF: usize = 256 * 1024;

/// Group id of files that dropped out.
const DROPPED: u32 = u32::MAX;

/// Finds groups of byte-identical files of at least `dup_min_bytes` below `roots`.
pub fn scan(
    roots: &[PathBuf],
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Scanned<DupReport>> {
    let roots = distinct_roots(roots);
    if roots.is_empty() {
        return Ok(Scanned {
            report: DupReport::default(),
            targets: Vec::new(),
        });
    }

    // Stage 1: collect by length.
    ctx.set_phase(Phase::Scanning);
    let collector = Collector {
        min_len: settings.dup_min_bytes.max(1),
        skip_dot: settings.skip_hidden && !walker.options().skip_hidden,
        found: PerThread::new(walker),
        denied: DeniedLog::default(),
    };
    walker.walk(&roots, ctx, &collector);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let denied = collector.denied.into_inner();
    let (dirs, mut candidates) = merge(collector.found.into_inner().collect())?;
    walker.install(|| same_length(&mut candidates, &dirs));

    // Stages 2 and 3: partial, then full content.
    ctx.set_phase(Phase::Hashing);
    let partial = EDGE.saturating_mul(2);
    walker.install(|| refine(&mut candidates, &dirs, partial, ctx, partial_hash));
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    walker.install(|| refine(&mut candidates, &dirs, FULL_BUF, ctx, full_hash));
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }

    Ok(report(candidates, &dirs, settings, denied))
}

/// A file that may have a duplicate.
#[derive(Debug)]
struct Candidate {
    /// File name; the directory is `dirs[dir]`.
    name: Box<OsStr>,
    dir: u32,
    /// Files with equal `group` are still equal so far ([`DROPPED`] = out).
    group: u32,
    len: u64,
    /// Bytes on disk.
    size: u64,
    modified: Option<i64>,
    dev: u64,
    ino: u64,
    /// Digest of the last hashing stage.
    hash: u128,
    /// Has more than one hard link.
    linked: bool,
}

impl Candidate {
    fn path(&self, dirs: &[Box<Path>]) -> Option<PathBuf> {
        let dir = dirs.get(usize::try_from(self.dir).ok()?)?;
        Some(dir.join(&*self.name))
    }

    /// Orders by directory path, then name.
    fn cmp_path(&self, other: &Self, dirs: &[Box<Path>]) -> Ordering {
        let dir = |c: &Self| usize::try_from(c.dir).ok().and_then(|d| dirs.get(d));
        dir(self)
            .cmp(&dir(other))
            .then_with(|| self.name.cmp(&other.name))
    }
}

/// One thread's finds: directories (each stored once per burst of its files) and files
/// indexing into them.
#[derive(Default)]
struct Found {
    dirs: Vec<Box<Path>>,
    files: Vec<Candidate>,
}

struct Collector {
    min_len: u64,
    skip_dot: bool,
    found: PerThread<Found>,
    denied: DeniedLog,
}

impl Visitor for Collector {
    fn enter_dir(&self, dir: &Path, _depth: u32) -> bool {
        !(self.skip_dot && is_dot_name(dir))
    }

    fn file(&self, path: &Path, meta: &FileMeta) {
        if meta.placeholder || meta.len < self.min_len || (self.skip_dot && is_dot_name(path)) {
            return;
        }
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        self.found.with(|found| {
            if found.dirs.last().is_none_or(|d| **d != *parent) {
                found.dirs.push(parent.into());
            }
            let Some(dir) = found
                .dirs
                .len()
                .checked_sub(1)
                .and_then(|d| u32::try_from(d).ok())
            else {
                return;
            };
            found.files.push(Candidate {
                name: name.into(),
                dir,
                group: 0,
                len: meta.len,
                size: meta.size,
                modified: meta.modified,
                dev: meta.dev,
                ino: meta.ino,
                hash: 0,
                linked: meta.nlink > 1,
            });
        });
    }

    fn denied(&self, path: &Path, err: &io::Error) {
        self.denied.push(path, err);
    }
}

/// Joins the per-thread finds into one directory table and one candidate list, leaving
/// out files whose length no other file has (most of them, on a typical disk) before
/// anything is copied.
fn merge(mut per_thread: Vec<Found>) -> Result<(Vec<Box<Path>>, Vec<Candidate>)> {
    let mut lens: HashMap<u64, u32> = HashMap::new();
    for c in per_thread.iter().flat_map(|f| &f.files) {
        let n = lens.entry(c.len).or_insert(0);
        *n = n.saturating_add(1);
    }
    for found in &mut per_thread {
        found
            .files
            .retain(|c| lens.get(&c.len).is_some_and(|n| *n >= 2));
    }
    drop(lens);
    let sum = |f: fn(&Found) -> usize| {
        per_thread
            .iter()
            .map(f)
            .fold(0_usize, usize::saturating_add)
    };
    let mut dirs = Vec::with_capacity(sum(|f| f.dirs.len()));
    let mut files = Vec::with_capacity(sum(|f| f.files.len()));
    for found in per_thread {
        let offset = u32::try_from(dirs.len())
            .map_err(|_| Error::Invalid("too many directories".to_owned()))?;
        dirs.extend(found.dirs);
        for mut c in found.files {
            c.dir = c
                .dir
                .checked_add(offset)
                .ok_or_else(|| Error::Invalid("too many directories".to_owned()))?;
            files.push(c);
        }
    }
    Ok((dirs, files))
}

/// Collapses hard links and keeps lengths shared by two or more files, largest first
/// (one group per length, so big files are hashed first).
fn same_length(all: &mut Vec<Candidate>, dirs: &[Box<Path>]) {
    all.par_sort_unstable_by(|a, b| {
        (Reverse(a.len), a.dev, a.ino)
            .cmp(&(Reverse(b.len), b.dev, b.ino))
            .then_with(|| a.cmp_path(b, dirs))
    });
    // Hard links to one inode are adjacent now; keep the first path.
    all.dedup_by(|later, first| {
        later.linked && first.linked && later.dev == first.dev && later.ino == first.ino
    });
    regroup(all, |a, b| a.len == b.len);
}

/// Numbers the runs of equal neighbours (by `same`) with two or more files and drops
/// the rest.
fn regroup(all: &mut Vec<Candidate>, same: impl Fn(&Candidate, &Candidate) -> bool) {
    let mut next = 0_u32;
    for run in all.chunk_by_mut(|a, b| same(a, b)) {
        let group = if run.len() >= 2 && run.first().is_some_and(|c| c.group != DROPPED) {
            let group = next;
            next = next.saturating_add(1);
            group
        } else {
            DROPPED
        };
        for c in run {
            c.group = group;
        }
    }
    all.retain(|c| c.group != DROPPED);
    all.shrink_to_fit();
}

/// Hashes every candidate in parallel and splits each group by digest, dropping
/// unreadable files and singletons.
fn refine(
    all: &mut Vec<Candidate>,
    dirs: &[Box<Path>],
    buf_len: usize,
    ctx: &JobCtx,
    hash: fn(&Path, &Candidate, &mut [u8]) -> Option<u128>,
) {
    ctx.set_total(u64::try_from(all.len()).unwrap_or(u64::MAX));
    all.par_iter_mut().for_each_init(
        || vec![0_u8; buf_len],
        |buf, c| {
            if ctx.is_cancelled() {
                c.group = DROPPED;
                return;
            }
            let digest = c.path(dirs).and_then(|path| {
                let digest = hash(&path, c, buf);
                if digest.is_none() {
                    tracing::debug!(path = %path.display(), "duplicate candidate unreadable");
                }
                digest
            });
            match digest {
                Some(digest) => c.hash = digest,
                None => c.group = DROPPED,
            }
            ctx.add_done(1);
        },
    );
    all.retain(|c| c.group != DROPPED);
    all.par_sort_unstable_by_key(|c| (c.group, c.hash));
    regroup(all, |a, b| a.group == b.group && a.hash == b.hash);
}

/// Opens `path` and checks it still has the length it was bucketed by.
fn open_unchanged(path: &Path, c: &Candidate) -> Option<File> {
    let file = File::open(path).ok()?;
    (file.metadata().ok()?.len() == c.len).then_some(file)
}

/// XXH3-128 of the first [`EDGE`] bytes, plus the last [`EDGE`] bytes of files longer
/// than [`TAIL_FROM`]. `buf` holds at least `2 * EDGE` bytes.
fn partial_hash(path: &Path, c: &Candidate, buf: &mut [u8]) -> Option<u128> {
    let mut file = open_unchanged(path, c)?;
    let head_len = usize::try_from(c.len.min(EDGE_LEN)).ok()?;
    let (head, tail) = buf.split_at_mut_checked(head_len)?;
    file.read_exact(head).ok()?;
    let mut used = head_len;
    if c.len > TAIL_FROM {
        let tail = tail.get_mut(..EDGE)?;
        file.seek(SeekFrom::Start(c.len.checked_sub(EDGE_LEN)?))
            .ok()?;
        file.read_exact(tail).ok()?;
        used = used.checked_add(EDGE)?;
    }
    Some(XxHash3_128::oneshot(buf.get(..used)?))
}

/// XXH3-128 of the whole content, streamed through `buf`. Files of at most [`EDGE`]
/// bytes were fully hashed by [`partial_hash`] and keep their group unchanged.
fn full_hash(path: &Path, c: &Candidate, buf: &mut [u8]) -> Option<u128> {
    if c.len <= EDGE_LEN {
        return Some(0);
    }
    let mut file = open_unchanged(path, c)?;
    let mut hasher = XxHash3_128::new();
    let mut total = 0_u64;
    loop {
        let n = match file.read(buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        hasher.write(buf.get(..n)?);
        total = total.saturating_add(u64::try_from(n).ok()?);
    }
    (total == c.len).then(|| hasher.finish_128())
}

/// Builds the report from confirmed groups (the list is sorted by group).
fn report(
    candidates: Vec<Candidate>,
    dirs: &[Box<Path>],
    settings: &CleanSettings,
    denied: Vec<Denied>,
) -> Scanned<DupReport> {
    let mut groups: Vec<(u64, Vec<Candidate>)> = Vec::new();
    let mut current = None;
    for candidate in candidates {
        if current != Some(candidate.group) || groups.is_empty() {
            current = Some(candidate.group);
            groups.push((candidate.len, Vec::new()));
        }
        if let Some((_, files)) = groups.last_mut() {
            files.push(candidate);
        }
    }
    for (_, files) in &mut groups {
        // Oldest first (the usual original); unknown times last.
        files.sort_unstable_by(|a, b| {
            (a.modified.is_none(), a.modified)
                .cmp(&(b.modified.is_none(), b.modified))
                .then_with(|| a.cmp_path(b, dirs))
        });
    }
    let wasted = |(len, files): &(u64, Vec<Candidate>)| {
        let copies = u64::try_from(files.len()).unwrap_or(u64::MAX);
        len.saturating_mul(copies.saturating_sub(1))
    };
    groups.sort_unstable_by(|a, b| {
        wasted(b).cmp(&wasted(a)).then(b.0.cmp(&a.0)).then_with(|| {
            match (a.1.first(), b.1.first()) {
                (Some(x), Some(y)) => x.cmp_path(y, dirs),
                (x, y) => x.is_some().cmp(&y.is_some()),
            }
        })
    });
    let truncated = groups.len() > MAX_GROUPS;
    groups.truncate(MAX_GROUPS);

    let mut targets = Vec::new();
    let mut out = Vec::with_capacity(groups.len());
    for (len, files) in groups {
        let entries = files
            .iter()
            .filter_map(|c| {
                let path_buf = c.path(dirs)?;
                let path = path_buf.display().to_string();
                let bytes = if c.size == 0 { c.len } else { c.size };
                let id = push_target(
                    &mut targets,
                    Target::path(path.clone(), bytes, settings.files_delete),
                );
                Some(FileEntry {
                    id,
                    kind: file_kind(&path_buf),
                    path,
                    bytes,
                    modified: c.modified,
                    accessed: None,
                    large: false,
                    old: false,
                })
            })
            .collect();
        out.push(DupGroup {
            bytes: len,
            files: entries,
        });
    }
    Scanned {
        report: DupReport {
            groups: out,
            truncated,
            denied,
        },
        targets,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;

    use crate::walk::WalkOptions;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omc-dupes-{name}-{}", std::process::id()));
        let _ignored = fs::remove_dir_all(&dir);
        assert!(fs::create_dir_all(&dir).is_ok(), "mkdir {}", dir.display());
        dir
    }

    fn write(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            assert!(
                fs::create_dir_all(parent).is_ok(),
                "mkdir {}",
                parent.display()
            );
        }
        assert!(fs::write(&path, content).is_ok(), "write {name}");
        path
    }

    fn run(dir: &Path, min: u64) -> Option<Scanned<DupReport>> {
        let walker = Walker::new(WalkOptions::default());
        assert!(walker.is_ok(), "pool builds");
        let walker = walker.ok()?;
        let settings = CleanSettings {
            dup_min_bytes: min,
            ..CleanSettings::default()
        };
        let result = scan(&[dir.to_path_buf()], &settings, &walker, &JobCtx::new());
        assert!(result.is_ok(), "scan runs: {result:?}");
        result.ok()
    }

    /// Group contents as sets of file names, for order-independent checks.
    fn names(report: &DupReport) -> Vec<HashSet<String>> {
        report
            .groups
            .iter()
            .map(|g| {
                g.files
                    .iter()
                    .filter_map(|f| {
                        Some(
                            Path::new(&f.path)
                                .file_name()?
                                .to_string_lossy()
                                .into_owned(),
                        )
                    })
                    .collect()
            })
            .collect()
    }

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn groups_identical_content_only() {
        let dir = temp_dir("identical");
        let big: Vec<u8> = (0..3_000_000_u32)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect();
        write(&dir, "a/one.bin", &big);
        write(&dir, "b/two.bin", &big);
        write(&dir, "c/three.bin", &big);
        // Same length, different middle: survives the partial hash, split by the full one.
        let mut middle = big.clone();
        if let Some(b) = middle.get_mut(1_500_000) {
            *b ^= 0xff;
        }
        write(&dir, "middle.bin", &middle);
        // Same length and prefix, different tail.
        let mut tail = big.clone();
        if let Some(b) = tail.last_mut() {
            *b ^= 0xff;
        }
        write(&dir, "tail.bin", &tail);
        // Same length, different content (small files).
        write(&dir, "s1", &[1_u8; 5_000]);
        write(&dir, "s2", &[2_u8; 5_000]);
        // Small identical pair.
        write(&dir, "p1", &[3_u8; 7_000]);
        write(&dir, "p2", &[3_u8; 7_000]);
        // Below the minimum.
        write(&dir, "tiny1", &[4_u8; 10]);
        write(&dir, "tiny2", &[4_u8; 10]);
        // Empty files are never duplicates.
        write(&dir, "e1", &[]);
        write(&dir, "e2", &[]);

        let Some(scanned) = run(&dir, 100) else {
            return;
        };
        let groups = names(&scanned.report);
        assert_eq!(
            groups,
            vec![
                set(&["one.bin", "two.bin", "three.bin"]),
                set(&["p1", "p2"])
            ],
            "only identical files, most wasted first"
        );
        assert!(
            scanned
                .report
                .groups
                .first()
                .is_some_and(|g| g.bytes == 3_000_000 && g.wasted() == 6_000_000),
            "group size is the file length"
        );
        let ids: Vec<u32> = scanned
            .report
            .groups
            .iter()
            .flat_map(|g| g.files.iter().map(|f| f.id))
            .collect();
        assert_eq!(
            ids,
            (0..5).collect::<Vec<u32>>(),
            "ids unique and sequential"
        );
        assert_eq!(scanned.targets.len(), 5, "one target per file");
        let _ignored = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_are_not_duplicates() {
        let dir = temp_dir("links");
        let original = write(&dir, "orig", &[9_u8; 4_096]);
        assert!(
            fs::hard_link(&original, dir.join("link")).is_ok(),
            "hard link"
        );
        let Some(scanned) = run(&dir, 1) else { return };
        assert!(
            scanned.report.groups.is_empty(),
            "one inode: {:?}",
            scanned.report
        );

        write(&dir, "copy", &[9_u8; 4_096]);
        let Some(scanned) = run(&dir, 1) else { return };
        let groups = names(&scanned.report);
        assert_eq!(
            groups.len(),
            1,
            "copy duplicates the inode once: {groups:?}"
        );
        assert!(
            groups
                .first()
                .is_some_and(|g| g.len() == 2 && g.contains("copy")),
            "link collapsed into one entry: {groups:?}"
        );
        let _ignored = fs::remove_dir_all(&dir);
    }

    #[test]
    fn oldest_copy_first() {
        let dir = temp_dir("order");
        write(&dir, "new", &[5_u8; 2_000]);
        let old = write(&dir, "old", &[5_u8; 2_000]);
        let past = std::time::SystemTime::now() - std::time::Duration::from_hours(48);
        let file = File::options().write(true).open(&old);
        assert!(file.is_ok_and(|f| f.set_modified(past).is_ok()), "backdate");
        let Some(scanned) = run(&dir, 1) else { return };
        let first = scanned.report.groups.first().and_then(|g| g.files.first());
        assert!(
            first.is_some_and(|f| f.path == old.display().to_string()),
            "oldest first: {first:?}"
        );
        let _ignored = fs::remove_dir_all(&dir);
    }
}
