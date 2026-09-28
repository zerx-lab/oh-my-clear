//! Bridge to `oh-my-clear-daemon` (ADR 0008): a tokio runtime (only for the omc-ipc client)
//! and the [`EngineHandle`], held by one [`Engine`] entity in a GPUI global. A single
//! foreground task drains the client's event channel and re-emits every [`ClientEvent`]
//! from the entity; views subscribe instead of owning channels.

use std::path::PathBuf;

use gpui_kit::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};
use omc_ipc::client::{ClientConfig, ClientEvent, ConnState, EngineHandle};

/// The daemon connection as seen by views.
pub struct Engine {
    handle: Option<EngineHandle>,
    state: ConnState,
    runtime: Option<tokio::runtime::Runtime>,
    _drain: Option<Task<()>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<ClientEvent> for Engine {}

impl Engine {
    /// The request handle; `None` when the engine could not be started at all.
    pub fn handle(&self) -> Option<&EngineHandle> {
        self.handle.as_ref()
    }

    /// The latest connection state.
    pub fn state(&self) -> &ConnState {
        &self.state
    }

    fn failed(reason: String) -> Self {
        Self {
            handle: None,
            state: ConnState::Failed { reason },
            runtime: None,
            _drain: None,
        }
    }

    fn started(daemon_exe: PathBuf, cx: &mut Context<'_, Self>) -> Self {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("omc-ipc")
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                tracing::error!("failed to build the IPC runtime: {err}");
                return Self::failed(err.to_string());
            }
        };
        let (handle, mut events) =
            EngineHandle::start(runtime.handle(), ClientConfig { daemon_exe });
        let state = handle.state();
        let drain = cx.spawn(async move |this, cx| {
            while let Some(first) = events.recv().await {
                // Everything already queued is applied in one update, so a burst costs
                // one render.
                let mut batch = vec![first];
                while let Ok(event) = events.try_recv() {
                    batch.push(event);
                }
                let applied = this.update(cx, |engine, cx| {
                    for event in batch {
                        let ClientEvent::State(state) = &event;
                        tracing::debug!(?state, "daemon connection");
                        engine.state = state.clone();
                        cx.emit(event);
                    }
                });
                if applied.is_err() {
                    break;
                }
            }
        });
        Self {
            handle: Some(handle),
            state,
            runtime: Some(runtime),
            _drain: Some(drain),
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

struct GlobalEngine(Entity<Engine>);

impl Global for GlobalEngine {}

/// Starts the daemon connection (spawning `daemon_exe` when no daemon answers). Call
/// once, after [`crate::init`] and before opening the main window. Failures leave the
/// engine in [`ConnState::Failed`]; they never abort the UI.
pub fn start(daemon_exe: PathBuf, cx: &mut App) {
    let engine = cx.new(|cx| Engine::started(daemon_exe, cx));
    cx.set_global(GlobalEngine(engine));
}

/// Records that the engine could not be configured (e.g. the executable path is unknown).
pub fn unavailable(reason: String, cx: &mut App) {
    let engine = cx.new(|_| Engine::failed(reason));
    cx.set_global(GlobalEngine(engine));
}

/// The engine entity; installs an unavailable one when [`start`] was never called
/// (headless tests).
pub(crate) fn entity(cx: &mut App) -> Entity<Engine> {
    if let Some(engine) = cx.try_global::<GlobalEngine>() {
        return engine.0.clone();
    }
    let engine = cx.new(|_| Engine::failed("engine not started".to_owned()));
    cx.set_global(GlobalEngine(engine.clone()));
    engine
}
