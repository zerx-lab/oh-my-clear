//! Is a build running in a Cargo `target` directory? Cargo holds an advisory file lock on
//! `<target>/<profile>/.cargo-lock` (and `<target>/<triple>/<profile>/.cargo-lock` when
//! cross-compiling) for the length of a build, so a clean rule must not remove the folder
//! while any of them is held: it would delete artifacts under the compiler.
//!
//! The probe takes the lock itself and lets go at once; it never modifies the file.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

const LOCK_FILE: &str = ".cargo-lock";
/// A `target` directory is at most `target/<triple>/<profile>/.cargo-lock` deep.
const MAX_LEVELS: usize = 2;

/// `true` when `path` is a folder named `target` (what the scanner reports for Cargo).
pub(crate) fn is_target_dir(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "target")
}

/// The first of `targets` in which a build holds a lock, if any. Blocking (file I/O).
pub(crate) fn held<'a>(targets: impl IntoIterator<Item = &'a Path>) -> Option<&'a Path> {
    targets
        .into_iter()
        .filter(|target| is_target_dir(target))
        .find(|target| lock_files(target).iter().any(|file| is_locked(file)))
}

/// Every `.cargo-lock` up to [`MAX_LEVELS`] below `target`.
fn lock_files(target: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut level = vec![target.to_path_buf()];
    for depth in 0..=MAX_LEVELS {
        let mut next = Vec::new();
        for dir in &level {
            let file = dir.join(LOCK_FILE);
            if file.is_file() {
                found.push(file);
            }
            // Deeper folders (`deps`, `build`…) hold thousands of files and no lock.
            if depth == MAX_LEVELS {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            next.extend(
                entries
                    .filter_map(|entry| entry.ok().map(|e| e.path()))
                    .filter(|path| path.is_dir()),
            );
        }
        level = next;
    }
    found
}

/// `true` when another process holds `file`'s lock. A file that cannot be opened or locked
/// for another reason counts as free (logged): a lock file nobody can open is not a build.
fn is_locked(file: &Path) -> bool {
    let (handle, exclusive) = match OpenOptions::new().read(true).write(true).open(file) {
        Ok(handle) => (handle, true),
        Err(_) => match File::open(file) {
            Ok(handle) => (handle, false),
            Err(err) => {
                tracing::debug!(file = %file.display(), %err, "cannot open the cargo lock");
                return false;
            }
        },
    };
    let probe = if exclusive {
        handle.try_lock()
    } else {
        handle.try_lock_shared()
    };
    match probe {
        Ok(()) => {
            if let Err(err) = handle.unlock() {
                tracing::debug!(file = %file.display(), %err, "cannot release the probe lock");
            }
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(err)) => {
            tracing::warn!(file = %file.display(), %err, "cannot probe the cargo lock");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_target(name: &str) -> Result<PathBuf, String> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let base = std::env::temp_dir().join(format!(
            "omc-buildlock-{name}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let target = base.join("proj").join("target");
        std::fs::create_dir_all(target.join("debug"))
            .map_err(|err| format!("create {target:?}: {err}"))?;
        std::fs::create_dir_all(target.join("x86_64-unknown-linux-gnu").join("release"))
            .map_err(|err| format!("create cross dir: {err}"))?;
        Ok(target)
    }

    fn lock(path: &Path) -> Result<File, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|err| format!("open {path:?}: {err}"))?;
        file.try_lock()
            .map_err(|err| format!("lock {path:?}: {err}"))?;
        Ok(file)
    }

    #[test]
    fn a_held_profile_lock_is_seen_and_a_released_one_is_not() {
        let target = temp_target("held");
        assert!(target.is_ok(), "temp target: {target:?}");
        let Ok(target) = target else { return };
        let debug_lock = target.join("debug").join(LOCK_FILE);
        assert!(
            std::fs::write(&debug_lock, b"").is_ok(),
            "touch the lock file"
        );
        assert!(
            held([target.as_path()]).is_none(),
            "an unlocked lock file means no build"
        );
        let guard = lock(&debug_lock);
        assert!(guard.is_ok(), "hold the lock: {guard:?}");
        assert_eq!(
            held([target.as_path()]),
            Some(target.as_path()),
            "a build holding the profile lock blocks the clean"
        );
        drop(guard);
        assert!(
            held([target.as_path()]).is_none(),
            "released again once the build ends"
        );
        remove_fixture(&target);
    }

    #[test]
    fn cross_compile_locks_count_and_other_folders_are_not_probed() {
        let target = temp_target("cross");
        assert!(target.is_ok(), "temp target: {target:?}");
        let Ok(target) = target else { return };
        let cross = target
            .join("x86_64-unknown-linux-gnu")
            .join("release")
            .join(LOCK_FILE);
        let guard = lock(&cross);
        assert!(guard.is_ok(), "hold the lock: {guard:?}");
        assert!(
            held([target.as_path()]).is_some(),
            "<target>/<triple>/<profile>/.cargo-lock is probed"
        );
        let plain = target.with_file_name("build-output");
        assert!(
            held([plain.as_path()]).is_none(),
            "only folders named target are Cargo build dirs"
        );
        drop(guard);
        remove_fixture(&target);
    }

    fn remove_fixture(target: &Path) {
        if let Some(base) = target.parent().and_then(Path::parent) {
            let removed = std::fs::remove_dir_all(base);
            assert!(removed.is_ok(), "remove the fixture: {removed:?}");
        }
    }
}
