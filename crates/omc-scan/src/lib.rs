//! Filesystem scanning and cleaning: parallel walker, junk catalogues, space lens,
//! large/old files, duplicates, deletion.
//!
//! Linked by omc-engine (and omc-apps); never by the UI. Allowed internal deps: `LAYERS` in
//! xtask/src/layers.rs. Every scan is synchronous, runs on the caller's blocking thread with
//! its own rayon pool ([`walk::Walker`]), reports progress through a [`ctx::JobCtx`], and
//! returns a report plus one removal [`target::Target`] per item ([`target::Scanned`]).

pub mod ctx;
pub mod delete;
pub mod dupes;
pub mod errors;
pub mod guard;
pub mod junk;
pub mod large;
pub mod paths;
pub mod procs;
pub mod space;
pub mod target;
pub mod walk;

pub use ctx::JobCtx;
pub use guard::Guard;
pub use target::{Scanned, Target};
pub use walk::{WalkOptions, Walker};

/// Scanner failures that end a job (per-path problems are reported, not raised).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The thread pool could not be built.
    #[error("scanner thread pool: {0}")]
    Pool(String),
    /// A root or input is unusable.
    #[error("invalid input: {0}")]
    Invalid(String),
    /// I/O error outside a per-path report.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// The job was cancelled.
    #[error("cancelled")]
    Cancelled,
}

/// Result alias of this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;
