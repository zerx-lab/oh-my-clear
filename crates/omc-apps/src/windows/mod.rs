//! Windows: programs from the `Uninstall` registry keys and Store packages, their files,
//! shortcuts, registry keys, services and scheduled tasks; vendor uninstallers (MSI and
//! EXE, including self-copying NSIS/Inno uninstallers); leftovers; `Run` keys, Startup
//! folders, logon tasks and auto-start services; registry removal with `.reg` backups.
//!
//! Registry access goes through the safe `windows-registry` crate; everything else runs
//! helper programs (`reg`, `sc`, `schtasks`, `taskkill`, `msiexec`, `powershell`,
//! `whoami`) through [`crate::cmd`] without a console window.

use std::path::Path;

use omc_proto::apps::{StartupChange, StartupItem, UninstallerOutcome};
use omc_proto::jobs::{Location, SpecialAction};
use omc_proto::junk::JunkReport;
use omc_proto::settings::{CleanSettings, SystemInfo};
use omc_scan::{JobCtx, Scanned, Walker};

use crate::{AppFiles, AppRecord, Result, StartupRecord};

mod apps;
mod clues;
mod files;
mod known;
mod leftovers;
mod parse;
mod reg;
mod related;
mod startup;
mod system;
mod tasks;
mod vendor;

use parse::Hive;

/// Platform data behind an [`AppRecord`].
#[derive(Debug, Clone, Default)]
pub(crate) struct AppDetail {
    /// `Uninstall` keys (hive, sub path) merged into this app.
    keys: Vec<(Hive, String)>,
    /// `UninstallString`.
    uninstall: Option<String>,
    /// `QuietUninstallString`.
    quiet_uninstall: Option<String>,
    /// Windows Installer product code.
    msi_code: Option<String>,
    /// Usable install folder (never a shared/system folder).
    location: Option<String>,
    /// Program named by `DisplayIcon`.
    icon_exe: Option<String>,
    /// Lower-case program file names (running detection, quitting).
    exes: Vec<String>,
    /// Store package.
    package: Option<Package>,
}

/// A Store / MSIX package.
#[derive(Debug, Clone)]
struct Package {
    /// For `Remove-AppxPackage`.
    full_name: String,
    /// `%LOCALAPPDATA%\Packages\<family>`.
    family_name: Option<String>,
    /// `WindowsApps` folder.
    location: Option<String>,
}

/// Platform data behind a [`StartupRecord`].
#[derive(Debug, Clone)]
pub(crate) enum StartupDetail {
    /// A `Run` registry value.
    Run {
        hive: Hive,
        key: String,
        value: String,
        /// `StartupApproved` key holding its enabled state.
        approved: String,
    },
    /// A file in a Startup folder.
    Folder {
        hive: Hive,
        file: String,
        approved: String,
    },
    /// A scheduled task.
    Task { path: String },
    /// An auto-start service.
    Service { name: String },
}

/// Extra names of `app` besides its display name: Electron `productName`/`name` and the
/// program names (without `.exe`). Reads the Electron manifest only (no walks).
pub(crate) fn app_aliases(app: &AppRecord) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(dir) = app.detail.location.as_deref() {
        for name in related::electron_manifest_names(dir) {
            push_alias(&mut out, &name);
        }
    }
    for exe in &app.detail.exes {
        let stem = exe.strip_suffix(".exe").unwrap_or(exe);
        push_alias(&mut out, stem);
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

pub(crate) fn list_apps(
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Vec<AppRecord>> {
    apps::list(settings, walker, ctx)
}

pub(crate) fn app_files(
    app: &AppRecord,
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<AppFiles> {
    files::collect(app, settings, walker, ctx)
}

/// Discovery again with the identifiers of `before` after the vendor uninstaller ran:
/// only items that exist now and were not offered before (ids renumbered from 0), so the
/// orchestrator can offer leftovers the uninstaller left or revealed.
#[expect(
    dead_code,
    reason = "platform hook for the post-uninstall rescan; the orchestrator in uninstall.rs wires it"
)]
pub(crate) fn rescan_app_files(
    before: &AppFiles,
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<AppFiles> {
    files::rescan(before, settings, walker, ctx)
}

/// Quits the app gracefully, then forcibly; `Ok` when it is not running.
pub(crate) fn quit_app(app: &AppRecord, ctx: &JobCtx) -> Result<()> {
    vendor::quit(app, ctx)
}

/// Creates a System Restore point when enabled (failures are logged, never fatal).
#[expect(
    clippy::unnecessary_wraps,
    reason = "signature shared by every platform module"
)]
pub(crate) fn before_uninstall(
    app: &AppRecord,
    settings: &CleanSettings,
    ctx: &JobCtx,
) -> Result<()> {
    vendor::restore_point(app, settings, ctx);
    Ok(())
}

/// Runs the app's own uninstaller (MSI, vendor EXE, `Remove-AppxPackage`) and waits for it
/// and the helper processes it spawns.
pub(crate) fn run_uninstaller(
    files: &AppFiles,
    settings: &CleanSettings,
    ctx: &JobCtx,
) -> UninstallerOutcome {
    vendor::run(files, settings, ctx)
}

pub(crate) fn leftovers(
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Scanned<JunkReport>> {
    leftovers::scan(settings, walker, ctx)
}

pub(crate) fn list_startup(settings: &CleanSettings, ctx: &JobCtx) -> Result<Vec<StartupRecord>> {
    startup::list(settings, ctx)
}

pub(crate) fn change_startup(
    record: &StartupRecord,
    change: StartupChange,
    settings: &CleanSettings,
    ctx: &JobCtx,
) -> Result<Option<StartupItem>> {
    startup::change(record, change, settings, ctx)
}

pub(crate) fn system_info() -> SystemInfo {
    system::info()
}

/// Performs a [`Location::Special`] action (non-elevated).
pub(crate) fn run_special(action: &SpecialAction) -> Result<()> {
    system::special(action)
}

/// Deletes a registry key/value; `backup_dir` receives a `.reg` export first.
pub(crate) fn remove_registry(location: &Location, backup_dir: Option<&Path>) -> Result<()> {
    reg::remove(location, backup_dir)
}
