//! Daemon core: the connection router behind `oh-my-clear-daemon`.
//!
//! Linked only by apps/oh-my-clear-daemon; never by the UI. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0008-process-split-daemon-ipc.md
//!
//! [`Engine::serve`] serves authenticated [`omc_ipc::server::Connection`]s: today the meta
//! requests (`ping`, `shutdown`). Every outgoing frame goes through one bounded
//! per-connection queue drained by a writer task.

mod engine;

pub use engine::Engine;

/// Engine failures that end a connection.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading a frame from the client failed (I/O, oversized or malformed frame).
    #[error("ipc: {0}")]
    Ipc(#[from] omc_ipc::Error),
}

/// Result alias of this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;
