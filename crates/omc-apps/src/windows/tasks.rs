//! Scheduled tasks. Listed through `Get-ScheduledTask` (locale-independent JSON with
//! trigger classes) rather than `schtasks /query /fo csv`, whose column headers and
//! schedule types are translated on non-English Windows. Changed with `schtasks`.

use std::time::Duration;

use super::known;
use super::parse::{self, TaskInfo};
use crate::{Result, cmd};

const LIST_SCRIPT: &str = "$ErrorActionPreference='SilentlyContinue'; Get-ScheduledTask | ForEach-Object { [pscustomobject]@{ Path = $_.TaskPath + $_.TaskName; State = [string]$_.State; UserId = [string]$_.Principal.UserId; Exec = @($_.Actions | Where-Object { $_.Execute } | ForEach-Object { ($_.Execute + ' ' + $_.Arguments).Trim() }); Triggers = @($_.Triggers | ForEach-Object { $_.CimClass.CimClassName }) } } | ConvertTo-Json -Compress -Depth 3";

/// Every scheduled task visible to this user.
pub(crate) fn list() -> Result<Vec<TaskInfo>> {
    let text = known::powershell(LIST_SCRIPT, Duration::from_secs(120))?;
    parse::parse_json_list(&text)
}

/// The task runs as the current user (so the user may change or delete it).
pub(crate) fn runs_as_current_user(task: &TaskInfo) -> bool {
    let Some(user) = std::env::var("USERNAME").ok().filter(|u| !u.is_empty()) else {
        return false;
    };
    task.user_id.as_deref().is_some_and(|id| {
        let id = id.trim();
        let account = id.rsplit('\\').next().unwrap_or(id);
        account.eq_ignore_ascii_case(&user)
    })
}

/// `schtasks /query /tn <path>` succeeds.
pub(crate) fn exists(path: &str) -> bool {
    cmd::run(
        "schtasks",
        &["/Query", "/TN", path],
        Duration::from_secs(30),
    )
    .is_ok_and(|out| out.status.success())
}

/// `schtasks <args…>`; a failure of an existing task is reported as access denied (the
/// only failure `schtasks` has for a task that exists and a well-formed command).
pub(crate) fn schtasks(args: &[&str]) -> Result<()> {
    let out = cmd::run("schtasks", args, Duration::from_secs(60))?;
    if out.status.success() {
        return Ok(());
    }
    tracing::debug!(stderr = %out.stderr.trim(), ?args, "schtasks failed");
    Err(crate::Error::Io(std::io::Error::from_raw_os_error(5)))
}
