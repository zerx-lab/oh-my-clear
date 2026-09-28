//! Binary terminal snapshots (`GHOSTSNP` format) for daemon → UI reattach.

use std::ptr::{self, NonNull};

use crate::error::{Error, Result, check};
use crate::ffi;
use crate::terminal::{OwnedBuffer, Terminal};

/// Complete encoded terminal state: screen, scrollback, modes, and any unfinished
/// escape sequence. Only guaranteed to decode with the same libghostty-vt build that
/// encoded it (the format carries its own version and CRC32C checksums).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    bytes: Vec<u8>,
}

impl Snapshot {
    /// Wrap bytes received from elsewhere (e.g. the daemon). Validation happens in
    /// [`Snapshot::restore`].
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Encoded bytes, e.g. to send over IPC.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Take the encoded bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Decode into a new, independent terminal.
    ///
    /// # Errors
    /// [`Error::Ghostty`] with `InvalidValue` for malformed or truncated data, or on
    /// allocation failure.
    pub fn restore(&self) -> Result<Terminal> {
        let decoder = Decoder::new(&self.bytes)?;
        let mut raw: ffi::GhosttyTerminal = ptr::null_mut();
        // SAFETY: the decoder is live and has not started decoding; `raw` is a valid
        // out-pointer that receives a caller-owned terminal (NULL on error).
        let code =
            unsafe { ffi::ghostty_snapshot_decoder_decode(decoder.raw.as_ptr(), &raw mut raw) };
        check("snapshot_decoder_decode", code)?;
        Terminal::from_raw(raw, "snapshot_decoder_decode")
    }
}

impl Terminal {
    /// Encode the complete terminal state.
    ///
    /// # Errors
    /// [`Error::Ghostty`] if encoding fails (e.g. out of memory).
    pub fn snapshot(&self) -> Result<Snapshot> {
        let mut buffer = OwnedBuffer::empty();
        // SAFETY: the terminal is live; the out-pointers are valid and receive a
        // default-allocator buffer that `OwnedBuffer` frees.
        let code = unsafe {
            ffi::ghostty_snapshot_encode_alloc(
                self.as_raw(),
                ptr::null(),
                &raw mut buffer.ptr,
                &raw mut buffer.len,
            )
        };
        check("snapshot_encode_alloc", code)?;
        Ok(Snapshot {
            bytes: buffer.to_vec(),
        })
    }
}

/// Decoder over a borrowed buffer; the borrow outlives the decoder, as the C API requires.
struct Decoder<'b> {
    raw: NonNull<ffi::GhosttySnapshotDecoderImpl>,
    _bytes: &'b [u8],
}

impl<'b> Decoder<'b> {
    fn new(bytes: &'b [u8]) -> Result<Self> {
        let mut raw: ffi::GhosttySnapshotDecoder = ptr::null_mut();
        // SAFETY: `bytes` is valid and immutable for `'b`, which the decoder cannot
        // outlive; `raw` is a valid out-pointer.
        let code = unsafe {
            ffi::ghostty_snapshot_decoder_new_buf(
                ptr::null(),
                &raw mut raw,
                bytes.as_ptr(),
                bytes.len(),
            )
        };
        check("snapshot_decoder_new_buf", code)?;
        let raw = NonNull::new(raw).ok_or(Error::NullHandle("snapshot_decoder_new_buf"))?;
        Ok(Self { raw, _bytes: bytes })
    }
}

impl Drop for Decoder<'_> {
    fn drop(&mut self) {
        // SAFETY: the decoder is owned, live, and never used after this call.
        unsafe { ffi::ghostty_snapshot_decoder_free(self.raw.as_ptr()) };
    }
}
