//! Well-known folders from the environment, the "too broad to belong to one app" test,
//! small file-system helpers, the PowerShell runner, and a scoped parallel map.

use std::path::Path;
use std::time::Duration;

use omc_scan::JobCtx;

use super::parse;
use crate::{Result, cmd};

/// An environment folder as a display string (no trailing separator).
pub(crate) fn env(var: &str) -> Option<String> {
    omc_scan::paths::env_dir(var).and_then(|p| parse::clean_path(&p.display().to_string()))
}

/// `%VAR%\rest`.
pub(crate) fn env_join(var: &str, rest: &str) -> Option<String> {
    env(var).map(|base| format!("{base}\\{rest}"))
}

/// `%WINDIR%` (falls back to `C:\Windows`).
pub(crate) fn windir() -> String {
    env("WINDIR")
        .or_else(|| env("SystemRoot"))
        .unwrap_or_else(|| "C:\\Windows".to_owned())
}

/// `%LOCALAPPDATA%Low` (`…\AppData\LocalLow`).
pub(crate) fn local_low() -> Option<String> {
    env("LOCALAPPDATA").map(|l| format!("{l}Low"))
}

/// Folders whose removal would hit more than one app (or the OS), and trees an app's
/// install location must never be derived from.
pub(crate) fn is_broad_dir(path: &str) -> bool {
    if !parse::is_absolute(path) {
        return true;
    }
    let trimmed = path.trim_end_matches(['\\', '/']);
    if trimmed.len() <= 2 {
        return true;
    }
    let within_any = [
        Some(windir()),
        env_join("ProgramData", "Package Cache"),
        env_join("ProgramData", "Microsoft"),
        env_join("LOCALAPPDATA", "Temp"),
        env("TEMP"),
        env_join("LOCALAPPDATA", "Microsoft\\WindowsApps"),
    ];
    if within_any
        .iter()
        .flatten()
        .any(|base| parse::path_within(path, base))
    {
        return true;
    }
    let equal_any = [
        env("ProgramFiles"),
        env("ProgramFiles(x86)"),
        env("ProgramW6432"),
        env("CommonProgramFiles"),
        env("CommonProgramFiles(x86)"),
        env("CommonProgramW6432"),
        env("ProgramData"),
        env("ALLUSERSPROFILE"),
        env("APPDATA"),
        env("LOCALAPPDATA"),
        env_join("LOCALAPPDATA", "Programs"),
        env_join("LOCALAPPDATA", "Packages"),
        local_low(),
        env("USERPROFILE"),
        env_join("USERPROFILE", "AppData"),
        env_join("USERPROFILE", "Desktop"),
        env_join("USERPROFILE", "Documents"),
        env_join("USERPROFILE", "Downloads"),
        env_join("USERPROFILE", "OneDrive"),
        env("PUBLIC"),
        env("USERPROFILE").and_then(|p| parse::parent_dir(&p).map(str::to_owned)),
        env_join("ProgramFiles", "WindowsApps"),
        env_join("ProgramFiles", "Common Files"),
        env_join("ProgramFiles(x86)", "Common Files"),
    ];
    equal_any
        .iter()
        .flatten()
        .any(|base| parse::path_within(path, base) && parse::path_within(base, path))
}

/// Removing it needs administrator rights (anything outside the user's profile).
pub(crate) fn needs_admin(path: &str) -> bool {
    env("USERPROFILE").is_none_or(|home| !parse::path_within(path, &home))
}

/// `path` is an existing file.
pub(crate) fn is_file(path: &str) -> bool {
    parse::is_absolute(path) && Path::new(path).is_file()
}

/// `path` exists (file or directory).
pub(crate) fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

/// A command's target is gone: its program (or `rundll32` DLL) is an absolute path on a
/// present drive, outside `%WINDIR%` (32-bit redirection makes those unreliable), and
/// does not exist. Unknown = not missing.
pub(crate) fn command_missing(command: &str) -> bool {
    let expanded = super::reg::expand(command);
    let Some(target) = parse::command_target(&expanded, is_file) else {
        return false;
    };
    target_missing(&target)
}

/// [`command_missing`] for a plain path.
pub(crate) fn target_missing(target: &str) -> bool {
    if !parse::is_absolute(target) || parse::path_within(target, &windir()) {
        return false;
    }
    let drive_present = parse::drive_root(target).is_some_and(|root| Path::new(&root).is_dir());
    drive_present && !exists(target)
}

/// Directory entries of `dir` (name, path, `is_dir`); symlinks and junctions are skipped.
pub(crate) fn entries(dir: &str) -> Vec<(String, String, bool)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.flatten()
        .filter_map(|entry| {
            let kind = entry.file_type().ok()?;
            if kind.is_symlink() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            Some((name, parse::path_str(&entry.path()), kind.is_dir()))
        })
        .collect()
}

/// `.lnk` files below `dir` (depth-limited), with the folders visited.
pub(crate) fn shortcuts(dir: &str, max_depth: u32) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_owned(), 0_u32)];
    while let Some((current, depth)) = stack.pop() {
        for (name, path, is_dir) in entries(&current) {
            if is_dir {
                if depth < max_depth {
                    stack.push((path, depth.saturating_add(1)));
                }
            } else if name.to_ascii_lowercase().ends_with(".lnk") {
                out.push(path);
            }
        }
    }
    out
}

/// Reads a small file (shortcuts); `None` for large or unreadable files.
pub(crate) fn read_small(path: &str) -> Option<Vec<u8>> {
    const MAX: u64 = 1 << 20;
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX {
        return None;
    }
    std::fs::read(path).ok()
}

/// Top-level `*.exe` file names (lower case) of an install folder.
pub(crate) fn top_level_exes(dir: &str) -> Vec<String> {
    entries(dir)
        .into_iter()
        .filter(|(name, _, is_dir)| !is_dir && name.to_ascii_lowercase().ends_with(".exe"))
        .map(|(name, _, _)| name.to_ascii_lowercase())
        .take(64)
        .collect()
}

/// Runs a PowerShell script (UTF-8 output, no profile, no window).
pub(crate) fn powershell(script: &str, timeout: Duration) -> Result<String> {
    let script = format!("[Console]::OutputEncoding=[Text.Encoding]::UTF8; {script}");
    cmd::run(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ],
        timeout,
    )?
    .ok("powershell")
}

/// Maps `items` on a few scoped threads (registry enumeration is I/O-bound and the
/// registry API is synchronous); stops early when the job is cancelled.
pub(crate) fn par_flat_map<T: Sync, R: Send>(
    items: &[T],
    ctx: &JobCtx,
    f: impl Fn(&T) -> Vec<R> + Sync,
) -> Vec<R> {
    let threads = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .clamp(1, 8);
    let chunk = items.len().div_ceil(threads).max(1);
    let f = &f;
    std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    let mut out = Vec::new();
                    for item in part {
                        if ctx.is_cancelled() {
                            break;
                        }
                        out.extend(f(item));
                    }
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| {
                h.join().unwrap_or_else(|_| {
                    tracing::warn!("registry worker panicked");
                    Vec::new()
                })
            })
            .collect()
    })
}
