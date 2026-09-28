//! Client side: discover or spawn the daemon, authenticate, keep one connection alive.
//!
//! [`EngineHandle`] is the UI's handle. A supervisor task on the caller's tokio runtime
//! owns the connection: it connects via `endpoint.json`, spawns `<daemon_exe> run` when
//! nothing answers, restarts a daemon built from another executable, pings every 5 s,
//! and reconnects with a 50 ms → 2 s backoff. Requests, state changes and daemon events
//! cross into the UI through tokio channels, whose futures run on any executor. A
//! [`Event::Quit`] ends the supervisor: the user quit the app, so nothing reconnects.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use omc_proto::{
    ClientFrame, ClientKind, Event, Hello, PROTOCOL, Request, Response, ServerFrame, Welcome,
};
use tokio::io::{AsyncBufReadExt as _, BufReader, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::runtime::{Endpoint, READY_PREFIX};
use crate::{BoxedStream, Error, Result, RuntimeDir, build_id, frame, spawn_detached};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const READY_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const PING_INTERVAL: Duration = Duration::from_secs(5);
const PING_TIMEOUT: Duration = Duration::from_secs(20);
const BACKOFF_MIN: Duration = Duration::from_millis(50);
const BACKOFF_MAX: Duration = Duration::from_secs(2);
/// Retry interval while the daemon cannot be started at all (e.g. executable missing).
const FAILED_RETRY: Duration = Duration::from_secs(5);
const EVENT_CAPACITY: usize = 256;
const REQUEST_CAPACITY: usize = 64;

/// Where the daemon comes from.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Spawned as `<daemon_exe> run` when no daemon answers. Its [`build_id`] is
    /// recomputed on every connect: a daemon of another build is stopped and replaced.
    pub daemon_exe: PathBuf,
}

/// Connection state, as shown in the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnState {
    /// First connect (or spawn) in progress.
    Connecting,
    /// Attached. Every (re)connect starts with fresh per-connection daemon state.
    Connected {
        /// Daemon epoch; a new value means the daemon restarted.
        epoch: String,
    },
    /// Lost the connection; retrying.
    Reconnecting {
        /// Retry count since the last successful connect.
        attempt: u32,
        /// Why the last attempt or connection failed.
        reason: String,
    },
    /// The daemon cannot be started (e.g. its executable is missing); retrying slowly.
    Failed {
        /// What went wrong.
        reason: String,
    },
}

/// What the supervisor tells the UI.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// The connection state changed.
    State(ConnState),
    /// The daemon pushed an event. [`Event::Quit`] is the last thing the supervisor sends.
    Daemon(Event),
}

struct Outgoing {
    req: Request,
    reply: oneshot::Sender<Result<Response>>,
}

/// Cloneable handle to the daemon connection.
#[derive(Debug, Clone)]
pub struct EngineHandle {
    requests: mpsc::Sender<Outgoing>,
    state: watch::Receiver<ConnState>,
}

impl std::fmt::Debug for Outgoing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outgoing")
            .field("req", &self.req)
            .finish_non_exhaustive()
    }
}

impl EngineHandle {
    /// Spawns the connection supervisor on `rt`. It runs until every handle is dropped.
    /// Events arrive on the returned receiver, starting with the first state change.
    pub fn start(
        rt: &tokio::runtime::Handle,
        config: ClientConfig,
    ) -> (Self, mpsc::Receiver<ClientEvent>) {
        let (requests, requests_rx) = mpsc::channel(REQUEST_CAPACITY);
        let (events, events_rx) = mpsc::channel(EVENT_CAPACITY);
        let (state_tx, state) = watch::channel(ConnState::Connecting);
        rt.spawn(supervise(config, requests_rx, events, state_tx));
        (Self { requests, state }, events_rx)
    }

    /// Sends `req` and awaits its answer. Fails fast with [`Error::NotConnected`] while
    /// no daemon is attached; a daemon-side error arrives as [`Error::Rpc`].
    pub async fn request(&self, req: Request) -> Result<Response> {
        if !matches!(*self.state.borrow(), ConnState::Connected { .. }) {
            return Err(Error::NotConnected);
        }
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Outgoing { req, reply })
            .await
            .map_err(|_| Error::Disconnected)?;
        answer.await.map_err(|_| Error::Disconnected)?
    }

    /// The current connection state.
    pub fn state(&self) -> ConnState {
        self.state.borrow().clone()
    }
}

async fn supervise(
    config: ClientConfig,
    mut requests: mpsc::Receiver<Outgoing>,
    events: mpsc::Sender<ClientEvent>,
    state: watch::Sender<ConnState>,
) {
    let publish = |next: ConnState| {
        let events = events.clone();
        let changed = state.send_if_modified(|current| {
            if *current == next {
                false
            } else {
                *current = next.clone();
                true
            }
        });
        async move {
            if changed && events.send(ClientEvent::State(next)).await.is_err() {
                tracing::debug!("UI stopped listening for daemon events");
            }
        }
    };
    let mut attempt: u32 = 0;
    loop {
        let wait = match establish(&config).await {
            Ok(session) => {
                attempt = 0;
                publish(ConnState::Connected {
                    epoch: session.welcome.epoch.clone(),
                })
                .await;
                tracing::info!(epoch = %session.welcome.epoch, "attached to daemon");
                match serve(session, &mut requests, &events).await {
                    ServeEnd::HandlesDropped => return,
                    ServeEnd::Quit => {
                        tracing::info!("oh-my-clear was quit from the daemon; not reconnecting");
                        return;
                    }
                    ServeEnd::Lost(reason) => {
                        tracing::warn!("daemon connection lost: {reason}");
                        publish(ConnState::Reconnecting { attempt, reason }).await;
                        BACKOFF_MIN
                    }
                }
            }
            Err(err) => {
                attempt = attempt.saturating_add(1);
                let reason = err.to_string();
                tracing::warn!(attempt, "cannot reach the daemon: {reason}");
                if matches!(err, Error::DaemonMissing(_) | Error::DaemonDidNotStart(_)) {
                    publish(ConnState::Failed { reason }).await;
                    FAILED_RETRY
                } else {
                    publish(ConnState::Reconnecting { attempt, reason }).await;
                    backoff(attempt)
                }
            }
        };
        if !refuse_requests_for(wait, &mut requests).await {
            return;
        }
    }
}

/// 50 ms doubling per attempt, capped at 2 s.
fn backoff(attempt: u32) -> Duration {
    let factor = 1_u32.checked_shl(attempt.min(16)).unwrap_or(u32::MAX);
    BACKOFF_MIN.saturating_mul(factor).min(BACKOFF_MAX)
}

/// Answers requests with `NotConnected` for `wait`. `false` once every handle is gone.
async fn refuse_requests_for(wait: Duration, requests: &mut mpsc::Receiver<Outgoing>) -> bool {
    let deadline = tokio::time::sleep(wait);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => return true,
            next = requests.recv() => match next {
                Some(out) => reply(out.reply, Err(Error::NotConnected)),
                None => return false,
            },
        }
    }
}

fn reply(to: oneshot::Sender<Result<Response>>, result: Result<Response>) {
    if to.send(result).is_err() {
        tracing::debug!("requester went away before its answer");
    }
}

/// Connects to the running daemon, or spawns one. A daemon of another build (or an
/// incompatible protocol) is stopped and replaced once per call.
async fn establish(config: &ClientConfig) -> Result<Session> {
    let dir = RuntimeDir::resolve()?;
    let expected = build_id(&config.daemon_exe);
    match connect_existing(&dir, ClientKind::Ui).await {
        Ok(Some(session)) if session.welcome.build == expected => return Ok(session),
        Ok(Some(session)) => {
            tracing::info!(
                running = %session.welcome.build,
                %expected,
                "daemon is from another build; restarting it"
            );
            session.shutdown().await;
        }
        Ok(None) => {}
        Err(Error::ProtocolMismatch { ours, theirs }) => {
            // The frozen handshake told us; a daemon speaking another protocol cannot be
            // driven further from here.
            return Err(Error::ProtocolMismatch { ours, theirs });
        }
        Err(err) => return Err(err),
    }
    spawn_daemon(&config.daemon_exe, &dir).await?;
    connect_existing(&dir, ClientKind::Ui)
        .await?
        .ok_or_else(|| Error::DaemonDidNotStart(dir.log_file()))
}

/// Starts `<exe> run` detached from this process and waits for its `READY` line. A
/// daemon that exits without it (another instance won the lock) is not an error: the
/// caller simply connects again.
async fn spawn_daemon(exe: &Path, dir: &RuntimeDir) -> Result<()> {
    if !exe.is_file() {
        return Err(Error::DaemonMissing(exe.to_owned()));
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.log_file())?;
    let mut child = spawn_detached(|| {
        let mut cmd = tokio::process::Command::new(exe);
        cmd.arg("run")
            .current_dir(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log.try_clone()?));
        Ok(cmd)
    })?;
    let Some(stdout) = child.stdout.take() else {
        return Err(Error::DaemonDidNotStart(dir.log_file()));
    };
    let mut lines = BufReader::new(stdout).lines();
    let first = tokio::time::timeout(READY_TIMEOUT, lines.next_line())
        .await
        .map_err(|_| Error::DaemonDidNotStart(dir.log_file()))??;
    match first {
        Some(line) if line.starts_with(READY_PREFIX) => {
            tracing::info!(exe = %exe.display(), "spawned daemon: {line}");
        }
        Some(line) => tracing::warn!("unexpected daemon stdout: {line}"),
        None => tracing::info!("spawned daemon exited without READY (another one is running?)"),
    }
    // Dropping `child` leaves the daemon running (`kill_on_drop` is off); tokio reaps it
    // if it exits while this process lives.
    Ok(())
}

enum ServeEnd {
    HandlesDropped,
    /// The daemon sent [`Event::Quit`].
    Quit,
    Lost(String),
}

/// Runs one attached connection: forwards requests, routes answers and events, pings.
async fn serve(
    session: Session,
    requests: &mut mpsc::Receiver<Outgoing>,
    events: &mpsc::Sender<ClientEvent>,
) -> ServeEnd {
    let Session {
        reader,
        mut writer,
        mut next_id,
        ..
    } = session;
    // `frame::read` is not cancel-safe, so the reader gets its own task.
    let (frames_tx, mut frames) = mpsc::channel::<Result<Option<ServerFrame>>>(EVENT_CAPACITY);
    let reader_task = tokio::spawn(read_frames(reader, frames_tx));
    let mut pending: HashMap<u64, oneshot::Sender<Result<Response>>> = HashMap::new();
    let mut ping = tokio::time::interval(PING_INTERVAL);
    let mut last_seen = Instant::now();
    let end = loop {
        tokio::select! {
            next = requests.recv() => {
                let Some(Outgoing { req, reply: to }) = next else {
                    break ServeEnd::HandlesDropped;
                };
                let id = next_id;
                next_id = next_id.wrapping_add(1);
                if let Err(err) = frame::write(&mut writer, &ClientFrame::Req { id, req }).await {
                    reply(to, Err(Error::Disconnected));
                    break ServeEnd::Lost(err.to_string());
                }
                pending.insert(id, to);
            }
            incoming = frames.recv() => match incoming {
                Some(Ok(Some(ServerFrame::Res { id, res }))) => {
                    last_seen = Instant::now();
                    if let Some(to) = pending.remove(&id) {
                        reply(to, res.map_err(Error::Rpc));
                    } else {
                        tracing::trace!(id, "answer without a waiting requester (ping)");
                    }
                }
                Some(Ok(Some(ServerFrame::Event { ev }))) => {
                    last_seen = Instant::now();
                    tracing::debug!(?ev, "daemon event");
                    if events.send(ClientEvent::Daemon(ev)).await.is_err() {
                        tracing::debug!("UI stopped listening for daemon events");
                    }
                    if ev == Event::Quit {
                        break ServeEnd::Quit;
                    }
                }
                Some(Ok(None)) | None => break ServeEnd::Lost("daemon closed the connection".to_owned()),
                Some(Err(err)) => break ServeEnd::Lost(err.to_string()),
            },
            _ = ping.tick() => {
                if last_seen.elapsed() > PING_TIMEOUT {
                    break ServeEnd::Lost("daemon stopped answering".to_owned());
                }
                let id = next_id;
                next_id = next_id.wrapping_add(1);
                let frame = ClientFrame::Req { id, req: Request::Ping { nonce: id } };
                if let Err(err) = frame::write(&mut writer, &frame).await {
                    break ServeEnd::Lost(err.to_string());
                }
            }
        }
    };
    reader_task.abort();
    for (_, to) in pending.drain() {
        reply(to, Err(Error::Disconnected));
    }
    end
}

async fn read_frames(
    mut reader: ReadHalf<BoxedStream>,
    frames: mpsc::Sender<Result<Option<ServerFrame>>>,
) {
    loop {
        let next = frame::read::<_, ServerFrame>(&mut reader).await;
        let done = !matches!(next, Ok(Some(_)));
        if frames.send(next).await.is_err() || done {
            return;
        }
    }
}

/// A direct, request-at-a-time connection (CLI subcommands; the supervisor's building
/// block).
pub struct Session {
    reader: ReadHalf<BoxedStream>,
    writer: WriteHalf<BoxedStream>,
    welcome: Welcome,
    next_id: u64,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("welcome", &self.welcome)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// The daemon's handshake answer.
    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    /// Sends `req` and waits (10 s) for its answer.
    pub async fn request(&mut self, req: Request) -> Result<Response> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        frame::write(&mut self.writer, &ClientFrame::Req { id, req }).await?;
        let answer = async {
            loop {
                match frame::read::<_, ServerFrame>(&mut self.reader).await? {
                    Some(ServerFrame::Res { id: got, res }) if got == id => {
                        return res.map_err(Error::Rpc);
                    }
                    Some(_) => {}
                    None => return Err(Error::Disconnected),
                }
            }
        };
        tokio::time::timeout(REQUEST_TIMEOUT, answer)
            .await
            .map_err(|_| Error::Timeout)?
    }

    /// Asks the daemon to stop and waits (5 s) for it to close the connection.
    async fn shutdown(mut self) {
        if let Err(err) = self.request(Request::Shutdown).await {
            tracing::warn!("daemon shutdown request failed: {err}");
            return;
        }
        let closed = tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
            while let Ok(Some(_)) = frame::read::<_, ServerFrame>(&mut self.reader).await {}
        })
        .await;
        if closed.is_err() {
            tracing::warn!("daemon did not close after shutdown");
        }
    }
}

/// Connects to the daemon published in `dir` without spawning one. `Ok(None)` when none
/// is running (no endpoint file, or nobody listening at its address).
pub async fn connect_existing(dir: &RuntimeDir, kind: ClientKind) -> Result<Option<Session>> {
    let Some(endpoint) = Endpoint::load(dir)? else {
        return Ok(None);
    };
    let Some(stream) = open_stream(&endpoint.address).await? else {
        return Ok(None);
    };
    let (mut reader, mut writer) = tokio::io::split(stream);
    let hello = ClientFrame::Req {
        id: 0,
        req: Request::Hello(Hello {
            protocol: PROTOCOL,
            build: env!("CARGO_PKG_VERSION").to_owned(),
            token: endpoint.token,
            client: kind,
        }),
    };
    frame::write(&mut writer, &hello).await?;
    let answer = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        frame::read::<_, ServerFrame>(&mut reader),
    )
    .await
    .map_err(|_| Error::HandshakeTimeout)??;
    let welcome = match answer {
        Some(ServerFrame::Res {
            id: 0,
            res: Ok(Response::Welcome(welcome)),
        }) => welcome,
        Some(ServerFrame::Res {
            id: 0,
            res: Err(err),
        }) if err.code == omc_proto::ErrorCode::ProtocolMismatch => {
            return Err(Error::ProtocolMismatch {
                ours: PROTOCOL,
                theirs: endpoint.protocol,
            });
        }
        Some(ServerFrame::Res { res: Err(err), .. }) => return Err(Error::Rpc(err)),
        // A daemon that closes on our token lost the race with a newer one: treat the
        // endpoint as stale.
        None => return Ok(None),
        Some(_) => return Err(Error::Handshake("unexpected answer to hello")),
    };
    Ok(Some(Session {
        reader,
        writer,
        welcome,
        next_id: 1,
    }))
}

/// Opens the transport; `Ok(None)` when nothing listens there (stale endpoint).
async fn open_stream(address: &str) -> Result<Option<BoxedStream>> {
    #[cfg(unix)]
    {
        match tokio::net::UnixStream::connect(address).await {
            Ok(stream) => Ok(Some(Box::new(stream))),
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                Ok(None)
            }
            Err(err) => Err(err.into()),
        }
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        /// `ERROR_PIPE_BUSY`: every instance is taken; the server creates the next soon.
        const ERROR_PIPE_BUSY: i32 = 231;
        for _ in 0..20 {
            match ClientOptions::new().open(address) {
                Ok(pipe) => return Ok(Some(Box::new(pipe))),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    tokio::time::sleep(BACKOFF_MIN).await;
                }
                Err(err) => return Err(err.into()),
            }
        }
        Err(Error::Timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_50ms_and_caps_at_2s() {
        assert_eq!(
            backoff(0),
            Duration::from_millis(50),
            "first retry is quick"
        );
        assert_eq!(backoff(1), Duration::from_millis(100), "doubles");
        assert_eq!(backoff(6), BACKOFF_MAX, "capped");
        assert_eq!(backoff(u32::MAX), BACKOFF_MAX, "no overflow");
    }

    #[tokio::test]
    async fn quit_event_reaches_the_ui_and_ends_the_connection_for_good() {
        let (ours, theirs) = tokio::io::duplex(1 << 12);
        let (reader, writer) = tokio::io::split(Box::new(ours) as BoxedStream);
        let session = Session {
            reader,
            writer,
            welcome: Welcome {
                protocol: PROTOCOL,
                build: "test".to_owned(),
                epoch: "epoch".to_owned(),
                pid: 1,
            },
            next_id: 1,
        };
        let (mut daemon_reader, mut daemon_writer) = tokio::io::split(theirs);
        let (_requests_tx, mut requests) = mpsc::channel(1);
        let (events_tx, mut events) = mpsc::channel(4);

        let daemon = async {
            for ev in [Event::Activate, Event::Quit] {
                frame::write(&mut daemon_writer, &ServerFrame::Event { ev }).await?;
            }
            // Stay connected: the client must stop on the event, not on a closed socket.
            frame::read::<_, ClientFrame>(&mut daemon_reader).await
        };
        let client = tokio::time::timeout(
            Duration::from_secs(5),
            serve(session, &mut requests, &events_tx),
        );
        let (end, _) = tokio::join!(client, daemon);

        assert!(
            matches!(end, Ok(ServeEnd::Quit)),
            "the connection ends with Quit, so the supervisor does not reconnect"
        );
        let forwarded: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert!(
            matches!(
                forwarded.as_slice(),
                [
                    ClientEvent::Daemon(Event::Activate),
                    ClientEvent::Daemon(Event::Quit)
                ]
            ),
            "both events reach the UI in order: {forwarded:?}"
        );
    }
}
