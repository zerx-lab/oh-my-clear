//! Shared helpers of the Linux platform module: XDG base directories, `PATH` lookup,
//! helper commands with a stable locale, and name normalization.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use omc_scan::paths;

use crate::cmd;

/// Deadline of package-database queries (`rpm -qa` can take seconds on big systems).
pub(super) const LIST_TIMEOUT: Duration = Duration::from_secs(120);

/// Deadline of quick helpers (`id`, `df`, `systemctl`).
pub(super) const QUICK_TIMEOUT: Duration = Duration::from_secs(20);

/// Seconds per day.
pub(super) const DAY_SECS: i64 = 86_400;

/// The home folder.
pub(super) fn home() -> Option<PathBuf> {
    paths::home()
}

fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    paths::env_dir(var)
        .filter(|p| p.is_absolute())
        .or_else(|| home().map(|h| h.join(fallback)))
}

/// `$XDG_CONFIG_HOME` or `~/.config`.
pub(super) fn config_home() -> Option<PathBuf> {
    xdg("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_DATA_HOME` or `~/.local/share`.
pub(super) fn data_home() -> Option<PathBuf> {
    xdg("XDG_DATA_HOME", ".local/share")
}

/// `$XDG_STATE_HOME` or `~/.local/state`.
pub(super) fn state_home() -> Option<PathBuf> {
    xdg("XDG_STATE_HOME", ".local/state")
}

/// `$XDG_CACHE_HOME` or `~/.cache`.
pub(super) fn cache_home() -> Option<PathBuf> {
    xdg("XDG_CACHE_HOME", ".cache")
}

/// The desktop folder (`XDG_DESKTOP_DIR` of `user-dirs.dirs`, else `~/Desktop`).
pub(super) fn desktop_dir() -> Option<PathBuf> {
    let home = home()?;
    let configured = config_home()
        .and_then(|c| std::fs::read_to_string(c.join("user-dirs.dirs")).ok())
        .and_then(|text| parse_user_dir(&text, "XDG_DESKTOP_DIR", &home));
    Some(configured.unwrap_or_else(|| home.join("Desktop")))
}

/// `key` of a `user-dirs.dirs` file (`XDG_DESKTOP_DIR="$HOME/Bureau"`); `$HOME` expanded,
/// relative or empty values ignored.
pub(super) fn parse_user_dir(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    text.lines().find_map(|line| {
        let value = line
            .trim()
            .strip_prefix(key)?
            .trim_start()
            .strip_prefix('=')?;
        let value = value.trim().trim_matches('"');
        let path = match value.strip_prefix("$HOME") {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(value),
        };
        (path.is_absolute() && path != home).then_some(path)
    })
}

/// Directories of `$PATH` plus the usual binary directories (the daemon may be started
/// with a minimal environment).
pub(super) fn path_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for extra in [
        "/usr/local/sbin",
        "/usr/local/bin",
        "/usr/sbin",
        "/usr/bin",
        "/sbin",
        "/bin",
        "/snap/bin",
        "/var/lib/flatpak/exports/bin",
    ] {
        dirs.push(PathBuf::from(extra));
    }
    if let Some(home) = home() {
        dirs.push(home.join(".local/bin"));
    }
    let mut seen = BTreeSet::new();
    dirs.retain(|d| d.is_absolute() && seen.insert(d.clone()));
    dirs
}

/// The first executable file called `name` in [`path_dirs`].
pub(super) fn which(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    path_dirs()
        .into_iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Resolves the program of a desktop entry's `Exec`: absolute paths as they are, bare
/// names through `PATH`.
pub(super) fn resolve_program(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else if let Some(rest) = program.strip_prefix("~/") {
        home().map(|h| h.join(rest))
    } else {
        which(program)
    }
}

/// Whether a program referenced by a desktop entry exists (absolute path or on `PATH`).
pub(super) fn program_exists(program: &str) -> bool {
    resolve_program(program).is_some_and(|p| p.exists())
}

/// Output of a helper run with `LC_ALL=C`; `None` (logged) when it cannot be started or
/// times out. With `require_success`, a failing exit status is `None` too; otherwise the
/// stdout of a failing run is returned (`dpkg -S` exits 1 when one path is unowned).
pub(super) fn run_stdout(
    program: &str,
    args: &[&str],
    timeout: Duration,
    require_success: bool,
) -> Option<String> {
    let mut command = cmd::command(program);
    command.args(args).env("LC_ALL", "C");
    match cmd::run_command(program, &mut command, timeout) {
        Ok(out) if out.status.success() || !require_success => Some(out.stdout),
        Ok(out) => {
            tracing::debug!(program, status = %out.status, stderr = %out.stderr.trim(), "helper failed");
            None
        }
        Err(err) => {
            tracing::debug!(%err, program, "helper unavailable");
            None
        }
    }
}

/// Lowercased, trimmed name used for matching folders to apps.
pub(super) fn norm(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Last dot-separated segment of a reverse-DNS id (`org.gnome.Nautilus` → `nautilus`).
pub(super) fn last_segment(id: &str) -> Option<String> {
    let (_, last) = id.rsplit_once('.')?;
    (!last.is_empty()).then(|| norm(last))
}

/// File name as UTF-8, when it is.
pub(super) fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(OsStr::to_str)
}

/// Children of `dir` (empty when unreadable, logged).
pub(super) fn children(dir: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.flatten().map(|e| e.path()).collect(),
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(%err, dir = %dir.display(), "cannot list");
            }
            Vec::new()
        }
    }
}

/// `*.desktop` files directly in `dir`.
pub(super) fn desktop_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = children(dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "desktop"))
        .collect();
    files.sort();
    files
}

/// Names too generic to identify an app's folder (and folders every desktop has).
pub(super) fn is_generic_name(name: &str) -> bool {
    const GENERIC: &[&str] = &[
        "app",
        "apps",
        "application",
        "applications",
        "autostart",
        "bash",
        "bin",
        "cache",
        "com",
        "config",
        "data",
        "default",
        "desktop",
        "electron",
        "env",
        "flatpak",
        "fonts",
        "gnome",
        "google",
        "gtk",
        "icons",
        "java",
        "kde",
        "lib",
        "local",
        "main",
        "mozilla",
        "net",
        "node",
        "org",
        "perl",
        "python",
        "python3",
        "qt",
        "run",
        "sh",
        "share",
        "snap",
        "state",
        "systemd",
        "user",
        "wine",
        "xdg",
    ];
    name.chars().count() < 3 || GENERIC.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_dirs_expand_home_and_skip_invalid() {
        let home = Path::new("/home/u");
        let text =
            "# comment\nXDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\nXDG_DESKTOP_DIR=\"$HOME/Bureau\"\n";
        assert_eq!(
            parse_user_dir(text, "XDG_DESKTOP_DIR", home),
            Some(PathBuf::from("/home/u/Bureau")),
            "$HOME expanded"
        );
        assert_eq!(
            parse_user_dir("XDG_DESKTOP_DIR=\"/data/desk\"", "XDG_DESKTOP_DIR", home),
            Some(PathBuf::from("/data/desk")),
            "absolute value kept"
        );
        assert_eq!(
            parse_user_dir("XDG_DESKTOP_DIR=\"$HOME/\"", "XDG_DESKTOP_DIR", home),
            None,
            "home itself (desktop disabled) ignored"
        );
        assert_eq!(
            parse_user_dir("XDG_DESKTOP_DIR=\"desk\"", "XDG_DESKTOP_DIR", home),
            None,
            "relative ignored"
        );
    }
}
