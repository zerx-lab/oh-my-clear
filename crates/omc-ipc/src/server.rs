//! Daemon side: bind the platform endpoint, accept, authenticate (ADR 0008).
//!
//! Security: the runtime directory is private (`0700`, owner-checked) and unix peers must
//! run as the same uid; Windows uses a random, unpublished-elsewhere pipe name with
//! `first_pipe_instance` and remote clients rejected. On every OS the first frame must
//! carry the token from `endpoint.json`, and the daemon writes nothing before it checks.

use std::time::Duration;

use omc_proto::{
    ClientFrame, ClientKind, ErrorCode, PROTOCOL, Request, RpcError, ServerFrame, Welcome,
};
use tokio::io::{ReadHalf, WriteHalf};

use crate::runtime::{Endpoint, new_token};
use crate::{BoxedStream, Error, Result, RuntimeDir, frame};

/// How long a new connection may take to send `hello`.
const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
/// Size limit of the unauthenticated first frame.
const HELLO_MAX_LEN: u32 = 64 << 10;

/// An authenticated connection, split for a reader task and a writer task.
pub struct Connection {
    /// Incoming frames.
    pub reader: ReadHalf<BoxedStream>,
    /// Outgoing frames.
    pub writer: WriteHalf<BoxedStream>,
    /// Who connected.
    pub client: ClientKind,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

/// Reads `hello` (2 s deadline) and checks token and protocol. A wrong or missing token
/// fails without writing a byte; a protocol mismatch is answered with
/// `ErrorCode::ProtocolMismatch` before failing; otherwise `welcome` is sent back.
pub async fn handshake(stream: BoxedStream, token: &str, welcome: &Welcome) -> Result<Connection> {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let first = tokio::time::timeout(
        HELLO_TIMEOUT,
        frame::read_limited::<_, ClientFrame>(&mut reader, HELLO_MAX_LEN),
    )
    .await
    .map_err(|_| Error::HandshakeTimeout)??;
    let Some(ClientFrame::Req {
        id,
        req: Request::Hello(hello),
    }) = first
    else {
        return Err(Error::Handshake("first frame is not hello"));
    };
    if !constant_time_eq(hello.token.as_bytes(), token.as_bytes()) {
        return Err(Error::BadToken);
    }
    if hello.protocol != PROTOCOL {
        let err = RpcError::new(
            ErrorCode::ProtocolMismatch,
            format!("daemon speaks protocol {PROTOCOL}"),
        );
        frame::write(&mut writer, &ServerFrame::Res { id, res: Err(err) }).await?;
        return Err(Error::ProtocolMismatch {
            ours: PROTOCOL,
            theirs: hello.protocol,
        });
    }
    let res = Ok(omc_proto::Response::Welcome(welcome.clone()));
    frame::write(&mut writer, &ServerFrame::Res { id, res }).await?;
    Ok(Connection {
        reader,
        writer,
        client: hello.client,
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The bound daemon endpoint. Bind only while holding [`crate::DaemonLock`], and drop
/// the listener before the lock: dropping unpublishes the endpoint.
#[derive(Debug)]
pub struct Listener {
    dir: RuntimeDir,
    token: String,
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    pipe: String,
    #[cfg(windows)]
    next: tokio::net::windows::named_pipe::NamedPipeServer,
}

impl Listener {
    /// Binds the endpoint in `dir` and publishes `endpoint.json` with a fresh token.
    /// Must be called from within a tokio runtime.
    pub fn bind(dir: &RuntimeDir, build: &str, epoch: &str) -> Result<Self> {
        let token = new_token();
        #[cfg(unix)]
        let (listener, address) = {
            let socket = dir.socket_path();
            // Safe only because the caller holds the daemon lock: a socket file left
            // behind belongs to a dead daemon.
            match std::fs::remove_file(&socket) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
            let inner = tokio::net::UnixListener::bind(&socket)?;
            let listener = Self {
                dir: dir.clone(),
                token: token.clone(),
                inner,
            };
            (listener, socket.to_string_lossy().into_owned())
        };
        #[cfg(windows)]
        let (listener, address) = {
            use tokio::net::windows::named_pipe::ServerOptions;
            let pipe = format!(r"\\.\pipe\oh-my-clear-{}", new_token());
            let next = ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create(&pipe)?;
            let listener = Self {
                dir: dir.clone(),
                token: token.clone(),
                pipe: pipe.clone(),
                next,
            };
            (listener, pipe)
        };
        Endpoint {
            protocol: PROTOCOL,
            build: build.to_owned(),
            pid: std::process::id(),
            epoch: epoch.to_owned(),
            address,
            token,
        }
        .store(dir)?;
        Ok(listener)
    }

    /// The token clients must present.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Waits for the next client. Unix peers running as another user are dropped.
    pub async fn accept(&mut self) -> Result<BoxedStream> {
        #[cfg(unix)]
        loop {
            let (stream, _) = self.inner.accept().await?;
            match stream.peer_cred() {
                Ok(cred) if cred.uid() == self.dir.uid() => return Ok(Box::new(stream)),
                Ok(cred) => tracing::warn!(uid = cred.uid(), "rejected peer of another user"),
                Err(err) => tracing::warn!("rejected peer without credentials: {err}"),
            }
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            self.next.connect().await?;
            let fresh = ServerOptions::new()
                .reject_remote_clients(true)
                .create(&self.pipe)?;
            let connected = std::mem::replace(&mut self.next, fresh);
            Ok(Box::new(connected))
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        Endpoint::remove_if_owned(&self.dir, &self.token);
        #[cfg(unix)]
        if let Err(err) = std::fs::remove_file(self.dir.socket_path()) {
            tracing::debug!("could not remove socket: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use omc_proto::{Hello, Response};
    use tokio::io::AsyncReadExt as _;

    use super::*;

    fn welcome() -> Welcome {
        Welcome {
            protocol: PROTOCOL,
            build: "test".to_owned(),
            epoch: "e".to_owned(),
            pid: 1,
        }
    }

    fn hello(token: &str, protocol: u32) -> ClientFrame {
        ClientFrame::Req {
            id: 0,
            req: Request::Hello(Hello {
                protocol,
                build: "test".to_owned(),
                token: token.to_owned(),
                client: ClientKind::Ui,
            }),
        }
    }

    #[tokio::test]
    async fn wrong_token_gets_no_bytes() {
        let (mut client, server) = tokio::io::duplex(4096);
        assert!(
            frame::write(&mut client, &hello("nope", PROTOCOL))
                .await
                .is_ok(),
            "hello sent"
        );
        let got = handshake(Box::new(server), "secret", &welcome()).await;
        assert!(matches!(got, Err(Error::BadToken)), "rejected: {got:?}");
        drop(got);
        let mut buf = Vec::new();
        let read = client.read_to_end(&mut buf).await;
        assert!(
            read.is_ok() && buf.is_empty(),
            "daemon wrote nothing before closing: {buf:?}"
        );
    }

    #[tokio::test]
    async fn non_hello_first_frame_is_rejected() {
        let (mut client, server) = tokio::io::duplex(4096);
        let ping = ClientFrame::Req {
            id: 0,
            req: Request::Ping { nonce: 1 },
        };
        assert!(frame::write(&mut client, &ping).await.is_ok(), "ping sent");
        let got = handshake(Box::new(server), "secret", &welcome()).await;
        assert!(matches!(got, Err(Error::Handshake(_))), "rejected: {got:?}");
    }

    #[tokio::test]
    async fn protocol_mismatch_is_answered_then_refused() {
        let (mut client, server) = tokio::io::duplex(4096);
        let theirs = PROTOCOL.saturating_add(1);
        assert!(
            frame::write(&mut client, &hello("secret", theirs))
                .await
                .is_ok(),
            "hello sent"
        );
        let got = handshake(Box::new(server), "secret", &welcome()).await;
        assert!(
            matches!(got, Err(Error::ProtocolMismatch { .. })),
            "refused: {got:?}"
        );
        let answer = frame::read::<_, ServerFrame>(&mut client).await;
        assert!(
            matches!(&answer, Ok(Some(ServerFrame::Res { res: Err(e), .. })) if e.code == ErrorCode::ProtocolMismatch),
            "client learns why: {answer:?}"
        );
    }

    #[tokio::test]
    async fn good_hello_is_welcomed() {
        let (mut client, server) = tokio::io::duplex(4096);
        assert!(
            frame::write(&mut client, &hello("secret", PROTOCOL))
                .await
                .is_ok(),
            "hello sent"
        );
        let got = handshake(Box::new(server), "secret", &welcome()).await;
        assert!(
            got.as_ref().is_ok_and(|c| c.client == ClientKind::Ui),
            "accepted: {got:?}"
        );
        let answer = frame::read::<_, ServerFrame>(&mut client).await;
        assert!(
            matches!(&answer, Ok(Some(ServerFrame::Res { id: 0, res: Ok(Response::Welcome(w)) })) if *w == welcome()),
            "welcome sent: {answer:?}"
        );
    }
}
