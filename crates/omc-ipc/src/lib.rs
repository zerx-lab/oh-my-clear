//! UI<->daemon IPC: Unix socket / named pipe transport, length-prefixed frames (JSON control + raw binary data), handshake, auth token, endpoint discovery, daemon auto-spawn.
//!
//! tokio UDS (macOS/Linux) + named pipes (Windows), `u32 LE len | u8 kind | payload` framing, token-first handshake, `seq`/`epoch` resync. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0008-process-split-daemon-ipc.md
//!
//! - [`RuntimeDir`], [`DaemonLock`]: the private meeting place and the single-daemon lock;
//! - [`frame`]: the wire framing;
//! - [`server`]: daemon side (bind, accept, handshake);
//! - [`client`]: UI/CLI side ([`client::EngineHandle`] supervisor, one-shot sessions).

pub mod client;
mod error;
pub mod frame;
mod runtime;
pub mod server;

use tokio::io::{AsyncRead, AsyncWrite};

pub use error::{Error, Result};
pub use runtime::{DaemonLock, RuntimeDir, announce_ready, build_id, new_epoch};

/// A bidirectional byte stream (socket, pipe, or an in-memory duplex in tests).
pub trait IoStream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> IoStream for T {}

/// A type-erased [`IoStream`].
pub type BoxedStream = Box<dyn IoStream>;
