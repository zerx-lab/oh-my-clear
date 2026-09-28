//! oh-my-clear-daemon: headless execution layer (ADR 0008). Runs the engine on one tokio
//! runtime; UI processes attach over omc-ipc and may come and go. Never links gpui
//! (checked by `cargo xtask layers`).
//!
//! Subcommands:
//! - `run` (default): single-instance daemon. Takes the runtime-dir lock (another daemon
//!   holds it → exit 0), binds the endpoint, prints the one `READY` stdout line, then serves
//!   clients through [`omc_engine::Engine`] until a `shutdown` request, SIGTERM/SIGINT, or
//!   10 minutes without any client. SIGHUP is ignored so closing a terminal does not stop it.
//! - `stop`: asks a running daemon to shut down; exits 0 when none is running.

use std::process::ExitCode;
use std::time::Duration;

use omc_engine::Engine;
use omc_ipc::server::{self, Listener};
use omc_ipc::{DaemonLock, RuntimeDir};
use omc_proto::{ClientKind, PROTOCOL, Request, Welcome};
use tokio::sync::watch;

/// Exit after this long with no client attached (and no other work, which does not exist yet).
const IDLE_EXIT: Duration = Duration::from_mins(10);
/// Grace period for in-flight tasks once the daemon decided to exit.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);
/// Back-off after a failed `accept`, so a persistent error does not spin.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

const USAGE: &str = "usage: oh-my-clear-daemon [run|stop]";

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("ipc: {0}")]
    Ipc(#[from] omc_ipc::Error),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
}

type Result<T, E = Error> = std::result::Result<T, E>;

fn main() -> ExitCode {
    if let Err(err) = omc_telemetry::init() {
        report_startup_error(&err);
        return ExitCode::FAILURE;
    }
    let mut args = std::env::args().skip(1);
    let command = args.next();
    if let Some(extra) = args.next() {
        tracing::error!(%extra, "unexpected argument; {USAGE}");
        return ExitCode::from(2);
    }
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
    let result = match command.as_deref() {
        None | Some("run") => runtime.block_on(run()),
        Some("stop") => runtime.block_on(stop()),
        Some(other) => {
            tracing::error!(command = other, "unknown subcommand; {USAGE}");
            return ExitCode::from(2);
        }
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

async fn run() -> Result<()> {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "oh-my-clear-daemon starting"
    );
    let dir = RuntimeDir::resolve()?;
    let Some(_lock) = DaemonLock::acquire(&dir)? else {
        tracing::info!("another oh-my-clear-daemon is already running; exiting");
        return Ok(());
    };
    let epoch = omc_ipc::new_epoch();
    let build = omc_ipc::build_id(&std::env::current_exe()?);
    let mut listener = Listener::bind(&dir, &build, &epoch)?;
    let welcome = Welcome {
        protocol: PROTOCOL,
        build,
        epoch,
        pid: std::process::id(),
    };
    let engine = Engine::new();
    omc_ipc::announce_ready(&welcome.epoch)?;
    tracing::info!(epoch = %welcome.epoch, dir = %dir.path().display(), "oh-my-clear-daemon ready");

    let idle = idle_timeout(engine.clients());
    tokio::pin!(idle);
    let signals = exit_signal();
    tokio::pin!(signals);
    loop {
        tokio::select! {
            () = engine.shutdown_requested() => break,
            () = &mut signals => {
                tracing::info!("termination signal received");
                break;
            }
            () = &mut idle => {
                tracing::info!(idle = ?IDLE_EXIT, "no clients; exiting");
                break;
            }
            accepted = listener.accept() => match accepted {
                Ok(stream) => {
                    let engine = engine.clone();
                    let token = listener.token().to_owned();
                    let welcome = welcome.clone();
                    tokio::spawn(async move {
                        let conn = match server::handshake(stream, &token, &welcome).await {
                            Ok(conn) => conn,
                            Err(err) => {
                                tracing::warn!(%err, "handshake failed; dropping connection");
                                return;
                            }
                        };
                        if let Err(err) = engine.serve(conn).await {
                            tracing::warn!(%err, "connection ended with an error");
                        }
                    });
                }
                Err(err) => {
                    tracing::warn!(%err, "accept failed");
                    tokio::time::sleep(ACCEPT_RETRY).await;
                }
            },
        }
    }
    tracing::info!("oh-my-clear-daemon stopping");
    Ok(())
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

/// Resolves once no client has been attached for [`IDLE_EXIT`] without interruption.
async fn idle_timeout(mut clients: watch::Receiver<usize>) {
    loop {
        if *clients.borrow_and_update() == 0 {
            tokio::select! {
                () = tokio::time::sleep(IDLE_EXIT) => return,
                changed = clients.changed() => if changed.is_err() { return },
            }
        } else if clients.changed().await.is_err() {
            return;
        }
    }
}

/// Resolves on SIGTERM/SIGINT (Ctrl-C elsewhere); logs and ignores SIGHUP on unix.
#[cfg(unix)]
async fn exit_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let streams = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
        signal(SignalKind::hangup()),
    );
    let (Ok(mut term), Ok(mut int), Ok(mut hup)) = streams else {
        tracing::warn!("cannot install signal handlers; relying on shutdown/idle exit");
        std::future::pending::<()>().await;
        return;
    };
    loop {
        tokio::select! {
            _ = term.recv() => return,
            _ = int.recv() => return,
            _ = hup.recv() => tracing::info!("SIGHUP ignored"),
        }
    }
}

#[cfg(not(unix))]
async fn exit_signal() {
    if let Err(err) = tokio::signal::ctrl_c().await {
        tracing::warn!(%err, "cannot listen for Ctrl-C; relying on shutdown/idle exit");
        std::future::pending::<()>().await;
    }
}

#[expect(
    clippy::print_stderr,
    reason = "tracing failed to initialise, stderr is the only channel left"
)]
fn report_startup_error(err: &omc_telemetry::Error) {
    eprintln!("oh-my-clear-daemon: {err}");
}
