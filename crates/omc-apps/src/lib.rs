//! Installed applications per OS: inventory, related files and registry entries,
//! uninstall, leftovers, startup items, elevation, and the removal executor every clean
//! goes through.
//!
//! Linked by omc-engine and oh-my-clear-daemon (the `elevated` helper subcommand); never by
//! the UI. Allowed internal deps: `LAYERS` in xtask/src/layers.rs.
//!
//! The public functions below are the whole API; each forwards to the platform module
//! (`macos`, `windows`, or `linux` for every other Unix), which implements the same set of
//! `pub(crate)` items (see `docs` at the top of each platform module). Everything is
//! synchronous and runs on the engine's blocking threads.

use std::path::Path;

use omc_proto::apps::{AppFilesReport, AppInfo, StartupChange, StartupItem, UninstallReport};
use omc_proto::jobs::{CleanReport, ItemId};
use omc_proto::junk::JunkReport;
use omc_proto::settings::{CleanSettings, SystemInfo};
use omc_scan::{JobCtx, Scanned, Target, WalkOptions, Walker};

mod cmd;
pub mod elevate;
pub mod names;
pub mod remove;
mod uninstall;
mod userconf;

#[cfg(not(any(target_os = "macos", windows)))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(not(any(target_os = "macos", windows)))]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
use windows as platform;

/// Failures that end an apps job (per-item problems are reported, not raised).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A scanner failure (thread pool, cancellation).
    #[error(transparent)]
    Scan(#[from] omc_scan::Error),
    /// I/O error.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// A helper program failed.
    #[error("{program}: {message}")]
    Command {
        /// The program.
        program: String,
        /// What went wrong (exit status, stderr).
        message: String,
    },
    /// Unexpected data from the OS (plist, registry, package database).
    #[error("cannot parse {what}: {message}")]
    Parse {
        /// What was being parsed.
        what: String,
        /// Detail.
        message: String,
    },
    /// JSON (elevation manifest, PowerShell output).
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// The operation does not exist on this OS or for this item.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// Administrator rights could not be obtained.
    #[error("elevation: {0}")]
    Elevation(String),
    /// The job was cancelled.
    #[error("cancelled")]
    Cancelled,
}

/// Result alias of this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// An installed app: what the UI sees plus what the platform needs to act on it.
#[derive(Debug, Clone)]
pub struct AppRecord {
    /// Wire form (its `id` is the index in the list).
    pub info: AppInfo,
    pub(crate) detail: platform::AppDetail,
}

/// Everything that belongs to one app, ready for [`uninstall`].
#[derive(Debug, Clone)]
pub struct AppFiles {
    /// Wire form.
    pub report: AppFilesReport,
    /// Removal target of each item: `targets[id]`.
    pub targets: Vec<Target>,
    /// The app.
    pub app: AppRecord,
}

/// A startup item plus what the platform needs to change it.
#[derive(Debug, Clone)]
pub struct StartupRecord {
    /// Wire form (its `id` is the index in the list).
    pub item: StartupItem,
    pub(crate) detail: platform::StartupDetail,
}

/// Installed apps, sorted by name, ids = indices. System apps are included only when
/// `settings.show_system_apps` is set.
pub fn list_apps(
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Vec<AppRecord>> {
    let mut apps = platform::list_apps(settings, walker, ctx)?;
    if !settings.show_system_apps {
        apps.retain(|a| !a.info.system);
    }
    apps.sort_by_cached_key(|a| a.info.name.to_lowercase());
    for (i, app) in apps.iter_mut().enumerate() {
        app.info.id = ItemId::try_from(i).unwrap_or(ItemId::MAX);
    }
    Ok(apps)
}

/// Everything that belongs to `app`.
pub fn app_files(
    app: &AppRecord,
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<AppFiles> {
    if app.info.system {
        return Err(Error::Unsupported(format!(
            "{} ships with the operating system",
            app.info.name
        )));
    }
    platform::app_files(app, settings, walker, ctx)
}

/// Uninstalls `files.app`: quits it, runs its own uninstaller when requested, then removes
/// the selected `items` that still exist.
pub fn uninstall(
    files: &AppFiles,
    items: &[ItemId],
    run_uninstaller: bool,
    settings: &CleanSettings,
    ctx: &JobCtx,
) -> Result<UninstallReport> {
    uninstall::run(files, items, run_uninstaller, settings, ctx)
}

/// Support files and registrations of apps that are no longer installed, named after
/// their apps where known (see [`names`]).
pub fn leftovers(
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Scanned<JunkReport>> {
    let mut scan = platform::leftovers(settings, walker, ctx)?;
    annotate_junk(&mut scan.report, settings, walker, ctx);
    Ok(scan)
}

/// Gives junk items friendly names (installed app name and icon, or a readable product
/// name); the raw name moves to `ident`. Ids, groups and order are unchanged. Builds an
/// app inventory, so call it once per scan and only for reports named by identifiers
/// (system junk, leftovers).
pub fn annotate_junk(
    report: &mut JunkReport,
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) {
    if report.groups.is_empty() {
        return;
    }
    let namer = names::Namer::build(settings, walker, ctx);
    names::annotate_junk(report, &namer);
}

/// Programs started at login or boot, named after their apps where known, sorted by name,
/// ids = indices.
pub fn list_startup(settings: &CleanSettings, ctx: &JobCtx) -> Result<Vec<StartupRecord>> {
    let mut items = platform::list_startup(settings, ctx)?;
    if !items.is_empty() {
        match Walker::new(WalkOptions::from_settings(settings)) {
            Ok(walker) => {
                let namer = names::Namer::build(settings, &walker, ctx);
                names::annotate_startup(&mut items, &namer);
            }
            Err(err) => tracing::warn!(%err, "startup items keep their raw names"),
        }
    }
    items.sort_by_cached_key(|s| s.item.name.to_lowercase());
    for (i, item) in items.iter_mut().enumerate() {
        item.item.id = ItemId::try_from(i).unwrap_or(ItemId::MAX);
    }
    Ok(items)
}

/// Enables, disables or removes a startup item. Returns its new state (`None` once
/// removed).
pub fn change_startup(
    record: &StartupRecord,
    change: StartupChange,
    settings: &CleanSettings,
    ctx: &JobCtx,
) -> Result<Option<StartupItem>> {
    platform::change_startup(record, change, settings, ctx)
}

/// OS, permissions and volumes.
pub fn system_info() -> SystemInfo {
    platform::system_info()
}

/// Removes `targets`: permanently or to the Trash, special actions and registry entries
/// through the platform, items that need administrator rights through one elevated helper
/// run (when `settings.elevate`). Never fails as a whole; per-item problems are in the
/// report.
pub fn remove(targets: &[Target], settings: &CleanSettings, ctx: &JobCtx) -> CleanReport {
    remove::execute(targets, settings, ctx)
}

/// Body of the daemon's `elevated <manifest>` subcommand: runs as administrator/root,
/// removes what the manifest lists (after the same guard checks), and writes the result
/// next to it.
pub fn serve_elevated(manifest: &Path) -> Result<()> {
    elevate::serve(manifest)
}
