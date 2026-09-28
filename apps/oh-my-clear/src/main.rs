//! oh-my-clear GUI process: gpui-kit viewport over the background `oh-my-clear-daemon` (ADR 0008).
//! Starts the omc-ipc client (spawning the sibling `oh-my-clear-daemon` executable when no
//! daemon answers) before opening the main window.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use gpui_kit::App;

fn main() -> ExitCode {
    if let Err(err) = omc_telemetry::init() {
        report_startup_error(&err);
        return ExitCode::FAILURE;
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "oh-my-clear starting");
    let daemon = daemon_exe();
    if let Ok(daemon) = &daemon {
        build_daemon_under_cargo_run(daemon);
    }

    gpui_kit::application()
        .with_assets(omc_ui::assets::Assets)
        .run(move |cx| {
            if let Err(err) = start(daemon, cx) {
                tracing::error!("oh-my-clear failed to start: {err}");
                cx.quit();
            }
        });
    ExitCode::SUCCESS
}

fn start(daemon: std::io::Result<PathBuf>, cx: &mut App) -> omc_ui::Result<()> {
    omc_ui::init(cx)?;
    match daemon {
        Ok(exe) => {
            tracing::info!(daemon = %exe.display(), "starting the engine connection");
            omc_ui::engine::start(exe, cx);
        }
        Err(err) => {
            tracing::error!("cannot locate the oh-my-clear-daemon executable: {err}");
            omc_ui::engine::unavailable(err.to_string(), cx);
        }
    }
    // The UI is a viewport: closing its last window ends the process; the daemon keeps
    // running and a new UI reattaches.
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
    omc_ui::open_main_window(cx)?;
    cx.activate(true);
    Ok(())
}

/// `oh-my-clear-daemon` next to this executable.
fn daemon_exe() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} has no parent directory", exe.display()),
        )
    })?;
    Ok(dir.join(format!(
        "oh-my-clear-daemon{}",
        std::env::consts::EXE_SUFFIX
    )))
}

/// `cargo run` builds only this package, so the daemon next to the GUI is missing or stale.
/// When `cargo run` launched this binary (cargo sets `CARGO_PKG_NAME` for the process it
/// runs), build `oh-my-clear-daemon` first with the same cargo and profile; cargo's output
/// goes to the terminal. Installed apps and `cargo omc` on macOS start the GUI next to an
/// already built daemon and skip this. On failure the existing binary, if any, is used.
fn build_daemon_under_cargo_run(daemon: &Path) {
    let run_by_cargo =
        std::env::var_os("CARGO_PKG_NAME").is_some_and(|name| name == env!("CARGO_PKG_NAME"));
    let (true, Some(cargo), Some(manifest_dir)) = (
        run_by_cargo,
        std::env::var_os("CARGO"),
        std::env::var_os("CARGO_MANIFEST_DIR"),
    ) else {
        return;
    };
    // `target/<dir>/`: `debug` is the `dev` profile; every other profile uses its own name.
    let Some(profile_dir) = daemon.parent().and_then(Path::file_name) else {
        return;
    };
    let profile = if profile_dir == "debug" {
        OsStr::new("dev")
    } else {
        profile_dir
    };
    tracing::info!(?profile, "cargo run: building oh-my-clear-daemon");
    match Command::new(cargo)
        .args(["build", "--package", "oh-my-clear-daemon", "--profile"])
        .arg(profile)
        .current_dir(manifest_dir)
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => tracing::warn!(%status, "building oh-my-clear-daemon failed"),
        Err(err) => tracing::warn!("cannot run cargo to build oh-my-clear-daemon: {err}"),
    }
}

#[expect(
    clippy::print_stderr,
    reason = "tracing failed to initialise, stderr is the only channel left"
)]
fn report_startup_error(err: &omc_telemetry::Error) {
    eprintln!("oh-my-clear: {err}");
}
