//! Bounded same-UID listener, first-frame ingress, and server dispatch.
#[cfg(unix)]
use tokio::net::UnixStream as LocalStream;
#[cfg(windows)]
use voytty_platform::local::Stream as LocalStream;

use crate::{error::DaemonError, service::DaemonService};
#[cfg(any(unix, windows))]
use crate::{
    service::{ServiceReply, protocol_error},
    session_wire::{SessionWireLimits, SessionWireServer, finish_unary, read_first},
};
#[cfg(any(unix, windows))]
use iroh::SecretKey;
use std::sync::Arc;
#[cfg(any(unix, windows))]
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
#[cfg(any(unix, windows))]
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    sync::{Semaphore, mpsc},
    task::{AbortHandle, JoinSet},
};
use voytty_core::DomainErrorKind;
#[cfg(any(unix, windows))]
use voytty_core::{AttachmentId, DeviceId, ResourceLimits};
#[cfg(any(unix, windows))]
use voytty_proto::{DecodedFrame, WireKind, v2};
#[cfg(any(unix, windows))]
const DEFAULT_DEADLINE: Duration = Duration::from_secs(5);
/// After a stop, how long local terminal attachments may take to write their
/// Session's end. Their Sessions have already ended, so this covers only the
/// final update and one small write; a client that stops reading waits it out.
/// It is short because `voytty daemon stop` allows five seconds for the whole
/// remaining shutdown.
#[cfg(any(unix, windows))]
const ATTACHMENT_END_DELIVERY: Duration = Duration::from_secs(1);

// Existing low-level callers retain their entry points during the module move.
#[cfg(any(unix, windows))]
pub use crate::client::{
    LocalAttachmentEvent, LocalPairingClient, LocalTakeoverRetryToken,
    SessionClient as LocalAttachmentClient,
};
pub use crate::client::{LocalClient, LocalDeviceClient};

/// Fixed production limits with a reduced test constructor for deadline evidence.
#[cfg(any(unix, windows))]
#[derive(Clone, Copy, Debug)]
pub struct LocalIpcLimits {
    initial_read_timeout: Duration,
    default_request_deadline: Duration,
    maximum_request_deadline: Duration,
    maximum_connections: usize,
    injected_accept_failures: usize,
    injected_accept_after: usize,
    injected_fatal_accept_failures: usize,
    injected_fatal_accept_after: usize,
}

#[cfg(any(unix, windows))]
impl Default for LocalIpcLimits {
    fn default() -> Self {
        let resources = ResourceLimits::default();
        Self {
            initial_read_timeout: DEFAULT_DEADLINE,
            default_request_deadline: DEFAULT_DEADLINE,
            maximum_request_deadline: Duration::from_secs(u64::from(
                resources.max_local_deadline_seconds,
            )),
            maximum_connections: resources.max_local_connections,
            injected_accept_failures: 0,
            injected_accept_after: 0,
            injected_fatal_accept_failures: 0,
            injected_fatal_accept_after: 0,
        }
    }
}

#[cfg(any(unix, windows))]
impl LocalIpcLimits {
    /// Creates bounded limits for deterministic isolated tests.
    #[doc(hidden)]
    #[must_use]
    pub const fn for_test(read_timeout: Duration) -> Self {
        Self {
            initial_read_timeout: read_timeout,
            default_request_deadline: read_timeout,
            maximum_request_deadline: read_timeout,
            maximum_connections: 32,
            injected_accept_failures: 0,
            injected_accept_after: 0,
            injected_fatal_accept_failures: 0,
            injected_fatal_accept_after: 0,
        }
    }

    /// Injects recoverable listener accept failures for lifecycle tests.
    #[doc(hidden)]
    #[must_use]
    pub const fn with_accept_failures_for_test(mut self, failures: usize) -> Self {
        self.injected_accept_failures = failures;
        self
    }

    /// Injects failures only after a number of successful accepts.
    #[doc(hidden)]
    #[must_use]
    pub const fn with_accept_failure_after_for_test(mut self, successful_accepts: usize) -> Self {
        self.injected_accept_failures = 1;
        self.injected_accept_after = successful_accepts;
        self
    }

    /// Injects one fatal accept failure after the requested successful accepts.
    #[doc(hidden)]
    #[must_use]
    pub const fn with_fatal_accept_failure_after_for_test(
        mut self,
        successful_accepts: usize,
    ) -> Self {
        self.injected_fatal_accept_failures = 1;
        self.injected_fatal_accept_after = successful_accepts;
        self
    }

    pub(crate) const fn without_accept_failure_injection(mut self) -> Self {
        self.injected_accept_failures = 0;
        self.injected_fatal_accept_failures = 0;
        self
    }

    fn request_deadline(self, requested_ms: u32) -> Duration {
        if requested_ms == 0 {
            self.default_request_deadline
        } else {
            Duration::from_millis(u64::from(requested_ms)).min(self.maximum_request_deadline)
        }
    }

    fn session_wire_limits(self) -> SessionWireLimits {
        SessionWireLimits::new(
            self.default_request_deadline,
            self.maximum_request_deadline,
            DEFAULT_DEADLINE,
        )
    }
}

/// Runs one local listener until a committed stop finishes its acknowledgement attempt.
#[cfg(any(unix, windows))]
pub async fn serve_local(
    listener: voytty_platform::local::Listener,
    expected_uid: u32,
    service: Arc<DaemonService>,
) -> Result<(), DaemonError> {
    serve_local_with_limits(listener, expected_uid, service, LocalIpcLimits::default()).await
}

/// Runs a listener with reduced limits for isolated protocol tests.
#[cfg(any(unix, windows))]
#[doc(hidden)]
pub async fn serve_local_with_limits(
    listener: voytty_platform::local::Listener,
    expected_uid: u32,
    service: Arc<DaemonService>,
    limits: LocalIpcLimits,
) -> Result<(), DaemonError> {
    serve_local_inner(
        listener,
        expected_uid,
        service,
        limits,
        #[cfg(test)]
        None,
    )
    .await
}

#[cfg(any(unix, windows))]
async fn serve_local_inner(
    listener: voytty_platform::local::Listener,
    expected_uid: u32,
    service: Arc<DaemonService>,
    limits: LocalIpcLimits,
    #[cfg(test)] handler_reaped_for_test: Option<mpsc::UnboundedSender<usize>>,
) -> Result<(), DaemonError> {
    listener
        .set_nonblocking(true)
        .map_err(|error| daemon_io("configure local listener", error))?;
    let listener = voytty_platform::local::AsyncListener::from_std(listener)
        .map_err(|error| daemon_io("adopt local listener", error))?;
    let permits = Arc::new(Semaphore::new(limits.maximum_connections));
    let (stop_sender, mut stop_receiver) = mpsc::unbounded_channel();
    let mut handlers = JoinSet::new();
    // Each live handler, and whether it serves a local terminal attachment.
    let mut handler_kinds: Vec<(AbortHandle, Arc<AtomicBool>)> = Vec::new();
    let mut injected_accept_failures = limits.injected_accept_failures;
    let mut injected_fatal_accept_failures = limits.injected_fatal_accept_failures;
    let mut successful_accepts = 0_usize;
    let mut fatal_error = None;

    loop {
        tokio::select! {
            biased;
            stop = stop_receiver.recv() => {
                if stop.is_some() {
                    break;
                }
            }
            joined = handlers.join_next(), if !handlers.is_empty() => {
                observe_local_handler_completion(joined);
                handler_kinds.retain(|(handler, _)| !handler.is_finished());
                #[cfg(test)]
                if let Some(observer) = &handler_reaped_for_test {
                    let _ = observer.send(handlers.len());
                }
            }
            accepted = async {
                if injected_fatal_accept_failures > 0
                    && successful_accepts >= limits.injected_fatal_accept_after
                {
                    injected_fatal_accept_failures -= 1;
                    Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "injected fatal local listener accept failure",
                    ))
                } else if injected_accept_failures > 0
                    && successful_accepts >= limits.injected_accept_after
                {
                    injected_accept_failures -= 1;
                    Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "injected recoverable local listener accept failure",
                    ))
                } else {
                    listener.accept().await
                }
            } => {
                let (stream, _) = match accepted {
                    Ok(accepted) => {
                        successful_accepts = successful_accepts.saturating_add(1);
                        accepted
                    }
                    Err(error) => {
                        if recoverable_accept_error(&error) {
                            // Per-connection accept failures do not transfer or
                            // invalidate daemon/session ownership.
                            voytty_diagnostics::record(voytty_diagnostics::Event::new(voytty_diagnostics::Kind::ListenerFailed).level(voytty_diagnostics::Level::Warn).error(DomainErrorKind::TransportUnavailable));
                            tracing::warn!(
                                error_kind = ?error.kind(),
                                "local listener accept failed; retrying"
                            );
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            continue;
                        }
                        fatal_error = Some(daemon_io("accept local connection", error));
                        break;
                    }
                };
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    voytty_diagnostics::record(voytty_diagnostics::Event::new(voytty_diagnostics::Kind::AdmissionRejected).level(voytty_diagnostics::Level::Warn).error(DomainErrorKind::ResourceExhausted));
                    drop(stream);
                    continue;
                };
                let service = Arc::clone(&service);
                let stop_sender = stop_sender.clone();
                let local_attachment = Arc::new(AtomicBool::new(false));
                let marked = Arc::clone(&local_attachment);
                let handler = handlers.spawn(async move {
                    let _permit = permit;
                    handle_connection(stream, expected_uid, service, stop_sender, limits, marked)
                        .await;
                });
                handler_kinds.push((handler, local_attachment));
            }
        }
    }

    drop(listener);
    drop(stop_sender);
    // An explicit stop arrives only after Session shutdown has succeeded and
    // its acknowledgement has been attempted. Retire this listener's remaining
    // connections now: an idle first frame or a remote controller tunnel has
    // no completion that the stopping daemon should await. A local terminal
    // attachment does: its writer may still be fetching the final update for
    // the SessionEnded that tells the user why the terminal closed. Give those
    // a short bound to finish on their own. After a fatal listener error the
    // Sessions are still live, so their attachments would never finish. The
    // lifecycle owner still joins Session/pairing/network effects before
    // releasing its lock.
    let delivered = if fatal_error.is_none() {
        for (handler, local_attachment) in &handler_kinds {
            if !local_attachment.load(Ordering::Acquire) {
                handler.abort();
            }
        }
        tokio::time::timeout(ATTACHMENT_END_DELIVERY, async {
            while let Some(joined) = handlers.join_next().await {
                observe_retired_local_handler(joined);
            }
        })
        .await
        .is_ok()
    } else {
        false
    };
    if !delivered {
        handlers.abort_all();
        while let Some(joined) = handlers.join_next().await {
            observe_retired_local_handler(joined);
        }
    }
    fatal_error.map_or(Ok(()), Err)
}

#[cfg(any(unix, windows))]
fn observe_retired_local_handler(joined: Result<(), tokio::task::JoinError>) {
    if !joined
        .as_ref()
        .is_err_and(tokio::task::JoinError::is_cancelled)
    {
        observe_local_handler_completion(Some(joined));
    }
}

#[cfg(any(unix, windows))]
fn observe_local_handler_completion(joined: Option<Result<(), tokio::task::JoinError>>) {
    match joined {
        Some(Ok(())) => {}
        Some(Err(error)) => tracing::warn!(
            cancelled = error.is_cancelled(),
            panicked = error.is_panic(),
            "local connection handler task ended unexpectedly"
        ),
        None => {
            tracing::warn!("local connection handler ownership ended without a join result");
        }
    }
}

#[cfg(any(unix, windows))]
fn recoverable_accept_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::TimedOut
    )
}

#[cfg(not(any(unix, windows)))]
/// Returns the current platform limitation on unsupported targets.
pub async fn serve_local(
    _listener: (),
    _expected_uid: u32,
    _service: Arc<DaemonService>,
) -> Result<(), DaemonError> {
    Err(DaemonError::new(
        DomainErrorKind::UnsupportedPlatform,
        "local daemon IPC requires Unix or Windows",
    ))
}

#[cfg(any(unix, windows))]
async fn handle_connection(
    mut stream: LocalStream,
    expected_uid: u32,
    service: Arc<DaemonService>,
    stop_sender: mpsc::UnboundedSender<()>,
    limits: LocalIpcLimits,
    local_attachment: Arc<AtomicBool>,
) {
    if voytty_platform::local::authorize_stream_peer(&stream, expected_uid).is_err() {
        return;
    }

    let started = Instant::now();
    let initial_deadline = started + limits.initial_read_timeout;
    let first =
        match tokio::time::timeout_at(initial_deadline.into(), read_first(&mut stream)).await {
            Ok(Ok(first)) => first,
            Ok(Err(error)) => {
                reject_local_request(&mut stream, 0, &error, initial_deadline).await;
                return;
            }
            Err(_) => {
                let error = DaemonError::new(
                    DomainErrorKind::DeadlineExceeded,
                    "local request frame read exceeded its deadline",
                );
                reject_local_request(&mut stream, 0, &error, initial_deadline).await;
                return;
            }
        };
    // Copy only routing metadata. Pair/device payloads move into their handler.
    let request_id = first.frame.request_id;
    let absolute_deadline = started + limits.request_deadline(first.frame.deadline_ms);
    if first.frame.kind == WireKind::LocalSessionTunnelOpenRequest {
        let result = match service.remote_sessions() {
            Some(remote) => {
                remote
                    .serve_tunnel(
                        stream,
                        first,
                        limits.session_wire_limits(),
                        absolute_deadline,
                    )
                    .await
            }
            None => {
                let error = DaemonError::new(
                    DomainErrorKind::TransportUnavailable,
                    "remote Session transport is not composed into this daemon",
                );
                reject_local_request(&mut stream, request_id, &error, absolute_deadline).await;
                Err(error)
            }
        };
        if let Err(error) = result {
            voytty_diagnostics::record(
                voytty_diagnostics::Event::new(voytty_diagnostics::Kind::RequestFailed)
                    .level(if error.kind() == DomainErrorKind::Cancelled {
                        voytty_diagnostics::Level::Info
                    } else {
                        voytty_diagnostics::Level::Warn
                    })
                    .error(error.kind()),
            );
            tracing::debug!(
                error_kind = error.kind().code(),
                "local remote-Session tunnel closed"
            );
        }
        return;
    }
    if first.frame.kind == WireKind::TerminalAttachRequest {
        let target = match terminal_attach_target(&first.frame) {
            Ok(target) => target,
            Err(error) => {
                reject_local_request(&mut stream, request_id, &error, absolute_deadline).await;
                return;
            }
        };
        let local_view_id = local_view_id();
        if target.is_some() {
            let error = malformed(
                "remote terminal attachment requires a local Session tunnel Open request",
            );
            reject_local_request(&mut stream, request_id, &error, absolute_deadline).await;
            return;
        }
        local_attachment.store(true, Ordering::Release);
        let result = SessionWireServer::new(service.sessions().clone())
            .handle_local_attachment(
                stream,
                first,
                local_view_id,
                limits.session_wire_limits(),
                absolute_deadline,
            )
            .await;
        if let Err(error) = result {
            voytty_diagnostics::record(
                voytty_diagnostics::Event::new(voytty_diagnostics::Kind::RequestFailed)
                    .level(if error.kind() == DomainErrorKind::Cancelled {
                        voytty_diagnostics::Level::Info
                    } else {
                        voytty_diagnostics::Level::Warn
                    })
                    .error(error.kind()),
            );
            tracing::debug!(
                error_kind = error.kind().code(),
                "local terminal attachment closed"
            );
        }
        return;
    }
    let unary_finished =
        tokio::time::timeout_at(absolute_deadline.into(), finish_unary(&mut stream, first)).await;
    let frame = match unary_finished.unwrap_or_else(|_| {
        Err(DaemonError::new(
            DomainErrorKind::DeadlineExceeded,
            "local unary request did not finish before its deadline",
        ))
    }) {
        Ok(frame) => frame,
        Err(error) => {
            reject_local_request(&mut stream, request_id, &error, absolute_deadline).await;
            return;
        }
    };
    let session_wire = SessionWireServer::new(service.sessions().clone());
    let report_progress = frame.kind == WireKind::LocalSessionUnaryRequest;
    // These services arbitrate admission, commit and timeout themselves. Dropping
    // their result here could lose a committed stop or a typed pairing outcome.
    let owns_outcome = matches!(
        frame.kind,
        WireKind::LocalPairAcceptRequest
            | WireKind::LocalDeviceRevokeRequest
            | WireKind::LocalStopRequest
    );
    let (reply, output) = crate::connection_progress::local::observe(
        stream,
        request_id,
        absolute_deadline,
        report_progress,
        |progress| async {
            let dispatch = async {
                if SessionWireServer::handles_unary(frame.kind) {
                    session_wire
                        .dispatch_local_unary_until(frame, absolute_deadline)
                        .await
                } else {
                    service
                        .dispatch_until_with_progress(frame, absolute_deadline, progress)
                        .await
                }
            };
            if owns_outcome {
                dispatch.await
            } else {
                tokio::time::timeout_at(absolute_deadline.into(), dispatch)
                    .await
                    .unwrap_or_else(|_| {
                        ServiceReply::error(
                            request_id,
                            &DaemonError::new(
                                DomainErrorKind::DeadlineExceeded,
                                "local request exceeded its deadline",
                            ),
                        )
                    })
            }
        },
    )
    .await;
    if let Ok(mut stream) = output {
        let _ = tokio::time::timeout_at(absolute_deadline.into(), write_reply(&mut stream, &reply))
            .await;
    }
    if reply.stop_committed {
        let _ = stop_sender.send(());
    }
}

#[cfg(any(unix, windows))]
fn local_view_id() -> AttachmentId {
    let secret = SecretKey::generate().to_bytes();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&secret[..16]);
    AttachmentId::from_array(bytes)
}

#[cfg(any(unix, windows))]
fn terminal_attach_target(frame: &DecodedFrame) -> Result<Option<DeviceId>, DaemonError> {
    let request: v2::TerminalAttachRequest = frame
        .decode_message(WireKind::TerminalAttachRequest)
        .map_err(protocol_error)?;
    match request.target.and_then(|target| target.target) {
        Some(v2::target_selector::Target::Local(true)) => Ok(None),
        Some(v2::target_selector::Target::Device(device)) => {
            device.try_into().map(Some).map_err(protocol_error)
        }
        _ => Err(malformed(
            "terminal attach requires either target.local=true or one full device target",
        )),
    }
}

#[cfg(any(unix, windows))]
pub(crate) async fn reject_local_request(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    request_id: u64,
    error: &DaemonError,
    deadline: Instant,
) {
    let reply = ServiceReply::error(request_id, error);
    let _ = tokio::time::timeout_at(deadline.into(), async {
        write_reply(stream, &reply).await?;
        #[cfg(unix)]
        {
            // Linux resets the peer on close if request bytes are still queued.
            // Send the reply and its EOF first, then discard raw input until the
            // peer finishes or the original request deadline expires.
            use tokio::io::AsyncReadExt;
            let mut discard = zeroize::Zeroizing::new([0_u8; 4096]);
            while stream.read(&mut *discard).await? != 0 {}
        }
        std::io::Result::Ok(())
    })
    .await;
}

#[cfg(any(unix, windows))]
async fn write_reply(
    stream: &mut (impl AsyncWrite + Unpin),
    reply: &ServiceReply,
) -> std::io::Result<()> {
    stream.write_all(&reply.bytes).await?;
    stream.shutdown().await
}

#[cfg(any(unix, windows))]
fn daemon_io(operation: &str, error: std::io::Error) -> DaemonError {
    DaemonError::new(
        DomainErrorKind::DaemonStopped,
        format!("{operation}: {error}"),
    )
}

#[cfg(any(unix, windows))]
fn malformed(detail: impl Into<String>) -> DaemonError {
    DaemonError::new(DomainErrorKind::MalformedFrame, detail)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::device_directory::ResolvedSessionTarget;
    use voytty_client::protocol::resolved_target_wire;
    use voytty_proto::{FrameDecoder, encode_message};

    fn stop_setup() -> crate::bootstrap::BootstrapResult {
        crate::bootstrap::BootstrapResult {
            device_id: DeviceId::from_array([0x72; DeviceId::LENGTH]),
            endpoint_id: "stop-fixture-endpoint".to_owned(),
            config: crate::config::validate_setup_input(
                "stop-fixture",
                crate::config::ValidatedInfrastructure::OfficialN0,
            )
            .expect("fixture setup"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_early_error_write_finishes_within_the_ingress_deadline() {
        let (mut writer, mut reader) = tokio::io::duplex(1);
        let budget = Duration::from_secs(1);
        let deadline = tokio::time::Instant::now() + budget;
        let error = malformed("invalid local request");
        // A full error frame cannot fit until this peer starts reading.
        tokio::time::timeout(
            budget * 2,
            reject_local_request(&mut writer, 0, &error, deadline.into_std()),
        )
        .await
        .expect("early error write is bounded");
        assert!(tokio::time::Instant::now() >= deadline);
        drop(writer);
        let mut received = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut received)
            .await
            .expect("read error prefix");
        assert_eq!(received.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn rejection_delivers_eof_before_waiting_for_an_open_peer() {
        use tokio::io::AsyncReadExt;

        let (mut server, mut client) = tokio::io::duplex(4096);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let error = malformed("invalid local request");
        let expected = ServiceReply::error(7, &error).bytes;
        let handler = tokio::spawn(async move {
            reject_local_request(&mut server, 7, &error, deadline.into_std()).await;
        });
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), client.read_to_end(&mut bytes))
            .await
            .expect("reply precedes the drain deadline")
            .expect("response EOF");
        assert_eq!(bytes, expected.as_slice());
        // Keep the client's write side open. It cannot extend the drain deadline.
        tokio::time::timeout_at(deadline + Duration::from_millis(1), handler)
            .await
            .expect("bounded rejection")
            .expect("handler completed");
    }

    #[tokio::test]
    async fn early_rejections_deliver_one_error_and_eof_with_a_large_unread_body() {
        use tokio::io::AsyncReadExt;

        let service = Arc::new(DaemonService::with_started_at(stop_setup(), 1));
        let unary = encode_message(
            WireKind::LocalStatusRequest,
            7,
            0,
            &v2::LocalStatusRequest {},
        )
        .expect("status request");
        let attach = encode_message(
            WireKind::TerminalAttachRequest,
            8,
            0,
            &v2::TerminalAttachRequest {
                target: Some(v2::TargetSelector {
                    target: Some(v2::target_selector::Target::Local(true)),
                }),
                ..Default::default() // missing session selector
            },
        )
        .expect("local attachment request");
        let tunnel = encode_message(
            WireKind::LocalSessionTunnelOpenRequest,
            9,
            0,
            &v2::LocalSessionTunnelOpenRequest::default(),
        )
        .expect("tunnel request");
        let mut oversized = Vec::new();
        prost::encoding::encode_varint(voytty_proto::MAX_FRAME_BYTES as u64 + 1, &mut oversized);
        for (mut request, request_id, code) in [
            (unary, 7, "malformed_frame"),
            (attach, 8, "malformed_frame"),
            (tunnel, 9, "transport_unavailable"),
            (oversized, 0, "frame_too_large"),
        ] {
            // A partial following frame spans multiple ingress reads. For an
            // oversized first prefix, these same bytes are its rejected body.
            prost::encoding::encode_varint(512 * 1024, &mut request);
            request.resize(request.len() + 256 * 1024, 0);
            let (mut client, server) = LocalStream::pair().expect("Unix socket pair");
            let (stop, _stops) = mpsc::unbounded_channel();
            let handler = tokio::spawn(handle_connection(
                server,
                nix::unistd::Uid::effective().as_raw(),
                Arc::clone(&service),
                stop,
                LocalIpcLimits::default(),
                Arc::new(AtomicBool::new(false)),
            ));
            client.write_all(&request).await.expect("submit request");
            client.shutdown().await.expect("finish request");
            handler.await.expect("server closed the socket");

            let mut bytes = Vec::new();
            client.read_to_end(&mut bytes).await.expect("clean EOF");
            let mut decoder = FrameDecoder::new();
            let frames = decoder.feed(&bytes).expect("decode error response");
            decoder.finish().expect("complete response");
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].request_id, request_id);
            let error: v2::ServiceError = frames[0]
                .decode_message(WireKind::ServiceErrorResponse)
                .expect("service error");
            assert_eq!(error.code, code);
        }
    }

    #[tokio::test]
    async fn committed_stop_survives_a_disconnected_client() {
        let temporary = tempfile::tempdir().expect("private stop fixture");
        let socket = temporary.path().join("stop.sock");
        let listener = voytty_platform::local::Listener::bind(&socket).expect("fixture listener");
        let mut client = tokio::net::UnixStream::connect(&socket)
            .await
            .expect("connect");
        let frame = encode_message(
            WireKind::LocalStopRequest,
            1,
            1000,
            &v2::LocalStopRequest {
                force: false,
                operation_id: None,
            },
        )
        .expect("stop frame");
        client.write_all(&frame).await.expect("submit stop");
        drop(client);
        // Start serving after the close so the reply cannot reach the requester.
        tokio::time::timeout(
            Duration::from_secs(2),
            serve_local(
                listener,
                nix::unistd::Uid::effective().as_raw(),
                Arc::new(DaemonService::with_started_at(stop_setup(), 1)),
            ),
        )
        .await
        .expect("listener exited after committed stop")
        .expect("clean stop");
    }

    #[tokio::test]
    async fn accepted_stop_does_not_wait_for_an_idle_ingress_handler() {
        let temporary = tempfile::tempdir().expect("private stop fixture");
        let socket = temporary.path().join("stop.sock");
        let listener = voytty_platform::local::Listener::bind(&socket).expect("fixture listener");
        let mut server = tokio::spawn(serve_local_inner(
            listener,
            nix::unistd::Uid::effective().as_raw(),
            Arc::new(DaemonService::with_started_at(stop_setup(), 1)),
            LocalIpcLimits::default(),
            None,
        ));
        let mut idle = tokio::net::UnixStream::connect(&socket)
            .await
            .expect("idle connection");
        idle.write_all(b"Z").await.expect("partial ingress frame");
        // This subsequently accepted request proves the idle socket was
        // already admitted; simply closing the listener cannot retire it.
        let client = LocalClient::new(&socket);
        client.readiness().await.expect("later request is served");
        assert!(
            client
                .stop(false)
                .await
                .expect("flushed stop response")
                .stopping
        );
        let stopped = tokio::time::timeout(Duration::from_millis(100), &mut server).await;
        drop(idle);
        if stopped.is_err() {
            tokio::time::timeout(Duration::from_secs(1), server)
                .await
                .expect("fixture cleanup")
                .expect("server task")
                .expect("server result");
        }
        assert!(
            stopped.is_ok(),
            "accepted stop waited for an unrelated first-frame deadline"
        );
    }

    #[tokio::test]
    async fn completed_handlers_are_reaped_before_subsequent_connection_churn() {
        let temporary = tempfile::tempdir().expect("temporary handler-reap fixture");
        let socket = temporary.path().join("handler-reap.sock");
        let listener =
            voytty_platform::local::Listener::bind(&socket).expect("bind handler-reap listener");
        let config = crate::config::validate_setup_input(
            "handler-reap",
            crate::config::ValidatedInfrastructure::OfficialN0,
        )
        .expect("valid handler-reap setup");
        let setup = crate::bootstrap::BootstrapResult {
            device_id: DeviceId::from_array([0x71; DeviceId::LENGTH]),
            endpoint_id: "handler-reap-endpoint".to_owned(),
            config,
        };
        let service = Arc::new(DaemonService::with_started_at(setup, 1));
        let (reaped, mut reaped_observations) = mpsc::unbounded_channel();
        let server = tokio::spawn(serve_local_inner(
            listener,
            nix::unistd::Uid::effective().as_raw(),
            service,
            LocalIpcLimits::for_test(Duration::from_secs(2)),
            Some(reaped),
        ));
        let client = LocalClient::new(&socket);

        for request_index in 0..4 {
            client
                .readiness()
                .await
                .expect("readiness handler completes");
            let retained = tokio::time::timeout(Duration::from_secs(2), reaped_observations.recv())
                .await
                .expect("completed handler is observed without polling")
                .expect("handler-reap observer remains installed");
            assert_eq!(
                retained, 0,
                "request {request_index} returned JoinSet ownership to baseline before the next request",
            );
        }

        assert!(
            client
                .stop(false)
                .await
                .expect("stop empty fixture")
                .stopping
        );
        server
            .await
            .expect("handler-reap server task")
            .expect("handler-reap server result");
    }

    #[test]
    fn local_attachment_views_are_fresh_fixed_width_ids() {
        let first = local_view_id();
        let second = local_view_id();
        assert_ne!(first, second);
        assert_eq!(first.to_bytes().len(), AttachmentId::LENGTH);
    }

    #[test]
    fn terminal_first_frame_router_separates_local_and_exact_device_targets() {
        let target = DeviceId::from_array([0x91; DeviceId::LENGTH]);
        let local = decoded_attach_target(v2::TargetSelector {
            target: Some(v2::target_selector::Target::Local(true)),
        });
        assert_eq!(
            terminal_attach_target(&local).expect("local target routes"),
            None
        );

        let remote =
            decoded_attach_target(resolved_target_wire(ResolvedSessionTarget::device(target)));
        assert_eq!(
            terminal_attach_target(&remote).expect("device target routes"),
            Some(target)
        );

        let false_local = decoded_attach_target(v2::TargetSelector {
            target: Some(v2::target_selector::Target::Local(false)),
        });
        assert_eq!(
            terminal_attach_target(&false_local)
                .expect_err("false local selector is not a routing target")
                .kind(),
            DomainErrorKind::MalformedFrame
        );
    }

    fn decoded_attach_target(target: v2::TargetSelector) -> DecodedFrame {
        let bytes = encode_message(
            WireKind::TerminalAttachRequest,
            1,
            0,
            &v2::TerminalAttachRequest {
                accept_zlib_frames: false,
                acknowledge_surface_frames: false,
                base_colors: Some(voytty_core::terminal::TerminalColorProfile::default().into()),

                target: Some(target),
                session_id: None,
                takeover: false,
                session_name: String::new(),
                create_main: true,
                viewport: None,
                resume_view_id: None,
                known_revision: None,
            },
        )
        .expect("bounded attach routing fixture");
        let mut decoder = FrameDecoder::new();
        let mut frames = decoder.feed(&bytes).expect("decode attach routing fixture");
        decoder.finish().expect("complete attach routing fixture");
        assert_eq!(frames.len(), 1);
        frames.remove(0)
    }
}
