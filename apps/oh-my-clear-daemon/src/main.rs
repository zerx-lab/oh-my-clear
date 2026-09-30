//! oh-my-clear-daemon: execution layer (ADR 0008) and owner of the system tray (ADR 0020).
//! Runs the engine on one tokio runtime; UI processes attach over omc-ipc and may come and
//! go. Never links gpui (checked by `cargo xtask layers`).
//!
//! Subcommands:
//! - `run` (default): single-instance daemon. Takes the runtime-dir lock (another daemon
//!   holds it → exit 0), binds the endpoint, prints the one `READY` stdout line, shows the
//!   tray, then serves clients through [`omc_engine::Engine`] until the tray's Quit, a
//!   `shutdown` request, or SIGTERM/SIGINT. Without a tray (no `StatusNotifierItem` host on
//!   Linux) it also exits after 10 minutes without any client. SIGHUP is ignored so closing
//!   a terminal does not stop it.
//! - `stop`: asks a running daemon to shut down; exits 0 when none is running.
//! - `elevated <manifest>`: the administrator helper the daemon starts through the OS
//!   prompt (`osascript`, `pkexec`, UAC) to remove items that need administrator rights.
//!   No tray, lock or IPC; logs to stderr; exit code 0 when the result file was written.
//!
//! Modules: [`serve`] (the daemon loop), [`host`] (the main thread: event loop + tray
//! lifetime), [`tray`] (icon, menu incl. pending automation runs, events), [`ui`] (launching /
//! activating the GUI; prompting for automation runs, ADR 0024).

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use omc_ipc::RuntimeDir;
use omc_proto::{ClientKind, Request};

mod host;
mod locale;
mod serve;
mod tray;
mod ui;

rust_i18n::i18n!("locales", fallback = "en");

/// Grace period for in-flight tasks once the daemon decided to exit.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

const USAGE: &str = "usage: oh-my-clear-daemon [run|stop|elevated <manifest>]";

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("ipc: {0}")]
    Ipc(#[from] omc_ipc::Error),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("daemon task: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("tray: {0}")]
    Tray(#[from] tray_icon::Error),
    #[error("tray menu: {0}")]
    Menu(#[from] tray_icon::menu::Error),
    #[error("tray icon: {0}")]
    Icon(#[from] tray_icon::BadIcon),
    #[cfg(not(windows))]
    #[error("tray icon image: {0}")]
    Image(#[from] image::ImageError),
}

type Result<T, E = Error> = std::result::Result<T, E>;

fn main() -> ExitCode {
    if let Err(err) = omc_telemetry::init() {
        report_startup_error(&err);
        return ExitCode::FAILURE;
    }
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let run = match args.as_slice() {
        [] => true,
        [command] if command == "run" => true,
        [command] if command == "stop" => false,
        [command, manifest] if command == "elevated" => return elevated(Path::new(manifest)),
        [command] if command == "elevated" => {
            tracing::error!("missing manifest path; {USAGE}");
            return ExitCode::from(2);
        }
        [command, .., extra] if command == "run" || command == "stop" || command == "elevated" => {
            tracing::error!(extra = %extra.to_string_lossy(), "unexpected argument; {USAGE}");
            return ExitCode::from(2);
        }
        [command, ..] => {
            tracing::error!(command = %command.to_string_lossy(), "unknown subcommand; {USAGE}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("oh-my-clear-daemon")
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!(%err, "cannot start the tokio runtime");
            return ExitCode::FAILURE;
        }
    };
    let result = if run {
        locale::apply();
        host::run(&runtime)
    } else {
        runtime.block_on(stop())
    };
    runtime.shutdown_timeout(SHUTDOWN_GRACE);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(%err, "oh-my-clear-daemon failed");
            ExitCode::FAILURE
        }
    }
}

/// `elevated <manifest>`: runs as administrator/root, removes what the manifest lists.
fn elevated(manifest: &Path) -> ExitCode {
    match omc_apps::serve_elevated(manifest) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(%err, manifest = %manifest.display(), "elevated helper failed");
            ExitCode::FAILURE
        }
    }
}

async fn stop() -> Result<()> {
    let dir = RuntimeDir::resolve()?;
    let Some(mut session) = omc_ipc::client::connect_existing(&dir, ClientKind::Cli).await? else {
        tracing::info!("oh-my-clear-daemon is not running");
        return Ok(());
    };
    session.request(Request::Shutdown).await?;
    tracing::info!("oh-my-clear-daemon stopped");
    Ok(())
}

#[expect(
    clippy::print_stderr,
    reason = "tracing failed to initialise, stderr is the only channel left"
)]
fn report_startup_error(err: &omc_telemetry::Error) {
    eprintln!("oh-my-clear-daemon: {err}");
}
