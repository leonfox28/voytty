//! One route-neutral attachment protocol and recovery owner.
#[cfg(all(test, unix))]
pub(crate) use tokio::net::{UnixListener as TestListener, UnixStream as TestStream};
#[cfg(all(test, windows))]
pub(crate) use voytty_platform::local::{AsyncListener as TestListener, Stream as TestStream};

mod connect;
mod reconnect;
#[cfg(test)]
mod tests;

use reconnect::ReconnectedAttachment;

use crate::protocol::mutate_response;
use crate::protocol::{
    decode_response, malformed, resolved_target_wire, resource_error, service_error,
};
use crate::transport::{AttachmentConnector, AttachmentTransport, AttachmentTransportItem};
use crate::{
    error::ClientError as DaemonError, model::ResolvedSessionTarget, protocol::protocol_error,
};
use ring::rand::{SecureRandom, SystemRandom};
use std::{
    collections::VecDeque,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use voytty_core::terminal::{
    TerminalClipboardWrite, TerminalColorProfile, TerminalHistoryWindowQuery, TerminalNotification,
    TerminalSurfaceDelta, TerminalSurfaceHistoryWindowResult, TerminalSurfaceSnapshot,
};
use voytty_core::{
    AttachmentId, DomainErrorKind, OperationId, OperationLease, ResumeViewId, Revision, SessionId,
};
use voytty_proto::{DecodedFrame, WireKind, encode_message, v2};

const MAX_DEFERRED_FRAMES: usize = 8;

/// One typed server message received on a local terminal attachment.
#[derive(Clone, PartialEq)]
#[doc(hidden)]
pub enum LocalAttachmentEvent {
    /// Latest frontend-owned attachment transport state.
    TransportState(v2::TerminalTransportStateEvent),
    /// Address-free selected path and RTT from the remote tunnel sideband.
    ConnectionStatus(v2::TerminalConnectionStatusEvent),
    /// A full host-authoritative replacement state.
    Snapshot(TerminalSurfaceSnapshot),
    /// A merged revision update from the acknowledged checkpoint.
    Delta(TerminalSurfaceDelta),
    /// Correlated reconnect barrier; acknowledge once after successful application.
    ResumeDelta(TerminalSurfaceDelta),
    /// One correlated exact semantic history-window outcome.
    HistoryWindow(TerminalSurfaceHistoryWindowResult),
    /// One validated latest-only child clipboard write.
    ClipboardWrite(TerminalClipboardWrite),
    /// One attachment-scoped ordinary notification.
    Notification(TerminalNotification),
    /// The following snapshot must replace the client state atomically.
    SyncRequired(v2::TerminalSyncRequired),
    /// A prepared takeover committed successfully.
    Takeover(crate::model::SessionSummary),
    /// Another attachment replaced this controller.
    LeaseLost(v2::TerminalLeaseLost),
    /// The underlying session and PTY ended.
    SessionEnded(v2::TerminalSessionEnded),
}

impl fmt::Debug for LocalAttachmentEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TransportState(state) => formatter
                .debug_struct("TransportState")
                .field("state", &state.state)
                .finish_non_exhaustive(),
            Self::ConnectionStatus(status) => formatter
                .debug_struct("ConnectionStatus")
                .field("path", &status.path)
                .field("rtt_ms", &status.rtt_ms)
                .finish_non_exhaustive(),
            Self::Snapshot(snapshot) => formatter
                .debug_struct("SemanticSnapshot")
                .field("revision", &snapshot.revision)
                .field("row_count", &snapshot.surface.rows.len())
                .finish_non_exhaustive(),
            Self::Delta(delta) | Self::ResumeDelta(delta) => formatter
                .debug_struct(if matches!(self, Self::ResumeDelta(_)) {
                    "SemanticResumeDelta"
                } else {
                    "SemanticDelta"
                })
                .field("from_revision", &delta.from_revision)
                .field("to_revision", &delta.to_revision)
                .field("row_patch_count", &delta.row_patches.len())
                .finish_non_exhaustive(),
            Self::HistoryWindow(result) => formatter
                .debug_tuple("SemanticHistoryWindow")
                .field(result)
                .finish(),
            Self::Notification(value) => {
                formatter.debug_tuple("Notification").field(value).finish()
            }
            Self::ClipboardWrite(write) => formatter
                .debug_tuple("ClipboardWrite")
                .field(write)
                .finish(),
            Self::SyncRequired(required) => formatter
                .debug_struct("SyncRequired")
                .field("latest_revision", &required.latest_revision)
                .finish_non_exhaustive(),
            Self::Takeover(summary) => formatter.debug_tuple("Takeover").field(summary).finish(),
            Self::LeaseLost(lost) => formatter
                .debug_struct("LeaseLost")
                .field("generation", &lost.generation)
                .finish_non_exhaustive(),
            Self::SessionEnded(ended) => formatter
                .debug_struct("SessionEnded")
                .field("reason", &ended.reason)
                .field("exit_code", &ended.exit_code)
                .field("has_signal", &!ended.signal.is_empty())
                .finish_non_exhaustive(),
        }
    }
}

/// Opaque same-daemon retry token for one takeover whose response was lost.
///
/// It is intentionally process-memory only in M4. Callers must export and
/// retain it explicitly if they need fresh-process ambiguity recovery.
#[derive(Clone, Copy)]
#[doc(hidden)]
pub struct LocalTakeoverRetryToken {
    operation_id: OperationId,
    session_id: SessionId,
}

impl fmt::Debug for LocalTakeoverRetryToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LocalTakeoverRetryToken([REDACTED])")
    }
}

/// Real same-UID duplex socket adapter for one frontend-owned terminal attachment.
#[doc(hidden)]
pub struct SessionClient {
    reconnect_diagnostic: Option<voytty_diagnostics::Operation>,
    last_diagnostic_path: Option<i32>,
    latest_colors: TerminalColorProfile,
    color_sequence: u64,
    transport: AttachmentTransport,
    write_error: Option<DaemonError>,
    takeover_deadline: Option<tokio::time::Instant>,
    deferred: VecDeque<DecodedFrame>,
    pending_transport_events: VecDeque<LocalAttachmentEvent>,
    connector: Arc<dyn AttachmentConnector>,
    session_id: SessionId,
    attachment_id: AttachmentId,
    target: ResolvedSessionTarget,
    resume_view_id: Option<ResumeViewId>,
    applied_revision: Arc<AtomicU64>,
    latest_viewport: voytty_core::terminal::TerminalSize,
    reconnect_pending: bool,
    reconnect_attempt: tokio::task::JoinSet<Result<ReconnectedAttachment, DaemonError>>,
    reconnect_retry: u32,
    retry_after: Option<tokio::time::Instant>,
    resize_after_ack: bool,
    colors_after_ack: bool,
    force_full_sync: bool,
    initial_snapshot: Option<TerminalSurfaceSnapshot>,
    terminal_rows: u32,
    next_request_id: u64,
    operation_lease: Option<OperationLease>,
    next_operation_sequence: u64,
    pending_takeover_request_id: Option<u64>,
    pending_history_window: Option<(u64, TerminalHistoryWindowQuery)>,
    /// This attachment asked its host to number live surface frames.
    surface_frame_receipts: bool,
    /// Highest numbered surface frame decoded on the current stream.
    surface_frames_received: u64,
    /// Highest receipt written on the current stream.
    surface_frames_reported: u64,
}

impl fmt::Debug for SessionClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionClient")
            .field("session_id", &self.session_id)
            .field("attachment_id", &self.attachment_id)
            .field("has_initial_snapshot", &self.initial_snapshot.is_some())
            .field("queued_frames", &self.transport.queued_session_count())
            .field("deferred_frames", &self.deferred.len())
            .field("has_operation_lease", &self.operation_lease.is_some())
            .field(
                "has_pending_takeover",
                &self.pending_takeover_request_id.is_some(),
            )
            .field(
                "has_pending_history_window",
                &self.pending_history_window.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl SessionClient {
    pub(crate) fn upload_origin(&self) -> Result<crate::upload::UploadOrigin, DaemonError> {
        if self.reconnect_pending || self.target.is_local() {
            return Err(DaemonError::new(
                DomainErrorKind::NotSynchronized,
                "upload requires a connected remote attachment",
            ));
        }
        Ok(crate::upload::UploadOrigin {
            target: self.target,
            binding: voytty_core::upload::UploadBinding {
                session_id: self.session_id,
                attachment_id: self.attachment_id,
            },
        })
    }

    /// Returns the attached daemon-lifetime session identity.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Reports whether this frontend is waiting to establish and acknowledge a
    /// replacement remote stream epoch.
    #[must_use]
    pub(crate) const fn reconnect_pending(&self) -> bool {
        self.reconnect_pending
    }

    /// Returns this socket view's attachment identity.
    #[must_use]
    pub const fn attachment_id(&self) -> AttachmentId {
        self.attachment_id
    }

    /// Whether this frontend Session client uses an opaque remote tunnel.
    #[must_use]
    pub const fn is_remote(&self) -> bool {
        self.target.device_id().is_some()
    }

    /// Shared frontend-applied revision observed by reconnect establishment.
    pub(crate) fn applied_revision_tracker(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.applied_revision)
    }

    /// Transfers the initial full state to the prepared view exactly once.
    #[must_use]
    pub fn take_initial_snapshot(&mut self) -> Option<TerminalSurfaceSnapshot> {
        self.initial_snapshot.take()
    }

    /// Atomically acknowledges the exact full snapshot revision.
    pub async fn snapshot_applied(&mut self, revision: Revision) -> Result<(), DaemonError> {
        // This is the frontend's rendered baseline even when the following
        // transport write is interrupted. A resume may safely advertise it;
        // the target falls back to a full snapshot when its checkpoint differs.
        self.applied_revision
            .store(revision.get(), Ordering::Release);
        let result = self
            .send(
                WireKind::TerminalSnapshotApplied,
                &v2::TerminalSnapshotApplied {
                    attachment_id: Some(self.attachment_id.into()),
                    revision: revision.get(),
                },
            )
            .await
            .map(|_| ());
        if result.is_ok() && voytty_diagnostics::detail_enabled() {
            voytty_diagnostics::record(
                voytty_diagnostics::Event::new(voytty_diagnostics::Kind::SyncChanged)
                    .level(voytty_diagnostics::Level::Debug)
                    .session(self.session_id)
                    .attachment(self.attachment_id)
                    .stage(voytty_diagnostics::Stage::SnapshotApplied)
                    .epoch(revision.get()),
            );
        }
        result?;
        if !self.reconnect_pending {
            if std::mem::take(&mut self.colors_after_ack) {
                self.update_colors(self.latest_colors.clone()).await?;
            }
            if std::mem::take(&mut self.resize_after_ack) {
                self.resize(self.latest_viewport).await?;
            }
        }
        Ok(())
    }

    /// Sends controller input without waiting for a redundant success ACK.
    pub async fn write_input(&mut self, bytes: Vec<u8>) -> Result<(), DaemonError> {
        self.send(
            WireKind::TerminalInput,
            &v2::TerminalInput {
                operation_id: None,
                attachment_id: Some(self.attachment_id.into()),
                bytes,
            },
        )
        .await
        .map(|_| ())
    }

    /// Sends observations from the current physical terminal; never child input.
    pub async fn update_colors(
        &mut self,
        profile: TerminalColorProfile,
    ) -> Result<(), DaemonError> {
        self.colors_after_ack = false;
        self.latest_colors = profile.clone();
        self.color_sequence = self
            .color_sequence
            .checked_add(1)
            .ok_or_else(|| resource_error("color observation sequence exhausted"))?;
        self.send(
            WireKind::TerminalBaseColors,
            &v2::TerminalBaseColors {
                attachment_id: Some(self.attachment_id.into()),
                profile: Some(profile.into()),
                sequence: self.color_sequence,
            },
        )
        .await
        .map(|_| ())
    }

    /// Requests one validated native/model viewport change.
    pub async fn resize(
        &mut self,
        size: voytty_core::terminal::TerminalSize,
    ) -> Result<(), DaemonError> {
        if size != self.latest_viewport && voytty_diagnostics::detail_enabled() {
            voytty_diagnostics::record(
                voytty_diagnostics::Event::new(voytty_diagnostics::Kind::ViewportChanged)
                    .level(voytty_diagnostics::Level::Debug)
                    .session(self.session_id)
                    .attachment(self.attachment_id)
                    .viewport(size.columns, size.rows),
            );
        }
        // Retain the frontend's latest desired viewport across a tunnel epoch.
        self.resize_after_ack = false;
        self.latest_viewport = size;
        self.send(
            WireKind::TerminalResize,
            &v2::TerminalResize {
                operation_id: None,
                attachment_id: Some(self.attachment_id.into()),
                rows: u32::from(size.rows),
                columns: u32::from(size.columns),
            },
        )
        .await
        .map(|_| ())
    }

    /// Discards the client baseline and requests a fresh snapshot.
    pub async fn request_sync(&mut self, known_revision: Revision) -> Result<(), DaemonError> {
        if voytty_diagnostics::detail_enabled() {
            voytty_diagnostics::record(
                voytty_diagnostics::Event::new(voytty_diagnostics::Kind::SyncChanged)
                    .level(voytty_diagnostics::Level::Debug)
                    .session(self.session_id)
                    .attachment(self.attachment_id)
                    .reason(voytty_diagnostics::Reason::SyncRequired)
                    .epoch(known_revision.get()),
            );
        }
        if self.target.device_id().is_some() {
            self.force_full_sync = true;
        }
        self.send(
            WireKind::TerminalSyncRequest,
            &v2::TerminalSyncRequest {
                attachment_id: Some(self.attachment_id.into()),
                known_revision: known_revision.get(),
            },
        )
        .await
        .map(|_| ())
    }

    /// Checks the local admission rules of [`Self::request_history_window`]
    /// without writing a frame.
    pub(crate) fn check_history_window_request(
        &self,
        query: TerminalHistoryWindowQuery,
    ) -> Result<(), DaemonError> {
        if self.pending_history_window.is_some() {
            return Err(resource_error(
                "a terminal history window response is already pending",
            ));
        }
        if !query.is_valid() {
            return Err(resource_error(
                "terminal history window query is outside the allowed range",
            ));
        }
        Ok(())
    }

    /// Requests one stateless bounded history window. Exactly one such request
    /// may be outstanding per view.
    pub(crate) async fn request_history_window(
        &mut self,
        query: TerminalHistoryWindowQuery,
    ) -> Result<(), DaemonError> {
        self.check_history_window_request(query)?;
        let request_id = self
            .send(
                WireKind::TerminalHistoryWindowRequest,
                &v2::TerminalHistoryWindowRequest {
                    attachment_id: Some(self.attachment_id.into()),
                    anchor: Some(query.anchor.into()),
                    target_offset_from_bottom: query.target_offset_from_bottom,
                    older_margin_rows: u32::from(query.older_margin_rows),
                    newer_margin_rows: u32::from(query.newer_margin_rows),
                },
            )
            .await?;
        self.pending_history_window = Some((request_id, query));
        if self.reconnect_pending {
            self.pending_history_window = None;
            self.pending_transport_events
                .push_front(LocalAttachmentEvent::HistoryWindow(reconnect_history_gap(
                    query,
                    self.applied_revision.load(Ordering::Acquire),
                )));
        }
        Ok(())
    }

    /// Commits a previously prepared and acknowledged takeover attachment.
    pub async fn takeover(&mut self) -> Result<(), DaemonError> {
        self.begin_takeover().await.map(|_| ())
    }

    /// Sends a takeover and returns the opaque token required after ambiguous
    /// response loss.
    #[doc(hidden)]
    pub async fn begin_takeover(&mut self) -> Result<LocalTakeoverRetryToken, DaemonError> {
        if self.pending_takeover_request_id.is_some() {
            return Err(malformed("a takeover response is already pending"));
        }
        let deadline = crate::protocol::control_deadline();
        let operation_id = match tokio::time::timeout_at(deadline, self.next_operation_id()).await {
            Ok(result) => result?,
            Err(_) => {
                self.invalidate_transport();
                return Err(crate::protocol::control_timeout());
            }
        };
        let receipt = LocalTakeoverRetryToken {
            operation_id,
            session_id: self.session_id,
        };
        self.takeover_deadline = Some(deadline);
        self.pending_takeover_request_id = Some(self.send_takeover(operation_id).await?);
        Ok(receipt)
    }

    /// Continues an ambiguously completed takeover on a newly synchronized
    /// attachment without inventing a new logical operation.
    #[doc(hidden)]
    pub async fn retry_takeover(
        &mut self,
        token: LocalTakeoverRetryToken,
    ) -> Result<(), DaemonError> {
        if token.session_id != self.session_id {
            return Err(malformed("takeover retry token belongs to another session"));
        }
        if self.pending_takeover_request_id.is_some() {
            return Err(malformed("a takeover response is already pending"));
        }
        self.takeover_deadline = Some(crate::protocol::control_deadline());
        self.pending_takeover_request_id = Some(self.send_takeover(token.operation_id).await?);
        Ok(())
    }

    async fn send_takeover(&mut self, operation_id: OperationId) -> Result<u64, DaemonError> {
        let deadline = self
            .takeover_deadline
            .unwrap_or_else(crate::protocol::control_deadline);
        // Expiry before write is definitive; once polling a write, its outcome may be unknown.
        if deadline <= tokio::time::Instant::now() {
            return Err(crate::protocol::control_timeout());
        }
        let result = tokio::time::timeout_at(
            deadline,
            self.send(
                WireKind::SessionTakeoverRequest,
                &v2::SessionTakeoverRequest {
                    operation_id: Some(operation_id.into()),
                    target: Some(resolved_target_wire(self.target)),
                    session_id: Some(self.session_id.into()),
                    attachment_id: Some(self.attachment_id.into()),
                },
            ),
        )
        .await;
        match result {
            Ok(Ok(request_id)) => Ok(request_id),
            Ok(Err(error)) if !is_retryable_remote_reconnect(error.kind()) => Err(error),
            Ok(Err(_)) | Err(_) => {
                self.invalidate_transport();
                Err(takeover_outcome_unknown())
            }
        }
    }

    /// Reads one typed terminal event, bounded by the caller's deadline.
    pub async fn read_event(
        &mut self,
        deadline: Duration,
    ) -> Result<LocalAttachmentEvent, DaemonError> {
        let event = tokio::time::timeout(deadline, self.read_next_event())
            .await
            .map_err(|_| {
                DaemonError::new(
                    DomainErrorKind::DeadlineExceeded,
                    "timed out waiting for local terminal event",
                )
            })??;
        // Outside the cancellable read, so the receipt write cannot be abandoned.
        self.report_surface_frames().await?;
        Ok(event)
    }

    /// Reads the next event without imposing a timeout on an active view.
    ///
    /// The owner may cancel and repoll this future: decoder and queued-byte
    /// state stay in this same client object.
    pub(crate) async fn read_next_event(&mut self) -> Result<LocalAttachmentEvent, DaemonError> {
        if self.pending_takeover_request_id.is_some()
            && let Some(deadline) = self.takeover_deadline
        {
            if deadline > tokio::time::Instant::now()
                && let Ok(result) =
                    tokio::time::timeout_at(deadline, self.read_next_event_inner()).await
                && !result
                    .as_ref()
                    .err()
                    .is_some_and(|error| is_retryable_remote_reconnect(error.kind()))
            {
                return result;
            }
            self.invalidate_transport();
            return Err(takeover_outcome_unknown());
        }
        let result = self.read_next_event_inner().await;
        if let Ok(LocalAttachmentEvent::ConnectionStatus(status)) = &result
            && self.last_diagnostic_path != Some(status.path)
        {
            self.last_diagnostic_path = Some(status.path);
            let stage = match v2::TerminalConnectionPath::try_from(status.path) {
                Ok(v2::TerminalConnectionPath::Direct) => voytty_diagnostics::Stage::Direct,
                Ok(v2::TerminalConnectionPath::Relay) => voytty_diagnostics::Stage::Relay,
                _ => voytty_diagnostics::Stage::Unknown,
            };
            voytty_diagnostics::record(
                voytty_diagnostics::Event::new(voytty_diagnostics::Kind::RouteChanged)
                    .session(self.session_id)
                    .attachment(self.attachment_id)
                    .stage(stage),
            );
        }
        result
    }

    async fn read_next_event_inner(&mut self) -> Result<LocalAttachmentEvent, DaemonError> {
        if let Some(event) = self.pending_transport_events.pop_front() {
            return Ok(event);
        }
        if self.reconnect_pending {
            self.reconnect_remote().await?;
            return self
                .pending_transport_events
                .pop_front()
                .ok_or_else(|| malformed("remote resume omitted synchronization state"));
        }
        let frame = if let Some(frame) = self.deferred.pop_front() {
            frame
        } else {
            let item = match self.transport.read_item().await {
                Ok(item) => item,
                Err(error)
                    if self.target.device_id().is_some()
                        && is_retryable_remote_reconnect(error.kind()) =>
                {
                    if self.pending_takeover_request_id.is_some() {
                        self.pending_takeover_request_id = None;
                        self.operation_lease = None;
                        self.next_operation_sequence = 1;
                        return Err(DaemonError::new(
                            DomainErrorKind::OperationOutcomeUnknown,
                            "terminal takeover may have committed before tunnel loss",
                        ));
                    }
                    self.enter_reconnecting();
                    return self
                        .pending_transport_events
                        .pop_front()
                        .ok_or_else(|| malformed("remote reconnect omitted its transition event"));
                }
                Err(error) => return Err(error),
            };
            match item {
                AttachmentTransportItem::Session(frame) => frame,
                AttachmentTransportItem::Path(path) => {
                    return Ok(LocalAttachmentEvent::ConnectionStatus(
                        v2::TerminalConnectionStatusEvent {
                            attachment_id: Some(self.attachment_id.into()),
                            path: path.path,
                            rtt_ms: path.rtt_ms,
                        },
                    ));
                }
            }
        };
        if frame.kind == WireKind::ServiceErrorResponse {
            let error = service_error(&frame)?;
            if self.pending_takeover_request_id == Some(frame.request_id) {
                self.pending_takeover_request_id = None;
            }
            if self
                .pending_history_window
                .is_some_and(|(request_id, _)| request_id == frame.request_id)
            {
                self.pending_history_window = None;
            }
            if error.kind() == DomainErrorKind::OperationOutcomeUnknown {
                self.operation_lease = None;
                self.next_operation_sequence = 1;
            }
            return Err(error);
        }
        match frame.kind {
            WireKind::TerminalTransportStateEvent => {
                if self.target.device_id().is_some() {
                    return Err(malformed(
                        "remote target Session stream carried a same-UID-only transport state",
                    ));
                }
                let state: v2::TerminalTransportStateEvent = frame
                    .decode_message(WireKind::TerminalTransportStateEvent)
                    .map_err(protocol_error)?;
                self.require_attachment(state.attachment_id.clone())?;
                v2::TerminalTransportState::try_from(state.state)
                    .map_err(|_| malformed("unknown terminal transport state"))?;
                Ok(LocalAttachmentEvent::TransportState(state))
            }
            WireKind::TerminalConnectionStatusEvent => {
                if self.target.device_id().is_some() {
                    return Err(malformed(
                        "remote target Session stream carried a same-UID-only connection status",
                    ));
                }
                let status: v2::TerminalConnectionStatusEvent = frame
                    .decode_message(WireKind::TerminalConnectionStatusEvent)
                    .map_err(protocol_error)?;
                self.require_attachment(status.attachment_id.clone())?;
                match v2::TerminalConnectionPath::try_from(status.path) {
                    Ok(v2::TerminalConnectionPath::Unknown)
                    | Ok(v2::TerminalConnectionPath::Direct)
                    | Ok(v2::TerminalConnectionPath::Relay) => {}
                    Ok(v2::TerminalConnectionPath::Unspecified) | Err(_) => {
                        return Err(malformed("unknown terminal connection path"));
                    }
                }
                Ok(LocalAttachmentEvent::ConnectionStatus(status))
            }
            WireKind::TerminalSemanticSnapshot => {
                let snapshot: v2::TerminalSemanticSnapshot = frame
                    .decode_message(WireKind::TerminalSemanticSnapshot)
                    .map_err(protocol_error)?;
                let frame_sequence = snapshot.frame_sequence;
                let (session_id, attachment_id, surface) =
                    voytty_proto::terminal_surface_snapshot_from_message(snapshot)
                        .map_err(protocol_error)?;
                if session_id != self.session_id || attachment_id != self.attachment_id {
                    return Err(malformed("semantic terminal snapshot identity mismatch"));
                }
                self.observe_surface_frame(frame_sequence)?;
                self.force_full_sync = false;
                self.terminal_rows = u32::from(surface.surface.size.rows);
                if voytty_diagnostics::detail_enabled() {
                    voytty_diagnostics::record(
                        voytty_diagnostics::Event::new(voytty_diagnostics::Kind::SyncChanged)
                            .level(voytty_diagnostics::Level::Debug)
                            .session(self.session_id)
                            .attachment(self.attachment_id)
                            .stage(voytty_diagnostics::Stage::SnapshotReceived)
                            .epoch(surface.revision.get())
                            .viewport(surface.surface.size.columns, surface.surface.size.rows),
                    );
                }
                Ok(LocalAttachmentEvent::Snapshot(surface))
            }
            WireKind::TerminalSemanticDelta => {
                let delta: v2::TerminalSemanticDelta = frame
                    .decode_message(WireKind::TerminalSemanticDelta)
                    .map_err(protocol_error)?;
                let frame_sequence = delta.frame_sequence;
                let (attachment_id, semantic) =
                    voytty_proto::terminal_surface_delta_from_message(delta)
                        .map_err(protocol_error)?;
                if attachment_id != self.attachment_id {
                    return Err(malformed("semantic terminal delta attachment_id mismatch"));
                }
                self.observe_surface_frame(frame_sequence)?;
                self.terminal_rows = u32::from(semantic.size.rows);
                Ok(LocalAttachmentEvent::Delta(semantic))
            }
            WireKind::TerminalSemanticHistoryWindowFrame => {
                let Some((request_id, query)) = self.pending_history_window else {
                    return Err(malformed(
                        "semantic terminal history window correlation mismatch",
                    ));
                };
                if request_id != frame.request_id {
                    return Err(malformed(
                        "semantic terminal history window correlation mismatch",
                    ));
                }
                let window: v2::TerminalSemanticHistoryWindowFrame = frame
                    .decode_message(WireKind::TerminalSemanticHistoryWindowFrame)
                    .map_err(protocol_error)?;
                let (attachment_id, result) =
                    voytty_proto::terminal_surface_history_window_from_message(window, query)
                        .map_err(protocol_error)?;
                if attachment_id != self.attachment_id {
                    return Err(malformed(
                        "semantic terminal history window attachment_id mismatch",
                    ));
                }
                self.pending_history_window = None;
                Ok(LocalAttachmentEvent::HistoryWindow(result))
            }
            WireKind::TerminalNotification => {
                if frame.request_id != 0 || frame.deadline_ms != 0 {
                    return Err(malformed("terminal notification must be unsolicited"));
                }
                let message = frame
                    .decode_message(WireKind::TerminalNotification)
                    .map_err(protocol_error)?;
                let (attachment_id, notification) =
                    voytty_proto::terminal_notification_from_message(message)
                        .map_err(protocol_error)?;
                if attachment_id != self.attachment_id {
                    return Err(malformed("terminal notification attachment_id mismatch"));
                }
                Ok(LocalAttachmentEvent::Notification(notification))
            }
            WireKind::TerminalClipboardWrite => {
                if frame.request_id != 0 {
                    return Err(malformed(
                        "terminal clipboard effect must use request_id zero",
                    ));
                }
                let message: v2::TerminalClipboardWrite = frame
                    .decode_message(WireKind::TerminalClipboardWrite)
                    .map_err(protocol_error)?;
                let (attachment_id, write) =
                    voytty_proto::terminal_clipboard_write_from_message(message)
                        .map_err(protocol_error)?;
                if attachment_id != self.attachment_id {
                    return Err(malformed(
                        "terminal clipboard effect attachment_id mismatch",
                    ));
                }
                Ok(LocalAttachmentEvent::ClipboardWrite(write))
            }
            WireKind::TerminalSyncRequired => {
                let required: v2::TerminalSyncRequired = frame
                    .decode_message(WireKind::TerminalSyncRequired)
                    .map_err(protocol_error)?;
                self.require_attachment(required.attachment_id.clone())?;
                Ok(LocalAttachmentEvent::SyncRequired(required))
            }
            WireKind::SessionMutateResponse => {
                if self.pending_takeover_request_id != Some(frame.request_id) {
                    return Err(malformed("takeover response correlation mismatch"));
                }
                self.pending_takeover_request_id = None;
                Ok(LocalAttachmentEvent::Takeover(mutate_response(frame)?))
            }
            WireKind::TerminalLeaseLost => {
                let lost: v2::TerminalLeaseLost = frame
                    .decode_message(WireKind::TerminalLeaseLost)
                    .map_err(protocol_error)?;
                self.require_attachment(lost.attachment_id.clone())?;
                voytty_diagnostics::record(
                    voytty_diagnostics::Event::new(voytty_diagnostics::Kind::TerminalLeaseLost)
                        .session(self.session_id)
                        .attachment(self.attachment_id)
                        .reason(voytty_diagnostics::Reason::LeaseLost),
                );
                Ok(LocalAttachmentEvent::LeaseLost(lost))
            }
            WireKind::TerminalSessionEnded => {
                let ended: v2::TerminalSessionEnded = frame
                    .decode_message(WireKind::TerminalSessionEnded)
                    .map_err(protocol_error)?;
                self.require_attachment(ended.attachment_id.clone())?;
                let session_id: SessionId = ended
                    .session_id
                    .clone()
                    .ok_or_else(|| malformed("session-ended event omitted session_id"))?
                    .try_into()
                    .map_err(protocol_error)?;
                if session_id != self.session_id {
                    return Err(malformed("session-ended event session_id mismatch"));
                }
                Ok(LocalAttachmentEvent::SessionEnded(ended))
            }
            kind => Err(malformed(format!(
                "wire kind {kind:?} is invalid from a terminal attachment"
            ))),
        }
    }

    /// Detaches this view while leaving the session and PTY running.
    pub async fn detach(&mut self) -> Result<(), DaemonError> {
        let operation = voytty_diagnostics::Operation::default();
        let session_id = self.session_id;
        let attachment_id = self.attachment_id;
        let record = |stage| {
            voytty_diagnostics::record(
                voytty_diagnostics::Event::new(voytty_diagnostics::Kind::ControllerDetached)
                    .level(voytty_diagnostics::Level::Debug)
                    .operation(&operation)
                    .session(session_id)
                    .attachment(attachment_id)
                    .stage(stage),
            );
        };
        record(voytty_diagnostics::Stage::WritingDetach);
        self.send(
            WireKind::TerminalDetach,
            &v2::TerminalDetach {
                attachment_id: Some(self.attachment_id.into()),
            },
        )
        .await?;
        if self.reconnect_pending {
            return Ok(());
        }
        record(voytty_diagnostics::Stage::AwaitingDetach);
        let result = self.transport.shutdown().await;
        record(if result.is_ok() {
            voytty_diagnostics::Stage::Detached
        } else {
            voytty_diagnostics::Stage::Failed
        });
        result
    }

    async fn send<Message: prost::Message>(
        &mut self,
        kind: WireKind,
        message: &Message,
    ) -> Result<u64, DaemonError> {
        let request_id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| resource_error("local attachment request ID exhausted"))?;
        let bytes = encode_message(kind, request_id, 0, message).map_err(protocol_error)?;
        let result = match &self.write_error {
            Some(error) => Err(error.clone()),
            None => self.transport.write_session_bytes(&bytes).await,
        };
        if let Err(error) = result {
            self.write_error = Some(error.clone());
            if self.target.device_id().is_some() && is_retryable_remote_reconnect(error.kind()) {
                self.enter_reconnecting();
                return match kind {
                    // These messages are never replayed. The frontend state
                    // retained above is enough to converge after resume;
                    // input at the exact loss boundary remains intentionally
                    // best-effort, matching an interrupted terminal stream.
                    WireKind::TerminalInput
                    | WireKind::TerminalResize
                    | WireKind::TerminalBaseColors
                    | WireKind::TerminalSnapshotApplied
                    | WireKind::TerminalSurfaceFramesReceived
                    | WireKind::TerminalSyncRequest
                    | WireKind::TerminalHistoryWindowRequest
                    | WireKind::TerminalDetach => Ok(request_id),
                    // A takeover may have committed at the target. Never mint
                    // a fresh logical operation or report a false failure.
                    WireKind::SessionTakeoverRequest => Err(DaemonError::new(
                        DomainErrorKind::OperationOutcomeUnknown,
                        "terminal takeover may have committed before tunnel loss",
                    )),
                    _ => Err(error),
                };
            }
            return Err(error);
        }
        Ok(request_id)
    }

    pub(super) fn invalidate_transport(&mut self) {
        self.transport.invalidate();
        self.write_error = Some(crate::protocol::attachment_cancelled());
        self.deferred.clear();
        self.pending_transport_events.clear();
        self.reset_surface_frame_receipts();
    }

    /// Receipts are stream-local; a replacement stream numbers from one.
    fn reset_surface_frame_receipts(&mut self) {
        self.surface_frames_received = 0;
        self.surface_frames_reported = 0;
    }

    /// Records one decoded live surface frame's stream sequence.
    fn observe_surface_frame(&mut self, frame_sequence: u64) -> Result<(), DaemonError> {
        if !self.surface_frame_receipts || frame_sequence == 0 {
            return Ok(());
        }
        if Some(frame_sequence) != self.surface_frames_received.checked_add(1) {
            return Err(malformed(
                "terminal surface frame sequence is not consecutive",
            ));
        }
        self.surface_frames_received = frame_sequence;
        Ok(())
    }

    /// Reports decoded numbered surface frames to the host.
    ///
    /// Callers invoke this outside any cancellable read: an abandoned write
    /// retires the whole epoch. The receipt is stream-local and never
    /// replayed; reconnect starts a new count.
    pub(crate) async fn report_surface_frames(&mut self) -> Result<(), DaemonError> {
        let frame_sequence = self.surface_frames_received;
        if self.reconnect_pending || frame_sequence <= self.surface_frames_reported {
            return Ok(());
        }
        self.send(
            WireKind::TerminalSurfaceFramesReceived,
            &v2::TerminalSurfaceFramesReceived {
                attachment_id: Some(self.attachment_id.into()),
                frame_sequence,
            },
        )
        .await?;
        // A retryable loss enters reconnect and resets this stream's count.
        if !self.reconnect_pending {
            self.surface_frames_reported = frame_sequence;
        }
        Ok(())
    }

    fn reserve_deferred(&mut self, bytes: usize) -> Result<(), DaemonError> {
        let retained = self
            .deferred
            .iter()
            .map(|frame| frame.payload.len())
            .sum::<usize>();
        // Sideband statuses have fixed identity and scalar fields, bounded by 64 bytes.
        let sideband_bytes = self.pending_transport_events.len() * 64;
        if self.deferred.len() + self.pending_transport_events.len() >= MAX_DEFERRED_FRAMES
            || retained + sideband_bytes + bytes > voytty_proto::MAX_FRAME_BYTES
        {
            self.invalidate_transport();
            return Err(resource_error(
                "terminal control response buffer is exhausted",
            ));
        }
        Ok(())
    }

    async fn read_transport_frame(&mut self) -> Result<DecodedFrame, DaemonError> {
        loop {
            match self.transport.read_item().await? {
                AttachmentTransportItem::Session(frame) => return Ok(frame),
                AttachmentTransportItem::Path(path) => {
                    self.reserve_deferred(64)?;
                    self.pending_transport_events.push_back(
                        LocalAttachmentEvent::ConnectionStatus(v2::TerminalConnectionStatusEvent {
                            attachment_id: Some(self.attachment_id.into()),
                            path: path.path,
                            rtt_ms: path.rtt_ms,
                        }),
                    );
                }
            }
        }
    }

    async fn next_operation_id(&mut self) -> Result<OperationId, DaemonError> {
        if self.operation_lease.is_none() {
            let request_id = self.next_request_id;
            self.send(
                WireKind::SessionOperationLeaseRequest,
                &v2::SessionOperationLeaseRequest {
                    target: Some(resolved_target_wire(self.target)),
                },
            )
            .await?;
            loop {
                let frame = self.read_transport_frame().await?;
                if frame.kind == WireKind::TerminalLeaseLost {
                    // Takeover can retire this attachment while its operation
                    // lease is in flight. Waiting for the correlated reply
                    // would hide the terminal event behind EOF or a timeout,
                    // and a later lease reply must not enable another takeover.
                    if frame.request_id != 0 || frame.deadline_ms != 0 {
                        return Err(malformed("lease-lost event must be unsolicited"));
                    }
                    let lost: v2::TerminalLeaseLost = frame
                        .decode_message(WireKind::TerminalLeaseLost)
                        .map_err(protocol_error)?;
                    self.require_attachment(lost.attachment_id)?;
                    voytty_diagnostics::record(
                        voytty_diagnostics::Event::new(voytty_diagnostics::Kind::TerminalLeaseLost)
                            .session(self.session_id)
                            .attachment(self.attachment_id)
                            .reason(voytty_diagnostics::Reason::LeaseLost),
                    );
                    let error = DaemonError::new(
                        DomainErrorKind::LeaseLost,
                        "another attachment took over while waiting for an operation lease",
                    );
                    self.invalidate_transport();
                    self.write_error = Some(error.clone());
                    return Err(error);
                }
                if frame.request_id != request_id {
                    self.reserve_deferred(frame.payload.len())?;
                    self.deferred.push_back(frame);
                    continue;
                }
                if frame.kind == WireKind::ServiceErrorResponse {
                    return Err(service_error(&frame)?);
                }
                if frame.kind != WireKind::SessionOperationLeaseResponse {
                    return Err(malformed("operation lease response kind mismatch"));
                }
                let response: v2::SessionOperationLeaseResponse = decode_response(&frame)?;
                self.operation_lease = Some(
                    response
                        .lease
                        .ok_or_else(|| malformed("operation lease response omitted lease"))?
                        .try_into()
                        .map_err(protocol_error)?,
                );
                break;
            }
        }
        let sequence = self.next_operation_sequence;
        self.next_operation_sequence = sequence.checked_add(1).ok_or_else(|| {
            self.operation_lease = None;
            self.next_operation_sequence = 1;
            resource_error("local attachment operation sequence exhausted")
        })?;
        Ok(OperationId {
            lease: self.operation_lease.expect("lease was allocated above"),
            sequence,
        })
    }

    fn require_attachment(
        &self,
        attachment_id: Option<v2::AttachmentId>,
    ) -> Result<(), DaemonError> {
        let attachment_id: AttachmentId = attachment_id
            .ok_or_else(|| malformed("terminal event omitted attachment_id"))?
            .try_into()
            .map_err(protocol_error)?;
        if attachment_id == self.attachment_id {
            Ok(())
        } else {
            Err(malformed("terminal event attachment_id mismatch"))
        }
    }
}

fn check_initial_capacity(retained: usize) -> Result<(), DaemonError> {
    if retained >= MAX_DEFERRED_FRAMES {
        Err(resource_error(
            "terminal handshake metadata buffer is exhausted",
        ))
    } else {
        Ok(())
    }
}

fn takeover_outcome_unknown() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::OperationOutcomeUnknown,
        "terminal takeover may have committed before its response was received",
    )
}

fn random_resume_view_id() -> Result<ResumeViewId, DaemonError> {
    let mut bytes = [0_u8; ResumeViewId::LENGTH];
    SystemRandom::new().fill(&mut bytes).map_err(|_| {
        DaemonError::new(
            DomainErrorKind::TransportUnavailable,
            "operating-system randomness is unavailable for a terminal view",
        )
    })?;
    Ok(ResumeViewId::from_array(bytes))
}

fn reconnect_history_gap(
    query: TerminalHistoryWindowQuery,
    applied_revision: u64,
) -> TerminalSurfaceHistoryWindowResult {
    TerminalSurfaceHistoryWindowResult::HistoryGap {
        epoch: query.anchor.epoch,
        revision: Revision::new(applied_revision.max(query.anchor.revision.get()).max(1)),
    }
}

fn terminal_transport_state(
    attachment_id: AttachmentId,
    state: v2::TerminalTransportState,
) -> v2::TerminalTransportStateEvent {
    v2::TerminalTransportStateEvent {
        attachment_id: Some(attachment_id.into()),
        state: state as i32,
    }
}

fn changed_tunnel_path_events_after_unknown(
    attachment_id: AttachmentId,
    paths: impl IntoIterator<Item = v2::LocalSessionTunnelPath>,
) -> VecDeque<LocalAttachmentEvent> {
    let mut last_path = (
        v2::TerminalConnectionPath::Unknown as i32,
        Option::<u32>::None,
    );
    paths
        .into_iter()
        .filter_map(|path| {
            let sample = (path.path, path.rtt_ms);
            if sample == last_path {
                return None;
            }
            last_path = sample;
            Some(LocalAttachmentEvent::ConnectionStatus(
                v2::TerminalConnectionStatusEvent {
                    attachment_id: Some(attachment_id.into()),
                    path: sample.0,
                    rtt_ms: sample.1,
                },
            ))
        })
        .collect()
}

const fn is_retryable_remote_reconnect(kind: DomainErrorKind) -> bool {
    matches!(
        kind,
        DomainErrorKind::AddressUnavailable
            | DomainErrorKind::TransportUnavailable
            | DomainErrorKind::DeadlineExceeded
            | DomainErrorKind::DaemonStopped
            | DomainErrorKind::Cancelled
    )
}

fn create_main_outcome_unknown() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::OperationOutcomeUnknown,
        "the default Session may have been created, but no complete correlated initial attachment result was received",
    )
}
