//! Who we run as and whether a path can be removed without administrator rights. The uid
//! and groups come from `id` once per process (no `unsafe` libc calls).

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use crate::cmd;

/// `id` answers instantly; this only bounds a wedged system.
const ID_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct Account {
    uid: u32,
    groups: Vec<u32>,
}

static ACCOUNT: LazyLock<Account> = LazyLock::new(|| {
    let uid = id_numbers("-u").first().copied().unwrap_or_else(|| {
        // Fall back to the owner of the home folder.
        omc_scan::paths::home()
            .and_then(|h| std::fs::metadata(h).ok())
            .map_or(u32::MAX, |m| m.uid())
    });
    Account {
        uid,
        groups: id_numbers("-G"),
    }
});

fn account() -> &'static Account {
    &ACCOUNT
}

fn id_numbers(flag: &str) -> Vec<u32> {
    match cmd::run("/usr/bin/id", &[flag], ID_TIMEOUT).and_then(|o| o.ok("id")) {
        Ok(out) => out
            .split_whitespace()
            .filter_map(|n| n.parse().ok())
            .collect(),
        Err(err) => {
            tracing::warn!(%err, flag, "id failed");
            Vec::new()
        }
    }
}

/// The current user id.
pub(super) fn uid() -> u32 {
    account().uid
}

/// Running as root.
pub(super) fn is_root() -> bool {
    uid() == 0
}

/// The uid of the console (GUI) user: ours, or the owner of `/dev/console` when running as
/// root (the elevated helper acting on the user's launchd domain).
pub(super) fn console_uid() -> u32 {
    if is_root()
        && let Ok(meta) = std::fs::metadata("/dev/console")
        && meta.uid() != 0
    {
        return meta.uid();
    }
    uid()
}

/// We may modify entries of a directory with this metadata.
fn dir_writable(meta: &std::fs::Metadata) -> bool {
    let acct = account();
    if acct.uid == 0 {
        return true;
    }
    let mode = meta.mode();
    if meta.uid() == acct.uid {
        return mode & 0o200 != 0;
    }
    if acct.groups.contains(&meta.gid()) {
        return mode & 0o020 != 0;
    }
    mode & 0o002 != 0
}

/// Removing `path` needs administrator rights: it is under `/Library` or the system
/// databases (`/private/var/db`), or its parent (or, for a folder, itself) is not writable
/// by us.
pub(super) fn needs_admin(path: &Path) -> bool {
    if is_root() {
        return false;
    }
    if ["/Library", "/private/var/db", "/var/db"]
        .iter()
        .any(|base| path.starts_with(base))
    {
        return true;
    }
    let parent_ok = path
        .parent()
        .and_then(|p| std::fs::metadata(p).ok())
        .is_some_and(|m| dir_writable(&m));
    if !parent_ok {
        return true;
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => !dir_writable(&meta),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_temp_dir_needs_no_admin() {
        let dir = std::env::temp_dir().join(format!("omc-access-{}", std::process::id()));
        assert!(std::fs::create_dir_all(&dir).is_ok(), "temp dir");
        assert!(!needs_admin(&dir), "own folder");
        assert!(
            is_root() || needs_admin(Path::new("/Library/Preferences/x.plist")),
            "/Library needs admin"
        );
        if let Err(err) = std::fs::remove_dir_all(&dir) {
            tracing::debug!(%err, "cleanup");
        }
    }
}
