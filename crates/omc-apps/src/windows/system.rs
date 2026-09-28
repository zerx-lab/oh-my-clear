//! System facts (version, elevation, volumes) and [`SpecialAction`]s.

use std::time::Duration;

use omc_proto::jobs::SpecialAction;
use omc_proto::settings::{Access, Os, SystemInfo, Volume};

use super::parse::{self, Hive, LogicalDisk};
use super::{known, reg, tasks};
use crate::{Error, Result, cmd};

pub(crate) fn info() -> SystemInfo {
    SystemInfo {
        os: Os::Windows,
        os_version: os_version(),
        home: omc_scan::paths::home()
            .map(|h| h.display().to_string())
            .unwrap_or_default(),
        elevated: is_elevated(),
        full_disk_access: Access::NotApplicable,
        volumes: volumes(),
    }
}

fn os_version() -> String {
    let Some(key) = reg::open(
        Hive::LocalMachine,
        "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion",
    ) else {
        return "Windows".to_owned();
    };
    let product = reg::string(&key, "ProductName").unwrap_or_default();
    let display = reg::string(&key, "DisplayVersion").or_else(|| reg::string(&key, "ReleaseId"));
    let build =
        reg::string(&key, "CurrentBuild").or_else(|| reg::string(&key, "CurrentBuildNumber"));
    parse::os_display(&product, display.as_deref(), build.as_deref())
}

/// The process token has the High (or System) mandatory level.
pub(crate) fn is_elevated() -> bool {
    match cmd::run("whoami", &["/groups"], Duration::from_secs(15)) {
        Ok(out) => parse::whoami_elevated(&out.stdout),
        Err(err) => {
            tracing::warn!(%err, "whoami failed");
            false
        }
    }
}

fn volumes() -> Vec<Volume> {
    let script = "Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3' | Select-Object DeviceID,VolumeName,Size,FreeSpace | ConvertTo-Json -Compress";
    let disks = match known::powershell(script, Duration::from_secs(30))
        .and_then(|text| parse::parse_json_list::<LogicalDisk>(&text))
    {
        Ok(disks) => disks,
        Err(err) => {
            tracing::warn!(%err, "listing volumes failed");
            return Vec::new();
        }
    };
    disks
        .into_iter()
        .map(|d| Volume {
            mount: format!("{}\\", d.device_id.trim_end_matches('\\')),
            name: d.volume_name.unwrap_or_default(),
            total: d.size.unwrap_or(0),
            free: d.free_space.unwrap_or(0),
        })
        .collect()
}

/// `ERROR_ACCESS_DENIED` as an I/O error (the executor retries it elevated).
fn denied() -> Error {
    Error::Io(std::io::Error::from_raw_os_error(5))
}

pub(crate) fn special(action: &SpecialAction) -> Result<()> {
    match action {
        SpecialAction::EmptyTrash => {
            let script = "try { Clear-RecycleBin -Force -ErrorAction Stop } catch { $e = $_.Exception; if ($e.NativeErrorCode -eq 3 -or $e.HResult -eq -2147024893) { exit 0 }; throw }";
            known::powershell(script, Duration::from_mins(10)).map(|_| ())
        }
        SpecialAction::DeleteService { name } => delete_service(name),
        SpecialAction::DeleteScheduledTask { path } => {
            if !tasks::exists(path) {
                return Ok(());
            }
            tasks::schtasks(&["/Delete", "/TN", path, "/F"])
        }
        other => Err(Error::Unsupported(format!(
            "{other:?} is not a Windows action"
        ))),
    }
}

/// `sc stop` then `sc delete`; a missing service is success.
fn delete_service(name: &str) -> Result<()> {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
    const ERROR_SERVICE_MARKED_FOR_DELETE: i32 = 1072;
    match cmd::run("sc.exe", &["stop", name], Duration::from_secs(60)) {
        Ok(out) if out.status.code() == Some(ERROR_ACCESS_DENIED) => return Err(denied()),
        Ok(out) if out.status.code() == Some(ERROR_SERVICE_DOES_NOT_EXIST) => return Ok(()),
        Ok(_) => {}
        Err(err) => tracing::debug!(%err, name, "sc stop"),
    }
    let out = cmd::run("sc.exe", &["delete", name], Duration::from_secs(60))?;
    match out.status.code() {
        Some(0 | ERROR_SERVICE_DOES_NOT_EXIST | ERROR_SERVICE_MARKED_FOR_DELETE) => Ok(()),
        Some(ERROR_ACCESS_DENIED) => Err(denied()),
        code => Err(Error::Command {
            program: "sc delete".to_owned(),
            message: format!("exit {code:?}: {}", out.stdout.trim()),
        }),
    }
}
