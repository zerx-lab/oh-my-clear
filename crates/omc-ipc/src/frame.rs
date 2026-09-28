//! Length-prefixed frames: `u32 LE len | u8 kind | body[len - 1]` (ADR 0008). Kind `0x01`
//! is a `serde_json` control message; other kinds (raw binary data) are reserved.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::{Error, Result};

/// Largest accepted frame (`len` field), 16 MiB.
pub const MAX_FRAME_LEN: u32 = 16 << 20;

/// Kind byte of a JSON control frame.
const KIND_JSON: u8 = 0x01;

/// Reads one JSON control frame. `Ok(None)` on a clean EOF at a frame boundary.
///
/// Not cancel-safe: drive it from one task and don't race it in `select!`.
pub async fn read<R, T>(r: &mut R) -> Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    read_limited(r, MAX_FRAME_LEN).await
}

/// [`read`] with a smaller size limit (the unauthenticated first frame).
pub(crate) async fn read_limited<R, T>(r: &mut R, max_len: u32) -> Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut first = [0_u8; 1];
    if r.read(&mut first).await? == 0 {
        return Ok(None);
    }
    let mut rest = [0_u8; 3];
    r.read_exact(&mut rest).await?;
    let [b0] = first;
    let [b1, b2, b3] = rest;
    let len = u32::from_le_bytes([b0, b1, b2, b3]);
    if len > max_len {
        return Err(Error::FrameTooLarge(u64::from(len)));
    }
    let body_len = len.checked_sub(1).ok_or(Error::EmptyFrame)?;
    let kind = r.read_u8().await?;
    if kind != KIND_JSON {
        return Err(Error::UnexpectedKind(kind));
    }
    let body_len =
        usize::try_from(body_len).map_err(|_| Error::FrameTooLarge(u64::from(body_len)))?;
    let mut body = vec![0_u8; body_len];
    r.read_exact(&mut body).await?;
    Ok(Some(serde_json::from_slice(&body)?))
}

/// Writes and flushes one JSON control frame.
pub async fn write<W, T>(w: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = serde_json::to_vec(msg)?;
    let too_large = || Error::FrameTooLarge(u64::try_from(body.len()).unwrap_or(u64::MAX));
    let len = body
        .len()
        .checked_add(1)
        .and_then(|len| u32::try_from(len).ok())
        .filter(|len| *len <= MAX_FRAME_LEN)
        .ok_or_else(too_large)?;
    let mut frame = Vec::with_capacity(body.len().saturating_add(5));
    frame.extend_from_slice(&len.to_le_bytes());
    frame.push(KIND_JSON);
    frame.extend_from_slice(&body);
    w.write_all(&frame).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use omc_proto::{ClientFrame, Request};

    use super::*;

    #[tokio::test]
    async fn round_trips_and_reports_clean_eof() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let sent = ClientFrame::Req {
            id: 1,
            req: Request::Ping { nonce: 9 },
        };
        assert!(write(&mut a, &sent).await.is_ok(), "write succeeds");
        drop(a);
        let got = read::<_, ClientFrame>(&mut b).await;
        assert!(
            matches!(&got, Ok(Some(frame)) if *frame == sent),
            "reads back the frame: {got:?}"
        );
        let eof = read::<_, ClientFrame>(&mut b).await;
        assert!(matches!(eof, Ok(None)), "clean EOF is None: {eof:?}");
    }

    #[tokio::test]
    async fn rejects_oversized_and_foreign_frames_before_reading_the_body() {
        let (mut a, mut b) = tokio::io::duplex(64);
        let header = (MAX_FRAME_LEN + 1).to_le_bytes();
        assert!(a.write_all(&header).await.is_ok(), "header written");
        let got = read::<_, ClientFrame>(&mut b).await;
        assert!(
            matches!(got, Err(Error::FrameTooLarge(_))),
            "oversized frame rejected: {got:?}"
        );

        let (mut a, mut b) = tokio::io::duplex(64);
        assert!(
            a.write_all(&[5, 0, 0, 0, 0x02, 1, 2, 3, 4]).await.is_ok(),
            "raw frame written"
        );
        let got = read::<_, ClientFrame>(&mut b).await;
        assert!(
            matches!(got, Err(Error::UnexpectedKind(0x02))),
            "non-JSON kind rejected: {got:?}"
        );
    }

    #[tokio::test]
    async fn truncated_frame_is_an_error_not_eof() {
        let (mut a, mut b) = tokio::io::duplex(64);
        assert!(
            a.write_all(&[9, 0, 0, 0, KIND_JSON, b'{']).await.is_ok(),
            "partial frame written"
        );
        drop(a);
        let got = read::<_, ClientFrame>(&mut b).await;
        assert!(
            matches!(got, Err(Error::Io(_))),
            "truncation is an I/O error: {got:?}"
        );
    }
}
