//! Bounded first-frame decoding with retained stream leftovers.
use crate::{error::ClientError, protocol::protocol_error};
use std::collections::VecDeque;
use tokio::io::{AsyncRead, AsyncReadExt};
use voytty_core::DomainErrorKind;
use voytty_proto::{DecodedFrame, FrameDecoder};
use zeroize::Zeroizing;

/// One decoded first frame plus the single decoder's retained leftovers.
pub struct FirstFrame {
    /// First complete frame.
    pub frame: DecodedFrame,
    /// Decoder holding any partial following frame.
    pub decoder: FrameDecoder,
    /// Complete coalesced frames following the first.
    pub queued: VecDeque<DecodedFrame>,
}

/// Cancellation-safe framed reader reused by independent streaming services.
pub struct FrameReader<R> {
    reader: R,
    decoder: FrameDecoder,
    queued: VecDeque<DecodedFrame>,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    /// Starts framing a fresh stream.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            decoder: FrameDecoder::new(),
            queued: VecDeque::new(),
        }
    }

    /// Continues after a dispatcher consumed the first frame.
    pub fn after_first(reader: R, first: FirstFrame) -> Self {
        Self {
            reader,
            decoder: first.decoder,
            queued: first.queued,
        }
    }

    /// Reads one frame, retaining partial and coalesced bytes across cancellation.
    pub async fn read(&mut self) -> Result<DecodedFrame, ClientError> {
        read_queued_frame(
            &mut self.reader,
            &mut self.decoder,
            &mut self.queued,
            |_| {
                ClientError::new(
                    DomainErrorKind::TransportUnavailable,
                    "service stream read failed",
                )
            },
        )
        .await?
        .ok_or_else(|| {
            ClientError::new(
                DomainErrorKind::TransportUnavailable,
                "service stream closed",
            )
        })
    }
}

/// Reads the next frame, retaining partial bytes and coalesced frames if cancelled.
/// EOF leaves the decoder intact so the caller can validate a half-close or discard a lost transport.
pub async fn read_queued_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    decoder: &mut FrameDecoder,
    queued: &mut VecDeque<DecodedFrame>,
    map_io: impl Fn(std::io::Error) -> ClientError,
) -> Result<Option<DecodedFrame>, ClientError> {
    if let Some(frame) = queued.pop_front() {
        return Ok(Some(frame));
    }
    let mut buffer = Zeroizing::new([0_u8; 16 * 1024]);
    loop {
        let read = reader.read(&mut *buffer).await.map_err(&map_io)?;
        if read == 0 {
            return Ok(None);
        }
        queued.extend(decoder.feed(&buffer[..read]).map_err(protocol_error)?);
        if let Some(frame) = queued.pop_front() {
            return Ok(Some(frame));
        }
    }
}

/// Reads exactly through the first complete frame while retaining decoder
/// state and any additional frames received by the same bounded read.
pub async fn read_first<Reader>(reader: &mut Reader) -> Result<FirstFrame, ClientError>
where
    Reader: AsyncRead + Unpin,
{
    read_first_with_decoder(reader, FrameDecoder::new()).await
}

/// Reads controller requests without inflating host-to-controller surface frames.
pub async fn read_first_request<Reader: AsyncRead + Unpin>(
    reader: &mut Reader,
) -> Result<FirstFrame, ClientError> {
    read_first_with_decoder(reader, FrameDecoder::for_requests()).await
}

async fn read_first_with_decoder<Reader: AsyncRead + Unpin>(
    reader: &mut Reader,
    mut decoder: FrameDecoder,
) -> Result<FirstFrame, ClientError> {
    let mut queued = VecDeque::new();
    match read_queued_frame(reader, &mut decoder, &mut queued, |e| {
        daemon_io("read Session request", e)
    })
    .await?
    {
        Some(frame) => Ok(FirstFrame {
            frame,
            decoder,
            queued,
        }),
        None => {
            decoder.finish().map_err(protocol_error)?;
            Err(ClientError::new(
                DomainErrorKind::Cancelled,
                "client closed before sending a Session request",
            ))
        }
    }
}

fn daemon_io(operation: &str, error: std::io::Error) -> ClientError {
    ClientError::new(
        DomainErrorKind::DaemonStopped,
        format!("{operation}: {error}"),
    )
}
