//! Wire types shared by daemon and UI: control frames, requests, responses and errors.
//!
//! Pure data + serde, no I/O. IPC framing lives in omc-ipc. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0008-process-split-daemon-ipc.md
//!
//! Every control frame is one JSON value: the client sends [`ClientFrame`], the daemon
//! answers with [`ServerFrame`]. Unknown fields are ignored and additive fields carry
//! `#[serde(default)]`, so [`PROTOCOL`] only changes on a non-additive change.

use serde::{Deserialize, Serialize};

/// Protocol version. Bump on any non-additive wire change (ADR 0008).
pub const PROTOCOL: u32 = 1;

/// Client → daemon frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientFrame {
    /// A request; the daemon answers with a [`ServerFrame::Res`] carrying the same `id`.
    Req {
        /// Client-chosen id, unique per connection.
        id: u64,
        /// The request.
        req: Request,
    },
}

/// Daemon → client frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerFrame {
    /// The answer to the request with the same `id`.
    Res {
        /// Id of the request.
        id: u64,
        /// Success payload or error.
        res: Result<Response, RpcError>,
    },
}

/// Requests. `hello`, `ping` and `shutdown` are the frozen meta subset: their shape never
/// changes, so any client build can greet, probe and stop any daemon build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "m", content = "p", rename_all = "snake_case")]
pub enum Request {
    /// First frame of every connection; carries the auth token.
    Hello(Hello),
    /// Liveness probe; answered with [`Response::Pong`].
    Ping {
        /// Echoed back.
        nonce: u64,
    },
    /// Stops the daemon. Answered with [`Response::Unit`] before the daemon exits.
    Shutdown,
}

/// Successful responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "r", content = "p", rename_all = "snake_case")]
pub enum Response {
    /// Answer to [`Request::Hello`].
    Welcome(Welcome),
    /// Answer to [`Request::Ping`].
    Pong {
        /// The request's nonce.
        nonce: u64,
    },
    /// Empty acknowledgement.
    Unit,
}

/// Handshake request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The client's [`PROTOCOL`].
    pub protocol: u32,
    /// Informational client build id.
    pub build: String,
    /// Hex token from the endpoint file.
    pub token: String,
    /// What kind of client connects.
    pub client: ClientKind,
}

/// Kinds of client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// The GUI.
    Ui,
    /// A `oh-my-clear-daemon` subcommand (`stop`, `status`).
    Cli,
}

/// Handshake answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    /// The daemon's [`PROTOCOL`].
    pub protocol: u32,
    /// The daemon's build id; a client that expects another build restarts the daemon.
    pub build: String,
    /// Random per daemon start; a new epoch means every client cache is stale.
    pub epoch: String,
    /// Daemon process id.
    pub pid: u32,
}

/// Error answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct RpcError {
    /// Machine-readable class.
    pub code: ErrorCode,
    /// Human-readable detail.
    pub message: String,
}

impl RpcError {
    /// An error of class `code`.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Error classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Client and daemon speak different [`PROTOCOL`] versions.
    ProtocolMismatch,
    /// The request is malformed or not allowed (e.g. a path escaping its root).
    BadRequest,
    /// The requested object does not exist.
    NotFound,
    /// An I/O error in the daemon.
    Io,
    /// Unclassified daemon-side failure.
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_use_tagged_json() {
        let frame = ClientFrame::Req {
            id: 7,
            req: Request::Ping { nonce: 3 },
        };
        let json = serde_json::to_value(&frame).ok();
        assert_eq!(
            json,
            Some(serde_json::json!({"t": "req", "id": 7, "req": {"m": "ping", "p": {"nonce": 3}}})),
            "meta requests keep their frozen shape"
        );
    }
}
