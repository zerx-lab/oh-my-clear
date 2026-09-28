use std::path::PathBuf;

/// omc-ipc errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Socket, pipe or file I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A control frame is not valid JSON for the expected type.
    #[error("invalid control frame: {0}")]
    Json(#[from] serde_json::Error),
    /// A frame header announced more bytes than allowed.
    #[error("frame of {0} bytes exceeds the limit")]
    FrameTooLarge(u64),
    /// A frame header announced zero bytes (no kind byte).
    #[error("empty frame")]
    EmptyFrame,
    /// A frame of a kind this build does not handle.
    #[error("unexpected frame kind {0:#04x}")]
    UnexpectedKind(u8),
    /// No usable per-user runtime directory (missing `HOME`/`LOCALAPPDATA`, …).
    #[error("no runtime directory: {0}")]
    NoRuntimeDir(&'static str),
    /// The runtime directory exists but is not private to this user.
    #[error("runtime directory {path} is not private: {reason}")]
    UnsafeRuntimeDir {
        /// The directory.
        path: PathBuf,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The peer did not complete the handshake in time.
    #[error("handshake timed out")]
    HandshakeTimeout,
    /// The peer sent something other than the expected handshake frame.
    #[error("handshake failed: {0}")]
    Handshake(&'static str),
    /// The client presented a wrong token.
    #[error("bad token")]
    BadToken,
    /// Client and daemon speak different protocol versions.
    #[error("protocol mismatch: ours {ours}, theirs {theirs}")]
    ProtocolMismatch {
        /// This build's protocol.
        ours: u32,
        /// The peer's protocol.
        theirs: u32,
    },
    /// The daemon answered with an error.
    #[error("daemon error: {0}")]
    Rpc(#[from] omc_proto::RpcError),
    /// The daemon answered with a response of another type than the request expects.
    #[error("unexpected response")]
    UnexpectedResponse,
    /// A request was made while no daemon is attached.
    #[error("not connected to the daemon")]
    NotConnected,
    /// The connection dropped before the answer arrived.
    #[error("connection to the daemon was lost")]
    Disconnected,
    /// The daemon did not answer in time.
    #[error("the daemon did not answer in time")]
    Timeout,
    /// The daemon executable to spawn does not exist.
    #[error("daemon executable not found at {0}")]
    DaemonMissing(PathBuf),
    /// A spawned daemon exited or stalled before it could be reached.
    #[error("the daemon did not start; see {0}")]
    DaemonDidNotStart(PathBuf),
}

/// omc-ipc result.
pub type Result<T, E = Error> = std::result::Result<T, E>;
