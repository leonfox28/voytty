//! One bounded uploader shared by desktop and mobile. Never retries a transfer.

use crate::{
    error::ClientError,
    framing::FrameReader,
    model::ResolvedSessionTarget,
    protocol::{
        DEFAULT_DEADLINE, is_attachment_command_stream_closed, malformed, protocol_error,
        service_error, startup_budget,
    },
    transport::TransportFuture,
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::watch,
    time::{Instant, timeout, timeout_at},
};
use tokio_util::sync::CancellationToken;
use voytty_core::{Capabilities, DomainErrorKind, upload::*};
use voytty_proto::{DecodedFrame, WireKind, upload::UploadMessage};

/// Independently readable half; permits progress reads while a write is blocked.
pub trait UploadReader: Send {
    /// Reads the next service frame, retaining incomplete reads across cancellation.
    fn read(&mut self) -> TransportFuture<'_, Result<DecodedFrame, ClientError>>;
}
/// Independently writable half; adapters own local tunnel envelopes.
pub trait UploadWriter: Send {
    /// Sends one complete encoded service frame.
    fn write<'a>(&'a mut self, bytes: &'a [u8]) -> TransportFuture<'a, Result<(), ClientError>>;
}

/// Framed reader retaining the connection demand/admission for its lifetime.
pub struct FramedUploadReader<R> {
    frames: FrameReader<R>,
    _guard: Box<dyn Send>,
}
impl<R: AsyncRead + Unpin> FramedUploadReader<R> {
    /// Wraps a fresh raw stream and its adapter-owned admission guard.
    pub fn new(reader: R, guard: impl Send + 'static) -> Self {
        Self {
            frames: FrameReader::new(reader),
            _guard: Box::new(guard),
        }
    }
}
impl<R: AsyncRead + Unpin + Send> UploadReader for FramedUploadReader<R> {
    fn read(&mut self) -> TransportFuture<'_, Result<DecodedFrame, ClientError>> {
        Box::pin(self.frames.read())
    }
}

/// Raw writer for authenticated transports without local envelopes.
pub struct AsyncUploadWriter<W>(pub W);
impl<W: AsyncWrite + Unpin + Send> UploadWriter for AsyncUploadWriter<W> {
    fn write<'a>(&'a mut self, bytes: &'a [u8]) -> TransportFuture<'a, Result<(), ClientError>> {
        Box::pin(async move {
            self.0.write_all(bytes).await.map_err(|_| {
                ClientError::new(
                    DomainErrorKind::TransportUnavailable,
                    "upload stream write failed",
                )
            })
        })
    }
}

/// Capability metadata and independently owned halves for exactly one attempt.
pub struct UploadConnection {
    /// Negotiated on the connection actually serving this stream.
    pub capabilities: Capabilities,
    /// Progress and result reader.
    pub reader: Box<dyn UploadReader>,
    /// File-data writer.
    pub writer: Box<dyn UploadWriter>,
}

/// Platform adapter opening a separate service stream to a frozen target.
pub trait UploadConnector: Send + Sync {
    /// Opens one stream; never sends UploadBegin on a peer lacking the capability.
    fn open_upload(
        &self,
        target: ResolvedSessionTarget,
    ) -> TransportFuture<'_, Result<UploadConnection, ClientError>>;
}

/// Frozen destination captured by the existing terminal driver before preparation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UploadOrigin {
    /// Resolved host; display aliases never participate in transfer routing.
    pub target: ResolvedSessionTarget,
    /// Exact session and current attachment.
    pub binding: UploadBinding,
}

/// Latest preparation state used before source acquisition.
#[must_use]
pub const fn preparing() -> UploadProgress {
    UploadProgress {
        phase: UploadPhase::Preparing,
        accepted_bytes: 0,
        total_bytes: None,
    }
}

/// Encodes one validated upload path as a plain paste, with no modifiers or Enter.
#[must_use]
pub fn paste_bytes(file: &UploadedFile, modes: &voytty_core::terminal::TerminalModes) -> Vec<u8> {
    let path = if file.path().get(1..3) == Some(":/") {
        format!("'{}'", file.path())
    } else {
        file.path().to_owned()
    };
    if modes.bracketed_paste {
        [
            b"\x1b[200~".as_slice(),
            path.as_bytes(),
            b"\x1b[201~".as_slice(),
        ]
        .concat()
    } else {
        path.as_bytes().to_vec()
    }
}

/// Uploads one prepared readable source. Cancellation drops both independent halves.
pub async fn upload<R: AsyncRead + Unpin + Send>(
    connector: &dyn UploadConnector,
    target: ResolvedSessionTarget,
    binding: UploadBinding,
    metadata: UploadMetadata,
    source: R,
    progress: &watch::Sender<UploadProgress>,
    cancel: &CancellationToken,
) -> Result<UploadedFile, ClientError> {
    use voytty_diagnostics::{Event, Kind, Level, Operation, Outcome};
    let operation = Operation::default();
    let size = metadata.size();
    let context = |event: Event| {
        event
            .operation(&operation)
            .session(binding.session_id)
            .attachment(binding.attachment_id)
            .bytes(size)
    };
    voytty_diagnostics::record(context(Event::new(Kind::UploadStarted)));
    let result = upload_inner(
        connector, target, binding, metadata, source, progress, cancel,
    )
    .await;
    let event = match &result {
        Ok(_) => Event::new(Kind::UploadCompleted).outcome(Outcome::Success),
        Err(error) if error.kind() == DomainErrorKind::Cancelled => {
            Event::new(Kind::UploadCancelled).outcome(Outcome::Cancelled)
        }
        Err(error) => Event::new(Kind::UploadFailed)
            .level(Level::Warn)
            .error(error.kind())
            .outcome(if error.kind() == DomainErrorKind::UploadOutcomeUnknown {
                Outcome::Unknown
            } else {
                Outcome::Failed
            }),
    };
    voytty_diagnostics::record(context(event));
    result
}

const UPLOAD_CANCEL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

async fn upload_inner<R: AsyncRead + Unpin + Send>(
    connector: &dyn UploadConnector,
    target: ResolvedSessionTarget,
    binding: UploadBinding,
    metadata: UploadMetadata,
    source: R,
    progress: &watch::Sender<UploadProgress>,
    cancel: &CancellationToken,
) -> Result<UploadedFile, ClientError> {
    let mut connection = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(cancelled()),
        result = timeout(startup_budget(target), connector.open_upload(target)) => result.map_err(|_| timed_out())??,
    };
    if !connection
        .capabilities
        .contains(Capabilities::FILE_UPLOAD_SERVICE)
    {
        return Err(ClientError::new(
            DomainErrorKind::ServiceNotImplemented,
            "remote host or local daemon needs an upgrade to support file uploads",
        ));
    }
    let ready = async {
        send(
            &mut *connection.writer,
            UploadMessage::Begin {
                binding,
                metadata: metadata.clone(),
            },
        )
        .await?;
        match receive(&mut *connection.reader)
            .await
            .map_err(UploadReadError::into_error)?
        {
            UploadMessage::Ready { id, size } if size == metadata.size() => Ok(id),
            _ => Err(malformed("unexpected upload admission response")),
        }
    };
    let id = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(cancelled()),
        result = timeout(DEFAULT_DEADLINE, ready) => result.map_err(|_| timed_out())??,
    };
    let mut writer_clean = true;
    let result = {
        let operation = transfer(
            &mut connection,
            &mut writer_clean,
            id,
            metadata.size(),
            source,
            progress,
        );
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(cancelled()),
            result = operation => result,
        }
    };
    if writer_clean
        && result
            .as_ref()
            .is_err_and(|error| error.kind() == DomainErrorKind::Cancelled)
    {
        // Best effort only when no prior write was interrupted mid-frame.
        // Dropping the stream also retires staging if the link is stalled.
        let _ = timeout(
            UPLOAD_CANCEL_TIMEOUT,
            send(&mut *connection.writer, UploadMessage::Cancel { id }),
        )
        .await;
    }
    result
}

async fn transfer<R: AsyncRead + Unpin + Send>(
    connection: &mut UploadConnection,
    writer_clean: &mut bool,
    id: TransferId,
    size: u64,
    mut source: R,
    progress: &watch::Sender<UploadProgress>,
) -> Result<UploadedFile, ClientError> {
    report(progress, UploadPhase::Uploading, 0, size);
    let sent = Arc::new(AtomicU64::new(0));
    let (accepted, mut credit) = watch::channel(0_u64);
    let writer = &mut *connection.writer;
    let reader = &mut *connection.reader;
    let writing = async {
        let mut offset = 0;
        let mut bytes = vec![0_u8; UPLOAD_CHUNK_BYTES];
        while offset < size {
            while offset.saturating_sub(*credit.borrow_and_update()) >= UPLOAD_WINDOW_BYTES {
                timeout(UPLOAD_IDLE_TIMEOUT, credit.changed())
                    .await
                    .map_err(|_| timed_out())?
                    .map_err(|_| cancelled())?;
            }
            let available = UPLOAD_WINDOW_BYTES - offset.saturating_sub(*credit.borrow());
            let wanted = usize::try_from(
                (size - offset)
                    .min(UPLOAD_CHUNK_BYTES as u64)
                    .min(available),
            )
            .expect("bounded chunk length");
            let length = timeout(UPLOAD_IDLE_TIMEOUT, source.read(&mut bytes[..wanted]))
                .await
                .map_err(|_| timed_out())?
                .map_err(|_| source_invalid())?;
            if length == 0 {
                return Err(source_invalid());
            }
            sent.store(offset + length as u64, Ordering::Release);
            *writer_clean = false;
            timeout(
                UPLOAD_IDLE_TIMEOUT,
                send(
                    writer,
                    UploadMessage::Chunk {
                        id,
                        offset,
                        data: bytes[..length].to_vec(),
                    },
                ),
            )
            .await
            .map_err(|_| timed_out())??;
            *writer_clean = true;
            offset += length as u64;
        }
        let length = timeout(UPLOAD_IDLE_TIMEOUT, source.read(&mut bytes[..1]))
            .await
            .map_err(|_| timed_out())?
            .map_err(|_| source_invalid())?;
        if length != 0 {
            return Err(source_invalid());
        }
        Ok::<_, ClientError>(())
    };
    let reading = async {
        let mut previous = 0;
        let mut deadline = Instant::now() + UPLOAD_IDLE_TIMEOUT;
        while previous < size {
            match timeout_at(deadline, receive(reader))
                .await
                .map_err(|_| UploadReadError::Stream(timed_out()))??
            {
                UploadMessage::Progress {
                    id: received,
                    accepted_bytes,
                } if received == id
                    && accepted_bytes >= previous
                    && accepted_bytes <= sent.load(Ordering::Acquire) =>
                {
                    if accepted_bytes > previous {
                        previous = accepted_bytes;
                        accepted.send_replace(previous);
                        report(progress, UploadPhase::Uploading, previous, size);
                        deadline = Instant::now() + UPLOAD_IDLE_TIMEOUT;
                    }
                }
                _ => {
                    return Err(UploadReadError::Response(malformed(
                        "invalid upload progress",
                    )));
                }
            }
        }
        Ok::<_, UploadReadError>(())
    };
    {
        tokio::pin!(writing, reading);
        tokio::select! {
            biased;
            result = &mut writing => match result {
                Ok(()) => reading.await.map_err(UploadReadError::into_error),
                Err(error) if error.kind() == DomainErrorKind::TransportUnavailable
                    || is_attachment_command_stream_closed(&error) => {
                    // The host may retire its receive direction before its
                    // final error arrives. Keep the same reader, with one fixed
                    // response budget; the outer upload select still cancels it.
                    match timeout(DEFAULT_DEADLINE, reading).await {
                        Ok(Err(UploadReadError::Response(response))) => Err(response),
                        _ => Err(error),
                    }
                }
                Err(error) => Err(error),
            },
            result = &mut reading => {
                result.map_err(UploadReadError::into_error)?;
                writing.await
            },
        }?;
    }
    report(progress, UploadPhase::Finishing, size, size);
    // Once Finish begins, the host either commits or closes the ordered stream;
    // a subsequent Cancel could either split Finish or never be read.
    *writer_clean = false;
    let finish = async {
        send(&mut *connection.writer, UploadMessage::Finish { id, size }).await?;
        match receive(&mut *connection.reader)
            .await
            .map_err(UploadReadError::into_error)?
        {
            UploadMessage::Completed {
                id: completed,
                file,
            } if completed == id && file.size() == size => Ok(file),
            _ => Err(malformed("unexpected upload publication response")),
        }
    };
    let file = timeout(DEFAULT_DEADLINE, finish)
        .await
        .unwrap_or_else(|_| Err(timed_out()))
        .map_err(|error| {
            if matches!(
                error.kind(),
                DomainErrorKind::TransportUnavailable | DomainErrorKind::DeadlineExceeded
            ) {
                ClientError::new(
                    DomainErrorKind::UploadOutcomeUnknown,
                    "upload result was lost; a remote file may exist; retry creates a new file",
                )
            } else {
                error
            }
        })?;
    report(progress, UploadPhase::Completed, size, size);
    Ok(file)
}

async fn send(writer: &mut dyn UploadWriter, message: UploadMessage) -> Result<(), ClientError> {
    writer
        .write(&message.encode(1).map_err(protocol_error)?)
        .await
}
enum UploadReadError {
    Stream(ClientError),
    Response(ClientError),
}
impl UploadReadError {
    fn into_error(self) -> ClientError {
        match self {
            Self::Stream(error) | Self::Response(error) => error,
        }
    }
}

async fn receive(reader: &mut dyn UploadReader) -> Result<UploadMessage, UploadReadError> {
    let frame = reader.read().await.map_err(UploadReadError::Stream)?;
    decode_response(frame).map_err(UploadReadError::Response)
}
fn decode_response(frame: DecodedFrame) -> Result<UploadMessage, ClientError> {
    if frame.request_id != 1 || frame.deadline_ms != 0 {
        return Err(malformed("upload response correlation mismatch"));
    }
    if frame.kind == WireKind::ServiceErrorResponse {
        return Err(service_error(&frame)?);
    }
    UploadMessage::decode(&frame).map_err(protocol_error)
}
fn report(
    sender: &watch::Sender<UploadProgress>,
    phase: UploadPhase,
    accepted_bytes: u64,
    size: u64,
) {
    sender.send_replace(UploadProgress {
        phase,
        accepted_bytes,
        total_bytes: Some(size),
    });
}
fn cancelled() -> ClientError {
    ClientError::new(DomainErrorKind::Cancelled, "upload cancelled")
}
fn timed_out() -> ClientError {
    ClientError::new(
        DomainErrorKind::DeadlineExceeded,
        "upload made no progress within its deadline",
    )
}
fn source_invalid() -> ClientError {
    ClientError::new(
        DomainErrorKind::UploadSourceInvalid,
        "upload source is unreadable or changed size",
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_upload_paste_quotes_spaces_and_preserves_bracketed_mode() {
        let file = voytty_core::upload::UploadedFile::new(
            "C:/Users/用户 name/voytty-upload-S-1-5-21-1/a/b/file.txt".into(),
            0,
        )
        .expect("Windows path");
        let mut modes = voytty_core::terminal::TerminalModes::default();
        assert_eq!(
            super::paste_bytes(&file, &modes),
            format!("'{}'", file.path()).as_bytes()
        );
        modes.bracketed_paste = true;
        assert_eq!(
            super::paste_bytes(&file, &modes),
            format!("\x1b[200~'{}'\x1b[201~", file.path()).as_bytes()
        );
    }

    use super::*;
    use std::sync::Mutex;
    use voytty_core::{AttachmentId, SessionId};

    struct Connector(Mutex<Option<UploadConnection>>);
    impl UploadConnector for Connector {
        fn open_upload(
            &self,
            _: ResolvedSessionTarget,
        ) -> TransportFuture<'_, Result<UploadConnection, ClientError>> {
            Box::pin(async move {
                Ok(self
                    .0
                    .lock()
                    .expect("upload fixture")
                    .take()
                    .expect("a transfer must never reopen or replay"))
            })
        }
    }
    fn connection(capabilities: u64) -> (Connector, tokio::io::DuplexStream) {
        let (client, server) = tokio::io::duplex(1024);
        let (reader, writer) = tokio::io::split(client);
        (
            Connector(Mutex::new(Some(UploadConnection {
                capabilities: Capabilities::from_bits_retain(capabilities),
                reader: Box::new(FramedUploadReader::new(reader, ())),
                writer: Box::new(AsyncUploadWriter(writer)),
            }))),
            server,
        )
    }
    fn binding() -> UploadBinding {
        UploadBinding {
            session_id: SessionId::from_array([1; 16]),
            attachment_id: AttachmentId::from_array([2; 16]),
        }
    }

    struct RetirementWriter {
        inner: AsyncUploadWriter<tokio::io::DuplexStream>,
        tunnel: bool,
    }
    impl UploadWriter for RetirementWriter {
        fn write<'a>(
            &'a mut self,
            bytes: &'a [u8],
        ) -> TransportFuture<'a, Result<(), ClientError>> {
            Box::pin(async move {
                self.inner.write(bytes).await.map_err(|error| {
                    if self.tunnel {
                        crate::protocol::attachment_command_stream_closed()
                    } else {
                        error
                    }
                })
            })
        }
    }

    fn retiring_connection(
        tunnel: bool,
    ) -> (Connector, tokio::io::DuplexStream, tokio::io::DuplexStream) {
        // Independent directions let the host stop receiving data before its
        // final response arrives, as can happen on a real upload stream.
        let (writer, host_reader) = tokio::io::duplex(1024);
        let (host_writer, reader) = tokio::io::duplex(1024);
        (
            Connector(Mutex::new(Some(UploadConnection {
                capabilities: Capabilities::from_bits_retain(Capabilities::FILE_UPLOAD_SERVICE),
                reader: Box::new(FramedUploadReader::new(reader, ())),
                writer: Box::new(RetirementWriter {
                    inner: AsyncUploadWriter(writer),
                    tunnel,
                }),
            }))),
            host_reader,
            host_writer,
        )
    }

    async fn retire_upload(reader: tokio::io::DuplexStream, writer: &mut tokio::io::DuplexStream) {
        let mut reader = FrameReader::new(reader);
        assert!(matches!(
            UploadMessage::decode(&reader.read().await.expect("upload fixture"))
                .expect("upload fixture"),
            UploadMessage::Begin { .. }
        ));
        drop(reader);
        writer
            .write_all(
                &UploadMessage::Ready {
                    id: TransferId::from_array([3; 16]),
                    size: 3,
                }
                .encode(1)
                .expect("upload fixture"),
            )
            .await
            .expect("upload fixture");
    }

    #[tokio::test(start_paused = true)]
    async fn retired_upload_preserves_service_error() {
        for (tunnel, delay, kind) in [
            (false, std::time::Duration::ZERO, DomainErrorKind::LeaseLost),
            (
                false,
                std::time::Duration::from_secs(1),
                DomainErrorKind::LeaseLost,
            ),
            (
                true,
                std::time::Duration::from_secs(1),
                DomainErrorKind::LeaseLost,
            ),
            (
                false,
                std::time::Duration::from_secs(1),
                DomainErrorKind::DeadlineExceeded,
            ),
        ] {
            let (connector, reader, mut writer) = retiring_connection(tunnel);
            let host = tokio::spawn(async move {
                retire_upload(reader, &mut writer).await;
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                // A queued progress frame must not hide the terminal error.
                let mut response = UploadMessage::Progress {
                    id: TransferId::from_array([3; 16]),
                    accepted_bytes: 0,
                }
                .encode(1)
                .expect("upload fixture");
                response.extend(
                    voytty_proto::encode_message(
                        WireKind::ServiceErrorResponse,
                        1,
                        0,
                        &voytty_proto::v2::ServiceError {
                            code: kind.code().to_owned(),
                            message: "host rejected the upload".to_owned(),
                        },
                    )
                    .expect("upload fixture"),
                );
                writer.write_all(&response).await.expect("upload fixture");
            });
            let (progress, _) = watch::channel(preparing());
            let error = upload(
                &connector,
                ResolvedSessionTarget::local(),
                binding(),
                UploadMetadata::new(3, "").expect("upload fixture"),
                b"abc".as_slice(),
                &progress,
                &CancellationToken::new(),
            )
            .await
            .expect_err("upload must fail");
            assert_eq!(error.kind(), kind);
            assert_eq!(error.detail(), "host rejected the upload");
            assert_eq!(progress.borrow().phase, UploadPhase::Uploading);
            host.await.expect("upload fixture");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retired_upload_without_service_error_preserves_write_failure() {
        for keep_open in [false, true] {
            let (connector, reader, mut writer) = retiring_connection(false);
            let host = tokio::spawn(async move {
                retire_upload(reader, &mut writer).await;
                // The completed task retains the silent response direction
                // until its JoinHandle is consumed after the upload returns.
                keep_open.then_some(writer)
            });
            let (progress, _) = watch::channel(preparing());
            let started = Instant::now();
            let error = upload(
                &connector,
                ResolvedSessionTarget::local(),
                binding(),
                UploadMetadata::new(3, "").expect("upload fixture"),
                b"abc".as_slice(),
                &progress,
                &CancellationToken::new(),
            )
            .await
            .expect_err("upload must fail");
            assert_eq!(error.kind(), DomainErrorKind::TransportUnavailable);
            assert_eq!(error.detail(), "upload stream write failed");
            assert_eq!(
                Instant::now() - started,
                if keep_open {
                    DEFAULT_DEADLINE
                } else {
                    std::time::Duration::ZERO
                }
            );
            host.await.expect("upload fixture");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retired_upload_response_wait_remains_cancellable() {
        let (connector, reader, mut writer) = retiring_connection(false);
        let cancel = CancellationToken::new();
        let host_cancel = cancel.clone();
        let host = tokio::spawn(async move {
            retire_upload(reader, &mut writer).await;
            tokio::time::sleep(DEFAULT_DEADLINE / 2).await;
            host_cancel.cancel();
            writer
        });
        let (progress, _) = watch::channel(preparing());
        let started = Instant::now();
        let error = upload(
            &connector,
            ResolvedSessionTarget::local(),
            binding(),
            UploadMetadata::new(3, "").expect("upload fixture"),
            b"abc".as_slice(),
            &progress,
            &cancel,
        )
        .await
        .expect_err("upload must fail");
        assert_eq!(error.kind(), DomainErrorKind::Cancelled);
        assert_eq!(Instant::now() - started, DEFAULT_DEADLINE / 2);
        host.await.expect("upload fixture");
    }

    #[tokio::test(start_paused = true)]
    async fn invalid_upload_source_does_not_wait_for_retirement_response() {
        let (connector, reader, mut writer) = retiring_connection(false);
        let host = tokio::spawn(async move {
            retire_upload(reader, &mut writer).await;
            writer
        });
        let (progress, _) = watch::channel(preparing());
        let started = Instant::now();
        let error = upload(
            &connector,
            ResolvedSessionTarget::local(),
            binding(),
            UploadMetadata::new(3, "").expect("upload fixture"),
            [].as_slice(),
            &progress,
            &CancellationToken::new(),
        )
        .await
        .expect_err("upload must fail");
        assert_eq!(error.kind(), DomainErrorKind::UploadSourceInvalid);
        assert_eq!(Instant::now() - started, std::time::Duration::ZERO);
        host.await.expect("upload fixture");
    }

    #[tokio::test]
    async fn old_host_receives_no_upload_bytes() {
        let (connector, mut server) = connection(0);
        let (progress, _) = watch::channel(preparing());
        let error = upload(
            &connector,
            ResolvedSessionTarget::local(),
            binding(),
            UploadMetadata::new(0, "").expect("upload fixture"),
            [].as_slice(),
            &progress,
            &CancellationToken::new(),
        )
        .await
        .expect_err("upload must fail");
        assert_eq!(error.kind(), DomainErrorKind::ServiceNotImplemented);
        assert_eq!(server.read(&mut [0; 1]).await.expect("upload fixture"), 0);
    }

    #[tokio::test]
    async fn lost_publication_reply_is_unknown_without_replay() {
        let (connector, server) = connection(Capabilities::FILE_UPLOAD_SERVICE);
        let host = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server);
            let mut reader = FrameReader::new(reader);
            assert!(matches!(
                UploadMessage::decode(&reader.read().await.expect("upload fixture"))
                    .expect("upload fixture"),
                UploadMessage::Begin { .. }
            ));
            let id = TransferId::from_array([3; 16]);
            writer
                .write_all(
                    &UploadMessage::Ready { id, size: 3 }
                        .encode(1)
                        .expect("upload fixture"),
                )
                .await
                .expect("upload fixture");
            assert!(
                matches!(UploadMessage::decode(&reader.read().await.expect("upload fixture")).expect("upload fixture"), UploadMessage::Chunk { data, .. } if data == b"abc")
            );
            writer
                .write_all(
                    &UploadMessage::Progress {
                        id,
                        accepted_bytes: 3,
                    }
                    .encode(1)
                    .expect("upload fixture"),
                )
                .await
                .expect("upload fixture");
            assert!(matches!(
                UploadMessage::decode(&reader.read().await.expect("upload fixture"))
                    .expect("upload fixture"),
                UploadMessage::Finish { .. }
            ));
            // Publication occurred; deliberately lose its response.
        });
        let (progress, _) = watch::channel(preparing());
        let error = upload(
            &connector,
            ResolvedSessionTarget::local(),
            binding(),
            UploadMetadata::new(3, "").expect("upload fixture"),
            b"abc".as_slice(),
            &progress,
            &CancellationToken::new(),
        )
        .await
        .expect_err("upload must fail");
        assert_eq!(error.kind(), DomainErrorKind::UploadOutcomeUnknown);
        assert_eq!(progress.borrow().phase, UploadPhase::Finishing);
        host.await.expect("upload fixture");
    }

    #[tokio::test(start_paused = true)]
    async fn remote_open_upload_uses_remote_startup_deadline_while_local_keeps_control_deadline() {
        struct DelayedConnector {
            delay: std::time::Duration,
            connection: Mutex<Option<UploadConnection>>,
        }
        impl UploadConnector for DelayedConnector {
            fn open_upload(
                &self,
                _: ResolvedSessionTarget,
            ) -> TransportFuture<'_, Result<UploadConnection, ClientError>> {
                let delay = self.delay;
                Box::pin(async move {
                    tokio::time::sleep(delay).await;
                    Ok(self
                        .connection
                        .lock()
                        .expect("delayed upload fixture")
                        .take()
                        .expect("single upload connection"))
                })
            }
        }

        let remote_target =
            ResolvedSessionTarget::device(voytty_core::DeviceId::from_array([9; 32]));
        let (client, server) = tokio::io::duplex(1024);
        let (reader, writer) = tokio::io::split(client);
        let connector = DelayedConnector {
            delay: DEFAULT_DEADLINE + std::time::Duration::from_secs(1),
            connection: Mutex::new(Some(UploadConnection {
                capabilities: Capabilities::from_bits_retain(Capabilities::FILE_UPLOAD_SERVICE),
                reader: Box::new(FramedUploadReader::new(reader, ())),
                writer: Box::new(AsyncUploadWriter(writer)),
            })),
        };
        let host = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server);
            let mut reader = FrameReader::new(reader);
            assert!(matches!(
                UploadMessage::decode(&reader.read().await.expect("begin")).expect("decode begin"),
                UploadMessage::Begin { .. }
            ));
            let id = TransferId::from_array([4; 16]);
            writer
                .write_all(
                    &UploadMessage::Ready { id, size: 0 }
                        .encode(1)
                        .expect("ready"),
                )
                .await
                .expect("write ready");
            assert!(matches!(
                UploadMessage::decode(&reader.read().await.expect("finish"))
                    .expect("decode finish"),
                UploadMessage::Finish { size: 0, .. }
            ));
            let file = UploadedFile::new("/tmp/voytty-upload-1/s/t/file".into(), 0)
                .expect("uploaded file");
            writer
                .write_all(
                    &UploadMessage::Completed { id, file }
                        .encode(1)
                        .expect("completed"),
                )
                .await
                .expect("write completed");
        });
        let (progress, _) = watch::channel(preparing());
        let file = upload(
            &connector,
            remote_target,
            binding(),
            UploadMetadata::new(0, "").expect("metadata"),
            [].as_slice(),
            &progress,
            &CancellationToken::new(),
        )
        .await
        .expect("remote cold open within 15s budget must succeed");
        assert_eq!(file.size(), 0);
        host.await.expect("host task");

        for (target, expected) in [
            (ResolvedSessionTarget::local(), DEFAULT_DEADLINE),
            (remote_target, crate::protocol::REMOTE_STARTUP_DEADLINE),
        ] {
            let connector = DelayedConnector {
                delay: expected + std::time::Duration::from_secs(1),
                connection: Mutex::new(None),
            };
            let started = Instant::now();
            let error = upload(
                &connector,
                target,
                binding(),
                UploadMetadata::new(0, "").expect("metadata"),
                [].as_slice(),
                &progress,
                &CancellationToken::new(),
            )
            .await
            .expect_err("open exceeding budget must time out");
            assert_eq!(error.kind(), DomainErrorKind::DeadlineExceeded);
            assert_eq!(Instant::now() - started, expected);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_mid_chunk_write_drops_stream_without_sending_cancel_or_blocking() {
        struct StallingChunkWriter {
            inner: AsyncUploadWriter<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
            writes: Arc<std::sync::atomic::AtomicUsize>,
            chunk_started: Option<tokio::sync::oneshot::Sender<()>>,
        }
        impl UploadWriter for StallingChunkWriter {
            fn write<'a>(
                &'a mut self,
                bytes: &'a [u8],
            ) -> TransportFuture<'a, Result<(), ClientError>> {
                let count = self.writes.fetch_add(1, Ordering::SeqCst);
                if count == 0 {
                    self.inner.write(bytes)
                } else if count == 1 {
                    let started = self.chunk_started.take();
                    Box::pin(async move {
                        if let Some(tx) = started {
                            let _ = tx.send(());
                        }
                        // Simulate a partial frame write stalled mid-flight.
                        std::future::pending::<Result<(), ClientError>>().await
                    })
                } else {
                    panic!("must not write Cancel after cancelling a mid-frame chunk write");
                }
            }
        }

        let (client, server) = tokio::io::duplex(1024);
        let (reader, writer) = tokio::io::split(client);
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (chunk_started_tx, chunk_started_rx) = tokio::sync::oneshot::channel();
        let connector = Connector(Mutex::new(Some(UploadConnection {
            capabilities: Capabilities::from_bits_retain(Capabilities::FILE_UPLOAD_SERVICE),
            reader: Box::new(FramedUploadReader::new(reader, ())),
            writer: Box::new(StallingChunkWriter {
                inner: AsyncUploadWriter(writer),
                writes: Arc::clone(&writes),
                chunk_started: Some(chunk_started_tx),
            }),
        })));
        let host = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server);
            let mut reader = FrameReader::new(reader);
            assert!(matches!(
                UploadMessage::decode(&reader.read().await.expect("begin")).expect("decode begin"),
                UploadMessage::Begin { .. }
            ));
            writer
                .write_all(
                    &UploadMessage::Ready {
                        id: TransferId::from_array([5; 16]),
                        size: 3,
                    }
                    .encode(1)
                    .expect("ready"),
                )
                .await
                .expect("write ready");
            (reader, writer)
        });
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let canceller = tokio::spawn(async move {
            chunk_started_rx.await.expect("chunk write began");
            trigger.cancel();
        });
        let (progress, _) = watch::channel(preparing());
        let started = Instant::now();
        let error = upload(
            &connector,
            ResolvedSessionTarget::local(),
            binding(),
            UploadMetadata::new(3, "").expect("metadata"),
            b"abc".as_slice(),
            &progress,
            &cancel,
        )
        .await
        .expect_err("cancelled upload must fail");
        assert_eq!(error.kind(), DomainErrorKind::Cancelled);
        assert_eq!(Instant::now() - started, std::time::Duration::ZERO);
        assert_eq!(writes.load(Ordering::SeqCst), 2);
        canceller.await.expect("canceller");
        let _ = host.await.expect("host");
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_at_frame_boundary_sends_cancel_with_short_timeout() {
        // Case 1: healthy writer delivers clean UploadMessage::Cancel at a frame boundary.
        let (connector, server) = connection(Capabilities::FILE_UPLOAD_SERVICE);
        let id = TransferId::from_array([6; 16]);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let host = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server);
            let mut reader = FrameReader::new(reader);
            assert!(matches!(
                UploadMessage::decode(&reader.read().await.expect("begin")).expect("decode begin"),
                UploadMessage::Begin { .. }
            ));
            writer
                .write_all(
                    &UploadMessage::Ready { id, size: 3 }
                        .encode(1)
                        .expect("ready"),
                )
                .await
                .expect("write ready");
            let _ = ready_tx.send(());
            assert!(matches!(
                UploadMessage::decode(&reader.read().await.expect("cancel"))
                    .expect("decode cancel"),
                UploadMessage::Cancel { id: cancelled } if cancelled == id
            ));
        });
        let (source_writer, source_reader) = tokio::io::duplex(64);
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let canceller = tokio::spawn(async move {
            ready_rx.await.expect("ready sent");
            tokio::task::yield_now().await;
            trigger.cancel();
        });
        let (progress, _) = watch::channel(preparing());
        let error = upload(
            &connector,
            ResolvedSessionTarget::local(),
            binding(),
            UploadMetadata::new(3, "").expect("metadata"),
            source_reader,
            &progress,
            &cancel,
        )
        .await
        .expect_err("cancelled upload must fail");
        assert_eq!(error.kind(), DomainErrorKind::Cancelled);
        drop(source_writer);
        canceller.await.expect("canceller");
        host.await.expect("host must observe clean Cancel");

        // Case 2: stalled writer between frames bounds best-effort Cancel to UPLOAD_CANCEL_TIMEOUT.
        struct StallingCancelWriter {
            inner: AsyncUploadWriter<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
            writes: usize,
        }
        impl UploadWriter for StallingCancelWriter {
            fn write<'a>(
                &'a mut self,
                bytes: &'a [u8],
            ) -> TransportFuture<'a, Result<(), ClientError>> {
                self.writes += 1;
                if self.writes == 1 {
                    self.inner.write(bytes)
                } else {
                    Box::pin(std::future::pending())
                }
            }
        }
        let (client, server) = tokio::io::duplex(1024);
        let (reader, writer) = tokio::io::split(client);
        let connector = Connector(Mutex::new(Some(UploadConnection {
            capabilities: Capabilities::from_bits_retain(Capabilities::FILE_UPLOAD_SERVICE),
            reader: Box::new(FramedUploadReader::new(reader, ())),
            writer: Box::new(StallingCancelWriter {
                inner: AsyncUploadWriter(writer),
                writes: 0,
            }),
        })));
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let host = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server);
            let mut reader = FrameReader::new(reader);
            assert!(matches!(
                UploadMessage::decode(&reader.read().await.expect("begin")).expect("decode begin"),
                UploadMessage::Begin { .. }
            ));
            writer
                .write_all(
                    &UploadMessage::Ready { id, size: 3 }
                        .encode(1)
                        .expect("ready"),
                )
                .await
                .expect("write ready");
            let _ = ready_tx.send(());
            (reader, writer)
        });
        let (source_writer, source_reader) = tokio::io::duplex(64);
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let canceller = tokio::spawn(async move {
            ready_rx.await.expect("ready sent");
            tokio::task::yield_now().await;
            trigger.cancel();
        });
        let started = Instant::now();
        let error = upload(
            &connector,
            ResolvedSessionTarget::local(),
            binding(),
            UploadMetadata::new(3, "").expect("metadata"),
            source_reader,
            &progress,
            &cancel,
        )
        .await
        .expect_err("cancelled upload must fail");
        assert_eq!(error.kind(), DomainErrorKind::Cancelled);
        assert_eq!(Instant::now() - started, UPLOAD_CANCEL_TIMEOUT);
        drop(source_writer);
        canceller.await.expect("canceller");
        let _ = host.await.expect("host");
    }
}
