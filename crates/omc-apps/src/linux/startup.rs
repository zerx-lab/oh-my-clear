//! Startup items: XDG autostart entries (user entries override system ones of the same
//! file name) and systemd user units. System entries are never edited: disabling one writes
//! a user override with `Hidden=true`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use omc_proto::apps::{Scope, StartupChange, StartupItem, StartupKind};
use omc_proto::jobs::{Location, Phase};
use omc_proto::settings::CleanSettings;
use omc_scan::JobCtx;

use super::StartupDetail;
use super::common::{self, QUICK_TIMEOUT};
use super::desktop::{self, DesktopEntry};
use super::files;
use crate::{Error, Result, StartupRecord, cmd};

/// System autostart directories (`$XDG_CONFIG_DIRS`/autostart).
fn system_autostart_dirs() -> Vec<PathBuf> {
    std::env::var("XDG_CONFIG_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/etc/xdg".to_owned())
        .split(':')
        .filter(|d| d.starts_with('/'))
        .map(|d| Path::new(d).join("autostart"))
        .collect()
}

fn user_autostart_dir() -> Result<PathBuf> {
    common::config_home()
        .map(|c| c.join("autostart"))
        .ok_or_else(|| Error::Unsupported("no home folder".to_owned()))
}

fn systemd_user_dir() -> Option<PathBuf> {
    common::config_home().map(|c| c.join("systemd/user"))
}

/// The wire item of an autostart entry from its user and/or system file.
fn xdg_item(user: &Path, system: Option<&Path>, locales: &[String]) -> Option<StartupRecord> {
    let user_exists = user.is_file();
    let effective = if user_exists { Some(user) } else { system }?;
    let entry = DesktopEntry::load(effective, locales)?;
    let id = entry.id.clone();
    Some(StartupRecord {
        item: StartupItem {
            id: 0,
            name: entry.name.clone().unwrap_or(id),
            command: entry.exec.clone(),
            location: Location::Path {
                path: effective.display().to_string(),
            },
            kind: StartupKind::XdgAutostart,
            scope: if system.is_some() {
                Scope::System
            } else {
                Scope::User
            },
            enabled: !entry.hidden && entry.autostart_enabled,
            needs_admin: false,
            publisher: None,
            missing_target: entry.target_missing(),
            ident: None,
            icon: None,
        },
        detail: StartupDetail::Xdg {
            user: user.to_path_buf(),
            system: system.map(Path::to_path_buf),
        },
    })
}

/// Parses `systemctl list-unit-files --no-legend`: (unit, state).
pub(super) fn parse_unit_files(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let unit = cols.next()?;
            let state = cols.next()?;
            unit.ends_with(".service")
                .then(|| (unit.to_owned(), state.to_owned()))
        })
        .collect()
}

/// Unit search path of the systemd user manager.
fn unit_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    dirs.extend(systemd_user_dir());
    dirs.push(PathBuf::from("/etc/systemd/user"));
    if let Some(data) = common::data_home() {
        dirs.push(data.join("systemd/user"));
    }
    for d in [
        "/usr/local/lib/systemd/user",
        "/usr/local/share/systemd/user",
        "/usr/lib/systemd/user",
        "/lib/systemd/user",
        "/usr/share/systemd/user",
    ] {
        dirs.push(PathBuf::from(d));
    }
    dirs
}

/// File of `unit` (templates: `a@b.service` → `a@.service`).
fn unit_file(unit: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let template = unit
        .split_once('@')
        .map(|(prefix, _)| format!("{prefix}@.service"));
    dirs.iter().find_map(|d| {
        let direct = d.join(unit);
        if direct.is_file() {
            return Some(direct);
        }
        template.as_ref().map(|t| d.join(t)).filter(|p| p.is_file())
    })
}

/// `Description` and `ExecStart` of a unit file.
pub(super) fn unit_summary(text: &str) -> (Option<String>, Option<String>) {
    let get = |key: &str| {
        text.lines().find_map(|l| {
            l.trim()
                .strip_prefix(key)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        })
    };
    (get("Description="), get("ExecStart="))
}

fn systemd_item(unit: &str, enabled: bool, file: Option<PathBuf>) -> StartupRecord {
    let text = file
        .as_deref()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .unwrap_or_default();
    let (description, exec) = unit_summary(&text);
    let program = files::unit_exec(&text);
    let missing_target = program
        .as_deref()
        .is_some_and(|p| Path::new(p).is_absolute() && !Path::new(p).exists());
    StartupRecord {
        item: StartupItem {
            id: 0,
            name: description.unwrap_or_else(|| unit.to_owned()),
            command: exec,
            location: Location::Path {
                path: file
                    .as_deref()
                    .map_or_else(|| unit.to_owned(), |f| f.display().to_string()),
            },
            kind: StartupKind::SystemdUnit,
            scope: Scope::User,
            enabled,
            needs_admin: false,
            publisher: None,
            missing_target,
            ident: None,
            icon: None,
        },
        detail: StartupDetail::Systemd {
            unit: unit.to_owned(),
            file,
        },
    }
}

pub(super) fn list_startup(_settings: &CleanSettings, ctx: &JobCtx) -> Result<Vec<StartupRecord>> {
    ctx.set_phase(Phase::Scanning);
    let locales = desktop::locales();
    let user_dir = user_autostart_dir()?;
    // File name → system file (first directory wins, like the XDG lookup).
    let mut system: BTreeMap<String, PathBuf> = BTreeMap::new();
    for dir in system_autostart_dirs() {
        for file in common::desktop_files(&dir) {
            if let Some(name) = common::file_name(&file) {
                system
                    .entry(name.to_owned())
                    .or_insert_with(|| file.clone());
            }
        }
    }
    let mut names: Vec<String> = system.keys().cloned().collect();
    for file in common::desktop_files(&user_dir) {
        if let Some(name) = common::file_name(&file)
            && !system.contains_key(name)
        {
            names.push(name.to_owned());
        }
    }
    let mut out = Vec::new();
    for name in names {
        if ctx.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let user = user_dir.join(&name);
        if let Some(record) = xdg_item(&user, system.get(&name).map(PathBuf::as_path), &locales) {
            out.push(record);
        }
    }

    if common::which("systemctl").is_some()
        && let Some(text) = common::run_stdout(
            "systemctl",
            &[
                "--user",
                "list-unit-files",
                "--type=service",
                "--state=enabled,disabled",
                "--no-legend",
                "--no-pager",
            ],
            QUICK_TIMEOUT,
            true,
        )
    {
        let dirs = unit_dirs();
        let own_dir = systemd_user_dir();
        for (unit, state) in parse_unit_files(&text) {
            let file = unit_file(&unit, &dirs);
            let enabled = state == "enabled";
            // Disabled units are listed only when the user created them.
            let user_made = file
                .as_deref()
                .zip(own_dir.as_deref())
                .is_some_and(|(f, d)| f.starts_with(d));
            if enabled || user_made {
                out.push(systemd_item(&unit, enabled, file));
            }
        }
    }
    ctx.add_items(u64::try_from(out.len()).unwrap_or(u64::MAX));
    Ok(out)
}

/// Writes the user entry (copying the system one first) with the switches set.
fn write_user_entry(
    user: &Path,
    user_exists: bool,
    system: Option<&Path>,
    enabled: bool,
) -> Result<()> {
    let base = if user_exists { Some(user) } else { system }
        .ok_or_else(|| Error::Unsupported("autostart entry is gone".to_owned()))?;
    let text = std::fs::read_to_string(base)?;
    let flag = if enabled { "true" } else { "false" };
    let hidden = if enabled { "false" } else { "true" };
    let text = desktop::set_keys(
        &text,
        &[("Hidden", hidden), ("X-GNOME-Autostart-enabled", flag)],
    );
    if let Some(parent) = user.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(user, text)?;
    Ok(())
}

fn systemctl(verb: &str, unit: &str) -> Result<()> {
    cmd::run("systemctl", &["--user", verb, unit], QUICK_TIMEOUT)?
        .ok("systemctl")
        .map(drop)
}

pub(super) fn change_startup(
    record: &StartupRecord,
    change: StartupChange,
    _settings: &CleanSettings,
    _ctx: &JobCtx,
) -> Result<Option<StartupItem>> {
    let id = record.item.id;
    let with_id = |mut item: StartupItem| {
        item.id = id;
        item
    };
    match &record.detail {
        StartupDetail::Xdg { user, system } => {
            // Re-read the state: the list may be stale.
            let user_exists = user.is_file();
            let system = system.as_deref().filter(|s| s.is_file());
            match change {
                StartupChange::Enable => write_user_entry(user, user_exists, system, true)?,
                StartupChange::Remove if system.is_none() => {
                    if user_exists {
                        std::fs::remove_file(user)?;
                    }
                    return Ok(None);
                }
                // A system entry is never deleted: a hidden user override disables it.
                StartupChange::Disable | StartupChange::Remove => {
                    write_user_entry(user, user_exists, system, false)?;
                }
            }
            let locales = desktop::locales();
            Ok(xdg_item(user, system, &locales).map(|r| with_id(r.item)))
        }
        StartupDetail::Systemd { unit, file } => match change {
            StartupChange::Enable | StartupChange::Disable => {
                let enable = change == StartupChange::Enable;
                systemctl(if enable { "enable" } else { "disable" }, unit)?;
                Ok(Some(with_id(systemd_item(unit, enable, file.clone()).item)))
            }
            StartupChange::Remove => {
                systemctl("disable", unit)?;
                let own = file
                    .as_deref()
                    .filter(|f| systemd_user_dir().is_some_and(|d| f.starts_with(d)));
                let Some(own) = own else {
                    return Ok(Some(with_id(systemd_item(unit, false, file.clone()).item)));
                };
                std::fs::remove_file(own)?;
                if let Err(err) = cmd::run("systemctl", &["--user", "daemon-reload"], QUICK_TIMEOUT)
                    .and_then(|o| o.ok("systemctl"))
                {
                    tracing::debug!(%err, "systemctl daemon-reload");
                }
                Ok(None)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_file_listing_keeps_services() {
        let got = parse_unit_files(
            "pipewire.service   enabled enabled\nfoo.socket enabled enabled\nsyncthing.service disabled enabled\n",
        );
        assert_eq!(
            got,
            vec![
                ("pipewire.service".to_owned(), "enabled".to_owned()),
                ("syncthing.service".to_owned(), "disabled".to_owned())
            ],
            "services with state"
        );
    }

    #[test]
    fn unit_summary_reads_description_and_exec() {
        let (d, e) = unit_summary(
            "[Unit]\nDescription=Sync\n[Service]\nExecStart=/usr/bin/syncthing -no-browser\n",
        );
        assert_eq!(d.as_deref(), Some("Sync"), "description");
        assert_eq!(e.as_deref(), Some("/usr/bin/syncthing -no-browser"), "exec");
    }

    #[test]
    fn disabling_a_system_entry_writes_a_user_override() {
        let root = std::env::temp_dir().join(format!("omc-linux-startup-{}", std::process::id()));
        let system = root.join("etc/autostart/app.desktop");
        let user = root.join("home/autostart/app.desktop");
        let made = system
            .parent()
            .map(std::fs::create_dir_all)
            .is_some_and(|r| r.is_ok())
            && std::fs::write(
                &system,
                "[Desktop Entry]\nType=Application\nName=App\nExec=/bin/true\n",
            )
            .is_ok();
        assert!(made, "fixture written");
        let r = write_user_entry(&user, false, Some(&system), false);
        assert!(r.is_ok(), "override written: {r:?}");
        let item = xdg_item(&user, Some(&system), &[]);
        assert!(
            item.as_ref()
                .is_some_and(|i| !i.item.enabled && i.item.scope == Scope::System),
            "disabled system item: {item:?}"
        );
        let sys_text = std::fs::read_to_string(&system).unwrap_or_default();
        assert!(!sys_text.contains("Hidden"), "system file untouched");
        let r = write_user_entry(&user, true, Some(&system), true);
        assert!(r.is_ok(), "re-enabled: {r:?}");
        let item = xdg_item(&user, Some(&system), &[]);
        assert!(item.is_some_and(|i| i.item.enabled), "enabled again");
        if let Err(err) = std::fs::remove_dir_all(&root) {
            tracing::debug!(%err, "cleanup");
        }
    }
}
