//! [`Engine`]: serves authenticated connections, tracks live clients and shutdown, and
//! pushes [`Event`]s to attached UIs.

use std::sync::Arc;
use std::time::Duration;

use omc_ipc::frame;
use omc_ipc::server::Connection;
use omc_proto::{
    ClientFrame, ClientKind, ErrorCode, Event, Request, Response, RpcError, ServerFrame,
};
use tokio::io::AsyncWrite;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::Result;

/// Answers queued per connection before senders wait.
const OUTBOX: usize = 256;
/// How long a `shutdown` request waits for its answer to reach the socket.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(1);
/// UI events buffered per connection; they are rare user actions, so a lagging UI only
/// ever misses stale ones.
const UI_EVENTS: usize = 16;

/// An item for a connection's writer task.
#[derive(Debug)]
enum Outbound {
    /// Write this frame.
    Frame(ServerFrame),
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
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                clients: watch::Sender::new(0),
                ui_clients: watch::Sender::new(0),
                ui_events: broadcast::Sender::new(UI_EVENTS),
                shutdown: watch::Sender::new(false),
            }),
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
        match self.inner.ui_events.send(event) {
            Ok(receivers) => {
                tracing::debug!(?event, receivers, "event queued for attached UIs");
                receivers
            }
            Err(_) => 0,
        }
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
            .send(Outbound::Frame(ServerFrame::Event { ev }))
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
        let res = match req {
            Request::Hello(_) => Err(RpcError::new(
                ErrorCode::BadRequest,
                "hello is only valid as the first frame",
            )),
            Request::Ping { nonce } => Ok(Response::Pong { nonce }),
            Request::Shutdown => return self.shutdown(id).await,
        };
        self.reply(id, res).await
    }

    async fn reply(&self, id: u64, res: Result<Response, RpcError>) -> bool {
        self.out
            .send(Outbound::Frame(ServerFrame::Res { id, res }))
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

/// Owns the connection's write half: the only place frames are written.
async fn write_frames<W: AsyncWrite + Unpin>(mut writer: W, mut outbox: mpsc::Receiver<Outbound>) {
    while let Some(item) = outbox.recv().await {
        match item {
            Outbound::Frame(msg) => {
                if let Err(err) = frame::write(&mut writer, &msg).await {
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
}
