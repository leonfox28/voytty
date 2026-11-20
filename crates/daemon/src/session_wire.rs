//! Shared Session framing and transport-independent service dispatch.
//!
//! The local adapter invokes this module only after its same-UID credential
//! gate. The remote adapter supplies only the TLS-authenticated device and the
//! receiver-owned authorization generation; both paths terminate in the same
//! SessionService and framing owner.

#[cfg(any(unix, windows))]
mod attachment_io;
#[cfg(any(unix, windows))]
mod upload;
#[cfg(any(unix, windows))]
use attachment_io::{attachment_reader, attachment_writer};

#[cfg(any(unix, windows))]
use std::collections::VecDeque;
#[cfg(any(unix, windows))]
use std::path::PathBuf;
#[cfg(any(unix, windows))]
use std::sync::Arc;
#[cfg(any(unix, windows))]
use std::time::{Duration, Instant};

#[cfg(any(unix, windows))]
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
#[cfg(any(unix, windows))]
use tokio::sync::{mpsc, oneshot};
#[cfg(any(unix, windows))]
use tokio::task::{JoinHandle, JoinSet};
#[cfg(any(unix, windows))]
use tokio_util::task::AbortOnDropHandle;
#[cfg(any(unix, windows))]
use voytty_core::terminal::{
    TerminalHistoryWindowAnchor, TerminalHistoryWindowQuery, TerminalSize, TerminalSurfaceDelta,
    TerminalSurfaceSnapshot,
};
#[cfg(any(unix, windows))]
use voytty_core::{
    AttachmentId, AttachmentPrincipal, AuthGeneration, DeviceId, DomainErrorKind, OperationId,
    ResourceLimits, ResumeViewId, Revision, SessionId, SessionName, SessionSelector,
};
#[cfg(any(unix, windows))]
use voytty_proto::{DecodedFrame, FrameDecoder, WireKind, encode_message, v2};
#[cfg(any(unix, windows))]
use zeroize::Zeroizing;

#[cfg(any(unix, windows))]
use crate::authorization::AuthorizationRegistry;
#[cfg(any(unix, windows))]
use crate::connection_broker::{
    InboundAuthenticatedStream, RemoteServiceHandler, RemoteServiceHandlerFuture,
};
#[cfg(any(unix, windows))]
use crate::error::DaemonError;
#[cfg(any(unix, windows))]
use crate::service::{ServiceReply, protocol_error};
#[cfg(any(unix, windows))]
use crate::session::{
    AttachmentLifecycle, AttachmentUpdate, PreparedAttachment, RemoteAttachmentRequest,
    RemoteResumeRequest, SessionAttachment, SessionService, SessionSummary,
};

#[cfg(any(unix, windows))]
const ATTACHMENT_OUTBOUND_CAPACITY: usize = 8;

#[cfg(any(unix, windows))]
const DEFAULT_SESSION_WIRE_DEADLINE: Duration = Duration::from_secs(5);

#[cfg(any(unix, windows))]
pub(crate) use voytty_client::framing::{FirstFrame, read_first_request as read_first};

/// Deadlines consumed by the shared Session stream owner.
#[cfg(any(unix, windows))]
#[derive(Clone, Copy)]
pub(crate) struct SessionWireLimits {
    default_request_deadline: Duration,
    maximum_request_deadline: Duration,
    operation_timeout: Duration,
}

#[cfg(any(unix, windows))]
impl SessionWireLimits {
    pub(crate) const fn new(
        default_request_deadline: Duration,
        maximum_request_deadline: Duration,
        operation_timeout: Duration,
    ) -> Self {
        Self {
            default_request_deadline,
            maximum_request_deadline,
            operation_timeout,
        }
    }

    pub(crate) fn request_deadline(self, requested_ms: u32) -> Duration {
        if requested_ms == 0 {
            self.default_request_deadline
        } else {
            Duration::from_millis(u64::from(requested_ms)).min(self.maximum_request_deadline)
        }
    }

    pub(crate) const fn operation_timeout(self) -> Duration {
        self.operation_timeout
    }

    fn request_deadline_from(self, started: Instant, requested_ms: u32) -> Instant {
        started
            .checked_add(self.request_deadline(requested_ms))
            .unwrap_or(started)
    }
}

#[cfg(any(unix, windows))]
impl Default for SessionWireLimits {
    fn default() -> Self {
        Self::new(
            DEFAULT_SESSION_WIRE_DEADLINE,
            Duration::from_secs(u64::from(
                ResourceLimits::default().max_local_deadline_seconds,
            )),
            DEFAULT_SESSION_WIRE_DEADLINE,
        )
    }
}

/// Validated trust and routing context for one Session wire stream.
#[cfg(any(unix, windows))]
#[derive(Clone)]
pub(crate) enum SessionRequestContext {
    LocalSameUid {
        local_view_id: AttachmentId,
    },
    RemoteAuthenticated {
        own_device_id: DeviceId,
        remote_device_id: DeviceId,
        accepted_generation: AuthGeneration,
        authorization: AuthorizationRegistry,
        #[cfg(test)]
        commit_first_poll_observer: Option<mpsc::UnboundedSender<(DeviceId, bool)>>,
    },
}

#[cfg(any(unix, windows))]
impl SessionRequestContext {
    fn local(local_view_id: AttachmentId) -> Self {
        Self::LocalSameUid { local_view_id }
    }

    fn remote(
        own_device_id: DeviceId,
        remote_device_id: DeviceId,
        accepted_generation: AuthGeneration,
        authorization: AuthorizationRegistry,
    ) -> Result<Self, DaemonError> {
        if own_device_id == remote_device_id || accepted_generation == AuthGeneration::ZERO {
            return Err(DaemonError::new(
                DomainErrorKind::Unauthorized,
                "remote Session context is not an admitted device generation",
            ));
        }
        Ok(Self::RemoteAuthenticated {
            own_device_id,
            remote_device_id,
            accepted_generation,
            authorization,
            #[cfg(test)]
            commit_first_poll_observer: None,
        })
    }

    #[cfg(all(test, unix))]
    fn with_commit_first_poll_observer(
        mut self,
        observer: mpsc::UnboundedSender<(DeviceId, bool)>,
    ) -> Self {
        if let Self::RemoteAuthenticated {
            commit_first_poll_observer,
            ..
        } = &mut self
        {
            *commit_first_poll_observer = Some(observer);
        }
        self
    }

    #[cfg(test)]
    fn commit_first_poll_observer(&self) -> Option<&mpsc::UnboundedSender<(DeviceId, bool)>> {
        match self {
            Self::RemoteAuthenticated {
                commit_first_poll_observer,
                ..
            } => commit_first_poll_observer.as_ref(),
            Self::LocalSameUid { .. } => None,
        }
    }

    fn principal(&self, sessions: &SessionService) -> AttachmentPrincipal {
        match self {
            Self::LocalSameUid { local_view_id } => sessions.local_principal(*local_view_id),
            Self::RemoteAuthenticated {
                remote_device_id,
                accepted_generation,
                ..
            } => AttachmentPrincipal::RemoteEndpoint {
                device_id: *remote_device_id,
                auth_generation: accepted_generation.get(),
            },
        }
    }

    fn is_remote(&self) -> bool {
        !matches!(self, Self::LocalSameUid { .. })
    }

    fn require_target(&self, target: Option<v2::TargetSelector>) -> Result<(), DaemonError> {
        self.require_target_with_local_detail(
            target,
            "local session request requires target.local=true",
        )
    }

    fn require_terminal_target(
        &self,
        target: Option<v2::TargetSelector>,
    ) -> Result<(), DaemonError> {
        self.require_target_with_local_detail(
            target,
            "local terminal stream requires target.local=true",
        )
    }

    fn require_target_with_local_detail(
        &self,
        target: Option<v2::TargetSelector>,
        local_detail: &'static str,
    ) -> Result<(), DaemonError> {
        match (self, target.and_then(|target| target.target)) {
            (Self::LocalSameUid { .. }, Some(v2::target_selector::Target::Local(true))) => Ok(()),
            (Self::LocalSameUid { .. }, _) => Err(malformed(local_detail)),
            (
                Self::RemoteAuthenticated { own_device_id, .. },
                Some(v2::target_selector::Target::Device(device)),
            ) => {
                let target: DeviceId = device.try_into().map_err(protocol_error)?;
                if target == *own_device_id {
                    Ok(())
                } else {
                    Err(remote_target_mismatch())
                }
            }
            (Self::RemoteAuthenticated { .. }, _) => Err(remote_target_mismatch()),
        }
    }

    async fn run_effect<T, F>(
        &self,
        sessions: &SessionService,
        deadline: Instant,
        operation: F,
    ) -> Result<T, DaemonError>
    where
        T: Send + 'static,
        F: FnOnce(SessionService, AttachmentPrincipal) -> Result<T, DaemonError> + Send + 'static,
    {
        let sessions = sessions.clone();
        let principal = self.principal(&sessions);
        #[cfg(test)]
        let commit_first_poll_observer = self.commit_first_poll_observer();
        match self {
            Self::LocalSameUid { .. } => {
                run_blocking_until(deadline, move || operation(sessions, principal)).await
            }
            Self::RemoteAuthenticated {
                remote_device_id,
                accepted_generation,
                authorization,
                ..
            } => {
                #[cfg(test)]
                let acquire = async {
                    if let Some(observer) = commit_first_poll_observer {
                        let acquire = tokio::task::unconstrained(
                            authorization.acquire_commit(*remote_device_id, *accepted_generation),
                        );
                        tokio::pin!(acquire);
                        let mut observer = Some(observer);
                        std::future::poll_fn(|context| {
                            let result = acquire.as_mut().poll(context);
                            if let Some(observer) = observer.take() {
                                let _ = observer.send((*remote_device_id, result.is_pending()));
                            }
                            result
                        })
                        .await
                    } else {
                        authorization
                            .acquire_commit(*remote_device_id, *accepted_generation)
                            .await
                    }
                };
                #[cfg(not(test))]
                let acquire = authorization.acquire_commit(*remote_device_id, *accepted_generation);
                let commit = timeout_at(
                    deadline,
                    acquire,
                    "remote authorization commit acquisition exceeded its absolute deadline",
                )
                .await?
                .map_err(project_remote_authorization_error)?;
                Ok(timeout_at(
                    deadline,
                    commit.run(move || operation(sessions, principal)),
                    "remote Session effect exceeded its absolute deadline",
                )
                .await??)
            }
        }
    }
}

#[cfg(any(unix, windows))]
fn project_remote_authorization_error(error: DaemonError) -> DaemonError {
    if matches!(
        error.kind(),
        DomainErrorKind::Unauthorized | DomainErrorKind::AuthorizationRevoked
    ) {
        DaemonError::new(
            DomainErrorKind::Unauthorized,
            "device is not authorized to control this host",
        )
    } else {
        error
    }
}

#[cfg(any(unix, windows))]
struct AttachmentOutbound {
    bytes: Zeroizing<Vec<u8>>,
    flushed: Option<oneshot::Sender<()>>,
    deadline: Instant,
    synchronize_before_reply: bool,
    has_surface: bool,
}

#[cfg(any(unix, windows))]
impl AttachmentOutbound {
    fn queued(bytes: Vec<u8>, deadline: Instant) -> Self {
        Self {
            bytes: Zeroizing::new(bytes),
            flushed: None,
            deadline,
            synchronize_before_reply: false,
            has_surface: false,
        }
    }

    fn surface(bytes: Vec<u8>, deadline: Instant) -> Self {
        Self {
            has_surface: true,
            ..Self::queued(bytes, deadline)
        }
    }

    fn takeover_reply(bytes: Vec<u8>, deadline: Instant) -> Self {
        Self {
            synchronize_before_reply: true,
            ..Self::queued(bytes, deadline)
        }
    }
}

/// Production callback which connects authenticated broker streams to the one
/// transport-independent Session wire owner.
#[cfg(any(unix, windows))]
#[derive(Clone)]
pub(crate) struct RemoteSessionServiceHandler {
    server: SessionWireServer,
    own_device_id: DeviceId,
    authorization: AuthorizationRegistry,
    limits: SessionWireLimits,
}

#[cfg(any(unix, windows))]
impl RemoteSessionServiceHandler {
    pub(crate) fn new(
        sessions: SessionService,
        own_device_id: DeviceId,
        authorization: AuthorizationRegistry,
    ) -> Self {
        Self {
            server: SessionWireServer::new(sessions),
            own_device_id,
            authorization,
            limits: SessionWireLimits::default(),
        }
    }
}

#[cfg(any(unix, windows))]
impl RemoteServiceHandler for RemoteSessionServiceHandler {
    fn handle_service_stream(
        &self,
        stream: InboundAuthenticatedStream,
        first_frame_deadline: Instant,
    ) -> RemoteServiceHandlerFuture {
        let server = self.server.clone();
        let authorization = self.authorization.clone();
        let own_device_id = self.own_device_id;
        let limits = self.limits;
        Box::pin(async move {
            let remote_device_id = stream.remote_device_id();
            let accepted_generation = stream.accepted_generation();
            let context = SessionRequestContext::remote(
                own_device_id,
                remote_device_id,
                accepted_generation,
                authorization,
            )?;
            let (send, recv) = stream.into_parts();
            let stream = tokio::io::join(recv, send);
            server
                .handle_remote_stream(stream, context, limits, first_frame_deadline)
                .await
        })
    }
}

/// Requires EOF after the one decoded unary frame, including bytes that arrive
/// in a later read.
#[cfg(any(unix, windows))]
pub(crate) async fn finish_unary<Reader>(
    reader: &mut Reader,
    mut first: FirstFrame,
) -> Result<DecodedFrame, DaemonError>
where
    Reader: AsyncRead + Unpin,
{
    if !first.queued.is_empty() {
        return Err(malformed(
            "one unary connection may contain only one request",
        ));
    }
    let mut buffer = Zeroizing::new([0_u8; 16 * 1024]);
    loop {
        let read = reader
            .read(&mut *buffer)
            .await
            .map_err(|error| daemon_io("finish Session unary request", error))?;
        if read == 0 {
            first.decoder.finish().map_err(protocol_error)?;
            return Ok(first.frame);
        }
        if !first
            .decoder
            .feed(&buffer[..read])
            .map_err(protocol_error)?
            .is_empty()
        {
            return Err(malformed(
                "one unary connection may contain only one request",
            ));
        }
    }
}

/// The single wire adapter around the daemon's transport-independent SessionService.
#[cfg(any(unix, windows))]
#[derive(Clone)]
pub(crate) struct SessionWireServer {
    sessions: SessionService,
}

#[cfg(any(unix, windows))]
impl SessionWireServer {
    pub(crate) const fn new(sessions: SessionService) -> Self {
        Self { sessions }
    }

    pub(crate) const fn handles_unary(kind: WireKind) -> bool {
        matches!(
            kind,
            WireKind::SessionListRequest
                | WireKind::SessionOperationLeaseRequest
                | WireKind::SessionCreateRequest
                | WireKind::SessionRenameRequest
                | WireKind::SessionCloseRequest
                | WireKind::SessionTakeoverRequest
        )
    }

    /// Dispatches one strict, already-framed same-UID Session request without
    /// blocking the local IPC runtime thread.
    pub(crate) async fn dispatch_local_unary_until(
        &self,
        frame: DecodedFrame,
        deadline: Instant,
    ) -> ServiceReply {
        let context = SessionRequestContext::local(local_request_view_id(frame.request_id));
        self.dispatch_unary_until(&frame, context, deadline).await
    }

    /// Owns first-frame classification for one authenticated remote service
    /// stream. The broker deliberately passes untouched stream halves here.
    pub(crate) async fn handle_remote_stream<Stream>(
        &self,
        mut stream: Stream,
        context: SessionRequestContext,
        limits: SessionWireLimits,
        first_frame_deadline: Instant,
    ) -> Result<(), DaemonError>
    where
        Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let started = Instant::now();
        let first = match timeout_at(
            first_frame_deadline,
            read_first(&mut stream),
            "remote service first frame exceeded its absolute deadline",
        )
        .await
        {
            Ok(Ok(first)) => first,
            Ok(Err(error)) | Err(error) => {
                write_error_best_effort(&mut stream, 0, &error, first_frame_deadline).await;
                return Err(error);
            }
        };
        let request_id = first.frame.request_id;
        let request_deadline = limits.request_deadline_from(started, first.frame.deadline_ms);
        if first.frame.kind == WireKind::UploadBegin {
            return self.handle_upload(stream, first, context).await;
        }
        if first.frame.kind == WireKind::TerminalAttachRequest {
            return self
                .handle_attachment(stream, first, context, limits, request_deadline)
                .await;
        }
        if !Self::handles_unary(first.frame.kind) {
            let error = DaemonError::new(
                DomainErrorKind::ServiceNotImplemented,
                format!(
                    "wire service {:?} is not implemented by this Session server",
                    first.frame.kind
                ),
            );
            write_error_best_effort(&mut stream, request_id, &error, request_deadline).await;
            return Err(error);
        }
        let frame = match timeout_at(
            request_deadline,
            finish_unary(&mut stream, first),
            "remote unary request exceeded its absolute deadline",
        )
        .await
        {
            Ok(Ok(frame)) => frame,
            Ok(Err(error)) | Err(error) => {
                write_error_best_effort(&mut stream, request_id, &error, request_deadline).await;
                return Err(error);
            }
        };
        let reply = self
            .dispatch_unary_until(&frame, context, request_deadline)
            .await;
        timeout_at(
            request_deadline,
            stream.write_all(&reply.bytes),
            "remote unary response exceeded its absolute deadline",
        )
        .await?
        .map_err(|error| daemon_io("write remote unary response", error))?;
        timeout_at(
            request_deadline,
            stream.shutdown(),
            "remote unary finish exceeded its absolute deadline",
        )
        .await?
        .map_err(|error| daemon_io("finish remote unary response", error))
    }

    /// Runs one same-UID terminal attachment over generic bounded async I/O.
    pub(crate) async fn handle_local_attachment<Stream>(
        &self,
        stream: Stream,
        first: FirstFrame,
        local_view_id: AttachmentId,
        limits: SessionWireLimits,
        deadline: Instant,
    ) -> Result<(), DaemonError>
    where
        Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        self.handle_attachment(
            stream,
            first,
            SessionRequestContext::local(local_view_id),
            limits,
            deadline,
        )
        .await
    }

    async fn handle_attachment<Stream>(
        &self,
        mut stream: Stream,
        first: FirstFrame,
        context: SessionRequestContext,
        limits: SessionWireLimits,
        deadline: Instant,
    ) -> Result<(), DaemonError>
    where
        Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let prepared = match self
            .prepare_attachment(&context, &first.frame, deadline)
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                write_error_best_effort(&mut stream, first.frame.request_id, &error, deadline)
                    .await;
                return Err(error);
            }
        };
        let attachment = prepared.attachment;
        let request: v2::TerminalAttachRequest = first
            .frame
            .decode_message(WireKind::TerminalAttachRequest)
            .map_err(protocol_error)?;
        let encoding = AttachmentEncoding(request.accept_zlib_frames);
        let (surface_receipts, frames) = if request.acknowledge_surface_frames {
            let (receipts, frames) = SurfaceFrameReceipts::pair();
            (Some(receipts), frames)
        } else {
            (None, SurfaceFrameWindow::default())
        };
        let initial = if let Some(delta) = prepared.initial_delta {
            encode_delta(first.frame.request_id, attachment.attachment_id(), delta, 0)?
        } else {
            encode_snapshot(
                first.frame.request_id,
                attachment.session_id(),
                attachment.attachment_id(),
                prepared.snapshot,
                0,
            )?
        };
        let initial = encoding.encode(initial, deadline).await?;
        write_attachment_bytes_until(
            &mut stream,
            &initial,
            deadline,
            "initial terminal update exceeded its absolute deadline",
            "write initial terminal update",
        )
        .await?;
        let (reader, writer) = tokio::io::split(stream);
        let (outbound_sender, outbound_receiver) =
            mpsc::channel::<AttachmentOutbound>(ATTACHMENT_OUTBOUND_CAPACITY);
        let reader_attachment = Arc::clone(&attachment);
        let reader_server = self.clone();
        let reader_context = context.clone();
        let reader_task = tokio::spawn(async move {
            attachment_reader(
                reader,
                first.decoder,
                first.queued,
                AttachmentReaderContext {
                    server: reader_server,
                    attachment: reader_attachment,
                    request_context: reader_context,
                    outbound: outbound_sender,
                    limits,
                    surface_receipts,
                },
            )
            .await
        });
        let writer_attachment = Arc::clone(&attachment);
        let writer_server = self.clone();
        let writer_context = context.clone();
        let writer_task = tokio::spawn(async move {
            attachment_writer(
                writer,
                writer_server,
                writer_attachment,
                writer_context,
                outbound_receiver,
                limits.operation_timeout,
                SurfaceOutput { encoding, frames },
            )
            .await
        });

        let result =
            finish_attachment_tasks(reader_task, writer_task, limits.operation_timeout).await;
        let transport_loss = should_move_remote_resume_checkpoint(context.is_remote(), &result);
        if transport_loss {
            let attachment_worker = Arc::clone(&attachment);
            let save_deadline = Instant::now() + limits.operation_timeout;
            if context
                .run_effect(
                    &self.sessions,
                    save_deadline,
                    move |_sessions, _principal| {
                        attachment_worker.detach_for_remote_resume_until(save_deadline)
                    },
                )
                .await
                .is_err()
            {
                attachment.detach();
            }
        } else {
            attachment.detach();
        }
        result.map(|_| ())
    }

    async fn prepare_attachment(
        &self,
        context: &SessionRequestContext,
        frame: &DecodedFrame,
        deadline: Instant,
    ) -> Result<PreparedAttachment, DaemonError> {
        let request: v2::TerminalAttachRequest = frame
            .decode_message(WireKind::TerminalAttachRequest)
            .map_err(protocol_error)?;
        context.require_terminal_target(request.target.clone())?;
        let (selector, create_main) = terminal_selector(&request)?;
        let viewport = request
            .viewport
            .map(TryInto::try_into)
            .transpose()
            .map_err(protocol_error)?;
        let resume = terminal_resume_request(context, &request)?;
        let base_colors = request
            .base_colors
            .clone()
            .ok_or_else(|| {
                DaemonError::new(DomainErrorKind::MalformedFrame, "missing base colors")
            })?
            .try_into()
            .map_err(protocol_error)?;
        context
            .run_effect(&self.sessions, deadline, move |sessions, principal| {
                if let Some(resume) = resume {
                    sessions.prepare_remote_attach_until(
                        principal,
                        RemoteAttachmentRequest {
                            base_colors,
                            selector,
                            create_main,
                            takeover: request.takeover,
                            initial_viewport: viewport,
                            resume,
                        },
                        deadline,
                    )
                } else {
                    sessions.prepare_attach_with_colors_until(
                        principal,
                        selector,
                        create_main,
                        request.takeover,
                        crate::session::InitialTerminal {
                            viewport,
                            colors: base_colors,
                        },
                        deadline,
                    )
                }
            })
            .await
    }

    async fn dispatch_unary_until(
        &self,
        frame: &DecodedFrame,
        context: SessionRequestContext,
        deadline: Instant,
    ) -> ServiceReply {
        let request_id = frame.request_id;
        let result: Result<ServiceReply, DaemonError> = async {
            match frame.kind {
                WireKind::SessionListRequest => {
                    let request: v2::SessionListRequest = decode_request(frame)?;
                    context.require_target(request.target)?;
                    let sessions = context
                        .run_effect(&self.sessions, deadline, |sessions, _principal| {
                            sessions.list()
                        })
                        .await?
                        .into_iter()
                        .map(session_summary_proto)
                        .collect();
                    ServiceReply::message(
                        WireKind::SessionListResponse,
                        request_id,
                        &v2::SessionListResponse { sessions },
                        false,
                    )
                }
                WireKind::SessionOperationLeaseRequest => {
                    let request: v2::SessionOperationLeaseRequest = decode_request(frame)?;
                    context.require_target(request.target)?;
                    let lease = context
                        .run_effect(&self.sessions, deadline, |sessions, principal| {
                            sessions.issue_operation_lease(principal)
                        })
                        .await?;
                    ServiceReply::message(
                        WireKind::SessionOperationLeaseResponse,
                        request_id,
                        &v2::SessionOperationLeaseResponse {
                            lease: Some(lease.into()),
                        },
                        false,
                    )
                }
                WireKind::SessionCreateRequest => {
                    let request: v2::SessionCreateRequest = decode_request(frame)?;
                    let base_colors = request
                        .base_colors
                        .ok_or_else(|| {
                            DaemonError::new(DomainErrorKind::MalformedFrame, "missing base colors")
                        })?
                        .try_into()
                        .map_err(protocol_error)?;
                    context.require_target(request.target)?;
                    let operation_id = required_operation_id(request.operation_id)?;
                    let name = session_name(&request.name)?;
                    let working_directory = (!request.working_directory.is_empty())
                        .then(|| PathBuf::from(request.working_directory));
                    let viewport = request
                        .viewport
                        .map(TryInto::try_into)
                        .transpose()
                        .map_err(protocol_error)?;
                    let summary = context
                        .run_effect(&self.sessions, deadline, move |sessions, principal| {
                            sessions.create_with_colors_until(
                                principal,
                                operation_id,
                                name,
                                working_directory,
                                crate::session::InitialTerminal {
                                    viewport,
                                    colors: base_colors,
                                },
                                deadline,
                            )
                        })
                        .await?;
                    mutate_reply(request_id, summary)
                }
                WireKind::SessionRenameRequest => {
                    let request: v2::SessionRenameRequest = decode_request(frame)?;
                    context.require_target(request.target)?;
                    let operation_id = required_operation_id(request.operation_id)?;
                    let session_id = required_session_id(request.session_id)?;
                    let name = session_name(&request.name)?;
                    let summary = context
                        .run_effect(&self.sessions, deadline, move |sessions, principal| {
                            sessions.rename_until(
                                principal,
                                operation_id,
                                session_id,
                                name,
                                deadline,
                            )
                        })
                        .await?;
                    mutate_reply(request_id, summary)
                }
                WireKind::SessionCloseRequest => {
                    let request: v2::SessionCloseRequest = decode_request(frame)?;
                    context.require_target(request.target)?;
                    let operation_id = required_operation_id(request.operation_id)?;
                    let session_id = required_session_id(request.session_id)?;
                    let summary = context
                        .run_effect(&self.sessions, deadline, move |sessions, principal| {
                            sessions.close_until(principal, operation_id, session_id, deadline)
                        })
                        .await?;
                    mutate_reply(request_id, summary)
                }
                WireKind::SessionTakeoverRequest => {
                    let request: v2::SessionTakeoverRequest = decode_request(frame)?;
                    context.require_target(request.target)?;
                    let attachment_id = request
                        .attachment_id
                        .ok_or_else(|| malformed("takeover omitted attachment_id"))?
                        .try_into()
                        .map_err(protocol_error)?;
                    let operation_id = required_operation_id(request.operation_id)?;
                    let session_id = required_session_id(request.session_id)?;
                    let summary = context
                        .run_effect(&self.sessions, deadline, move |sessions, principal| {
                            sessions.takeover_by_id_until(
                                principal,
                                operation_id,
                                session_id,
                                attachment_id,
                                deadline,
                            )
                        })
                        .await?;
                    mutate_reply(request_id, summary)
                }
                _ => Err(DaemonError::new(
                    DomainErrorKind::ServiceNotImplemented,
                    format!(
                        "wire service {:?} is not implemented by this Session server",
                        frame.kind
                    ),
                )),
            }
        }
        .await;
        match result {
            Ok(reply) => reply,
            Err(error) => {
                voytty_diagnostics::record(
                    voytty_diagnostics::Event::new(voytty_diagnostics::Kind::RequestFailed)
                        .level(if error.kind() == DomainErrorKind::Cancelled {
                            voytty_diagnostics::Level::Info
                        } else {
                            voytty_diagnostics::Level::Warn
                        })
                        .error(error.kind()),
                );
                ServiceReply::error(request_id, &error)
            }
        }
    }
}

#[cfg(any(unix, windows))]
#[derive(Clone, Copy, Default)]
struct AttachmentEncoding(bool);

/// Numbered live surface frames written but not yet reported received.
///
/// QUIC flow-control credit lets a slow Relay path hold far more terminal
/// output than it can deliver. Without this bound every PTY revision became a
/// queued delta and a requested viewport's snapshot waited behind all of them.
#[cfg(any(unix, windows))]
const MAX_UNRECEIVED_SURFACE_FRAMES: usize = 4;

/// Encoded bytes of numbered live surface frames not yet reported received.
#[cfg(any(unix, windows))]
const MAX_UNRECEIVED_SURFACE_BYTES: usize = 32 * 1024;

/// Reader-owned validation of controller receipts for one attachment stream.
#[cfg(any(unix, windows))]
struct SurfaceFrameReceipts {
    received: tokio::sync::watch::Sender<u64>,
    highest_sent: Arc<std::sync::atomic::AtomicU64>,
}

#[cfg(any(unix, windows))]
impl SurfaceFrameReceipts {
    /// Creates the reader validator and the matching writer window.
    fn pair() -> (Self, SurfaceFrameWindow) {
        let (received, receiver) = tokio::sync::watch::channel(0);
        let highest_sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
        (
            Self {
                received,
                highest_sent: Arc::clone(&highest_sent),
            },
            SurfaceFrameWindow {
                receipts: Some(SurfaceFrameReceiptWatch {
                    received: receiver,
                    highest_sent,
                }),
                in_flight: VecDeque::new(),
                in_flight_bytes: 0,
            },
        )
    }

    /// Accepts one cumulative receipt within the exact written range.
    fn accept(&self, frame_sequence: u64) -> Result<(), DaemonError> {
        let current = *self.received.borrow();
        let highest_sent = self.highest_sent.load(std::sync::atomic::Ordering::SeqCst);
        if frame_sequence == 0 || frame_sequence < current || frame_sequence > highest_sent {
            return Err(malformed(
                "terminal surface frame receipt is outside the written range",
            ));
        }
        if frame_sequence > current {
            self.received.send_replace(frame_sequence);
        }
        Ok(())
    }
}

#[cfg(any(unix, windows))]
struct SurfaceFrameReceiptWatch {
    received: tokio::sync::watch::Receiver<u64>,
    highest_sent: Arc<std::sync::atomic::AtomicU64>,
}

/// Writer-owned bound on live surface frames; disabled unless negotiated.
#[cfg(any(unix, windows))]
#[derive(Default)]
struct SurfaceFrameWindow {
    receipts: Option<SurfaceFrameReceiptWatch>,
    in_flight: VecDeque<(u64, usize)>,
    in_flight_bytes: usize,
}

#[cfg(any(unix, windows))]
impl SurfaceFrameWindow {
    /// Sequence for the next written frame; zero means not numbered.
    fn next_sequence(&self) -> u64 {
        self.receipts.as_ref().map_or(0, |receipts| {
            receipts
                .highest_sent
                .load(std::sync::atomic::Ordering::SeqCst)
                + 1
        })
    }

    /// Accounts one frame before its bytes are written, so a receipt can never
    /// precede the accounting that validates it.
    fn record(&mut self, frame_sequence: u64, bytes: usize) {
        if let Some(receipts) = &self.receipts {
            receipts
                .highest_sent
                .store(frame_sequence, std::sync::atomic::Ordering::SeqCst);
            self.in_flight.push_back((frame_sequence, bytes));
            self.in_flight_bytes += bytes;
        }
    }

    /// Whether one more optional live update may be written now. One frame is
    /// always admitted, so a large snapshot or delta cannot deadlock the bound.
    fn has_room(&mut self) -> bool {
        let Some(receipts) = &mut self.receipts else {
            return true;
        };
        let received = *receipts.received.borrow_and_update();
        while let Some(&(frame_sequence, bytes)) = self.in_flight.front() {
            if frame_sequence > received {
                break;
            }
            self.in_flight.pop_front();
            self.in_flight_bytes -= bytes;
        }
        self.in_flight.is_empty()
            || (self.in_flight.len() < MAX_UNRECEIVED_SURFACE_FRAMES
                && self.in_flight_bytes < MAX_UNRECEIVED_SURFACE_BYTES)
    }

    /// Waits for a newer receipt. A closed reader never wakes this writer; the
    /// attachment owner retires both halves.
    async fn receipt(&mut self) {
        if let Some(receipts) = &mut self.receipts
            && receipts.received.changed().await.is_ok()
        {
            return;
        }
        std::future::pending::<()>().await;
    }
}

/// Encoding and optional receipt window owned by one attachment writer.
#[cfg(any(unix, windows))]
#[derive(Default)]
struct SurfaceOutput {
    encoding: AttachmentEncoding,
    frames: SurfaceFrameWindow,
}

#[cfg(any(unix, windows))]
impl AttachmentEncoding {
    async fn encode(self, bytes: Vec<u8>, deadline: Instant) -> Result<Vec<u8>, DaemonError> {
        // Payloads below the wire compression threshold always stay plain.
        if self.0 && bytes.len() >= 1024 {
            run_blocking_until(deadline, move || {
                voytty_proto::compress_terminal_frames(bytes).map_err(protocol_error)
            })
            .await
        } else {
            Ok(bytes)
        }
    }
}

#[cfg(any(unix, windows))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AttachmentTaskEnd {
    Explicit,
    TransportEof,
    Terminal,
    ReaderClosed,
}

#[cfg(any(unix, windows))]
async fn finish_attachment_tasks(
    reader_task: JoinHandle<Result<AttachmentTaskEnd, DaemonError>>,
    writer_task: JoinHandle<Result<AttachmentTaskEnd, DaemonError>>,
    operation_timeout: Duration,
) -> Result<AttachmentTaskEnd, DaemonError> {
    // Broker quiesce or local IPC shutdown can cancel this owner while both
    // halves are pending. Dropping a plain JoinHandle would detach its task,
    // retaining the stream and attachment after its service owner is gone.
    let mut reader_task = AbortOnDropHandle::new(reader_task);
    let mut writer_task = AbortOnDropHandle::new(writer_task);
    tokio::select! {
        biased;
        result = &mut reader_task => {
            writer_task.abort();
            let _ = writer_task.await;
            flatten_attachment_task(result)
        }
        result = &mut writer_task => {
            let writer_result = flatten_attachment_task(result);
            let reader_closed = matches!(&writer_result, Ok(AttachmentTaskEnd::ReaderClosed));
            if reader_closed || writer_result.as_ref().is_err_and(|error| {
                error.kind() == DomainErrorKind::DaemonStopped
            }) {
                // Dropping the reader's outbound sender can wake the writer
                // before the reader JoinHandle publishes its result. Only the
                // reader knows whether that end was EOF, detach or a protocol
                // error. A concurrent write failure also gives queued detach/EOF
                // the same bounded opportunity to finish classification.
                match tokio::time::timeout(operation_timeout, &mut reader_task).await {
                    Ok(result) => flatten_attachment_task(result),
                    Err(_) => {
                        reader_task.abort();
                        let _ = reader_task.await;
                        if reader_closed { Err(attachment_cancelled()) } else { writer_result }
                    }
                }
            } else {
                reader_task.abort();
                let _ = reader_task.await;
                writer_result
            }
        }
    }
}

#[cfg(any(unix, windows))]
fn should_move_remote_resume_checkpoint(
    remote_authenticated: bool,
    result: &Result<AttachmentTaskEnd, DaemonError>,
) -> bool {
    remote_authenticated && matches!(result, Ok(AttachmentTaskEnd::TransportEof))
}

#[cfg(any(unix, windows))]
fn flatten_attachment_task(
    result: Result<Result<AttachmentTaskEnd, DaemonError>, tokio::task::JoinError>,
) -> Result<AttachmentTaskEnd, DaemonError> {
    result.map_err(|error| {
        DaemonError::new(
            DomainErrorKind::Cancelled,
            format!("Session attachment task ended unexpectedly: {error}"),
        )
    })?
}

#[cfg(any(unix, windows))]
async fn run_blocking_until<T, F>(deadline: Instant, operation: F) -> Result<T, DaemonError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, DaemonError> + Send + 'static,
{
    let remaining = deadline.saturating_duration_since(Instant::now());
    match tokio::time::timeout(remaining, tokio::task::spawn_blocking(operation)).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(DaemonError::new(
            DomainErrorKind::Cancelled,
            format!("blocking local session worker ended unexpectedly: {error}"),
        )),
        Err(_) => Err(DaemonError::new(
            DomainErrorKind::DeadlineExceeded,
            "local session operation exceeded its absolute deadline",
        )),
    }
}

#[cfg(any(unix, windows))]
async fn timeout_at<F>(
    deadline: Instant,
    future: F,
    detail: &'static str,
) -> Result<F::Output, DaemonError>
where
    F: std::future::Future,
{
    if Instant::now() >= deadline {
        return Err(DaemonError::new(DomainErrorKind::DeadlineExceeded, detail));
    }
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), future)
        .await
        .map_err(|_| DaemonError::new(DomainErrorKind::DeadlineExceeded, detail))
}

#[cfg(any(unix, windows))]
async fn write_error_best_effort<Writer>(
    writer: &mut Writer,
    request_id: u64,
    error: &DaemonError,
    deadline: Instant,
) where
    Writer: AsyncWrite + Unpin,
{
    let bytes = ServiceReply::error(request_id, error).bytes;
    let write = async {
        writer.write_all(&bytes).await?;
        writer.shutdown().await
    };
    let _ = timeout_at(deadline, write, "Session error response deadline elapsed").await;
}

#[cfg(any(unix, windows))]
async fn write_attachment_bytes_until<Writer>(
    writer: &mut Writer,
    bytes: &[u8],
    deadline: Instant,
    deadline_detail: &'static str,
    operation: &'static str,
) -> Result<(), DaemonError>
where
    Writer: AsyncWrite + Unpin,
{
    timeout_at(
        deadline,
        async {
            writer.write_all(bytes).await?;
            writer.flush().await
        },
        deadline_detail,
    )
    .await?
    .map_err(|error| daemon_io(operation, error))
}

#[cfg(any(unix, windows))]
async fn send_attachment_outbound_until(
    outbound: &mpsc::Sender<AttachmentOutbound>,
    message: AttachmentOutbound,
    deadline: Instant,
) -> Result<(), DaemonError> {
    timeout_at(
        deadline,
        outbound.send(message),
        "terminal response queue exceeded its absolute deadline",
    )
    .await?
    .map_err(|_| attachment_cancelled())
}

#[cfg(any(unix, windows))]
struct AttachmentReaderContext {
    server: SessionWireServer,
    attachment: Arc<SessionAttachment>,
    request_context: SessionRequestContext,
    outbound: mpsc::Sender<AttachmentOutbound>,
    limits: SessionWireLimits,
    /// Present only after the controller opted into surface frame receipts.
    surface_receipts: Option<SurfaceFrameReceipts>,
}

#[cfg(any(unix, windows))]
fn encode_snapshot(
    request_id: u64,
    session_id: SessionId,
    attachment_id: AttachmentId,
    snapshot: TerminalSurfaceSnapshot,
    frame_sequence: u64,
) -> Result<Vec<u8>, DaemonError> {
    let mut message =
        voytty_proto::terminal_surface_snapshot_message(session_id, attachment_id, snapshot);
    message.frame_sequence = frame_sequence;
    encode_message(WireKind::TerminalSemanticSnapshot, request_id, 0, &message)
        .map_err(protocol_error)
}

#[cfg(any(unix, windows))]
fn encode_delta(
    request_id: u64,
    attachment_id: AttachmentId,
    delta: TerminalSurfaceDelta,
    frame_sequence: u64,
) -> Result<Vec<u8>, DaemonError> {
    let mut message = voytty_proto::terminal_surface_delta_message(attachment_id, delta);
    message.frame_sequence = frame_sequence;
    encode_message(WireKind::TerminalSemanticDelta, request_id, 0, &message).map_err(protocol_error)
}

#[cfg(any(unix, windows))]
fn terminal_selector(
    request: &v2::TerminalAttachRequest,
) -> Result<(Option<SessionSelector>, bool), DaemonError> {
    let has_id = request.session_id.is_some();
    let has_name = !request.session_name.is_empty();
    let selections = usize::from(has_id) + usize::from(has_name) + usize::from(request.create_main);
    if selections != 1 {
        return Err(malformed(
            "terminal attach requires exactly one session_id, session_name, or create_main",
        ));
    }
    if request.create_main {
        return Ok((None, true));
    }
    if let Some(session_id) = request.session_id.clone() {
        return Ok((
            Some(SessionSelector::Id(
                session_id.try_into().map_err(protocol_error)?,
            )),
            false,
        ));
    }
    let name = SessionName::new(request.session_name.clone()).map_err(|error| {
        DaemonError::new(DomainErrorKind::InvalidSessionName, error.to_string())
    })?;
    Ok((Some(SessionSelector::Name(name)), false))
}

#[cfg(any(unix, windows))]
fn terminal_resume_request(
    context: &SessionRequestContext,
    request: &v2::TerminalAttachRequest,
) -> Result<Option<RemoteResumeRequest>, DaemonError> {
    if !context.is_remote() {
        if request.resume_view_id.is_some() || request.known_revision.is_some() {
            return Err(malformed(
                "same-UID local attachments cannot submit remote resume state",
            ));
        }
        return Ok(None);
    }

    let Some(view_id) = request.resume_view_id.clone() else {
        if request.known_revision.is_some() {
            return Err(malformed("known_revision requires an exact resume_view_id"));
        }
        return Ok(None);
    };
    let view_id: ResumeViewId = view_id.try_into().map_err(protocol_error)?;
    if view_id.as_bytes().iter().all(|byte| *byte == 0) {
        return Err(malformed("resume_view_id must be random and non-zero"));
    }
    Ok(Some(RemoteResumeRequest {
        view_id,
        known_revision: request.known_revision.map(Revision::new),
    }))
}

#[cfg(any(unix, windows))]
fn require_attachment_id(
    attachment_id: Option<v2::AttachmentId>,
    attachment: &SessionAttachment,
) -> Result<(), DaemonError> {
    let attachment_id: AttachmentId = attachment_id
        .ok_or_else(|| malformed("terminal message omitted attachment_id"))?
        .try_into()
        .map_err(protocol_error)?;
    if attachment_id == attachment.attachment_id() {
        Ok(())
    } else {
        Err(malformed(
            "terminal message attachment_id does not match this stream",
        ))
    }
}

#[cfg(any(unix, windows))]
fn session_ended_message(
    attachment: &SessionAttachment,
    reason: voytty_core::SessionEndReason,
) -> v2::TerminalSessionEnded {
    let (reason, exit_code, signal) = match reason {
        voytty_core::SessionEndReason::NaturalExit { exit_code, signal } => (
            v2::TerminalSessionEndReason::NaturalExit,
            exit_code,
            signal.unwrap_or_default(),
        ),
        voytty_core::SessionEndReason::ExplicitClose => (
            v2::TerminalSessionEndReason::ExplicitClose,
            0,
            String::new(),
        ),
        voytty_core::SessionEndReason::DaemonStop => {
            (v2::TerminalSessionEndReason::DaemonStop, 0, String::new())
        }
        voytty_core::SessionEndReason::DriverFailure => (
            v2::TerminalSessionEndReason::DriverFailure,
            0,
            String::new(),
        ),
    };
    v2::TerminalSessionEnded {
        session_id: Some(attachment.session_id().into()),
        attachment_id: Some(attachment.attachment_id().into()),
        reason: reason as i32,
        exit_code,
        signal,
    }
}

#[cfg(any(unix, windows))]
fn attachment_cancelled() -> DaemonError {
    DaemonError::new(DomainErrorKind::Cancelled, "terminal attachment closed")
}

#[cfg(any(unix, windows))]
fn decode_request<Message>(frame: &DecodedFrame) -> Result<Message, DaemonError>
where
    Message: prost::Message + Default,
{
    frame.decode_message(frame.kind).map_err(protocol_error)
}

#[cfg(any(unix, windows))]
fn mutate_reply(request_id: u64, summary: SessionSummary) -> Result<ServiceReply, DaemonError> {
    ServiceReply::message(
        WireKind::SessionMutateResponse,
        request_id,
        &v2::SessionMutateResponse {
            session: Some(session_summary_proto(summary)),
        },
        false,
    )
}

#[cfg(any(unix, windows))]
fn session_summary_proto(summary: SessionSummary) -> v2::SessionSummary {
    v2::SessionSummary {
        session_id: Some(summary.session_id.into()),
        name: summary.name.to_string(),
        revision: summary.revision.get(),
        has_controller: summary.has_controller,
        working_directory: summary.working_directory.to_string_lossy().into_owned(),
        viewport: Some(summary.viewport.into()),
    }
}

#[cfg(any(unix, windows))]
fn required_operation_id(
    operation_id: Option<v2::OperationId>,
) -> Result<OperationId, DaemonError> {
    operation_id
        .ok_or_else(|| malformed("session mutation omitted operation_id"))?
        .try_into()
        .map_err(protocol_error)
}

#[cfg(any(unix, windows))]
fn required_session_id(session_id: Option<v2::SessionId>) -> Result<SessionId, DaemonError> {
    session_id
        .ok_or_else(|| malformed("session mutation omitted session_id"))?
        .try_into()
        .map_err(protocol_error)
}

#[cfg(any(unix, windows))]
fn session_name(value: &str) -> Result<SessionName, DaemonError> {
    SessionName::new(value.to_owned())
        .map_err(|error| DaemonError::new(DomainErrorKind::InvalidSessionName, error.to_string()))
}

#[cfg(any(unix, windows))]
fn local_request_view_id(request_id: u64) -> AttachmentId {
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&request_id.to_le_bytes());
    AttachmentId::from_array(bytes)
}

#[cfg(any(unix, windows))]
fn terminal_history_window_query(
    request: &v2::TerminalHistoryWindowRequest,
) -> Result<TerminalHistoryWindowQuery, DaemonError> {
    let anchor = request
        .anchor
        .as_ref()
        .ok_or_else(|| malformed("terminal history window omitted anchor"))?;
    let rows = u16::try_from(anchor.viewport_rows)
        .ok()
        .filter(|rows| *rows > 0)
        .ok_or_else(|| malformed("terminal history window rows are invalid"))?;
    let columns = u16::try_from(anchor.viewport_columns)
        .ok()
        .filter(|columns| *columns > 0)
        .ok_or_else(|| malformed("terminal history window columns are invalid"))?;
    let limits = ResourceLimits::default();
    if rows > limits.max_viewport_rows || columns > limits.max_viewport_columns {
        return Err(malformed(
            "terminal history window viewport exceeds product limits",
        ));
    }
    let query = TerminalHistoryWindowQuery {
        anchor: TerminalHistoryWindowAnchor {
            epoch: Revision::new(anchor.epoch),
            revision: Revision::new(anchor.revision),
            max_offset_from_bottom: anchor.max_offset_from_bottom,
            viewport: TerminalSize::new(rows, columns),
        },
        target_offset_from_bottom: request.target_offset_from_bottom,
        older_margin_rows: u16::try_from(request.older_margin_rows)
            .map_err(|_| malformed("terminal history window older margin is invalid"))?,
        newer_margin_rows: u16::try_from(request.newer_margin_rows)
            .map_err(|_| malformed("terminal history window newer margin is invalid"))?,
    };
    query
        .is_valid()
        .then_some(query)
        .ok_or_else(|| malformed("terminal history window query is invalid"))
}

#[cfg(any(unix, windows))]
fn malformed(detail: impl Into<String>) -> DaemonError {
    DaemonError::new(DomainErrorKind::MalformedFrame, detail)
}

#[cfg(any(unix, windows))]
fn remote_target_mismatch() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::Unauthorized,
        "remote Session request target does not identify this host",
    )
}

#[cfg(any(unix, windows))]
fn daemon_io(operation: &str, error: std::io::Error) -> DaemonError {
    DaemonError::new(
        DomainErrorKind::DaemonStopped,
        format!("{operation}: {error}"),
    )
}

#[cfg(all(test, unix))]
mod tests;
