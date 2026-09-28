//! Quitting apps (`SIGTERM`, then `SIGKILL`) and running the package manager that removes
//! them (`pkexec` for system packages; `pkexec` exit 126/127 = the prompt was dismissed).

use std::collections::HashSet;
use std::io::Read as _;
use std::process::Stdio;
use std::time::{Duration, Instant};

use omc_proto::apps::{AppSource, UninstallerOutcome};
use omc_proto::jobs::Phase;
use omc_proto::settings::CleanSettings;
use omc_scan::JobCtx;
use omc_scan::procs;

use super::common::{self, QUICK_TIMEOUT};
use super::{inventory, pkg, system};
use crate::{AppFiles, AppRecord, Error, Result, cmd};

/// Grace period between `SIGTERM` and `SIGKILL`.
const QUIT_GRACE: Duration = Duration::from_secs(5);
/// How often quitting and uninstalling poll.
const POLL: Duration = Duration::from_millis(200);
/// Deadline of a package-manager run.
const UNINSTALL_TIMEOUT: Duration = Duration::from_mins(30);
/// `pkexec`: authorization dismissed / not obtained.
const PKEXEC_DISMISSED: [i32; 2] = [126, 127];
/// Bytes of the uninstaller's stderr kept for the failure message.
const STDERR_TAIL: usize = 2048;

/// Processes of `app` (except this one).
pub(super) fn pids_of(app: &AppRecord) -> Vec<u32> {
    let own = std::process::id();
    let canonical = inventory::canonical_binaries(&app.detail.binaries);
    procs::running()
        .into_iter()
        .filter(|p| {
            p.pid != own
                && inventory::process_matches(
                    app.info.source,
                    app.info.ident.as_deref(),
                    &app.detail,
                    &canonical,
                    p,
                )
        })
        .map(|p| p.pid)
        .collect()
}

fn signal(sig: &str, pids: &[u32]) {
    let strs: Vec<String> = pids.iter().map(u32::to_string).collect();
    let mut args = vec![sig];
    args.extend(strs.iter().map(String::as_str));
    match cmd::run("kill", &args, QUICK_TIMEOUT) {
        Ok(out) if !out.status.success() => {
            tracing::debug!(sig, stderr = %out.stderr.trim(), "kill reported failures");
        }
        Ok(_) => {}
        Err(err) => tracing::warn!(%err, sig, "cannot signal processes"),
    }
}

/// Waits until `alive()` is empty or `grace` passed; returns what is still alive.
fn wait_gone(ctx: &JobCtx, grace: Duration, alive: impl Fn() -> Vec<u32>) -> Result<Vec<u32>> {
    let start = Instant::now();
    loop {
        let left = alive();
        if left.is_empty() || start.elapsed() >= grace {
            return Ok(left);
        }
        if ctx.is_cancelled() {
            return Err(Error::Cancelled);
        }
        std::thread::sleep(POLL);
    }
}

pub(super) fn quit_app(app: &AppRecord, ctx: &JobCtx) -> Result<()> {
    if app.info.source == AppSource::Flatpak {
        let Some(id) = app.detail.package.as_deref() else {
            return Ok(());
        };
        if !pkg::flatpak_running().contains(id) {
            return Ok(());
        }
        ctx.set_phase(Phase::Quitting);
        if let Err(err) =
            cmd::run("flatpak", &["kill", id], QUICK_TIMEOUT).and_then(|o| o.ok("flatpak"))
        {
            tracing::warn!(%err, id, "flatpak kill failed");
        }
        let left = wait_gone(ctx, QUIT_GRACE, || {
            if pkg::flatpak_running().contains(id) {
                vec![0]
            } else {
                Vec::new()
            }
        })?;
        return if left.is_empty() {
            Ok(())
        } else {
            Err(Error::Command {
                program: "flatpak kill".to_owned(),
                message: format!("{id} is still running"),
            })
        };
    }
    let pids = pids_of(app);
    if pids.is_empty() {
        return Ok(());
    }
    ctx.set_phase(Phase::Quitting);
    signal("-TERM", &pids);
    let targets: HashSet<u32> = pids.iter().copied().collect();
    let still = || -> Vec<u32> {
        pids_of(app)
            .into_iter()
            .filter(|p| targets.contains(p))
            .collect()
    };
    let left = wait_gone(ctx, QUIT_GRACE, still)?;
    if left.is_empty() {
        return Ok(());
    }
    tracing::info!(app = %app.info.name, pids = ?left, "force-quitting");
    signal("-KILL", &left);
    let left = wait_gone(ctx, Duration::from_secs(2), still)?;
    if left.is_empty() {
        Ok(())
    } else {
        Err(Error::Command {
            program: "kill".to_owned(),
            message: format!("{} is still running (pids {left:?})", app.info.name),
        })
    }
}

/// The command that removes `app` (`None` for `AppImage`/desktop entries).
fn argv(app: &AppRecord) -> Option<(Vec<String>, bool)> {
    let package = app.detail.package.as_deref()?;
    let have = |p: &str| common::which(p).is_some();
    let (mut argv, elevated): (Vec<&str>, bool) = match app.info.source {
        AppSource::Deb if have("apt-get") => (
            vec![
                "env",
                "DEBIAN_FRONTEND=noninteractive",
                "apt-get",
                "remove",
                "-y",
                package,
            ],
            true,
        ),
        AppSource::Deb => (vec!["dpkg", "-r", package], true),
        AppSource::Rpm if have("dnf") => (vec!["dnf", "remove", "-y", package], true),
        AppSource::Rpm if have("zypper") => (vec!["zypper", "-n", "rm", package], true),
        AppSource::Rpm if have("yum") => (vec!["yum", "remove", "-y", package], true),
        AppSource::Rpm => (vec!["rpm", "-e", package], true),
        AppSource::Pacman => (vec!["pacman", "-Rns", "--noconfirm", package], true),
        AppSource::Snap => (vec!["snap", "remove", "--purge", package], true),
        AppSource::Flatpak => (
            vec![
                "flatpak",
                "uninstall",
                "-y",
                "--noninteractive",
                if app.detail.flatpak_user {
                    "--user"
                } else {
                    "--system"
                },
                package,
            ],
            false,
        ),
        _ => return None,
    };
    let use_pkexec = elevated && !system::is_root();
    if use_pkexec {
        argv.insert(0, "pkexec");
    }
    Some((argv.into_iter().map(str::to_owned).collect(), use_pkexec))
}

/// The package-manager command for display (`pkexec apt-get remove -y firefox`).
pub(super) fn command_line(app: &AppRecord) -> Option<String> {
    argv(app).map(|(argv, _)| argv.join(" "))
}

fn stderr_tail(pipe: Option<std::process::ChildStderr>) -> Option<std::thread::JoinHandle<String>> {
    let mut pipe = pipe?;
    std::thread::Builder::new()
        .name("omc-uninstall-stderr".to_owned())
        .spawn(move || {
            let mut buf = Vec::new();
            if let Err(err) = pipe.read_to_end(&mut buf) {
                tracing::debug!(%err, "reading uninstaller stderr");
            }
            let start = buf.len().saturating_sub(STDERR_TAIL);
            let tail = buf.get(start..).unwrap_or_default();
            String::from_utf8_lossy(tail).trim().to_owned()
        })
        .ok()
}

pub(super) fn run_uninstaller(
    files: &AppFiles,
    _settings: &CleanSettings,
    ctx: &JobCtx,
) -> UninstallerOutcome {
    let Some((command_argv, pkexec)) = argv(&files.app) else {
        return UninstallerOutcome::NotRun;
    };
    let Some((program, rest)) = command_argv.split_first() else {
        return UninstallerOutcome::NotRun;
    };
    ctx.set_phase(Phase::Uninstalling);
    let line = command_argv.join(" ");
    tracing::info!(command = %line, "running package manager");
    ctx.set_current(line);
    let mut command = cmd::command(program);
    command
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return UninstallerOutcome::Failed {
                code: None,
                message: format!("{program}: {err}"),
            };
        }
    };
    let stderr = stderr_tail(child.stderr.take());
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(err) => break Err(err.to_string()),
        }
        let cancelled = ctx.is_cancelled();
        if cancelled || start.elapsed() >= UNINSTALL_TIMEOUT {
            if let Err(err) = child.kill() {
                tracing::debug!(%err, "killing package manager");
            }
            if let Err(err) = child.wait() {
                tracing::debug!(%err, "reaping package manager");
            }
            if cancelled {
                return UninstallerOutcome::Cancelled;
            }
            break Err(format!(
                "timed out after {} min",
                UNINSTALL_TIMEOUT.as_secs() / 60
            ));
        }
        std::thread::sleep(POLL);
    };
    let message = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
    match status {
        Ok(status) if status.success() => UninstallerOutcome::Succeeded,
        Ok(status) if pkexec && status.code().is_some_and(|c| PKEXEC_DISMISSED.contains(&c)) => {
            UninstallerOutcome::Cancelled
        }
        Ok(status) => UninstallerOutcome::Failed {
            code: status.code(),
            message: if message.is_empty() {
                status.to_string()
            } else {
                message
            },
        },
        Err(err) => UninstallerOutcome::Failed {
            code: None,
            message: err,
        },
    }
}
