//! The main thread of `run` (ADR 0020): runs the daemon loop ([`crate::serve`]) on the tokio
//! runtime and owns the tray for its whole life. The tray appears only once the daemon owns
//! the runtime dir (a daemon that loses the single-instance race never shows one) and goes
//! away before the process exits.
//!
//! - macOS / Windows: a tao event loop (`AppKit` run loop / Win32 message pump), which the
//!   status item and the notification icon need on this thread. It sleeps until the OS or
//!   the daemon wakes it (`ControlFlow::Wait`); on macOS the daemon is an accessory app:
//!   no Dock icon, and it never takes focus when it starts.
//! - Linux / BSD: no event loop: tray-icon's `StatusNotifierItem` runs on its own D-Bus
//!   thread, so this thread just drives the daemon loop.
//!
//! No tray host (a Linux session without a `StatusNotifierItem` watcher, D-Bus errors) is not
//! fatal: the daemon runs without a tray and exits when idle (unless automation rules keep it
//! running).
//!
//! The pending automation runs reach the tray through the [`AutomationStatus`] feed the
//! daemon hands over with its ready signal; every change rebuilds the tray menu on this
//! thread.

use omc_engine::AutomationStatus;
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, watch};
use tray_icon::Icon;

use crate::Result;
use crate::serve::{Shell, serve};
use crate::tray::{Tray, TrayEvent};

pub(crate) use imp::run;

/// Shows the tray, or logs why there is none.
fn show(events: &mpsc::UnboundedSender<TrayEvent>, icon: Result<Icon>) -> Option<Tray> {
    match icon.and_then(|icon| Tray::show(events, icon)) {
        Ok(tray) => Some(tray),
        Err(err) => {
            tracing::warn!("no tray ({err}); the daemon exits when idle");
            None
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod imp {
    use tao::event::Event;
    use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
    #[cfg(target_os = "macos")]
    use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS as _};
    use tao::platform::run_return::EventLoopExtRunReturn as _;

    use omc_engine::PendingRun;

    use super::{AutomationStatus, Icon, Result, Runtime, Shell, mpsc, serve, show, watch};

    /// Wakes the host thread from the daemon loop.
    #[derive(Debug)]
    enum HostEvent {
        /// The daemon is serving: show the tray.
        Ready,
        /// The runs waiting for the user changed: rebuild the tray menu.
        Pending(Vec<PendingRun>),
        /// The daemon loop ended: remove the tray and return from `run`.
        Exit,
    }

    /// Runs the daemon with its tray until the daemon loop ends.
    pub(crate) fn run(runtime: &Runtime) -> Result<()> {
        let mut event_loop = EventLoopBuilder::<HostEvent>::with_user_event().build();
        #[cfg(target_os = "macos")]
        {
            event_loop.set_activation_policy(ActivationPolicy::Accessory);
            event_loop.set_activate_ignoring_other_apps(false);
        }
        let (tray_events, tray) = mpsc::unbounded_channel();
        let ready = event_loop.create_proxy();
        let shell = Shell {
            on_ready: Box::new(move |feed| {
                post(&ready, HostEvent::Ready);
                tokio::spawn(forward_pending(feed, ready));
            }),
            tray,
        };
        let daemon = runtime.spawn(serve(shell));
        let exit = event_loop.create_proxy();
        // Also after a panic in the daemon loop, so the event loop never outlives it.
        let finished = runtime.spawn(async move {
            let result = daemon.await;
            post(&exit, HostEvent::Exit);
            result?
        });

        let mut tray = None;
        event_loop.run_return(|event, target, control_flow| {
            *control_flow = ControlFlow::Wait;
            match event {
                Event::UserEvent(HostEvent::Ready) => {
                    tray = show(&tray_events, icon(target));
                }
                Event::UserEvent(HostEvent::Pending(pending)) => {
                    if let Some(tray) = &tray {
                        tray.set_pending(&pending);
                    }
                }
                Event::UserEvent(HostEvent::Exit) => {
                    tray = None;
                    *control_flow = ControlFlow::Exit;
                }
                _ => {}
            }
        });
        drop(tray);
        runtime.block_on(finished)?
    }

    /// Posts the pending runs to the host thread whenever they change (and once at the start).
    async fn forward_pending(
        mut feed: watch::Receiver<AutomationStatus>,
        proxy: EventLoopProxy<HostEvent>,
    ) {
        let mut last: Option<Vec<PendingRun>> = None;
        loop {
            let pending = feed.borrow_and_update().pending.clone();
            if last.as_ref() != Some(&pending) {
                last = Some(pending.clone());
                post(&proxy, HostEvent::Pending(pending));
            }
            if feed.changed().await.is_err() {
                return;
            }
        }
    }

    fn post(proxy: &EventLoopProxy<HostEvent>, event: HostEvent) {
        if proxy.send_event(event).is_err() {
            tracing::debug!("host event loop already closed");
        }
    }

    #[cfg(target_os = "macos")]
    fn icon(_: &EventLoopWindowTarget<HostEvent>) -> Result<Icon> {
        crate::tray::icon()
    }

    #[cfg(target_os = "windows")]
    fn icon(target: &EventLoopWindowTarget<HostEvent>) -> Result<Icon> {
        let scale = target
            .primary_monitor()
            .map_or(1.0, |monitor| monitor.scale_factor());
        crate::tray::icon(scale)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod imp {
    use tokio::sync::oneshot;

    use super::{AutomationStatus, Result, Runtime, Shell, mpsc, serve, show, watch};
    use crate::tray::Tray;

    /// Runs the daemon with its tray until the daemon loop ends.
    pub(crate) fn run(runtime: &Runtime) -> Result<()> {
        let (tray_events, tray) = mpsc::unbounded_channel();
        let (ready, is_ready) = oneshot::channel();
        let shell = Shell {
            on_ready: Box::new(move |feed| {
                if ready.send(feed).is_err() {
                    tracing::debug!("host stopped waiting for the daemon");
                }
            }),
            tray,
        };
        // `block_on` keeps the (`!Send`) tray on this thread.
        runtime.block_on(async move {
            let mut daemon = std::pin::pin!(serve(shell));
            let (tray, feed) = tokio::select! {
                result = &mut daemon => return result,
                ready = is_ready => match ready {
                    Ok(feed) => (show(&tray_events, crate::tray::icon()), Some(feed)),
                    Err(_) => (None, None),
                },
            };
            let result = match (&tray, feed) {
                (Some(tray), Some(feed)) => tokio::select! {
                    result = &mut daemon => result,
                    () = follow_pending(tray, feed) => daemon.await,
                },
                _ => daemon.await,
            };
            drop(tray);
            result
        })
    }

    /// Keeps the tray menu in step with the pending runs until the feed ends.
    async fn follow_pending(tray: &Tray, mut feed: watch::Receiver<AutomationStatus>) {
        loop {
            let pending = feed.borrow_and_update().pending.clone();
            tray.set_pending(&pending);
            if feed.changed().await.is_err() {
                return;
            }
        }
    }
}
