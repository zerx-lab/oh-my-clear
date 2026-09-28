//! The removal executor every clean and uninstall goes through.
//!
//! Targets are removed in order: paths through [`omc_scan::delete`] (guarded, parallel,
//! Trash-aware), registry entries and special actions through the platform module.
//! Targets that need administrator rights, and targets that failed locally for lack of
//! them (`permission_denied`, `app_management`), are collected and — when
//! `settings.elevate` — sent to ONE elevated helper run, so the user sees a single
//! password prompt per clean. The helper runs [`execute_local`] (never elevating again)
//! and its results are merged into the report.

use std::path::{Path, PathBuf};

use omc_proto::jobs::{CleanReport, FailReason, Failure, Location, Phase};
use omc_proto::settings::CleanSettings;
use omc_scan::delete::{self, TrashBin};
use omc_scan::{Guard, JobCtx, Target};
use serde::{Deserialize, Serialize};

use crate::{Error, elevate, platform};

/// What removing one target did (also the elevated helper's per-item result).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TargetOutcome {
    /// Bytes freed (or moved to the Trash).
    pub(crate) freed: u64,
    /// What could not be removed; empty = fully removed.
    pub(crate) failures: Vec<Failure>,
}

impl TargetOutcome {
    /// Administrator rights could fix (part of) it.
    fn wants_elevation(&self) -> bool {
        self.failures.iter().any(|f| {
            matches!(
                f.reason,
                FailReason::PermissionDenied | FailReason::AppManagement
            )
        })
    }
}

/// Removes `targets`, elevating once for those that need it. See the module docs.
pub(crate) fn execute(targets: &[Target], settings: &CleanSettings, ctx: &JobCtx) -> CleanReport {
    ctx.set_phase(Phase::Removing);
    ctx.set_total(u64::try_from(targets.len()).unwrap_or(u64::MAX));
    let guard = Guard::new(settings);
    let backup = backup_dir(settings);
    let mut elevated: Option<bool> = None;
    let mut outcomes: Vec<(&Target, TargetOutcome)> = Vec::with_capacity(targets.len());
    // Indices into `outcomes` of targets for the elevated helper.
    let mut deferred: Vec<usize> = Vec::new();
    for target in targets {
        if ctx.is_cancelled() {
            break;
        }
        let can_elevate = |elevated: &mut Option<bool>| {
            settings.elevate && !*elevated.get_or_insert_with(|| platform::system_info().elevated)
        };
        if target.needs_admin && can_elevate(&mut elevated) {
            deferred.push(outcomes.len());
            outcomes.push((target, TargetOutcome::default()));
            continue;
        }
        let outcome = run_one(target, &guard, ctx, &TrashBin::System, backup.as_deref());
        if outcome.wants_elevation() && can_elevate(&mut elevated) {
            deferred.push(outcomes.len());
        } else {
            ctx.add_done(1);
        }
        outcomes.push((target, outcome));
    }
    if !deferred.is_empty() && !ctx.is_cancelled() {
        elevate_deferred(&mut outcomes, &deferred, settings, ctx);
        ctx.set_phase(Phase::Removing);
    }
    report(outcomes.into_iter().map(|(_, outcome)| outcome))
}

/// Runs the deferred targets in one elevated helper and merges the results.
fn elevate_deferred(
    outcomes: &mut [(&Target, TargetOutcome)],
    deferred: &[usize],
    settings: &CleanSettings,
    ctx: &JobCtx,
) {
    let batch: Vec<Target> = deferred
        .iter()
        .filter_map(|i| outcomes.get(*i).map(|(t, _)| (*t).clone()))
        .collect();
    let result = elevate::run(&batch, settings, ctx);
    for (n, i) in deferred.iter().enumerate() {
        let Some((target, outcome)) = outcomes.get_mut(*i) else {
            continue;
        };
        match &result {
            Ok(helper) => {
                let Some(helper) = helper.get(n) else {
                    continue;
                };
                ctx.add_bytes(helper.freed);
                outcome.freed = outcome.freed.saturating_add(helper.freed);
                outcome.failures.clone_from(&helper.failures);
            }
            Err(failed) => {
                // A declined prompt replaces the local reasons; otherwise the local ones
                // (e.g. App Management, which the UI explains) stay the most precise.
                if failed.reason == FailReason::ElevationCancelled || outcome.failures.is_empty() {
                    outcome.failures = vec![Failure {
                        location: target.location.clone(),
                        reason: failed.reason,
                        message: failed.message.clone(),
                    }];
                }
            }
        }
        ctx.add_done(1);
    }
}

/// Removes every target without elevating (the elevated helper's body). One outcome per
/// target, in order.
pub(crate) fn execute_local(
    targets: &[Target],
    settings: &CleanSettings,
    ctx: &JobCtx,
    bin: &TrashBin,
) -> Vec<TargetOutcome> {
    ctx.set_phase(Phase::Removing);
    ctx.set_total(u64::try_from(targets.len()).unwrap_or(u64::MAX));
    let guard = Guard::new(settings);
    let backup = backup_dir(settings);
    targets
        .iter()
        .map(|target| {
            let outcome = run_one(target, &guard, ctx, bin, backup.as_deref());
            ctx.add_done(1);
            outcome
        })
        .collect()
}

fn run_one(
    target: &Target,
    guard: &Guard,
    ctx: &JobCtx,
    bin: &TrashBin,
    backup: Option<&Path>,
) -> TargetOutcome {
    let result = match &target.location {
        Location::Path { .. } => {
            let removal = delete::remove_with(target, guard, ctx, bin);
            return TargetOutcome {
                freed: removal.freed,
                failures: removal
                    .failures
                    .into_iter()
                    .map(|f| Failure {
                        location: Location::Path {
                            path: f.path.display().to_string(),
                        },
                        reason: f.reason,
                        message: f.message,
                    })
                    .collect(),
            };
        }
        location @ (Location::RegistryKey { .. } | Location::RegistryValue { .. }) => {
            ctx.set_current(location.display());
            platform::remove_registry(location, backup)
        }
        Location::Special { action } => {
            ctx.set_current(target.location.display());
            platform::run_special(action)
        }
    };
    match result {
        Ok(()) => TargetOutcome {
            freed: target.bytes,
            failures: Vec::new(),
        },
        Err(err) => TargetOutcome {
            freed: 0,
            failures: vec![Failure {
                location: target.location.clone(),
                reason: reason_of(&err, &target.location),
                message: err.to_string(),
            }],
        },
    }
}

fn report(outcomes: impl Iterator<Item = TargetOutcome>) -> CleanReport {
    let mut report = CleanReport::default();
    for outcome in outcomes {
        if outcome.failures.is_empty() {
            report.removed = report.removed.saturating_add(1);
        }
        report.freed = report.freed.saturating_add(outcome.freed);
        report.failures.extend(outcome.failures);
    }
    report
}

/// Failure class of a platform (registry, special action) error.
fn reason_of(err: &Error, location: &Location) -> FailReason {
    match err {
        Error::Io(io) => {
            omc_scan::errors::classify(io, Path::new(location.as_path().unwrap_or("")))
        }
        Error::Elevation(_) => FailReason::PermissionDenied,
        Error::Command { message, .. } => {
            let message = message.to_ascii_lowercase();
            if [
                "access is denied",
                "permission denied",
                "not permitted",
                "requires elevation",
                "administrator",
            ]
            .iter()
            .any(|needle| message.contains(needle))
            {
                FailReason::PermissionDenied
            } else {
                FailReason::Other
            }
        }
        _ => FailReason::Other,
    }
}

/// Windows: `%LOCALAPPDATA%\oh-my-clear\registry-backups\<unix-ts>` when registry backups
/// are on; unused elsewhere.
fn backup_dir(settings: &CleanSettings) -> Option<PathBuf> {
    if !settings.backup_registry || !cfg!(windows) {
        return None;
    }
    let data = omc_scan::paths::env_dir("LOCALAPPDATA")?;
    Some(
        data.join("oh-my-clear")
            .join("registry-backups")
            .join(omc_scan::paths::now_secs().to_string()),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use omc_proto::jobs::DeleteMethod;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir =
            std::env::temp_dir().join(format!("omc-remove-{name}-{}-{nanos}", std::process::id()));
        assert!(fs::create_dir_all(&dir).is_ok(), "create temp dir");
        dir
    }

    #[test]
    fn report_counts_successes_bytes_and_failures() {
        let dir = temp_dir("report");
        let file = dir.join("a.bin");
        assert!(fs::write(&file, vec![1u8; 8192]).is_ok(), "write");
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        let targets = [
            Target::path(file.display().to_string(), 8192, DeleteMethod::Permanent),
            Target::path(
                dir.join("missing").display().to_string(),
                0,
                DeleteMethod::Permanent,
            ),
            Target::path(root, 0, DeleteMethod::Permanent),
        ];
        let settings = CleanSettings {
            elevate: false,
            ..CleanSettings::default()
        };
        let ctx = JobCtx::new();
        let report = execute(&targets, &settings, &ctx);
        assert_eq!(report.removed, 2, "file and missing path count as removed");
        assert!(report.freed >= 8192, "freed bytes: {}", report.freed);
        assert_eq!(
            report.failures.iter().map(|f| f.reason).collect::<Vec<_>>(),
            vec![FailReason::Protected],
            "the root is refused"
        );
        assert!(!file.exists(), "file removed");
        let progress = ctx.snapshot();
        assert_eq!(
            (progress.done, progress.total),
            (3, 3),
            "every target counted"
        );
        assert_eq!(progress.phase, Phase::Removing, "phase");
        if let Err(err) = fs::remove_dir_all(&dir) {
            tracing::debug!(%err, "temp cleanup");
        }
    }

    #[test]
    fn elevation_wanted_only_for_permission_failures() {
        let failure = |reason| Failure {
            location: Location::Path {
                path: "/x".to_owned(),
            },
            reason,
            message: String::new(),
        };
        let outcome = |reasons: &[FailReason]| TargetOutcome {
            freed: 0,
            failures: reasons.iter().map(|r| failure(*r)).collect(),
        };
        assert!(
            outcome(&[FailReason::InUse, FailReason::PermissionDenied]).wants_elevation(),
            "one denied entry is enough"
        );
        assert!(
            outcome(&[FailReason::AppManagement]).wants_elevation(),
            "app management"
        );
        assert!(
            !outcome(&[FailReason::FullDiskAccess, FailReason::Protected]).wants_elevation(),
            "root cannot fix privacy or guard refusals"
        );
        assert!(!outcome(&[]).wants_elevation(), "success");
    }
}
