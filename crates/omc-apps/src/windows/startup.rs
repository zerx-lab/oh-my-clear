//! Startup items: `Run` values, Startup folders, logon/boot scheduled tasks and
//! auto-start services. Enabled state of `Run` values and Startup-folder files lives in
//! `Explorer\StartupApproved` exactly as Task Manager keeps it.

use std::time::Duration;

use omc_proto::apps::{Scope, StartupChange, StartupItem, StartupKind};
use omc_proto::jobs::{Location, Phase};
use omc_proto::settings::CleanSettings;
use omc_scan::JobCtx;

use super::parse::{self, Hive};
use super::{StartupDetail, files, known, reg, system, tasks};
use crate::{Error, Result, StartupRecord, cmd};

/// `StartupApproved\StartupFolder` (under HKCU for the user folder, HKLM for the common one).
const APPROVED_FOLDER: &str =
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\StartupFolder";
const SERVICES: &str = "SYSTEM\\CurrentControlSet\\Services";

pub(crate) fn list(_settings: &CleanSettings, ctx: &JobCtx) -> Result<Vec<StartupRecord>> {
    ctx.set_phase(Phase::Scanning);
    let mut out = Vec::new();
    run_values(&mut out);
    startup_folders(&mut out);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    scheduled_tasks(&mut out);
    services(&mut out);
    ctx.add_items(u64::try_from(out.len()).unwrap_or(u64::MAX));
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    Ok(out)
}

fn record(item: StartupItem, detail: StartupDetail) -> StartupRecord {
    StartupRecord { item, detail }
}

fn scope_of(hive: Hive) -> Scope {
    if hive == Hive::CurrentUser {
        Scope::User
    } else {
        Scope::System
    }
}

fn run_values(out: &mut Vec<StartupRecord>) {
    for (hive, base, approved) in files::run_keys() {
        let path = format!("{base}\\Run");
        let Some(key) = reg::open(hive, &path) else {
            continue;
        };
        let state = reg::open(hive, approved);
        for (name, data) in reg::string_values(&key) {
            let enabled = parse::approved_enabled(
                state.as_ref().and_then(|s| reg::bytes(s, &name)).as_deref(),
            );
            let item = StartupItem {
                id: 0,
                name: if name.is_empty() {
                    "(default)".to_owned()
                } else {
                    name.clone()
                },
                command: Some(data.clone()),
                location: Location::RegistryValue {
                    key: hive.full(&path),
                    name: name.clone(),
                },
                kind: StartupKind::RunKey,
                scope: scope_of(hive),
                enabled,
                needs_admin: hive != Hive::CurrentUser,
                publisher: None,
                missing_target: known::command_missing(&data),
                ident: None,
                icon: None,
            };
            let detail = StartupDetail::Run {
                hive,
                key: path.clone(),
                value: name,
                approved: approved.to_owned(),
            };
            out.push(record(item, detail));
        }
    }
}

fn startup_folders(out: &mut Vec<StartupRecord>) {
    let folders = [
        (
            Hive::CurrentUser,
            known::env_join(
                "APPDATA",
                "Microsoft\\Windows\\Start Menu\\Programs\\Startup",
            ),
        ),
        (
            Hive::LocalMachine,
            known::env_join(
                "ProgramData",
                "Microsoft\\Windows\\Start Menu\\Programs\\StartUp",
            ),
        ),
    ];
    for (hive, dir) in folders {
        let Some(dir) = dir else { continue };
        let state = reg::open(hive, APPROVED_FOLDER);
        for (name, path, is_dir) in known::entries(&dir) {
            if is_dir || name.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            let is_link = name.to_ascii_lowercase().ends_with(".lnk");
            let target = if is_link {
                known::read_small(&path).and_then(|bytes| parse::lnk_target(&bytes))
            } else {
                None
            };
            let enabled = parse::approved_enabled(
                state.as_ref().and_then(|s| reg::bytes(s, &name)).as_deref(),
            );
            let display = if is_link {
                name.get(..name.len().saturating_sub(4))
                    .unwrap_or(&name)
                    .to_owned()
            } else {
                name.clone()
            };
            let item = StartupItem {
                id: 0,
                name: display,
                command: Some(target.clone().unwrap_or_else(|| path.clone())),
                location: Location::Path { path: path.clone() },
                kind: StartupKind::StartupFolder,
                scope: scope_of(hive),
                enabled,
                needs_admin: hive != Hive::CurrentUser,
                publisher: None,
                missing_target: target.as_deref().is_some_and(known::target_missing),
                ident: None,
                icon: None,
            };
            let detail = StartupDetail::Folder {
                hive,
                file: path,
                approved: APPROVED_FOLDER.to_owned(),
            };
            out.push(record(item, detail));
        }
    }
}

fn scheduled_tasks(out: &mut Vec<StartupRecord>) {
    let list = match tasks::list() {
        Ok(list) => list,
        Err(err) => {
            tracing::warn!(%err, "listing scheduled tasks failed");
            return;
        }
    };
    let task_dir = format!("{}\\System32\\Tasks", known::windir());
    for task in list.iter().filter(|t| !t.is_microsoft() && t.at_startup()) {
        let mine = tasks::runs_as_current_user(task);
        let item = StartupItem {
            id: 0,
            name: task.name().to_owned(),
            command: (!task.exec.is_empty()).then(|| task.exec.join(" | ")),
            location: Location::Path {
                path: format!("{task_dir}{}", task.path),
            },
            kind: StartupKind::ScheduledTask,
            scope: if mine { Scope::User } else { Scope::System },
            enabled: task.enabled(),
            needs_admin: !mine,
            publisher: None,
            missing_target: task.exec.first().is_some_and(|e| known::command_missing(e)),
            ident: None,
            icon: None,
        };
        out.push(record(
            item,
            StartupDetail::Task {
                path: task.path.clone(),
            },
        ));
    }
}

/// Paths of Microsoft/Windows services outside `%WINDIR%`.
fn is_microsoft_path(exe: &str) -> bool {
    let lower = exe.to_ascii_lowercase();
    lower.contains("\\microsoft")
        || lower.contains("\\windows defender")
        || lower.contains("\\windows ")
}

fn services(out: &mut Vec<StartupRecord>) {
    const AUTO_START: u32 = 2;
    const WIN32_SERVICE: u32 = 0x30;
    let Some(key) = reg::open(Hive::LocalMachine, SERVICES) else {
        return;
    };
    let windir = known::windir();
    for name in reg::subkeys(&key) {
        let path = format!("{SERVICES}\\{name}");
        let Some(service) = reg::open(Hive::LocalMachine, &path) else {
            continue;
        };
        if reg::dword(&service, "Start") != Some(AUTO_START)
            || reg::dword(&service, "Type").is_none_or(|t| t & WIN32_SERVICE == 0)
        {
            continue;
        }
        let Some(image) = reg::string(&service, "ImagePath") else {
            continue;
        };
        let expanded = reg::expand(&image);
        let Some(exe) = parse::service_image(&expanded, &windir, known::is_file) else {
            continue;
        };
        if parse::path_within(&exe, &windir) || is_microsoft_path(&exe) {
            continue;
        }
        let display = reg::string(&service, "DisplayName")
            .filter(|d| !d.starts_with('@'))
            .unwrap_or_else(|| name.clone());
        let item = StartupItem {
            id: 0,
            name: display,
            command: Some(image),
            location: Location::RegistryKey {
                key: Hive::LocalMachine.full(&path),
            },
            kind: StartupKind::Service,
            scope: Scope::System,
            enabled: true,
            needs_admin: true,
            publisher: None,
            missing_target: known::target_missing(&exe),
            ident: None,
            icon: None,
        };
        out.push(record(item, StartupDetail::Service { name }));
    }
}

/// Access denied → [`Error::Elevation`].
fn elevation(err: Error) -> Error {
    match &err {
        Error::Io(io)
            if io.raw_os_error() == Some(5)
                || io.kind() == std::io::ErrorKind::PermissionDenied =>
        {
            Error::Elevation("administrator rights are required to change this item".to_owned())
        }
        _ => err,
    }
}

fn approved_now(enabled: bool) -> [u8; 12] {
    parse::approved_value(enabled, parse::filetime(omc_scan::paths::now_secs()))
}

pub(crate) fn change(
    record: &StartupRecord,
    change: StartupChange,
    _settings: &CleanSettings,
    ctx: &JobCtx,
) -> Result<Option<StartupItem>> {
    ctx.set_phase(Phase::Removing);
    let updated = |enabled: bool| {
        Some(StartupItem {
            enabled,
            ..record.item.clone()
        })
    };
    match (&record.detail, change) {
        (
            StartupDetail::Run {
                hive,
                approved,
                value,
                ..
            },
            StartupChange::Enable | StartupChange::Disable,
        ) => {
            let enable = change == StartupChange::Enable;
            reg::set_binary(*hive, approved, value, &approved_now(enable)).map_err(elevation)?;
            Ok(updated(enable))
        }
        (
            StartupDetail::Run {
                hive,
                key,
                value,
                approved,
            },
            StartupChange::Remove,
        ) => {
            reg::delete_value(*hive, key, value).map_err(elevation)?;
            if let Err(err) = reg::delete_value(*hive, approved, value) {
                tracing::debug!(%err, "removing StartupApproved value");
            }
            Ok(None)
        }
        (
            StartupDetail::Folder {
                hive,
                file,
                approved,
            },
            StartupChange::Enable | StartupChange::Disable,
        ) => {
            let enable = change == StartupChange::Enable;
            reg::set_binary(
                *hive,
                approved,
                parse::file_name(file),
                &approved_now(enable),
            )
            .map_err(elevation)?;
            Ok(updated(enable))
        }
        (
            StartupDetail::Folder {
                hive,
                file,
                approved,
            },
            StartupChange::Remove,
        ) => {
            match std::fs::remove_file(file) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(elevation(Error::Io(err))),
            }
            if let Err(err) = reg::delete_value(*hive, approved, parse::file_name(file)) {
                tracing::debug!(%err, "removing StartupApproved value");
            }
            Ok(None)
        }
        (StartupDetail::Task { path }, StartupChange::Enable | StartupChange::Disable) => {
            let enable = change == StartupChange::Enable;
            let flag = if enable { "/ENABLE" } else { "/DISABLE" };
            tasks::schtasks(&["/Change", "/TN", path, flag]).map_err(elevation)?;
            Ok(updated(enable))
        }
        (StartupDetail::Task { path }, StartupChange::Remove) => {
            if tasks::exists(path) {
                tasks::schtasks(&["/Delete", "/TN", path, "/F"]).map_err(elevation)?;
            }
            Ok(None)
        }
        (StartupDetail::Service { .. }, StartupChange::Remove) => Err(Error::Unsupported(
            "services can only be disabled; uninstall the program that owns them".to_owned(),
        )),
        (StartupDetail::Service { name }, StartupChange::Enable | StartupChange::Disable) => {
            let enable = change == StartupChange::Enable;
            set_service_start(name, enable)?;
            Ok(updated(enable))
        }
    }
}

/// `sc config <name> start= auto|demand` (administrator only).
fn set_service_start(name: &str, enable: bool) -> Result<()> {
    if !system::is_elevated() {
        return Err(Error::Elevation(
            "administrator rights are required to change a service".to_owned(),
        ));
    }
    let mode = if enable { "auto" } else { "demand" };
    let out = cmd::run(
        "sc.exe",
        &["config", name, "start=", mode],
        Duration::from_secs(60),
    )?;
    match out.status.code() {
        Some(0) => Ok(()),
        Some(5) => Err(elevation(Error::Io(std::io::Error::from_raw_os_error(5)))),
        code => Err(Error::Command {
            program: "sc config".to_owned(),
            message: format!("exit {code:?}: {}", out.stdout.trim()),
        }),
    }
}
