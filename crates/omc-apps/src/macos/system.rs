//! OS facts (version, Full Disk Access, volumes), quitting apps, Homebrew uninstalls and
//! the special removal actions (Finder trash, launchd jobs, login items, receipts).

use std::path::Path;
use std::time::{Duration, Instant};

use omc_proto::apps::UninstallerOutcome;
use omc_proto::jobs::{Phase, SpecialAction};
use omc_proto::settings::{Access, CleanSettings, Os, SystemInfo, Volume};
use omc_scan::JobCtx;

use super::{access, inventory, startup};
use crate::cmd;
use crate::{AppFiles, AppRecord, Error, Result};

/// Short helper commands.
const SHORT: Duration = Duration::from_secs(10);
/// Emptying a large Trash through Finder.
const TRASH_TIMEOUT: Duration = Duration::from_secs(600);
/// `brew uninstall --cask`.
const BREW_TIMEOUT: Duration = Duration::from_secs(300);
/// Graceful quit, then SIGTERM grace.
const QUIT_WAIT: Duration = Duration::from_secs(10);
const TERM_WAIT: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(250);

/// `df -kP` output → volumes worth showing (`/` as "Macintosh HD" and `/Volumes/*`).
pub(super) fn parse_df(text: &str) -> Vec<Volume> {
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let Some(row) = parse_df_line(line) else {
            continue;
        };
        if row.filesystem == "devfs" || row.filesystem.starts_with("map ") {
            continue;
        }
        let name = if row.mount == "/" {
            "Macintosh HD".to_owned()
        } else if let Some(rest) = row.mount.strip_prefix("/Volumes/") {
            // The Recovery volume is part of the system container.
            if rest.is_empty() || rest.contains('/') || rest == "Recovery" {
                continue;
            }
            rest.to_owned()
        } else {
            // System volumes, simulator runtimes, firmlinked data volume: not user volumes.
            continue;
        };
        out.push(Volume {
            mount: row.mount.to_owned(),
            name,
            total: row.total_kib.saturating_mul(1024),
            free: row.free_kib.saturating_mul(1024),
        });
    }
    out
}

struct DfRow<'a> {
    filesystem: String,
    total_kib: u64,
    free_kib: u64,
    mount: &'a str,
}

/// One `df -kP` row. The filesystem and mount point may contain spaces, so the numbers are
/// found around the `NN%` capacity column.
fn parse_df_line(line: &str) -> Option<DfRow<'_>> {
    let tokens = tokens_with_offsets(line);
    let cap = tokens.iter().position(|(_, t)| {
        t.strip_suffix('%')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })?;
    let (free_at, total_at) = (cap.checked_sub(1)?, cap.checked_sub(3)?);
    let free_kib = tokens.get(free_at)?.1.parse().ok()?;
    let total_kib = tokens.get(total_at)?.1.parse().ok()?;
    let filesystem = tokens
        .get(..total_at)?
        .iter()
        .map(|(_, t)| *t)
        .collect::<Vec<_>>()
        .join(" ");
    let (cap_at, cap_tok) = tokens.get(cap)?;
    let after = cap_at.checked_add(cap_tok.len())?;
    let mount = line.get(after..)?.trim();
    (!mount.is_empty()).then_some(DfRow {
        filesystem,
        total_kib,
        free_kib,
        mount,
    })
}

/// Whitespace-separated tokens with their byte offsets.
fn tokens_with_offsets(line: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in line.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(s)) => {
                out.extend(line.get(s..i).map(|t| (s, t)));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        out.extend(line.get(s..).map(|t| (s, t)));
    }
    out
}

/// Full Disk Access of this process, probed on TCC-protected files.
fn full_disk_access() -> Access {
    let Some(home) = omc_scan::paths::home() else {
        return Access::Unknown;
    };
    let lib = home.join("Library");
    let files = [
        lib.join("Application Support")
            .join("com.apple.TCC")
            .join("TCC.db"),
        lib.join("Safari").join("Bookmarks.plist"),
    ];
    for file in &files {
        match std::fs::File::open(file) {
            Ok(_) => return Access::Granted,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) if err.raw_os_error() == Some(1) => return Access::Denied,
            Err(err) => tracing::debug!(%err, path = %file.display(), "FDA probe"),
        }
    }
    match std::fs::read_dir(lib.join("Mail")) {
        Ok(_) => Access::Granted,
        Err(err) if err.raw_os_error() == Some(1) => Access::Denied,
        Err(_) => Access::Unknown,
    }
}

pub(super) fn system_info() -> SystemInfo {
    let os_version = match cmd::run("/usr/bin/sw_vers", &["-productVersion"], SHORT)
        .and_then(|o| o.ok("sw_vers"))
    {
        Ok(v) => v.trim().to_owned(),
        Err(err) => {
            tracing::debug!(%err, "sw_vers");
            String::new()
        }
    };
    let volumes = match cmd::run("/bin/df", &["-kP"], SHORT).and_then(|o| o.ok("df")) {
        Ok(text) => parse_df(&text),
        Err(err) => {
            tracing::debug!(%err, "df");
            Vec::new()
        }
    };
    SystemInfo {
        os: Os::Macos,
        os_version,
        home: omc_scan::paths::home()
            .map(|h| h.display().to_string())
            .unwrap_or_default(),
        elevated: access::is_root(),
        full_disk_access: full_disk_access(),
        volumes,
    }
}

/// Pids of processes running from the app bundle.
fn app_pids(app: &AppRecord) -> Vec<u32> {
    let bundle = app.detail.bundle.to_string_lossy().to_lowercase();
    let real = app.detail.real.to_string_lossy().to_lowercase();
    omc_scan::procs::running()
        .into_iter()
        .filter(|p| {
            p.exe
                .as_deref()
                .and_then(inventory::outer_app)
                .is_some_and(|b| b == bundle || b == real)
        })
        .map(|p| p.pid)
        .collect()
}

/// Waits until the app has no process; `true` when it exited in time.
fn wait_exit(app: &AppRecord, timeout: Duration, ctx: &JobCtx) -> Result<bool> {
    let start = Instant::now();
    loop {
        if app_pids(app).is_empty() {
            return Ok(true);
        }
        if ctx.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if start.elapsed() >= timeout {
            return Ok(false);
        }
        std::thread::sleep(POLL);
    }
}

fn kill(signal: &str, pids: &[u32]) {
    if pids.is_empty() {
        return;
    }
    let pids: Vec<String> = pids.iter().map(u32::to_string).collect();
    let mut args = vec![signal];
    args.extend(pids.iter().map(String::as_str));
    if let Err(err) = cmd::run("/bin/kill", &args, SHORT).and_then(|o| o.ok("kill")) {
        // Some may have exited meanwhile.
        tracing::debug!(%err, signal, "kill");
    }
}

pub(super) fn quit_app(app: &AppRecord, ctx: &JobCtx) -> Result<()> {
    if app_pids(app).is_empty() {
        return Ok(());
    }
    ctx.set_phase(Phase::Quitting);
    if let Some(id) = &app.detail.bundle_id {
        let script = format!("quit app id {}", startup::applescript_string(id));
        if let Err(err) =
            cmd::run("/usr/bin/osascript", &["-e", &script], SHORT).and_then(|o| o.ok("osascript"))
        {
            tracing::info!(%err, "graceful quit failed");
        }
        if wait_exit(app, QUIT_WAIT, ctx)? {
            return Ok(());
        }
    }
    kill("-TERM", &app_pids(app));
    if wait_exit(app, TERM_WAIT, ctx)? {
        return Ok(());
    }
    kill("-KILL", &app_pids(app));
    if wait_exit(app, TERM_WAIT, ctx)? {
        Ok(())
    } else {
        Err(Error::Command {
            program: "kill".to_owned(),
            message: format!("{} is still running", app.info.name),
        })
    }
}

pub(super) fn run_uninstaller(
    files: &AppFiles,
    _settings: &CleanSettings,
    ctx: &JobCtx,
) -> UninstallerOutcome {
    let Some(cask) = &files.app.detail.cask else {
        return UninstallerOutcome::NotRun;
    };
    let Some(brew) = inventory::brew() else {
        return UninstallerOutcome::NotRun;
    };
    if access::is_root() {
        // Homebrew refuses to run as root.
        return UninstallerOutcome::NotRun;
    }
    ctx.set_phase(Phase::Uninstalling);
    let mut cmd = cmd::command(&brew.to_string_lossy());
    cmd.args(["uninstall", "--cask", &cask.token])
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .env("HOMEBREW_NO_INSTALL_CLEANUP", "1")
        .env("NONINTERACTIVE", "1");
    match cmd::run_command("brew", &mut cmd, BREW_TIMEOUT) {
        Ok(out) if out.status.success() => UninstallerOutcome::Succeeded,
        Ok(out) => UninstallerOutcome::Failed {
            code: out.status.code(),
            message: out.stderr.trim().to_owned(),
        },
        Err(err) => UninstallerOutcome::Failed {
            code: None,
            message: err.to_string(),
        },
    }
}

/// `pkgutil --forget`; already forgotten counts as done.
fn forget_package(id: &str) -> Result<()> {
    let receipt = Path::new("/private/var/db/receipts").join(format!("{id}.plist"));
    if !receipt.exists() {
        return Ok(());
    }
    if !access::is_root() {
        return Err(Error::Elevation(format!(
            "forgetting the installer receipt {id} needs administrator rights"
        )));
    }
    let out = cmd::run("/usr/sbin/pkgutil", &["--forget", id], SHORT)?;
    if out.status.success() || out.stderr.contains("No receipt") {
        Ok(())
    } else {
        out.ok("pkgutil").map(|_| ())
    }
}

pub(super) fn run_special(action: &SpecialAction) -> Result<()> {
    match action {
        SpecialAction::EmptyTrash => {
            cmd::run(
                "/usr/bin/osascript",
                &["-e", "tell application \"Finder\" to empty trash"],
                TRASH_TIMEOUT,
            )?
            .ok("osascript")?;
            Ok(())
        }
        SpecialAction::UnloadLaunchJob { label, plist } => {
            startup::unload_and_delete(label, Path::new(plist))
        }
        SpecialAction::RemoveLoginItem { name } => startup::remove_login_item(name),
        SpecialAction::ForgetPackage { id } => forget_package(id),
        SpecialAction::DeleteService { .. } | SpecialAction::DeleteScheduledTask { .. } => {
            Err(Error::Unsupported(
                "Windows services and scheduled tasks do not exist on macOS".to_owned(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn df_rows_parse_with_spaces() {
        let text = "Filesystem     1024-blocks      Used Available Capacity  Mounted on\n\
/dev/disk3s1s1   971298980  13339492 465887676     3%    /\n\
devfs                  240       240         0   100%    /dev\n\
/dev/disk3s5     971298980 476604080 465887676    51%    /System/Volumes/Data\n\
map auto_home            0         0         0   100%    /System/Volumes/Data/home\n\
/dev/disk18s1       499208    496392      2816   100%    /Volumes/Paseo 0.9.2-arm64\n";
        let vols = parse_df(text);
        assert_eq!(vols.len(), 2, "root and one external volume");
        assert!(
            vols.first().is_some_and(|v| v.name == "Macintosh HD"
                && v.total == 971_298_980 * 1024
                && v.free == 465_887_676 * 1024),
            "root volume"
        );
        assert!(
            vols.get(1).is_some_and(
                |v| v.mount == "/Volumes/Paseo 0.9.2-arm64" && v.name == "Paseo 0.9.2-arm64"
            ),
            "mount point with spaces"
        );
        assert!(
            parse_df_line("garbage line").is_none(),
            "no capacity column"
        );
    }
}
