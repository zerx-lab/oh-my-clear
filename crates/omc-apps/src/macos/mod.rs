//! macOS: `.app` bundles in the application folders, their Library files, launchd jobs,
//! login items and installer receipts. Everything is read with std, `plist`, and the stock
//! command-line tools (`codesign`, `pkgutil`, `launchctl`, `osascript`, `mdls`, `mdfind`,
//! `lsof`, `getconf`, `df`) — no `unsafe`.
//!
//! Implements the platform contract shared with `windows` and `linux`: `AppDetail`,
//! `StartupDetail`, `list_apps`, `app_files`, `quit_app`, `before_uninstall`,
//! `run_uninstaller`, `leftovers`, `list_startup`, `change_startup`, `system_info`,
//! `run_special`, `remove_registry`.

use std::path::{Path, PathBuf};

use omc_proto::apps::{StartupChange, StartupItem, UninstallerOutcome};
use omc_proto::jobs::{Location, SpecialAction};
use omc_proto::junk::JunkReport;
use omc_proto::settings::{CleanSettings, SystemInfo};
use omc_scan::{JobCtx, Scanned, Walker};

use crate::{AppFiles, AppRecord, Error, Result, StartupRecord};

mod access;
mod attribution;
mod files;
mod home;
mod icons;
mod ident;
mod inventory;
mod leftovers;
mod plist_util;
mod sources;
mod startup;
mod system;

/// What macOS needs to act on an app.
#[derive(Debug, Clone)]
pub(crate) struct AppDetail {
    /// The bundle as listed (may be a symlink).
    pub(crate) bundle: PathBuf,
    /// The bundle directory (symlinks resolved).
    pub(crate) real: PathBuf,
    /// `CFBundleIdentifier`.
    pub(crate) bundle_id: Option<String>,
    /// `CFBundleExecutable`.
    pub(crate) executable: Option<String>,
    /// The Homebrew cask that installed it.
    pub(crate) cask: Option<inventory::Cask>,
}

/// What macOS needs to change a startup item.
#[derive(Debug, Clone)]
pub(crate) enum StartupDetail {
    /// A login item (System Events).
    LoginItem {
        /// Its name.
        name: String,
    },
    /// A launch agent or daemon.
    Launchd {
        /// `Label`.
        label: String,
        /// The plist.
        plist: PathBuf,
        /// Which domain it loads into.
        domain: startup::Domain,
    },
}

/// Extra names of `app` besides its display name: Electron product names and the
/// executable. Reads the bundle's `package.json` only (no walks).
pub(crate) fn app_aliases(app: &AppRecord) -> Vec<String> {
    let mut out = Vec::new();
    for name in sources::electron(&app.detail.real).products {
        push_alias(&mut out, &name);
    }
    if let Some(exe) = &app.detail.executable {
        push_alias(&mut out, exe);
    }
    out
}

/// Adds `name` (trimmed, non-empty) unless already present, ignoring case.
fn push_alias(out: &mut Vec<String>, name: &str) {
    let name = name.trim();
    if !name.is_empty() && !out.iter().any(|n| n.eq_ignore_ascii_case(name)) {
        out.push(name.to_owned());
    }
}

/// Maps `f` over `items` on up to `threads` OS threads (0 = one per CPU), keeping order.
/// Used for per-bundle work that is I/O bound but not a tree walk.
fn par_map<T: Sync, R: Send>(items: &[T], threads: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = if threads == 0 {
        std::thread::available_parallelism().map_or(4, std::num::NonZero::get)
    } else {
        threads
    };
    let threads = threads.clamp(1, items.len().max(1));
    if threads == 1 {
        return items.iter().map(&f).collect();
    }
    let chunk = items.len().div_ceil(threads).max(1);
    let f = &f;
    std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|part| scope.spawn(move || part.iter().map(f).collect::<Vec<R>>()))
            .collect();
        let mut out = Vec::with_capacity(items.len());
        for handle in handles {
            // A panicking worker cannot happen without a bug; the order-preserving
            // contract then no longer holds, so return nothing rather than misalign.
            let Ok(part) = handle.join() else {
                tracing::error!("parallel worker panicked");
                return Vec::new();
            };
            out.extend(part);
        }
        out
    })
}

pub(crate) fn list_apps(
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Vec<AppRecord>> {
    inventory::list_apps(settings, walker, ctx)
}

pub(crate) fn app_files(
    app: &AppRecord,
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<AppFiles> {
    files::app_files(app, settings, walker, ctx)
}

/// Quits the app gracefully (Apple event), then with SIGTERM, then SIGKILL; `Ok` when it
/// is not running.
pub(crate) fn quit_app(app: &AppRecord, ctx: &JobCtx) -> Result<()> {
    system::quit_app(app, ctx)
}

/// Nothing to prepare on macOS.
#[expect(
    clippy::unnecessary_wraps,
    reason = "platform contract: Windows creates a restore point here, which can fail"
)]
pub(crate) fn before_uninstall(_: &AppRecord, _: &CleanSettings, _: &JobCtx) -> Result<()> {
    Ok(())
}

/// Homebrew casks: `brew uninstall --cask`; bundles are removed as the `Bundle` item.
pub(crate) fn run_uninstaller(
    files: &AppFiles,
    settings: &CleanSettings,
    ctx: &JobCtx,
) -> UninstallerOutcome {
    system::run_uninstaller(files, settings, ctx)
}

pub(crate) fn leftovers(
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Scanned<JunkReport>> {
    leftovers::leftovers(settings, walker, ctx)
}

pub(crate) fn list_startup(settings: &CleanSettings, ctx: &JobCtx) -> Result<Vec<StartupRecord>> {
    startup::list_startup(settings, ctx)
}

pub(crate) fn change_startup(
    record: &StartupRecord,
    change: StartupChange,
    settings: &CleanSettings,
    ctx: &JobCtx,
) -> Result<Option<StartupItem>> {
    startup::change_startup(record, change, settings, ctx)
}

pub(crate) fn system_info() -> SystemInfo {
    system::system_info()
}

/// Performs a [`Location::Special`] action. Actions that need root fail with
/// [`Error::Elevation`] unless already running as root (the elevated helper).
pub(crate) fn run_special(action: &SpecialAction) -> Result<()> {
    system::run_special(action)
}

/// The registry is Windows-only.
pub(crate) fn remove_registry(_: &Location, _: Option<&Path>) -> Result<()> {
    Err(Error::Unsupported("macOS has no registry".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn par_map_keeps_order() {
        let items: Vec<u32> = (0..103).collect();
        let doubled = par_map(&items, 8, |x| x.saturating_mul(2));
        let expected: Vec<u32> = items.iter().map(|x| x * 2).collect();
        assert_eq!(doubled, expected, "order preserved across chunks");
        assert!(
            par_map(&Vec::<u32>::new(), 0, |x| *x).is_empty(),
            "empty input"
        );
    }
}
