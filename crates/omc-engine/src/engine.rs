//! [`Engine`]: serves authenticated connections, tracks live clients and shutdown, routes
//! requests to the settings store and the job manager, and pushes [`Event`]s to attached
//! UIs.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use omc_ipc::frame;
use omc_ipc::server::Connection;
use omc_proto::settings::Settings;
use omc_proto::{
    ClientFrame, ClientKind, ErrorCode, Event, Request, Response, RpcError, ServerFrame,
};
use tokio::io::AsyncWrite;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::jobs::Jobs;
use crate::rules::{Automation, Zone};
use crate::settings::SettingsStore;
use crate::{Error, Result};

/// Answers queued per connection before senders wait.
const OUTBOX: usize = 256;
/// How long a `shutdown` request waits for its answer to reach the socket.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(1);
/// UI events buffered per connection. Job progress (≤ 10 per second per job) dominates; a
/// lagging UI only misses stale progress and asks `job_status` for the rest.
const UI_EVENTS: usize = 256;

/// An item for a connection's writer task.
#[derive(Debug)]
enum Outbound {
    /// Write this frame.
    Frame(Box<ServerFrame>),
    /// Signal once every earlier frame has been written.
    Flush(oneshot::Sender<()>),
}

/// The daemon core. Cheap to clone; clones share state.
#[derive(Debug, Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    clients: watch::Sender<usize>,
    ui_clients: watch::Sender<usize>,
    ui_events: broadcast::Sender<Event>,
    shutdown: watch::Sender<bool>,
    settings: SettingsStore,
    jobs: Jobs,
    automation: Automation,
}

/// How an [`Engine`] is set up.
#[derive(Debug, Clone, Default)]
pub struct EngineConfig {
    /// Where settings are persisted (see [`crate::default_settings_path`]); `None` keeps
    /// them in memory only.
    pub settings_path: Option<PathBuf>,
    /// Where automation rules and their history are persisted (see
    /// [`crate::default_rules_path`]); `None` keeps them in memory only.
    pub rules_path: Option<PathBuf>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// An engine with in-memory settings and rules.
    pub fn new() -> Self {
        Self::with_config(EngineConfig::default())
    }

    /// An engine per `config`. Reads the settings and rules files (blocking; call at
    /// startup). Rules run only after [`Self::start_automation`].
    pub fn with_config(config: EngineConfig) -> Self {
        Self::with_zone(config, Zone::Local)
    }

    /// [`Self::with_config`] with rules scheduled in `zone` (tests use a fixed offset).
    pub(crate) fn with_zone(config: EngineConfig, zone: Zone) -> Self {
        let settings = config
            .settings_path
            .map_or_else(SettingsStore::in_memory, SettingsStore::load);
        Self {
            inner: Arc::new(Inner {
                clients: watch::Sender::new(0),
                ui_clients: watch::Sender::new(0),
                ui_events: broadcast::Sender::new(UI_EVENTS),
                shutdown: watch::Sender::new(false),
                settings,
                jobs: Jobs::default(),
                automation: Automation::load(config.rules_path, zone),
            }),
        }
    }

    pub(crate) fn settings(&self) -> &SettingsStore {
        &self.inner.settings
    }

    pub(crate) fn jobs(&self) -> &Jobs {
        &self.inner.jobs
    }

    pub(crate) fn automation(&self) -> &Automation {
        &self.inner.automation
    }

    /// Validates and persists `settings` on a blocking thread, then tells every UI.
    async fn put_settings(&self, settings: Settings) -> Result<(), RpcError> {
        let engine = self.clone();
        match tokio::task::spawn_blocking(move || engine.settings().put(settings)).await {
            Ok(Ok(())) => {
                tracing::info!("settings saved");
                self.notify_ui(Event::SettingsChanged);
                Ok(())
            }
            Ok(Err(err @ Error::FileIo { .. })) => {
                tracing::warn!(%err, "cannot save settings");
                Err(RpcError::new(ErrorCode::Io, err.to_string()))
            }
            Ok(Err(err)) => Err(RpcError::new(ErrorCode::Internal, err.to_string())),
            Err(err) => Err(RpcError::new(
                ErrorCode::Internal,
                format!("saving settings: {err}"),
            )),
        }
    }

    /// Number of connections currently being served; changes are observable for idle
    /// tracking.
    pub fn clients(&self) -> watch::Receiver<usize> {
        self.inner.clients.subscribe()
    }

    /// Number of UI connections currently being served.
    pub fn ui_clients(&self) -> watch::Receiver<usize> {
        self.inner.ui_clients.subscribe()
    }

    /// Queues `event` for every attached UI. Returns how many UIs will receive it (0 when
    /// none is attached).
    pub fn notify_ui(&self, event: Event) -> usize {
        tracing::debug!(?event, "event for attached UIs");
        self.inner.ui_events.send(event).unwrap_or(0)
    }

    /// Asks the daemon to exit (what a `shutdown` request does).
    pub fn request_shutdown(&self) {
        self.inner.shutdown.send_replace(true);
    }

    /// Resolves once shutdown was requested.
    pub async fn shutdown_requested(&self) {
        let mut rx = self.inner.shutdown.subscribe();
        if rx.wait_for(|requested| *requested).await.is_err() {
            // Unreachable while `self` holds the sender; treat it as a request.
            tracing::debug!("shutdown channel closed");
        }
    }

    /// Serves one authenticated connection until the client disconnects. `Err` when a frame could not be read (I/O error or malformed frame).
    pub async fn serve(&self, conn: Connection) -> Result<()> {
        let _client = ClientGuard::enter(&self.inner.clients);
        let Connection {
            mut reader,
            writer,
            client,
        } = conn;
        tracing::info!(?client, "client attached");
        let (out, outbox) = mpsc::channel(OUTBOX);
        tokio::spawn(write_frames(writer, outbox));
        // Subscribed before the first await, so an event sent once this UI counts as
        // attached reaches it.
        let (_ui, events) = if client == ClientKind::Ui {
            let forward = forward_events(self.inner.ui_events.subscribe(), out.clone());
            (
                Some(ClientGuard::enter(&self.inner.ui_clients)),
                Some(AbortOnDrop(tokio::spawn(forward))),
            )
        } else {
            (None, None)
        };
        let session = Session {
            engine: self.clone(),
            out,
        };
        let result = loop {
            match frame::read::<_, ClientFrame>(&mut reader).await {
                Ok(Some(ClientFrame::Req { id, req })) => {
                    if !session.handle(id, req).await {
                        break Ok(());
                    }
                }
                Ok(None) => break Ok(()),
                Err(err) => break Err(err.into()),
            }
        };
        drop(events);
        tracing::info!(?client, "client detached");
        result
    }
}

/// Counts a served connection for its lifetime.
struct ClientGuard<'a>(&'a watch::Sender<usize>);

impl<'a> ClientGuard<'a> {
    fn enter(clients: &'a watch::Sender<usize>) -> Self {
        clients.send_modify(|n| *n = n.saturating_add(1));
        Self(clients)
    }
}

impl Drop for ClientGuard<'_> {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n = n.saturating_sub(1));
    }
}

/// Aborts the task when dropped: the event forwarder holds an outbox sender, which would
/// otherwise keep the connection's writer alive after the client left.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Forwards broadcast UI events into one connection's outbox.
async fn forward_events(mut events: broadcast::Receiver<Event>, out: mpsc::Sender<Outbound>) {
    loop {
        let ev = match events.recv().await {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!(missed, "UI lagged behind daemon events");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if out
            .send(Outbound::Frame(Box::new(ServerFrame::Event { ev })))
            .await
            .is_err()
        {
            return;
        }
    }
}

/// Per-connection request state.
struct Session {
    engine: Engine,
    out: mpsc::Sender<Outbound>,
}

impl Session {
    /// Handles one request. `false` once the connection cannot be answered any more.
    async fn handle(&self, id: u64, req: Request) -> bool {
        let engine = &self.engine;
        let res = match req {
            Request::Hello(_) => Err(RpcError::new(
                ErrorCode::BadRequest,
                "hello is only valid as the first frame",
            )),
            Request::Ping { nonce } => Ok(Response::Pong { nonce }),
            Request::Shutdown => return self.shutdown(id).await,
            Request::SystemInfo => tokio::task::spawn_blocking(omc_apps::system_info)
                .await
                .map(Response::SystemInfo)
                .map_err(|err| RpcError::new(ErrorCode::Internal, format!("system info: {err}"))),
            Request::GetSettings => Ok(Response::Settings(engine.settings().get())),
            Request::PutSettings(settings) => {
                engine.put_settings(settings).await.map(|()| Response::Unit)
            }
            Request::StartJob(spec) => engine
                .start_job(spec)
                .await
                .map(|job| Response::Job { job }),
            Request::JobStatus { job } => engine.jobs().status(job).map(Response::JobStatus),
            Request::JobResult { job } => engine.jobs().result(job).map(Response::JobResult),
            Request::CancelJob { job } => engine.jobs().cancel(job).map(|()| Response::Unit),
            Request::ReleaseJob { job } => engine.jobs().release(job).map(|()| Response::Unit),
            Request::SpaceChildren { job, node } => engine
                .jobs()
                .space_children(job, node)
                .map(Response::SpaceNodes),
            Request::ListRules => Ok(Response::Rules(engine.list_rules())),
            Request::PutRule(rule) => engine.put_rule(rule).await.map(Response::Rule),
            Request::DeleteRule { id } => engine.delete_rule(id).await.map(|()| Response::Unit),
            Request::RunRule { id } => engine.run_rule(id).await.map(Response::Run),
            Request::ListRuns => Ok(Response::Runs(engine.list_runs())),
            Request::GetRun { id } => engine.get_run(id).map(Response::Run),
            Request::DecideRun { id, decision } => engine
                .decide_run(id, decision)
                .await
                .map(|()| Response::Unit),
        };
        self.reply(id, res).await
    }

    async fn reply(&self, id: u64, res: Result<Response, RpcError>) -> bool {
        self.out
            .send(Outbound::Frame(Box::new(ServerFrame::Res { id, res })))
            .await
            .is_ok()
    }

    async fn shutdown(&self, id: u64) -> bool {
        tracing::info!("shutdown requested by a client");
        let answered = self.reply(id, Ok(Response::Unit)).await;
        if answered {
            let (done, flushed) = oneshot::channel();
            if self.out.send(Outbound::Flush(done)).await.is_ok() {
                match tokio::time::timeout(FLUSH_TIMEOUT, flushed).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => tracing::debug!("writer stopped before the shutdown answer"),
                    Err(_) => tracing::warn!("shutdown answer not flushed in time"),
                }
            }
        }
        self.engine.request_shutdown();
        answered
    }
}

/// Owns the connection's write half: the only place frames are written. An answer too
/// large for one frame is replaced by an `internal` error (nothing was written yet), so the
/// connection survives; an oversized event is dropped.
async fn write_frames<W: AsyncWrite + Unpin>(mut writer: W, mut outbox: mpsc::Receiver<Outbound>) {
    while let Some(item) = outbox.recv().await {
        match item {
            Outbound::Frame(msg) => {
                let written = match frame::write(&mut writer, &msg).await {
                    Err(omc_ipc::Error::FrameTooLarge(len)) => {
                        tracing::warn!(len, "outgoing frame exceeds the frame limit");
                        match *msg {
                            ServerFrame::Res { id, .. } => {
                                let res = Err(RpcError::new(
                                    ErrorCode::Internal,
                                    format!("answer of {len} bytes exceeds the frame limit"),
                                ));
                                frame::write(&mut writer, &ServerFrame::Res { id, res }).await
                            }
                            ServerFrame::Event { .. } => Ok(()),
                        }
                    }
                    other => other,
                };
                if let Err(err) = written {
                    tracing::debug!(%err, "client writer stopped");
                    return;
                }
            }
            Outbound::Flush(done) => {
                if done.send(()).is_err() {
                    tracing::debug!("flush waiter gone");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use omc_proto::files::{SpaceKind, SpaceListing};
    use omc_proto::jobs::{
        CleanSpec, DeleteMethod, JobId, JobOutput, JobSpec, JobState, Phase, ScanArea,
    };
    use omc_proto::{Hello, PROTOCOL, Welcome};
    use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

    use super::*;

    const TOKEN: &str = "test-token";
    const WAIT: Duration = Duration::from_secs(5);

    type TestResult<T = ()> = Result<T, String>;
    /// The daemon's answer to one request.
    type Answer = Result<Response, RpcError>;

    fn hello(client: ClientKind) -> Request {
        Request::Hello(Hello {
            protocol: PROTOCOL,
            build: "test".to_owned(),
            token: TOKEN.to_owned(),
            client,
        })
    }

    /// Test client speaking the wire protocol over an in-memory duplex.
    struct Client {
        reader: ReadHalf<DuplexStream>,
        writer: WriteHalf<DuplexStream>,
        next_id: u64,
        /// Events that arrived while [`Self::call`] waited for an answer.
        events: std::collections::VecDeque<Event>,
    }

    impl Client {
        async fn connect(engine: &Engine, kind: ClientKind) -> TestResult<Self> {
            let (client, server) = tokio::io::duplex(1 << 16);
            let engine = engine.clone();
            tokio::spawn(async move {
                let welcome = Welcome {
                    protocol: PROTOCOL,
                    build: "test".to_owned(),
                    epoch: "epoch".to_owned(),
                    pid: 1,
                };
                let conn = omc_ipc::server::handshake(Box::new(server), TOKEN, &welcome).await;
                if let Ok(conn) = conn {
                    let result = engine.serve(conn).await;
                    assert!(result.is_ok(), "serve ended with {result:?}");
                }
            });
            let (reader, writer) = tokio::io::split(client);
            let mut client = Self {
                reader,
                writer,
                next_id: 0,
                events: std::collections::VecDeque::new(),
            };
            match client.request(hello(kind)).await? {
                Ok(Response::Welcome(_)) => Ok(client),
                other => Err(format!("handshake answered {other:?}")),
            }
        }

        async fn request(&mut self, req: Request) -> TestResult<Answer> {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1);
            frame::write(&mut self.writer, &ClientFrame::Req { id, req })
                .await
                .map_err(|err| format!("write request: {err}"))?;
            match self.next_frame().await? {
                ServerFrame::Res { id: got, res } if got == id => Ok(res),
                other => Err(format!("expected the answer to {id}, got {other:?}")),
            }
        }

        async fn next_frame(&mut self) -> TestResult<ServerFrame> {
            let next = tokio::time::timeout(WAIT, frame::read::<_, ServerFrame>(&mut self.reader));
            match next.await {
                Ok(Ok(Some(frame))) => Ok(frame),
                other => Err(format!("expected a frame, got {other:?}")),
            }
        }

        /// Like [`Self::request`], but buffers events that arrive before the answer.
        async fn call(&mut self, req: Request) -> TestResult<Answer> {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1);
            frame::write(&mut self.writer, &ClientFrame::Req { id, req })
                .await
                .map_err(|err| format!("write request: {err}"))?;
            loop {
                match self.next_frame().await? {
                    ServerFrame::Res { id: got, res } if got == id => return Ok(res),
                    ServerFrame::Event { ev } => self.events.push_back(ev),
                    other @ ServerFrame::Res { .. } => {
                        return Err(format!("expected the answer to {id}, got {other:?}"));
                    }
                }
            }
        }

        /// The next event (buffered first).
        async fn next_event(&mut self) -> TestResult<Event> {
            if let Some(ev) = self.events.pop_front() {
                return Ok(ev);
            }
            match self.next_frame().await? {
                ServerFrame::Event { ev } => Ok(ev),
                other @ ServerFrame::Res { .. } => Err(format!("expected an event, got {other:?}")),
            }
        }

        /// Consumes `job` events up to its final update; returns the running updates seen
        /// and the final state.
        async fn job_end(&mut self, job: JobId) -> TestResult<(usize, JobState)> {
            let mut running = 0_usize;
            loop {
                match self.next_event().await? {
                    Event::Job(update) if update.job == job => {
                        if update.status.state.is_finished() {
                            return Ok((running, update.status.state));
                        }
                        running = running.saturating_add(1);
                    }
                    _ => {}
                }
            }
        }

        /// Starts `spec`; the new job's id.
        async fn start(&mut self, spec: JobSpec) -> TestResult<JobId> {
            match self.call(Request::StartJob(spec)).await? {
                Ok(Response::Job { job }) => Ok(job),
                other => Err(format!("start_job answered {other:?}")),
            }
        }
    }

    /// `assert!` for test fns returning [`TestResult`] (`panic_in_result_fn` is denied).
    macro_rules! ensure {
        ($cond:expr, $($msg:tt)+) => {
            if !$cond {
                return Err(format!($($msg)+));
            }
        };
    }

    /// `assert_eq!` for test fns returning [`TestResult`].
    macro_rules! ensure_eq {
        ($left:expr, $right:expr, $($msg:tt)+) => {{
            let (left, right) = (&$left, &$right);
            if left != right {
                return Err(format!("{}: {left:?} != {right:?}", format!($($msg)+)));
            }
        }};
    }

    #[tokio::test]
    async fn meta_requests() -> TestResult {
        let engine = Engine::new();
        let mut clients = engine.clients();
        let mut client = Client::connect(&engine, ClientKind::Ui).await?;
        ensure_eq!(*clients.borrow_and_update(), 1, "one client attached");
        ensure_eq!(
            client.request(Request::Ping { nonce: 42 }).await?,
            Ok(Response::Pong { nonce: 42 }),
            "ping echoes the nonce"
        );
        ensure_eq!(
            client
                .request(hello(ClientKind::Ui))
                .await?
                .err()
                .map(|err| err.code),
            Some(ErrorCode::BadRequest),
            "second hello"
        );
        ensure_eq!(
            client.request(Request::Shutdown).await?,
            Ok(Response::Unit),
            "shutdown is answered"
        );
        let stopped = tokio::time::timeout(WAIT, engine.shutdown_requested()).await;
        ensure!(stopped.is_ok(), "shutdown is signalled after the answer");

        drop(client);
        let idle = tokio::time::timeout(WAIT, clients.wait_for(|n| *n == 0)).await;
        ensure!(
            matches!(idle, Ok(Ok(_))),
            "client count drops when it disconnects"
        );
        Ok(())
    }

    #[tokio::test]
    async fn ui_events_reach_only_attached_uis() -> TestResult {
        let engine = Engine::new();
        let mut uis = engine.ui_clients();
        ensure_eq!(
            engine.notify_ui(Event::Activate),
            0,
            "nobody to notify before a UI attaches"
        );

        let mut cli = Client::connect(&engine, ClientKind::Cli).await?;
        ensure_eq!(*uis.borrow_and_update(), 0, "a CLI client is not a UI");
        ensure_eq!(
            engine.notify_ui(Event::Activate),
            0,
            "a CLI client gets no events"
        );
        ensure_eq!(
            cli.request(Request::Ping { nonce: 1 }).await?,
            Ok(Response::Pong { nonce: 1 }),
            "the CLI's next frame is its answer, not an event"
        );

        let mut ui = Client::connect(&engine, ClientKind::Ui).await?;
        ensure_eq!(*uis.borrow_and_update(), 1, "one UI attached");
        ensure_eq!(engine.notify_ui(Event::Quit), 1, "the UI is notified");
        ensure_eq!(
            ui.next_frame().await?,
            ServerFrame::Event { ev: Event::Quit },
            "the event is pushed unsolicited"
        );
        ensure_eq!(
            ui.request(Request::Ping { nonce: 2 }).await?,
            Ok(Response::Pong { nonce: 2 }),
            "requests keep working after an event"
        );

        drop(ui);
        let detached = tokio::time::timeout(WAIT, uis.wait_for(|n| *n == 0)).await;
        ensure!(
            matches!(detached, Ok(Ok(_))),
            "the UI count drops when it disconnects"
        );
        ensure_eq!(
            engine.notify_ui(Event::Activate),
            0,
            "a detached UI no longer receives events"
        );
        Ok(())
    }

    /// A fresh, unique directory under the system temp dir.
    fn temp_dir(name: &str) -> TestResult<PathBuf> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "omc-engine-{name}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|err| format!("clear {dir:?}: {err}"))?;
        }
        std::fs::create_dir_all(&dir).map_err(|err| format!("create {dir:?}: {err}"))?;
        Ok(dir)
    }

    fn write_file(path: &Path, len: usize) -> TestResult {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| format!("mkdir {parent:?}: {err}"))?;
        }
        std::fs::write(path, vec![7_u8; len]).map_err(|err| format!("write {path:?}: {err}"))
    }

    /// Settings for filesystem tests: permanent deletion (never the user's Trash).
    async fn permanent_deletes(client: &mut Client) -> TestResult {
        let mut settings = Settings::default();
        settings.clean.files_delete = DeleteMethod::Permanent;
        settings.clean.junk_delete = DeleteMethod::Permanent;
        ensure_eq!(
            client.call(Request::PutSettings(settings)).await?,
            Ok(Response::Unit),
            "settings accepted"
        );
        Ok(())
    }

    async fn space_scan(client: &mut Client, root: &Path) -> TestResult<(JobId, SpaceListing)> {
        let job = client
            .start(JobSpec::Scan(ScanArea::SpaceLens {
                root: root.display().to_string(),
            }))
            .await?;
        let (_, state) = client.job_end(job).await?;
        ensure_eq!(state, JobState::Done, "space scan finishes");
        match client.call(Request::JobResult { job }).await? {
            Ok(Response::JobResult(JobOutput::Space(listing))) => Ok((job, listing)),
            other => Err(format!("job_result answered {other:?}")),
        }
    }

    fn code(answer: &Answer) -> Option<ErrorCode> {
        answer.as_ref().err().map(|err| err.code)
    }

    #[tokio::test]
    async fn settings_persist_and_reload() -> TestResult {
        let dir = temp_dir("settings")?;
        let path = dir.join("nested").join("settings.toml");
        let config = EngineConfig {
            settings_path: Some(path.clone()),
            ..EngineConfig::default()
        };
        let engine = Engine::with_config(config.clone());
        let mut ui = Client::connect(&engine, ClientKind::Ui).await?;
        ensure_eq!(
            ui.call(Request::GetSettings).await?,
            Ok(Response::Settings(Settings::default())),
            "a missing file gives defaults"
        );

        let mut wanted = Settings::default();
        wanted.clean.scan_threads = 4;
        wanted.clean.exclude = vec!["~/Keep".to_owned()];
        wanted.ui.insert("theme".to_owned(), "dark".to_owned());
        let mut sent = wanted.clone();
        sent.clean.exclude.push("   ".to_owned());
        sent.clean.dev_project_max_depth = 0;
        wanted.clean.dev_project_max_depth = 1;
        ensure_eq!(
            ui.call(Request::PutSettings(sent)).await?,
            Ok(Response::Unit),
            "put is accepted"
        );
        ensure_eq!(
            ui.next_event().await?,
            Event::SettingsChanged,
            "UIs hear about the change"
        );
        ensure_eq!(
            ui.call(Request::GetSettings).await?,
            Ok(Response::Settings(wanted.clone())),
            "get returns the sanitized settings"
        );
        ensure!(path.is_file(), "the settings file is written");

        let reloaded = Engine::with_config(config.clone());
        let mut cli = Client::connect(&reloaded, ClientKind::Cli).await?;
        ensure_eq!(
            cli.call(Request::GetSettings).await?,
            Ok(Response::Settings(wanted)),
            "a new engine loads the saved settings"
        );

        std::fs::write(&path, "clean = [not toml").map_err(|err| format!("corrupt: {err}"))?;
        let recovered = Engine::with_config(config);
        let mut cli = Client::connect(&recovered, ClientKind::Cli).await?;
        ensure_eq!(
            cli.call(Request::GetSettings).await?,
            Ok(Response::Settings(Settings::default())),
            "a corrupt file gives defaults"
        );
        let backup = std::fs::read_to_string(path.with_extension("toml.bak"));
        ensure!(
            backup
                .as_deref()
                .is_ok_and(|text| text == "clean = [not toml"),
            "the corrupt file is kept as .bak: {backup:?}"
        );
        let _ignored = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[tokio::test]
    async fn space_lens_scan_drill_down_and_clean() -> TestResult {
        let root = temp_dir("space")?;
        // Files of at least 64 KiB get their own space-lens node; smaller ones go to `rest`.
        write_file(&root.join("a").join("one.bin"), 256 << 10)?;
        write_file(&root.join("b").join("two.bin"), 16 << 10)?;
        write_file(&root.join("b").join("three.bin"), 16 << 10)?;
        write_file(&root.join("c.bin"), 128 << 10)?;

        let engine = Engine::new();
        let mut ui = Client::connect(&engine, ClientKind::Ui).await?;
        permanent_deletes(&mut ui).await?;
        let (job, listing) = space_scan(&mut ui, &root).await?;
        ensure_eq!(listing.node, 0, "the result is the root listing");
        ensure_eq!(listing.files, 4, "every file is counted");
        let child = |name: &str| listing.children.iter().find(|c| c.name == name).cloned();
        let (Some(a), Some(b), Some(c)) = (child("a"), child("b"), child("c.bin")) else {
            return Err(format!("children a, b, c.bin: {:?}", listing.children));
        };
        ensure!(
            a.kind == SpaceKind::Dir && c.kind == SpaceKind::File,
            "kinds: {a:?} {c:?}"
        );
        ensure!(a.bytes >= 256 << 10, "a holds its 256 KiB file: {a:?}");
        ensure!(
            listing.bytes >= a.bytes.saturating_add(b.bytes).saturating_add(c.bytes),
            "the root size covers its children: {listing:?}"
        );
        ensure!(
            listing.children.first().map(|n| n.id) == Some(a.id),
            "largest child first: {:?}",
            listing.children
        );

        match ui.call(Request::SpaceChildren { job, node: b.id }).await? {
            Ok(Response::SpaceNodes(level)) => {
                ensure_eq!(level.node, b.id, "the requested node is listed");
                ensure_eq!(level.files, 2, "b holds two files");
            }
            other => return Err(format!("space_children answered {other:?}")),
        }
        ensure_eq!(
            code(&ui.call(Request::SpaceChildren { job, node: 9_999 }).await?),
            Some(ErrorCode::NotFound),
            "unknown node"
        );

        let clean = ui
            .start(JobSpec::Clean(CleanSpec {
                scan_job: job,
                items: vec![a.id],
            }))
            .await?;
        let (_, state) = ui.job_end(clean).await?;
        ensure_eq!(state, JobState::Done, "clean finishes");
        match ui.call(Request::JobResult { job: clean }).await? {
            Ok(Response::JobResult(JobOutput::Clean(report))) => {
                ensure!(report.failures.is_empty(), "nothing failed: {report:?}");
                ensure_eq!(report.removed, 1, "one item removed: {report:?}");
            }
            other => return Err(format!("clean result: {other:?}")),
        }
        ensure!(
            !root.join("a").exists(),
            "the cleaned node is gone from disk"
        );
        ensure!(root.join("b").exists(), "other nodes stay");
        ensure!(
            matches!(
                ui.call(Request::JobResult { job }).await?,
                Ok(Response::JobResult(JobOutput::Space(_)))
            ),
            "the scan tree is kept after a clean"
        );

        ensure_eq!(
            ui.call(Request::ReleaseJob { job }).await?,
            Ok(Response::Unit),
            "release a finished job"
        );
        ensure_eq!(
            code(&ui.call(Request::JobResult { job }).await?),
            Some(ErrorCode::NotFound),
            "a released job is gone"
        );
        let _ignored = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[tokio::test]
    async fn queued_mutation_can_be_cancelled() -> TestResult {
        let root = temp_dir("queue")?;
        write_file(&root.join("keep.bin"), 4 << 10)?;
        let engine = Engine::new();
        let mut ui = Client::connect(&engine, ClientKind::Ui).await?;
        permanent_deletes(&mut ui).await?;
        let (scan, listing) = space_scan(&mut ui, &root).await?;
        let items = listing.children.iter().map(|n| n.id).collect();

        // Another mutation holds the permit, so this clean waits.
        let permit = engine.jobs().hold_mutation();
        ensure!(permit.is_some(), "the mutation permit is free");
        let clean = ui
            .start(JobSpec::Clean(CleanSpec {
                scan_job: scan,
                items,
            }))
            .await?;
        match ui.call(Request::JobStatus { job: clean }).await? {
            Ok(Response::JobStatus(status)) => {
                ensure_eq!(status.state, JobState::Running, "queued job is running");
                ensure_eq!(status.progress.phase, Phase::Starting, "while it waits");
            }
            other => return Err(format!("job_status answered {other:?}")),
        }
        ensure_eq!(
            code(&ui.call(Request::JobResult { job: clean }).await?),
            Some(ErrorCode::BadRequest),
            "no result while running"
        );
        ensure_eq!(
            code(&ui.call(Request::ReleaseJob { job: clean }).await?),
            Some(ErrorCode::BadRequest),
            "a running job cannot be released"
        );
        ensure_eq!(
            ui.call(Request::CancelJob { job: clean }).await?,
            Ok(Response::Unit),
            "cancel is accepted"
        );
        let (_, state) = ui.job_end(clean).await?;
        ensure_eq!(state, JobState::Cancelled, "the queued job ends cancelled");
        ensure!(
            root.join("keep.bin").exists(),
            "a cancelled clean removed nothing"
        );
        ensure_eq!(
            code(&ui.call(Request::JobResult { job: clean }).await?),
            Some(ErrorCode::BadRequest),
            "a job cancelled before work has no output"
        );
        ensure_eq!(
            ui.call(Request::ReleaseJob { job: clean }).await?,
            Ok(Response::Unit),
            "a cancelled job can be released"
        );
        ensure_eq!(
            code(&ui.call(Request::JobStatus { job: clean }).await?),
            Some(ErrorCode::NotFound),
            "released"
        );
        drop(permit);
        let _ignored = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_job_requests_are_rejected() -> TestResult {
        let engine = Engine::new();
        let mut cli = Client::connect(&engine, ClientKind::Cli).await?;
        ensure_eq!(
            code(
                &cli.call(Request::StartJob(JobSpec::Clean(CleanSpec {
                    scan_job: 77,
                    items: vec![0],
                })))
                .await?
            ),
            Some(ErrorCode::NotFound),
            "clean of an unknown job"
        );
        ensure_eq!(
            code(
                &cli.call(Request::StartJob(JobSpec::Scan(ScanArea::SpaceLens {
                    root: "relative/dir".to_owned(),
                })))
                .await?
            ),
            Some(ErrorCode::BadRequest),
            "relative space-lens root"
        );
        let missing =
            std::env::temp_dir().join(format!("omc-engine-missing-{}", std::process::id()));
        ensure_eq!(
            code(
                &cli.call(Request::StartJob(JobSpec::Scan(ScanArea::SpaceLens {
                    root: missing.display().to_string(),
                })))
                .await?
            ),
            Some(ErrorCode::BadRequest),
            "missing space-lens root"
        );
        for req in [
            Request::JobStatus { job: 5 },
            Request::JobResult { job: 5 },
            Request::CancelJob { job: 5 },
            Request::ReleaseJob { job: 5 },
            Request::SpaceChildren { job: 5, node: 0 },
        ] {
            ensure_eq!(
                code(&cli.call(req.clone()).await?),
                Some(ErrorCode::NotFound),
                "{req:?} of an unknown job"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn oldest_finished_jobs_are_evicted() -> TestResult {
        let root = temp_dir("evict")?;
        let engine = Engine::new();
        let mut ui = Client::connect(&engine, ClientKind::Ui).await?;
        let mut jobs = Vec::new();
        for _ in 0..66 {
            jobs.push(space_scan(&mut ui, &root).await?.0);
        }
        let [first, second, third, ..] = *jobs.as_slice() else {
            return Err("66 jobs".to_owned());
        };
        for (job, gone, what) in [
            (first, true, "oldest evicted"),
            (second, true, "second oldest evicted"),
            (third, false, "the newest 64 are kept"),
        ] {
            let answer = ui.call(Request::JobStatus { job }).await?;
            ensure_eq!(
                code(&answer) == Some(ErrorCode::NotFound),
                gone,
                "{what}: {answer:?}"
            );
        }
        let _ignored = std::fs::remove_dir_all(&root);
        Ok(())
    }

    mod automation;
}
