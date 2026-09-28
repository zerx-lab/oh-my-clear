//! Quitting apps, restore points, and running vendor uninstallers (MSI, EXE, Store).

use std::collections::HashSet;
use std::os::windows::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use omc_proto::apps::UninstallerOutcome;
use omc_proto::jobs::Phase;
use omc_proto::settings::CleanSettings;
use omc_scan::JobCtx;

use super::parse::{self, ProcessPath};
use super::{known, reg};
use crate::{AppFiles, AppRecord, Error, Result, cmd};

/// Longest wait for a vendor uninstaller (including its helper processes).
const UNINSTALL_LIMIT: Duration = Duration::from_mins(30);
/// Poll interval while an uninstaller's helpers run.
const HELPER_POLL: Duration = Duration::from_secs(2);
/// Poll interval while the launched process runs.
const CHILD_POLL: Duration = Duration::from_millis(250);
/// Graceful quit timeout.
const QUIT_GRACE: Duration = Duration::from_secs(8);
/// `ERROR_ELEVATION_REQUIRED`.
const ERROR_ELEVATION_REQUIRED: i32 = 740;

/// Program names too generic to kill by name alone (shared by many apps).
fn is_generic_exe(name: &str) -> bool {
    matches!(
        name,
        "update.exe"
            | "updater.exe"
            | "setup.exe"
            | "launcher.exe"
            | "helper.exe"
            | "uninstall.exe"
            | "uninst.exe"
            | "install.exe"
            | "crashpad_handler.exe"
            | "crashreporter.exe"
            | "service.exe"
            | "server.exe"
            | "node.exe"
            | "python.exe"
            | "pythonw.exe"
            | "java.exe"
            | "javaw.exe"
            | "electron.exe"
            | "dotnet.exe"
            | "cmd.exe"
            | "conhost.exe"
            | "powershell.exe"
            | "rundll32.exe"
            | "msiexec.exe"
            | "explorer.exe"
            | "svchost.exe"
    ) || name.starts_with("unins")
}

/// Processes with a visible executable path.
fn process_paths() -> Vec<ProcessPath> {
    let script =
        "Get-Process | Where-Object Path | Select-Object Id,Path | ConvertTo-Json -Compress";
    match known::powershell(script, Duration::from_secs(60))
        .and_then(|text| parse::parse_json_list::<ProcessPath>(&text))
    {
        Ok(list) => list,
        Err(err) => {
            tracing::warn!(%err, "listing process paths failed");
            Vec::new()
        }
    }
}

/// Pids of the app: processes whose program lives in the install folder, plus processes
/// of other users/elevated ones (no visible path) with one of the app's distinctive
/// program names.
fn app_pids(app: &AppRecord) -> HashSet<u32> {
    let detail = &app.detail;
    let mut pids = HashSet::new();
    let mut with_path = HashSet::new();
    for process in process_paths() {
        let Some(path) = process.path.as_deref() else {
            continue;
        };
        with_path.insert(process.id);
        let inside = detail
            .location
            .as_deref()
            .is_some_and(|dir| parse::path_within(path, dir))
            || detail
                .icon_exe
                .as_deref()
                .is_some_and(|exe| exe.eq_ignore_ascii_case(path));
        if inside {
            pids.insert(process.id);
        }
    }
    for process in omc_scan::procs::running() {
        let name = process.name.to_ascii_lowercase();
        if !with_path.contains(&process.pid)
            && !is_generic_exe(&name)
            && detail.exes.contains(&name)
        {
            pids.insert(process.pid);
        }
    }
    pids
}

fn alive(pids: &HashSet<u32>) -> HashSet<u32> {
    omc_scan::procs::running()
        .into_iter()
        .map(|p| p.pid)
        .filter(|pid| pids.contains(pid))
        .collect()
}

fn taskkill(pids: &HashSet<u32>, force: bool) {
    let mut args: Vec<String> = Vec::new();
    if force {
        args.push("/F".to_owned());
        args.push("/T".to_owned());
    }
    for pid in pids {
        args.push("/PID".to_owned());
        args.push(pid.to_string());
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match cmd::run("taskkill", &refs, Duration::from_secs(30)) {
        Ok(out) if out.status.success() => {}
        Ok(out) => tracing::debug!(stderr = %out.stderr.trim(), force, "taskkill reported errors"),
        Err(err) => tracing::warn!(%err, force, "taskkill failed"),
    }
}

/// Waits until none of `pids` runs (`true`) or `limit` passes.
fn wait_exit(pids: &HashSet<u32>, limit: Duration, ctx: &JobCtx) -> Result<bool> {
    let start = Instant::now();
    loop {
        if alive(pids).is_empty() {
            return Ok(true);
        }
        if ctx.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if start.elapsed() >= limit {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub(crate) fn quit(app: &AppRecord, ctx: &JobCtx) -> Result<()> {
    let pids = app_pids(app);
    if pids.is_empty() {
        return Ok(());
    }
    ctx.set_phase(Phase::Quitting);
    taskkill(&pids, false);
    if wait_exit(&pids, QUIT_GRACE, ctx)? {
        return Ok(());
    }
    let left = alive(&pids);
    tracing::info!(app = %app.info.name, count = left.len(), "forcing the app to quit");
    taskkill(&left, true);
    if !wait_exit(&left, Duration::from_secs(5), ctx)? {
        tracing::warn!(app = %app.info.name, "some processes survived taskkill /F");
    }
    Ok(())
}

pub(crate) fn restore_point(app: &AppRecord, settings: &CleanSettings, _ctx: &JobCtx) {
    if !settings.restore_point {
        return;
    }
    let description = format!("oh-my-clear: uninstall {}", app.info.name);
    let script = format!(
        "Checkpoint-Computer -Description {} -RestorePointType APPLICATION_UNINSTALL -ErrorAction Stop",
        parse::ps_quote(&description)
    );
    match known::powershell(&script, Duration::from_mins(10)) {
        Ok(_) => tracing::info!(app = %app.info.name, "restore point created"),
        Err(err) => tracing::warn!(%err, app = %app.info.name, "restore point failed; continuing"),
    }
}

/// Uninstall registry keys of the app still exist.
fn keys_exist(app: &AppRecord) -> bool {
    app.detail
        .keys
        .iter()
        .any(|(hive, path)| reg::exists(*hive, path))
}

pub(crate) fn run(files: &AppFiles, _settings: &CleanSettings, ctx: &JobCtx) -> UninstallerOutcome {
    let app = &files.app;
    let detail = &app.detail;
    ctx.set_phase(Phase::Uninstalling);
    let start = Instant::now();
    if let Some(package) = detail.package.as_ref() {
        let script = format!(
            "Remove-AppxPackage -Package {} -ErrorAction Stop",
            parse::ps_quote(&package.full_name)
        );
        return match known::powershell(&script, Duration::from_mins(10)) {
            Ok(_) => UninstallerOutcome::Succeeded,
            Err(err) => UninstallerOutcome::Failed {
                code: None,
                message: err.to_string(),
            },
        };
    }
    if let Some(code) = detail.msi_code.as_deref() {
        return launch(
            "msiexec.exe",
            &format!("/x {code} /qb-! /norestart"),
            "",
            app,
            start,
            ctx,
        );
    }
    let Some(command) = detail
        .uninstall
        .as_deref()
        .or(detail.quiet_uninstall.as_deref())
    else {
        return UninstallerOutcome::NotRun;
    };
    let expanded = reg::expand(command);
    let Some(line) = parse::split_command(&expanded, known::is_file) else {
        return UninstallerOutcome::Failed {
            code: None,
            message: format!("cannot parse uninstall command: {command}"),
        };
    };
    let name = line.exe_name();
    if name.trim_end_matches(".exe") == "msiexec"
        && let Some(code) = parse::find_guid(&line.args)
    {
        return launch(
            "msiexec.exe",
            &format!("/x {code} /qb-! /norestart"),
            "",
            app,
            start,
            ctx,
        );
    }
    let helper = if matches!(
        name.trim_end_matches(".exe"),
        "msiexec" | "rundll32" | "cmd" | "powershell"
    ) {
        String::new()
    } else {
        name
    };
    launch(&line.exe, &line.args, &helper, app, start, ctx)
}

/// Runs `exe args`, relaunching elevated when Windows requires it, then waits for the
/// helper processes self-copying uninstallers leave behind.
fn launch(
    exe: &str,
    args: &str,
    helper: &str,
    app: &AppRecord,
    start: Instant,
    ctx: &JobCtx,
) -> UninstallerOutcome {
    tracing::info!(exe, args, app = %app.info.name, "running vendor uninstaller");
    let mut command = Command::new(exe);
    if !args.is_empty() {
        command.raw_arg(args);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let exit = match command.spawn() {
        Ok(child) => wait_child(child, start, ctx),
        Err(err) if err.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) => {
            elevated(exe, args, start, ctx)
        }
        Err(err) => {
            return UninstallerOutcome::Failed {
                code: None,
                message: format!("{exe}: {err}"),
            };
        }
    };
    let code = match exit {
        Ok(code) => code,
        Err(outcome) => return outcome,
    };
    let outcome = parse::exit_outcome(code, "the uninstaller reported an error");
    if outcome != UninstallerOutcome::Succeeded {
        return outcome;
    }
    wait_helpers(helper, app, start, ctx)
}

/// Exit code of `child`, or the outcome that ended the wait.
fn wait_child(
    mut child: Child,
    start: Instant,
    ctx: &JobCtx,
) -> Result<Option<i32>, UninstallerOutcome> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.code()),
            Ok(None) => {}
            Err(err) => {
                return Err(UninstallerOutcome::Failed {
                    code: None,
                    message: err.to_string(),
                });
            }
        }
        if ctx.is_cancelled() {
            // Killing an uninstaller half-way could leave the app broken; leave its
            // window to the user.
            tracing::info!("uninstall cancelled; the vendor uninstaller keeps running");
            return Err(UninstallerOutcome::Cancelled);
        }
        if start.elapsed() >= UNINSTALL_LIMIT {
            return Err(UninstallerOutcome::Failed {
                code: None,
                message: "the uninstaller did not finish within 30 minutes".to_owned(),
            });
        }
        std::thread::sleep(CHILD_POLL);
    }
}

/// `Start-Process -Verb RunAs -Wait` (UAC prompt); exit code 1223 = prompt declined.
fn elevated(
    exe: &str,
    args: &str,
    start: Instant,
    ctx: &JobCtx,
) -> Result<Option<i32>, UninstallerOutcome> {
    let arg_list = if args.is_empty() {
        String::new()
    } else {
        format!(" -ArgumentList {}", parse::ps_quote(args))
    };
    let script = format!(
        "try {{ $p = Start-Process -FilePath {}{arg_list} -Verb RunAs -Wait -PassThru -ErrorAction Stop; exit $p.ExitCode }} catch {{ $e = $_.Exception; if ($e.NativeErrorCode -eq 1223 -or ($e.InnerException -and $e.InnerException.NativeErrorCode -eq 1223)) {{ exit 1223 }}; exit 740 }}",
        parse::ps_quote(exe)
    );
    let mut command = cmd::command("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    ctx.set_phase(Phase::Elevating);
    let child = command.spawn().map_err(|err| UninstallerOutcome::Failed {
        code: None,
        message: format!("powershell: {err}"),
    })?;
    let result = wait_child(child, start, ctx);
    ctx.set_phase(Phase::Uninstalling);
    match result? {
        Some(1223) => Err(UninstallerOutcome::Cancelled),
        Some(ERROR_ELEVATION_REQUIRED) => Err(UninstallerOutcome::Failed {
            code: None,
            message: format!("{exe}: could not start the uninstaller as administrator"),
        }),
        code => Ok(code),
    }
}

/// After the launcher exited: waits while the uninstall key still exists and a helper
/// uninstaller process (NSIS `Au_.exe`, Inno `_iu*.tmp`, `unins*.exe`…) is running.
fn wait_helpers(helper: &str, app: &AppRecord, start: Instant, ctx: &JobCtx) -> UninstallerOutcome {
    let mut first = true;
    loop {
        if !keys_exist(app) {
            return UninstallerOutcome::Succeeded;
        }
        if !first {
            let running = omc_scan::procs::running()
                .iter()
                .any(|p| parse::is_uninstaller_process(&p.name, helper));
            if !running {
                return UninstallerOutcome::Succeeded;
            }
        }
        if ctx.is_cancelled() {
            return UninstallerOutcome::Cancelled;
        }
        if start.elapsed() >= UNINSTALL_LIMIT {
            return UninstallerOutcome::Failed {
                code: None,
                message: "the uninstaller did not finish within 30 minutes".to_owned(),
            };
        }
        std::thread::sleep(if first {
            Duration::from_secs(1)
        } else {
            HELPER_POLL
        });
        first = false;
    }
}
