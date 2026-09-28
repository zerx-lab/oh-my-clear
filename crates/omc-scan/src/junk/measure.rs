//! Sizing of junk candidates.
//!
//! Everything is measured in one parallel batch on the walker's pool, except folders in
//! macOS's protected trees (other apps' sandbox containers). Opening such a folder makes
//! the kernel ask a system daemon whether this process may access that app's data; every
//! so often (more often under concurrent opens) the answer never comes, and `open`
//! blocks for 5–6 s, then fails with `EINTR` (measured: `open$NOCANCEL` in `read_dir`,
//! error 4, a retry right after succeeds instantly). One such folder used to hold the
//! whole System scan for ~6 s while everything else had finished in ~100 ms. Those
//! folders are tiny (a few hundred files in total; empty ones are skipped without an
//! `open`, see [`surely_empty`]), so they are walked on a few detached threads fed from
//! a shared queue, and the scan waits for them only until a short deadline: a folder
//! whose `open` hangs is left out (the hung call would have failed anyway).

use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use omc_proto::jobs::Denied;
use parking_lot::Mutex;
use rayon::prelude::*;

use super::Candidate;
use crate::ctx::JobCtx;
use crate::walk::{FileMeta, Measure, Visitor, WalkOptions, Walker, is_hidden};
use crate::{errors, paths};

/// How long the scan waits for protected folders, from the start of measuring (it
/// always waits for the pooled batch).
const DETACHED_BUDGET: Duration = Duration::from_millis(300);
/// Most detached threads used for protected folders.
const MAX_DETACHED: usize = 8;

/// The size of one candidate and the first place below it that could not be read.
type Sized = (Measure, Option<Denied>);

/// Sizes every candidate; results are in input order. Temp files and logs only count
/// files older than the minimum age (what an age-limited removal deletes). Protected
/// folders that do not answer in time come back empty.
pub(super) fn measure(
    candidates: &[Candidate],
    min_age: u64,
    protected: &[PathBuf],
    walker: &Walker,
    ctx: &JobCtx,
) -> Vec<Sized> {
    ctx.set_total(u64::try_from(candidates.len()).unwrap_or(u64::MAX));
    let cutoff_at = paths::now_secs().saturating_sub(i64::try_from(min_age).unwrap_or(i64::MAX));
    let cutoff = |cand: &Candidate| (cand.aged && min_age > 0).then_some(cutoff_at);
    let started = Instant::now();

    let mut results: Vec<Option<Sized>> = vec![None; candidates.len()];
    let mut pooled = Vec::with_capacity(candidates.len());
    let mut detached = Vec::new();
    for (i, cand) in candidates.iter().enumerate() {
        if let Some(known) = cand.known {
            done(ctx, &known);
            if let Some(slot) = results.get_mut(i) {
                *slot = Some((known, None));
            }
        } else if protected.iter().any(|p| paths::is_within(&cand.path, p)) {
            detached.push((i, cand.path.clone(), cutoff(cand)));
        } else {
            pooled.push(i);
        }
    }
    let waiting = detached.len();
    let results_rx = spawn_detached(detached, walker.options());

    let measured: Vec<(usize, Sized)> = walker.install(|| {
        pooled
            .par_iter()
            .filter_map(|&i| {
                let cand = candidates.get(i)?;
                let sized = sum_tree(walker, &cand.path, cutoff(cand), ctx);
                done(ctx, &sized.0);
                Some((i, sized))
            })
            .collect()
    });
    for (i, sized) in measured {
        if let Some(slot) = results.get_mut(i) {
            *slot = Some(sized);
        }
    }

    let deadline = started.checked_add(DETACHED_BUDGET).unwrap_or(started);
    let mut received = 0_usize;
    while received < waiting && !ctx.is_cancelled() {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok((i, sized)) = results_rx.recv_timeout(left) else {
            break;
        };
        received = received.saturating_add(1);
        done(ctx, &sized.0);
        if let Some(slot) = results.get_mut(i) {
            *slot = Some(sized);
        }
    }
    if received < waiting {
        tracing::debug!(
            missing = waiting.saturating_sub(received),
            "protected folders did not answer in time; left out"
        );
    }
    results.into_iter().map(Option::unwrap_or_default).collect()
}

/// Counts one finished candidate.
fn done(ctx: &JobCtx, m: &Measure) {
    ctx.add_done(1);
    ctx.add_bytes(m.bytes);
}

/// Walks `jobs` on up to [`MAX_DETACHED`] threads the scan does not join, each taking
/// the next folder from a shared queue (a hung `open` holds up only its own folder);
/// results arrive on the returned channel.
fn spawn_detached(
    jobs: Vec<(usize, PathBuf, Option<i64>)>,
    opts: &WalkOptions,
) -> mpsc::Receiver<(usize, Sized)> {
    let (sender, receiver) = mpsc::channel();
    let threads = jobs.len().min(MAX_DETACHED);
    let queue = Arc::new(Mutex::new(jobs));
    for _ in 0..threads {
        let sender = sender.clone();
        let queue = Arc::clone(&queue);
        let opts = opts.clone();
        let spawned = std::thread::Builder::new()
            .name("omc-junk-protected".to_owned())
            .spawn(move || {
                loop {
                    let Some((i, path, cutoff)) = queue.lock().pop() else {
                        return;
                    };
                    if sender.send((i, serial_sum(&path, cutoff, &opts))).is_err() {
                        return;
                    }
                }
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "cannot spawn a measuring thread");
        }
    }
    receiver
}

/// Whether `file` counts (not newer than `cutoff`, first sighting of a hard link).
fn counts(meta: &FileMeta, cutoff: Option<i64>, linked: &Mutex<HashSet<(u64, u64)>>) -> bool {
    if let (Some(cutoff), Some(modified)) = (cutoff, meta.modified)
        && modified > cutoff
    {
        return false;
    }
    meta.nlink <= 1 || linked.lock().insert((meta.dev, meta.ino))
}

fn deny(path: &Path, err: &io::Error) -> Denied {
    Denied {
        path: path.display().to_string(),
        reason: errors::classify(err, path),
    }
}

/// Totals of one tree, updated lock-free from every pool thread (a per-file lock
/// serializes wide trees such as package caches with hundreds of thousands of files).
struct Sum {
    /// Only files last modified at or before this count (`None` = all).
    cutoff: Option<i64>,
    bytes: AtomicU64,
    files: AtomicU64,
    /// Newest counted modification; `i64::MIN` = none.
    newest: AtomicI64,
    /// Hard-linked files seen (only files with several links are tracked).
    linked: Mutex<HashSet<(u64, u64)>>,
    incomplete: AtomicBool,
    denied: Mutex<Option<Denied>>,
}

impl Visitor for Sum {
    fn file(&self, _path: &Path, meta: &FileMeta) {
        if !counts(meta, self.cutoff, &self.linked) {
            return;
        }
        self.bytes.fetch_add(meta.size, Ordering::Relaxed);
        self.files.fetch_add(1, Ordering::Relaxed);
        if let Some(modified) = meta.modified {
            self.newest.fetch_max(modified, Ordering::Relaxed);
        }
    }

    fn denied(&self, path: &Path, err: &io::Error) {
        self.incomplete.store(true, Ordering::Relaxed);
        let mut denied = self.denied.lock();
        if denied.is_none() {
            *denied = Some(deny(path, err));
        }
    }
}

/// One tree on the walker's pool.
fn sum_tree(walker: &Walker, path: &Path, cutoff: Option<i64>, ctx: &JobCtx) -> Sized {
    let sum = Sum {
        cutoff,
        bytes: AtomicU64::new(0),
        files: AtomicU64::new(0),
        newest: AtomicI64::new(i64::MIN),
        linked: Mutex::new(HashSet::new()),
        incomplete: AtomicBool::new(false),
        denied: Mutex::new(None),
    };
    walker.walk(std::slice::from_ref(&path.to_path_buf()), ctx, &sum);
    let newest = sum.newest.into_inner();
    let measure = Measure {
        bytes: sum.bytes.into_inner(),
        files: sum.files.into_inner(),
        newest: (newest != i64::MIN).then_some(newest),
        incomplete: sum.incomplete.into_inner(),
        missing: false,
    };
    (measure, sum.denied.into_inner())
}

/// One tree on the current thread, with the walker's rules (no symlinks, exclusions,
/// hidden entries on request, one file system).
fn serial_sum(root: &Path, cutoff: Option<i64>, opts: &WalkOptions) -> Sized {
    let mut total = Measure::default();
    let Ok(root_meta) = fs::symlink_metadata(root) else {
        total.missing = true;
        return (total, None);
    };
    let linked = Mutex::new(HashSet::new());
    let add = |meta: &FileMeta, total: &mut Measure| {
        if counts(meta, cutoff, &linked) {
            total.bytes = total.bytes.saturating_add(meta.size);
            total.files = total.files.saturating_add(1);
            total.newest = total.newest.max(meta.modified);
        }
    };
    if !root_meta.is_dir() {
        if root_meta.is_file() {
            add(&FileMeta::from_metadata(&root_meta), &mut total);
        }
        return (total, None);
    }
    let root_dev = FileMeta::from_metadata(&root_meta).dev;
    let mut denied = None;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) => {
                total.incomplete = true;
                denied.get_or_insert_with(|| deny(&dir, &err));
                continue;
            }
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_symlink() || opts.is_excluded(&path) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if opts.skip_hidden && is_hidden(&entry.file_name(), Some(&meta)) {
                continue;
            }
            let file = FileMeta::from_metadata(&meta);
            if file_type.is_dir() {
                if !opts.one_file_system || file.dev == root_dev {
                    stack.push(path);
                }
            } else {
                add(&file, &mut total);
            }
        }
    }
    (total, denied)
}

/// Whether `meta` is a folder known to be empty without opening it. APFS reports a
/// folder's size as 64 bytes plus 32 per entry and its link count as 2 plus its
/// subfolders, so `size == 64 && nlink == 2` is an empty APFS folder (checked on this
/// Mac: 725 container cache folders, 689 empty, no mismatch). Skipping them saves
/// hundreds of `open` calls, most of them into protected containers.
#[cfg(target_os = "macos")]
pub(super) fn surely_empty(meta: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    meta.is_dir() && meta.size() == 64 && meta.nlink() == 2
}

/// Other systems give no such guarantee.
#[cfg(not(target_os = "macos"))]
pub(super) fn surely_empty(_meta: &Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use omc_proto::junk::JunkKind;

    use super::super::tests::{put, temp_dir, walker};
    use super::*;

    #[test]
    fn protected_folders_measure_like_the_pool() {
        let base = temp_dir("measure-protected");
        let protected = base.join("Containers");
        let open = base.join("open");
        for root in [&protected, &open] {
            put(&root.join("app/Caches/a"), 5000);
            put(&root.join("app/Caches/sub/b"), 7000);
            put(&root.join("app/Caches/.hidden/c"), 3000);
        }
        let cands: Vec<Candidate> = [&protected, &open]
            .iter()
            .map(|root| Candidate::at(JunkKind::UserCache, root.join("app/Caches")).contents())
            .collect();
        let got = measure(
            &cands,
            0,
            std::slice::from_ref(&protected),
            &walker(),
            &JobCtx::new(),
        );
        let [(detached, None), (pooled, None)] = got.as_slice() else {
            assert!(got.len() == 2, "one result per candidate: {got:?}");
            return;
        };
        assert_eq!(
            detached.files, 3,
            "every file of the protected tree: {detached:?}"
        );
        assert_eq!(
            (detached.files, detached.bytes, detached.newest),
            (pooled.files, pooled.bytes, pooled.newest),
            "serial and pooled walks agree"
        );

        let aged = measure(
            &[Candidate::at(JunkKind::TempFiles, protected.join("app/Caches")).aged()],
            3600,
            std::slice::from_ref(&protected),
            &walker(),
            &JobCtx::new(),
        );
        assert_eq!(
            aged.first().map(|(m, _)| m.files),
            Some(0),
            "fresh files are not counted under an age limit"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn empty_apfs_folders_are_recognised_without_opening() {
        let base = temp_dir("measure-empty");
        let empty = base.join("empty");
        let full = base.join("full");
        assert!(fs::create_dir_all(&empty).is_ok(), "mkdir empty");
        put(&full.join("f"), 10);
        let meta = |p: &Path| fs::symlink_metadata(p).ok();
        assert!(
            meta(&empty).is_some_and(|m| surely_empty(&m)),
            "empty folder: {:?}",
            meta(&empty)
        );
        assert!(
            meta(&full).is_some_and(|m| !surely_empty(&m)),
            "folder with a file"
        );
        assert!(
            meta(&full.join("f")).is_some_and(|m| !surely_empty(&m)),
            "files are not folders"
        );
    }
}
