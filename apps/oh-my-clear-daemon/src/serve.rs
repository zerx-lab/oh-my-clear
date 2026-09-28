//! The daemon loop: owns the runtime-dir lock and the endpoint, accepts clients, and acts on
//! tray commands. Runs on the tokio runtime; the tray itself lives on [`crate::host`]'s
//! thread and talks to this loop only through [`Shell`].

use std::time::Duration;

use omc_engine::Engine;
use omc_ipc::server::{self, Listener};
use omc_ipc::{DaemonLock, RuntimeDir};
use omc_proto::{Event, PROTOCOL, Welcome};
use tokio::sync::{mpsc, watch};

use crate::Result;
use crate::tray::TrayEvent;
use crate::ui::UiLauncher;

/// Exit after this long with no client attached, unless the tray keeps the daemon resident.
const IDLE_EXIT: Duration = Duration::from_mins(10);
/// Back-off after a failed `accept`, so a persistent error does not spin.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);
/// How long Quit waits for attached UIs to exit before the daemon stops anyway.
const QUIT_GRACE: Duration = Duration::from_secs(3);

/// The daemon loop's link to the host thread.
pub(crate) struct Shell {
    /// Called once, after `READY`: the daemon owns the runtime dir and serves clients, so
    /// the host shows the tray. Never called when another daemon is already running.
    pub(crate) on_ready: Box<dyn FnOnce() + Send>,
    /// Tray lifecycle and user commands.
    pub(crate) tray: mpsc::UnboundedReceiver<TrayEvent>,
}

impl std::fmt::Debug for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shell").finish_non_exhaustive()
    }
}

pub(crate) async fn serve(shell: Shell) -> Result<()> {
    let Shell {
        on_ready,
        tray: mut tray_events,
    } = shell;
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
    let exe = std::env::current_exe()?;
    let build = omc_ipc::build_id(&exe);
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
    on_ready();

    let mut ui = UiLauncher::new(&exe, dir.clone());
    // With a tray the daemon stays until the user quits it; without one (no tray host) it
    // exits when idle, as a headless daemon.
    let mut resident = false;
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
            () = &mut idle, if !resident => {
                tracing::info!(idle = ?IDLE_EXIT, "no clients; exiting");
                break;
            }
            Some(event) = tray_events.recv() => match event {
                TrayEvent::Shown => {
                    tracing::info!("tray shown; staying resident while no UI is attached");
                    resident = true;
                }
                TrayEvent::Open => ui.open(&engine),
                TrayEvent::Quit => {
                    tracing::info!("quit from the tray");
                    quit_uis(&engine).await;
                    break;
                }
            },
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

/// Tells every attached UI to exit and waits (bounded) until they detached, so none of
/// them sees the daemon vanish first and respawns it.
async fn quit_uis(engine: &Engine) {
    let mut uis = engine.ui_clients();
    let notified = engine.notify_ui(Event::Quit);
    if notified == 0 {
        return;
    }
    let detached = tokio::time::timeout(QUIT_GRACE, uis.wait_for(|n| *n == 0)).await;
    if detached.is_ok() {
        tracing::info!(notified, "every UI exited");
    } else {
        tracing::warn!(notified, "UIs still attached after quit; stopping anyway");
    }
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
