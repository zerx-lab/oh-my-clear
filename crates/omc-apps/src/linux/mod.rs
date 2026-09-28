//! Linux (and every other Unix except macOS) platform module.
//!
//! Apps come from the distribution package manager (dpkg, rpm, pacman: only packages that
//! ship a visible `.desktop` application), Flatpak, Snap, `AppImage` files, Wine menu
//! entries and user desktop entries. Package-owned files are removed by the package
//! manager (`run_uninstaller`);
//! related items are only user data outside the package database. Startup items are XDG
//! autostart entries and enabled systemd user units.

use std::path::{Path, PathBuf};

use omc_proto::apps::{StartupChange, StartupItem, UninstallerOutcome};
use omc_proto::jobs::{Location, SpecialAction};
use omc_proto::junk::JunkReport;
use omc_proto::settings::{CleanSettings, SystemInfo};
use omc_scan::{JobCtx, Scanned, Walker};

use crate::{AppFiles, AppRecord, Error, Result, StartupRecord};

mod common;
mod desktop;
mod electron;
mod evidence;
mod files;
mod inventory;
mod leftovers;
mod pkg;
mod startup;
mod system;
mod uninstall;
mod wine;

use desktop::DesktopEntry;

/// What the Linux module needs to act on an app.
#[derive(Debug, Clone, Default)]
pub(crate) struct AppDetail {
    /// Package name (deb/rpm/pacman), Flatpak app id or snap name.
    pub(super) package: Option<String>,
    /// Flatpak installed per user (else system-wide).
    pub(super) flatpak_user: bool,
    /// The app's own desktop entries.
    pub(super) entries: Vec<DesktopEntry>,
    /// Programs its entries start (resolved, launchers such as `sh`/`python` excluded).
    pub(super) binaries: Vec<PathBuf>,
    /// The `AppImage` file.
    pub(super) appimage: Option<PathBuf>,
    /// Wine prefix, install folder and menu folder of a Windows program.
    pub(super) wine: Option<wine::WineDetail>,
}

/// What the Linux module needs to change a startup item.
#[derive(Debug, Clone)]
pub(crate) enum StartupDetail {
    /// XDG autostart entry: the user file (override or own entry) and/or the system one.
    Xdg {
        /// `~/.config/autostart/<file>`.
        user: PathBuf,
        /// `/etc/xdg/autostart/<file>`, when present.
        system: Option<PathBuf>,
    },
    /// systemd user unit.
    Systemd {
        /// Unit name (`foo.service`).
        unit: String,
        /// Its unit file, when found.
        file: Option<PathBuf>,
    },
}

/// Extra names of `app` besides its display name: desktop-entry ids, `StartupWMClass`
/// and the Electron `productName`/`name` (read beside its binaries, no walks).
pub(crate) fn app_aliases(app: &AppRecord) -> Vec<String> {
    let mut out = Vec::new();
    for entry in &app.detail.entries {
        push_alias(&mut out, &entry.id);
        if let Some(class) = &entry.wm_class {
            push_alias(&mut out, class);
        }
    }
    if let Some(names) = electron::electron_names(&app.detail.binaries, Vec::new) {
        for name in [names.product, names.name].into_iter().flatten() {
            push_alias(&mut out, &name);
        }
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

/// Quits the app gracefully (`SIGTERM`), then forcibly after 5 s; `Ok` when it is not
/// running.
pub(crate) fn quit_app(app: &AppRecord, ctx: &JobCtx) -> Result<()> {
    uninstall::quit_app(app, ctx)
}

/// Nothing to prepare on Linux.
#[expect(
    clippy::unnecessary_wraps,
    reason = "same signature on every platform; Windows creates a restore point here"
)]
pub(crate) fn before_uninstall(_: &AppRecord, _: &CleanSettings, _: &JobCtx) -> Result<()> {
    Ok(())
}

/// Runs the package manager (through `pkexec` when needed) and waits for it.
pub(crate) fn run_uninstaller(
    files: &AppFiles,
    settings: &CleanSettings,
    ctx: &JobCtx,
) -> UninstallerOutcome {
    uninstall::run_uninstaller(files, settings, ctx)
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

/// Performs a [`Location::Special`] action (non-elevated): only emptying the Trash exists
/// on Linux.
pub(crate) fn run_special(action: &SpecialAction) -> Result<()> {
    match action {
        SpecialAction::EmptyTrash => system::empty_trash(),
        other => Err(Error::Unsupported(format!("{other:?} on Linux"))),
    }
}

/// Registry entries exist only on Windows.
pub(crate) fn remove_registry(location: &Location, _backup_dir: Option<&Path>) -> Result<()> {
    Err(Error::Unsupported(format!(
        "registry removal on Linux: {}",
        location.display()
    )))
}
