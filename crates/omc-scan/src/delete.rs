//! Removing one [`Target`] from disk: permanently (in parallel, counting the on-disk bytes
//! of what is actually unlinked) or to the Trash.
//!
//! Rules every removal follows:
//! - the [`Guard`] is asked first; a refusal fails the item with `protected`;
//! - symbolic links (and Windows junctions) are removed as links, never followed;
//! - a missing path is a success that frees nothing;
//! - one failing entry (locked, denied) does not stop the rest; each is reported;
//! - `contents_only` keeps the folder itself; with `min_age_secs` only files older than
//!   that go, and emptied subfolders that were already old before are pruned;
//! - entries excluded by the user are skipped silently;
//! - Trash never falls back to a permanent delete: a volume without Trash fails the item
//!   with `trash_unavailable`.

use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use omc_proto::jobs::{DeleteMethod, FailReason};
use rayon::prelude::*;

use crate::errors::classify;
use crate::guard::{Guard, Refusal};
use crate::walk::FileMeta;
use crate::{JobCtx, Target};

/// What removing one target did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Removal {
    /// On-disk bytes removed (or moved to the Trash).
    pub freed: u64,
    /// Entries that could not be removed; empty = the target is fully gone.
    pub failures: Vec<PathFailure>,
}

/// One path that could not be removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathFailure {
    /// The entry.
    pub path: PathBuf,
    /// Why.
    pub reason: FailReason,
    /// OS error text.
    pub message: String,
}

/// Where [`DeleteMethod::Trash`] puts things.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TrashBin {
    /// The current user's Trash / Recycle Bin, through the OS.
    #[default]
    System,
    /// Renamed into this folder under a unique name, then (Unix) handed to `owner`: an
    /// elevated helper filing root-owned items into the invoking user's `~/.Trash`.
    Folder {
        /// The Trash folder (must be on the same volume as the items).
        dir: PathBuf,
        /// Unix uid that becomes the owner of the moved tree; ignored elsewhere.
        owner: Option<u32>,
    },
}

/// Removes `target` (a [`omc_proto::jobs::Location::Path`]; any other location fails with
/// `other`) into the system Trash when its method says so.
pub fn remove(target: &Target, guard: &Guard, ctx: &JobCtx) -> Removal {
    remove_with(target, guard, ctx, &TrashBin::System)
}

/// [`remove`] with an explicit Trash destination.
pub fn remove_with(target: &Target, guard: &Guard, ctx: &JobCtx, bin: &TrashBin) -> Removal {
    let Some(raw) = target.location.as_path() else {
        return Removal::failed(
            PathBuf::from(target.location.display()),
            FailReason::Other,
            "not a file-system location".to_owned(),
        );
    };
    ctx.set_current(raw);
    remove_path(Path::new(raw), target, guard, ctx, bin)
}

impl Removal {
    fn failed(path: PathBuf, reason: FailReason, message: String) -> Self {
        Self {
            freed: 0,
            failures: vec![PathFailure {
                path,
                reason,
                message,
            }],
        }
    }

    fn io(path: &Path, err: &io::Error) -> Self {
        Self::failed(path.to_path_buf(), classify(err, path), err.to_string())
    }

    fn freed(freed: u64) -> Self {
        Self {
            freed,
            failures: Vec::new(),
        }
    }

    fn merge(mut self, other: Self) -> Self {
        self.freed = self.freed.saturating_add(other.freed);
        self.failures.extend(other.failures);
        self
    }
}

/// A [`Removal`] plus whether anything below was deliberately left in place (too young,
/// excluded, cancelled), which makes the parent folder's non-emptiness expected.
#[derive(Debug, Default)]
struct Tally {
    removal: Removal,
    kept: bool,
}

impl Tally {
    fn kept() -> Self {
        Self {
            removal: Removal::default(),
            kept: true,
        }
    }

    fn merge(mut self, other: Self) -> Self {
        self.removal = self.removal.merge(other.removal);
        self.kept |= other.kept;
        self
    }
}

impl From<Removal> for Tally {
    fn from(removal: Removal) -> Self {
        Self {
            removal,
            kept: false,
        }
    }
}

fn remove_path(
    path: &Path,
    target: &Target,
    guard: &Guard,
    ctx: &JobCtx,
    bin: &TrashBin,
) -> Removal {
    if let Err(refusal) = admit(path, target.contents_only, guard) {
        return Removal::failed(
            path.to_path_buf(),
            FailReason::Protected,
            refusal.to_string(),
        );
    }
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Removal::default(),
        Err(err) => return Removal::io(path, &err),
    };
    let cutoff = cutoff(target.min_age_secs);
    if target.contents_only && !meta.is_dir() {
        return Removal::failed(
            path.to_path_buf(),
            FailReason::Other,
            "not a directory".to_owned(),
        );
    }
    // Freed bytes reach `ctx` as they go: file by file when deleting, per batch when
    // trashing (the OS moves a batch in one call).
    let trashed = match (target.method, target.contents_only) {
        (DeleteMethod::Permanent, false) => {
            return remove_entry(path, &meta, guard, ctx, cutoff).removal;
        }
        (DeleteMethod::Permanent, true) => {
            return remove_children(path, guard, ctx, cutoff, true).removal;
        }
        (DeleteMethod::Trash, false) => {
            let size = tree_size(path, &meta, ctx);
            if cutoff.is_some_and(|c| !size.older_than(c)) {
                return Removal::default();
            }
            trash_paths(vec![(path.to_path_buf(), size.bytes)], bin)
        }
        (DeleteMethod::Trash, true) => trash_children(path, guard, ctx, cutoff, bin),
    };
    ctx.add_bytes(trashed.freed);
    trashed
}

/// The guard check. Whole paths must pass it; a folder whose *contents* are removed may be
/// an essential container (`~/Library/Caches`) or contain an exclusion (skipped per entry),
/// but must not be excluded itself or lie in a system tree.
fn admit(path: &Path, contents_only: bool, guard: &Guard) -> Result<(), Refusal> {
    match guard.check(path) {
        Ok(()) => Ok(()),
        Err(Refusal::Essential) if contents_only => Ok(()),
        Err(Refusal::Excluded) if contents_only && !guard.is_excluded(path) => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

fn cutoff(min_age_secs: u64) -> Option<SystemTime> {
    if min_age_secs == 0 {
        return None;
    }
    SystemTime::now().checked_sub(Duration::from_secs(min_age_secs))
}

fn modified_before(meta: &Metadata, cutoff: SystemTime) -> bool {
    meta.modified().is_ok_and(|m| m < cutoff)
}

/// Removes one entry of any type. With a `cutoff`, files modified after it are kept and a
/// folder is removed only when it ends up empty and was itself older.
fn remove_entry(
    path: &Path,
    meta: &Metadata,
    guard: &Guard,
    ctx: &JobCtx,
    cutoff: Option<SystemTime>,
) -> Tally {
    if ctx.is_cancelled() {
        return Tally::kept();
    }
    if meta.is_dir() {
        let old = cutoff.is_none_or(|c| modified_before(meta, c));
        let children = remove_children(path, guard, ctx, cutoff, false);
        if ctx.is_cancelled() || !children.removal.failures.is_empty() || !old {
            return Tally {
                kept: true,
                ..children
            };
        }
        return match fs::remove_dir(path) {
            Ok(()) => children,
            Err(err) if err.kind() == io::ErrorKind::NotFound => children,
            Err(err) if err.kind() == io::ErrorKind::DirectoryNotEmpty && children.kept => children,
            Err(err) => children.merge(Removal::io(path, &err).into()),
        };
    }
    if cutoff.is_some_and(|c| !modified_before(meta, c)) {
        return Tally::kept();
    }
    let file = FileMeta::from_metadata(meta);
    // A hard link frees nothing while another name still points at the data.
    let size = if file.nlink <= 1 { file.size } else { 0 };
    match unlink(path, meta) {
        Ok(()) => {
            ctx.add_bytes(size);
            Removal::freed(size).into()
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Tally::default(),
        Err(err) => Removal::io(path, &err).into(),
    }
}

/// Removes the children of `dir` in parallel. `top` = `dir` is the target root, whose
/// children each pass the full guard check.
fn remove_children(
    dir: &Path,
    guard: &Guard,
    ctx: &JobCtx,
    cutoff: Option<SystemTime>,
    top: bool,
) -> Tally {
    let children = match list(dir) {
        Ok(children) => {
            ctx.entered_dir(dir);
            children
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Tally::default(),
        Err(err) => return Removal::io(dir, &err).into(),
    };
    children
        .into_par_iter()
        .map(|child| {
            if ctx.is_cancelled() {
                return Tally::kept();
            }
            if let Some(refused) = refuse_child(&child, guard, top) {
                return refused;
            }
            match fs::symlink_metadata(&child) {
                Ok(meta) => remove_entry(&child, &meta, guard, ctx, cutoff),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Tally::default(),
                Err(err) => Removal::io(&child, &err).into(),
            }
        })
        .reduce(Tally::default, Tally::merge)
}

/// `Some` when `child` must be left alone: excluded (silently) or protected (reported).
fn refuse_child(child: &Path, guard: &Guard, top: bool) -> Option<Tally> {
    if guard.is_excluded(child) {
        return Some(Tally::kept());
    }
    if top {
        match guard.check(child) {
            Ok(()) => {}
            Err(Refusal::Excluded) => return Some(Tally::kept()),
            Err(refusal) => {
                return Some(Tally {
                    removal: Removal::failed(
                        child.to_path_buf(),
                        FailReason::Protected,
                        refusal.to_string(),
                    ),
                    kept: true,
                });
            }
        }
    }
    None
}

fn list(dir: &Path) -> io::Result<Vec<PathBuf>> {
    fs::read_dir(dir)?
        .map(|entry| entry.map(|e| e.path()))
        .collect()
}

/// Unlinks a file or a link (Windows directory symlinks and junctions need `remove_dir`).
/// Read-only files on Windows are made writable once and retried.
#[cfg(windows)]
fn unlink(path: &Path, meta: &Metadata) -> io::Result<()> {
    use std::os::windows::fs::FileTypeExt as _;
    if meta.file_type().is_symlink_dir() {
        return fs::remove_dir(path);
    }
    match fs::remove_file(path) {
        Err(err)
            if err.kind() == io::ErrorKind::PermissionDenied && meta.permissions().readonly() =>
        {
            let mut perms = meta.permissions();
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "Windows only: clears the read-only attribute, no Unix mode bits"
            )]
            perms.set_readonly(false);
            fs::set_permissions(path, perms)?;
            fs::remove_file(path)
        }
        other => other,
    }
}

/// Unlinks a file or a link.
#[cfg(not(windows))]
fn unlink(path: &Path, _meta: &Metadata) -> io::Result<()> {
    fs::remove_file(path)
}

/// Size and newest modification of a tree (links not followed).
#[derive(Debug, Clone, Copy, Default)]
struct TreeSize {
    bytes: u64,
    newest: Option<SystemTime>,
}

impl TreeSize {
    fn merge(self, other: Self) -> Self {
        Self {
            bytes: self.bytes.saturating_add(other.bytes),
            newest: self.newest.max(other.newest),
        }
    }

    fn older_than(self, cutoff: SystemTime) -> bool {
        self.newest.is_some_and(|n| n < cutoff)
    }
}

fn tree_size(path: &Path, meta: &Metadata, ctx: &JobCtx) -> TreeSize {
    let own = TreeSize {
        bytes: if meta.is_dir() {
            0
        } else {
            FileMeta::from_metadata(meta).size
        },
        newest: meta.modified().ok(),
    };
    if !meta.is_dir() || ctx.is_cancelled() {
        return own;
    }
    let Ok(children) = list(path) else {
        return own;
    };
    children
        .into_par_iter()
        .filter_map(|child| {
            let meta = fs::symlink_metadata(&child).ok()?;
            Some(tree_size(&child, &meta, ctx))
        })
        .reduce(|| own, TreeSize::merge)
}

/// Trashes the admitted (and, with a cutoff, old enough) children of `dir` in one batch.
fn trash_children(
    dir: &Path,
    guard: &Guard,
    ctx: &JobCtx,
    cutoff: Option<SystemTime>,
    bin: &TrashBin,
) -> Removal {
    let children = match list(dir) {
        Ok(children) => children,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Removal::default(),
        Err(err) => return Removal::io(dir, &err),
    };
    let (batch, refused): (Vec<_>, Vec<_>) = children
        .into_par_iter()
        .filter_map(|child| {
            if ctx.is_cancelled() {
                return None;
            }
            if let Some(refused) = refuse_child(&child, guard, true) {
                return Some(Err(refused.removal));
            }
            let meta = fs::symlink_metadata(&child).ok()?;
            let size = tree_size(&child, &meta, ctx);
            if cutoff.is_some_and(|c| !size.older_than(c)) {
                return None;
            }
            Some(Ok((child, size.bytes)))
        })
        .partition_map(|r| match r {
            Ok(item) => rayon::iter::Either::Left(item),
            Err(removal) => rayon::iter::Either::Right(removal),
        });
    let removal = refused.into_iter().fold(Removal::default(), Removal::merge);
    if ctx.is_cancelled() || batch.is_empty() {
        return removal;
    }
    removal.merge(trash_paths(batch, bin))
}

/// Moves `(path, bytes)` items to `bin`.
fn trash_paths(items: Vec<(PathBuf, u64)>, bin: &TrashBin) -> Removal {
    match bin {
        TrashBin::System => system_trash(items),
        TrashBin::Folder { dir, owner } => items
            .into_iter()
            .map(|(path, bytes)| file_into(&path, bytes, dir, *owner))
            .fold(Removal::default(), Removal::merge),
    }
}

fn trash_context() -> trash::TrashContext {
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod as MacMethod, TrashContextExtMacos as _};
        let mut context = trash::TrashContext::default();
        // `NSFileManager` needs no Finder automation permission and makes no sound.
        context.set_delete_method(MacMethod::NsFileManager);
        context
    }
    #[cfg(not(target_os = "macos"))]
    {
        trash::TrashContext::default()
    }
}

/// One batch call; when it fails part-way, what is still there is retried one by one so
/// every failure is attributed to its path.
fn system_trash(items: Vec<(PathBuf, u64)>) -> Removal {
    let context = trash_context();
    let batch = context.delete_all(items.iter().map(|(path, _)| path));
    let err = match batch {
        Ok(()) => {
            return Removal::freed(items.iter().fold(0, |sum, (_, b)| sum.saturating_add(*b)));
        }
        Err(err) => err,
    };
    if let [(path, _)] = items.as_slice() {
        return Removal::failed(path.clone(), trash_reason(&err, path), err.to_string());
    }
    tracing::debug!(%err, "batch trash failed; retrying item by item");
    items
        .into_iter()
        .map(|(path, bytes)| {
            if fs::symlink_metadata(&path).is_err() {
                return Removal::freed(bytes);
            }
            match context.delete(&path) {
                Ok(()) => Removal::freed(bytes),
                Err(err) => {
                    Removal::failed(path.clone(), trash_reason(&err, &path), err.to_string())
                }
            }
        })
        .fold(Removal::default(), Removal::merge)
}

/// Maps a `trash` crate error. Its OS errors are mostly opaque strings, so the classes the
/// UI acts on are recognized from their text as well.
fn trash_reason(err: &trash::Error, path: &Path) -> FailReason {
    match err {
        trash::Error::Os { code, .. } => os_code_reason(*code, path),
        #[cfg(all(
            unix,
            not(target_os = "macos"),
            not(target_os = "ios"),
            not(target_os = "android")
        ))]
        trash::Error::FileSystem { path: at, source } => {
            // A failure on the trash folder itself (e.g. `/mnt/x/.Trash-1000` cannot be
            // created) means the volume has no usable Trash.
            if !crate::paths::is_within(at, path)
                && at
                    .as_os_str()
                    .as_encoded_bytes()
                    .windows(6)
                    .any(|w| w == b".Trash")
            {
                FailReason::TrashUnavailable
            } else {
                classify(source, at)
            }
        }
        trash::Error::CouldNotAccess { .. } => FailReason::PermissionDenied,
        trash::Error::TargetedRoot => FailReason::Protected,
        trash::Error::Unknown { description } => text_reason(description),
        _ => FailReason::Other,
    }
}

fn os_code_reason(code: i32, path: &Path) -> FailReason {
    // Windows reports HRESULTs; `0x8007xxxx` wraps a Win32 error code.
    let raw = u32::from_ne_bytes(code.to_ne_bytes());
    let errno = if raw & 0xFFFF_0000 == 0x8007_0000 {
        i32::try_from(raw & 0xFFFF).unwrap_or(code)
    } else {
        code
    };
    match classify(&io::Error::from_raw_os_error(errno), path) {
        FailReason::Other => text_reason(&io::Error::from_raw_os_error(errno).to_string()),
        reason => reason,
    }
}

fn text_reason(text: &str) -> FailReason {
    let text = text.to_ascii_lowercase();
    // NSFeatureUnsupportedError (3328): the volume has no Trash.
    if [
        "code=3328",
        "not supported",
        "unsupported",
        "no trash",
        "home trash",
    ]
    .iter()
    .any(|needle| text.contains(needle))
    {
        FailReason::TrashUnavailable
    } else if [
        "code=513",
        "permission",
        "not permitted",
        "access is denied",
    ]
    .iter()
    .any(|needle| text.contains(needle))
    {
        // NSFileWriteNoPermissionError (513).
        FailReason::PermissionDenied
    } else {
        FailReason::Other
    }
}

/// Renames `path` into `dir` under a free name, then hands the tree to `owner`.
fn file_into(path: &Path, bytes: u64, dir: &Path, owner: Option<u32>) -> Removal {
    let Some(dest) = free_name(path, dir) else {
        return Removal::failed(
            path.to_path_buf(),
            FailReason::TrashUnavailable,
            format!("no free name in {}", dir.display()),
        );
    };
    match fs::rename(path, &dest) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::CrossesDevices => {
            return Removal::failed(
                path.to_path_buf(),
                FailReason::TrashUnavailable,
                format!("{err} (the Trash is on another volume)"),
            );
        }
        Err(err) => return Removal::io(path, &err),
    }
    if let Some(uid) = owner {
        give_tree(&dest, uid);
    }
    Removal::freed(bytes)
}

/// `dir/<name>`, or `dir/<stem> 2.<ext>`, `… 3…` when taken.
fn free_name(path: &Path, dir: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let first = dir.join(name);
    if fs::symlink_metadata(&first).is_err() {
        return Some(first);
    }
    let stem = Path::new(name).file_stem()?.to_string_lossy().into_owned();
    let ext = Path::new(name)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..10_000u32)
        .map(|n| dir.join(format!("{stem} {n}{ext}")))
        .find(|candidate| fs::symlink_metadata(candidate).is_err())
}

/// `lchown`s `path` and everything below to `uid` (links not followed).
#[cfg(unix)]
fn give_tree(path: &Path, uid: u32) {
    if let Err(err) = std::os::unix::fs::lchown(path, Some(uid), None) {
        tracing::warn!(%err, path = %path.display(), "chown in Trash");
    }
    let is_dir = fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
    if !is_dir {
        return;
    }
    match list(path) {
        Ok(children) => children.par_iter().for_each(|child| give_tree(child, uid)),
        Err(err) => tracing::warn!(%err, path = %path.display(), "listing for chown"),
    }
}

#[cfg(not(unix))]
fn give_tree(_path: &Path, _uid: u32) {}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write as _;

    use omc_proto::settings::CleanSettings;

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir()
                .join(format!("omc-delete-{name}-{}-{nanos}", std::process::id()));
            assert!(fs::create_dir_all(&dir).is_ok(), "create temp dir");
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            if let Err(err) = fs::remove_dir_all(&self.0) {
                tracing::debug!(%err, "temp cleanup");
            }
        }
    }

    fn file(path: &Path, len: usize) {
        let created = File::create(path).and_then(|mut f| f.write_all(&vec![7u8; len]));
        assert!(created.is_ok(), "write {}", path.display());
    }

    #[cfg(unix)]
    fn age(path: &Path, secs: u64) {
        let past = SystemTime::now().checked_sub(Duration::from_secs(secs));
        assert!(past.is_some(), "time in range");
        let Some(past) = past else { return };
        let set = File::open(path).and_then(|f| f.set_modified(past));
        assert!(set.is_ok(), "set mtime of {}", path.display());
    }

    fn guard() -> Guard {
        Guard::new(&CleanSettings::default())
    }

    fn path_str(path: &Path) -> String {
        path.display().to_string()
    }

    #[test]
    fn removes_whole_file_and_dir_counting_bytes() {
        let tmp = TempDir::new("whole");
        let f = tmp.0.join("a.bin");
        file(&f, 10_000);
        let d = tmp.0.join("dir");
        assert!(fs::create_dir_all(d.join("x/y")).is_ok(), "mkdir");
        file(&d.join("x/y/z.bin"), 5_000);
        file(&d.join("top.bin"), 5_000);
        let ctx = JobCtx::new();
        let file_removal = remove(
            &Target::path(path_str(&f), 0, DeleteMethod::Permanent),
            &guard(),
            &ctx,
        );
        assert!(
            file_removal.failures.is_empty(),
            "file removed: {file_removal:?}"
        );
        assert!(
            file_removal.freed >= 10_000,
            "file bytes counted: {}",
            file_removal.freed
        );
        assert!(!f.exists(), "file gone");
        let r = remove(
            &Target::path(path_str(&d), 0, DeleteMethod::Permanent),
            &guard(),
            &ctx,
        );
        assert!(r.failures.is_empty(), "dir removed: {r:?}");
        assert!(r.freed >= 10_000, "dir bytes counted: {}", r.freed);
        assert!(!d.exists(), "dir gone");
        assert_eq!(
            ctx.snapshot().bytes,
            file_removal.freed + r.freed,
            "progress counts every freed byte exactly once"
        );
    }

    #[test]
    fn contents_only_keeps_the_root() {
        let tmp = TempDir::new("contents");
        let root = tmp.0.join("cache");
        assert!(fs::create_dir_all(root.join("sub")).is_ok(), "mkdir");
        file(&root.join("a"), 100);
        file(&root.join("sub/b"), 100);
        let r = remove(
            &Target::contents(path_str(&root), 0, DeleteMethod::Permanent),
            &guard(),
            &JobCtx::new(),
        );
        assert!(r.failures.is_empty(), "no failures: {r:?}");
        assert!(root.is_dir(), "root kept");
        assert!(list(&root).is_ok_and(|c| c.is_empty()), "root emptied");
    }

    #[cfg(unix)]
    #[test]
    fn min_age_keeps_new_files_and_prunes_old_empty_dirs() {
        let tmp = TempDir::new("age");
        let root = tmp.0.join("tmp");
        let old_dir = root.join("old_dir");
        let new_dir = root.join("new_dir");
        assert!(fs::create_dir_all(&old_dir).is_ok(), "mkdir old");
        assert!(fs::create_dir_all(&new_dir).is_ok(), "mkdir new");
        file(&root.join("old.log"), 10);
        file(&root.join("new.log"), 10);
        file(&old_dir.join("old2.log"), 10);
        let day = 86_400;
        age(&root.join("old.log"), 2 * day);
        age(&old_dir.join("old2.log"), 2 * day);
        age(&old_dir, 2 * day);
        let mixed = root.join("mixed_dir");
        assert!(fs::create_dir_all(&mixed).is_ok(), "mkdir mixed");
        file(&mixed.join("old3.log"), 10);
        age(&mixed.join("old3.log"), 2 * day);
        age(&mixed, 2 * day);
        let target = Target::contents(path_str(&root), 0, DeleteMethod::Permanent).older_than(day);
        let r = remove(&target, &guard(), &JobCtx::new());
        assert!(r.failures.is_empty(), "no failures: {r:?}");
        assert!(!root.join("old.log").exists(), "old file removed");
        assert!(root.join("new.log").exists(), "new file kept");
        assert!(!old_dir.exists(), "old emptied dir pruned");
        assert!(!mixed.exists(), "old dir with only old files pruned");
        assert!(new_dir.exists(), "new empty dir kept");
        assert!(root.is_dir(), "root kept");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_inside_is_removed_as_a_link() {
        let tmp = TempDir::new("link");
        let outside = tmp.0.join("outside");
        assert!(fs::create_dir_all(&outside).is_ok(), "mkdir");
        file(&outside.join("keep.txt"), 10);
        let d = tmp.0.join("victim");
        assert!(fs::create_dir_all(&d).is_ok(), "mkdir");
        assert!(
            std::os::unix::fs::symlink(&outside, d.join("link")).is_ok(),
            "symlink"
        );
        let r = remove(
            &Target::path(path_str(&d), 0, DeleteMethod::Permanent),
            &guard(),
            &JobCtx::new(),
        );
        assert!(r.failures.is_empty(), "no failures: {r:?}");
        assert!(!d.exists(), "dir with link removed");
        assert!(outside.join("keep.txt").exists(), "link target untouched");
    }

    #[test]
    fn guard_refusal_is_protected() {
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        let r = remove(
            &Target::path(root, 0, DeleteMethod::Permanent),
            &guard(),
            &JobCtx::new(),
        );
        assert_eq!(
            r.failures.first().map(|f| f.reason),
            Some(FailReason::Protected),
            "the root is refused"
        );
        let relative = remove(
            &Target::path("rel/x", 0, DeleteMethod::Permanent),
            &guard(),
            &JobCtx::new(),
        );
        assert_eq!(
            relative.failures.first().map(|f| f.reason),
            Some(FailReason::Protected),
            "relative paths are refused"
        );
    }

    #[test]
    fn missing_path_is_success() {
        let tmp = TempDir::new("missing");
        let r = remove(
            &Target::path(path_str(&tmp.0.join("nope")), 0, DeleteMethod::Permanent),
            &guard(),
            &JobCtx::new(),
        );
        assert_eq!(r, Removal::default(), "nothing to do, nothing failed");
    }

    #[test]
    fn excluded_child_is_kept() {
        let tmp = TempDir::new("exclude");
        let root = tmp.0.join("cache");
        assert!(fs::create_dir_all(root.join("keep/deep")).is_ok(), "mkdir");
        file(&root.join("keep/deep/f"), 10);
        file(&root.join("go"), 10);
        let settings = CleanSettings {
            exclude: vec![path_str(&root.join("keep"))],
            ..CleanSettings::default()
        };
        let guard = Guard::new(&settings);
        let r = remove(
            &Target::contents(path_str(&root), 0, DeleteMethod::Permanent),
            &guard,
            &JobCtx::new(),
        );
        assert!(
            r.failures.is_empty(),
            "exclusions are skipped silently: {r:?}"
        );
        assert!(root.join("keep/deep/f").exists(), "excluded tree kept");
        assert!(!root.join("go").exists(), "the rest removed");
        let whole = remove(
            &Target::path(path_str(&root), 0, DeleteMethod::Permanent),
            &guard,
            &JobCtx::new(),
        );
        assert_eq!(
            whole.failures.first().map(|f| f.reason),
            Some(FailReason::Protected),
            "a folder containing an exclusion is never removed whole"
        );
    }

    #[test]
    fn trash_folder_bin_renames_with_unique_names() {
        let tmp = TempDir::new("bin");
        let bin_dir = tmp.0.join("Trash");
        assert!(fs::create_dir_all(&bin_dir).is_ok(), "mkdir bin");
        file(&bin_dir.join("a.txt"), 1);
        let src = tmp.0.join("src");
        assert!(fs::create_dir_all(&src).is_ok(), "mkdir src");
        file(&src.join("a.txt"), 4_000);
        let bin = TrashBin::Folder {
            dir: bin_dir.clone(),
            owner: None,
        };
        let ctx = JobCtx::new();
        let r = remove_with(
            &Target::contents(path_str(&src), 0, DeleteMethod::Trash),
            &guard(),
            &ctx,
            &bin,
        );
        assert!(r.failures.is_empty(), "moved: {r:?}");
        assert!(r.freed >= 4_000, "moved bytes counted: {}", r.freed);
        assert_eq!(ctx.snapshot().bytes, r.freed, "progress counts moved bytes");
        assert!(
            bin_dir.join("a 2.txt").is_file(),
            "collision gets a numbered name"
        );
        assert!(
            bin_dir.join("a.txt").is_file(),
            "existing Trash item untouched"
        );
        assert!(
            src.is_dir() && !src.join("a.txt").exists(),
            "source emptied, root kept"
        );
    }

    #[test]
    fn non_path_location_fails() {
        let target = Target::location(
            omc_proto::jobs::Location::RegistryKey {
                key: "HKCU\\x".to_owned(),
            },
            0,
        );
        let r = remove(&target, &guard(), &JobCtx::new());
        assert_eq!(
            r.failures.first().map(|f| f.reason),
            Some(FailReason::Other),
            "registry keys are not files"
        );
    }
}
