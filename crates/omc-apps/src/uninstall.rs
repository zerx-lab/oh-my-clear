//! Uninstall orchestration: quit the app, prepare (restore point), run its own
//! uninstaller when asked, then remove the selected related items that still exist
//! through the removal executor (one elevation prompt for everything that needs it).

use std::collections::BTreeSet;

use omc_proto::apps::{UninstallReport, UninstallerOutcome};
use omc_proto::jobs::{ItemId, Location, Phase};
use omc_proto::settings::CleanSettings;
use omc_scan::{JobCtx, Target, paths, procs};

use crate::{AppFiles, AppRecord, Error, Result, platform, remove};

pub(crate) fn run(
    files: &AppFiles,
    items: &[ItemId],
    run_uninstaller: bool,
    settings: &CleanSettings,
    ctx: &JobCtx,
) -> Result<UninstallReport> {
    let app = &files.app;
    if settings.quit_running_apps && running(app).unwrap_or(app.info.running) {
        ctx.set_phase(Phase::Quitting);
        ctx.set_current(app.info.name.clone());
        if let Err(err) = platform::quit_app(app, ctx) {
            if running(app).unwrap_or(true) {
                return Err(err);
            }
            tracing::warn!(%err, app = %app.info.name, "quitting reported an error, but the app is gone");
        }
    }
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    match platform::before_uninstall(app, settings, ctx) {
        Ok(()) => {}
        Err(Error::Cancelled) => return Err(Error::Cancelled),
        Err(err) => tracing::warn!(%err, app = %app.info.name, "preparing the uninstall"),
    }
    let mut report = UninstallReport::default();
    if run_uninstaller && settings.run_vendor_uninstaller {
        ctx.set_phase(Phase::Uninstalling);
        report.uninstaller = platform::run_uninstaller(files, settings, ctx);
        if report.uninstaller == UninstallerOutcome::Cancelled {
            return Ok(report);
        }
    }
    if ctx.is_cancelled() {
        return Ok(report);
    }
    ctx.set_phase(Phase::Removing);
    let targets = still_present(&files.targets, items);
    report.clean = remove::execute(&targets, settings, ctx);
    Ok(report)
}

/// Whether a process of `app` runs now: its executable lies inside the app's location.
/// `None` when that cannot be told (no location, or the OS hides executable paths).
fn running(app: &AppRecord) -> Option<bool> {
    let location = app.info.location.as_deref()?;
    let processes = procs::running();
    if processes.iter().all(|p| p.exe.is_none()) {
        return None;
    }
    let location = std::path::Path::new(location);
    Some(
        processes
            .iter()
            .filter_map(|p| p.exe.as_deref())
            .any(|exe| paths::is_within(exe, location)),
    )
}

/// The targets of `items` (unknown ids ignored, duplicates once) in report order, minus
/// paths the vendor uninstaller already removed.
fn still_present(targets: &[Target], items: &[ItemId]) -> Vec<Target> {
    let wanted: BTreeSet<usize> = items
        .iter()
        .filter_map(|id| usize::try_from(*id).ok())
        .collect();
    targets
        .iter()
        .enumerate()
        .filter(|(i, _)| wanted.contains(i))
        .map(|(_, t)| t)
        .filter(|t| match &t.location {
            Location::Path { path } => std::fs::symlink_metadata(path).is_ok(),
            _ => true,
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use omc_proto::jobs::DeleteMethod;

    use super::*;

    #[test]
    fn selection_skips_unknown_duplicate_and_missing() {
        let existing = std::env::temp_dir();
        let missing = existing.join(format!("omc-uninstall-missing-{}", std::process::id()));
        let targets = vec![
            Target::path(existing.display().to_string(), 1, DeleteMethod::Trash),
            Target::path(missing.display().to_string(), 2, DeleteMethod::Trash),
            Target::location(
                Location::RegistryKey {
                    key: "HKCU\\Software\\X".to_owned(),
                },
                0,
            ),
        ];
        let selected = still_present(&targets, &[2, 0, 1, 0, 99]);
        assert_eq!(
            selected.iter().map(|t| t.bytes).collect::<Vec<_>>(),
            vec![1, 0],
            "existing path, then the registry key, once each; missing path and unknown id dropped"
        );
    }
}
