//! Error type for the libghostty-vt wrapper.

use crate::ffi;

/// Errors returned by [`Terminal`](crate::Terminal) and [`Snapshot`](crate::Snapshot).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A terminal needs at least one column and one row.
    #[error("terminal size must be at least 1x1 cells, got {cols}x{rows}")]
    InvalidSize {
        /// Requested columns.
        cols: u16,
        /// Requested rows.
        rows: u16,
    },
    /// A libghostty-vt call returned a failure code.
    #[error("libghostty-vt `{operation}` failed: {code}")]
    Ghostty {
        /// The C function (without the `ghostty_` prefix) that failed.
        operation: &'static str,
        /// The failure it reported.
        code: GhosttyError,
    },
    /// A libghostty-vt constructor reported success but produced no handle.
    #[error("libghostty-vt `{0}` returned a null handle")]
    NullHandle(&'static str),
}

/// Failure codes of libghostty-vt's `GhosttyResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GhosttyError {
    /// `GHOSTTY_OUT_OF_MEMORY`
    #[error("out of memory")]
    OutOfMemory,
    /// `GHOSTTY_INVALID_VALUE` (bad argument or malformed input such as a corrupt snapshot)
    #[error("invalid value")]
    InvalidValue,
    /// `GHOSTTY_OUT_OF_SPACE`
    #[error("out of space")]
    OutOfSpace,
    /// `GHOSTTY_NO_VALUE`
    #[error("no value")]
    NoValue,
    /// `GHOSTTY_IO_ERROR`
    #[error("I/O error")]
    Io,
    /// `GHOSTTY_LIMIT_EXCEEDED`
    #[error("limit exceeded")]
    LimitExceeded,
    /// `GHOSTTY_REJECTED`
    #[error("rejected")]
    Rejected,
    /// A code this wrapper does not know (newer library than the bindings).
    #[error("unknown result code {0}")]
    Unknown(i32),
}

/// `Result` alias used throughout this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Map a `GhosttyResult` to `Ok(())` or [`Error::Ghostty`].
pub(crate) fn check(operation: &'static str, code: ffi::GhosttyResult) -> Result<()> {
    let code = match code {
        ffi::GhosttyResult_GHOSTTY_SUCCESS => return Ok(()),
        ffi::GhosttyResult_GHOSTTY_OUT_OF_MEMORY => GhosttyError::OutOfMemory,
        ffi::GhosttyResult_GHOSTTY_INVALID_VALUE => GhosttyError::InvalidValue,
        ffi::GhosttyResult_GHOSTTY_OUT_OF_SPACE => GhosttyError::OutOfSpace,
        ffi::GhosttyResult_GHOSTTY_NO_VALUE => GhosttyError::NoValue,
        ffi::GhosttyResult_GHOSTTY_IO_ERROR => GhosttyError::Io,
        ffi::GhosttyResult_GHOSTTY_LIMIT_EXCEEDED => GhosttyError::LimitExceeded,
        ffi::GhosttyResult_GHOSTTY_REJECTED => GhosttyError::Rejected,
        other => GhosttyError::Unknown(other),
    };
    Err(Error::Ghostty { operation, code })
}
