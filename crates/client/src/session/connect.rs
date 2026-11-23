//! Initial attachment setup and progress-instrumented startup.

use std::collections::VecDeque;
use std::sync::{Arc, atomic::AtomicU64};

use voytty_core::terminal::TerminalColorProfile;
use voytty_core::{AttachmentId, DomainErrorKind, SessionSelector};
use voytty_proto::{WireKind, encode_message, v2};

use super::{
    SessionClient, changed_tunnel_path_events_after_unknown, check_initial_capacity,
    create_main_outcome_unknown, random_resume_view_id,
};
use crate::error::ClientError as DaemonError;
use crate::model::ResolvedSessionTarget;
use crate::protocol::{
    DEFAULT_DEADLINE, malformed, protocol_error, resolved_target_wire, service_error,
};
use crate::transport::{AttachmentConnector, AttachmentTransportItem};

impl SessionClient {
    /// Opens an attachment using the injected transport and exact target.
    pub async fn connect(
        connector: Arc<dyn AttachmentConnector>,
        target: ResolvedSessionTarget,
        selector: Option<SessionSelector>,
        create_main: bool,
        takeover: bool,
        viewport: Option<voytty_core::terminal::TerminalSize>,
        base_colors: TerminalColorProfile,
    ) -> Result<Self, DaemonError> {
        Self::connect_with_progress(
            connector,
            target,
            selector,
            create_main,
            takeover,
            viewport,
            base_colors,
            crate::progress::ProgressObserver::default(),
        )
        .await
    }

    /// Opens the initial attachment with content-free startup observations.
    #[expect(
        clippy::too_many_arguments,
        reason = "Retains the existing attachment inputs plus an optional observation sink"
    )]
    pub async fn connect_with_progress(
        connector: Arc<dyn AttachmentConnector>,
        target: ResolvedSessionTarget,
        selector: Option<SessionSelector>,
        create_main: bool,
        takeover: bool,
        viewport: Option<voytty_core::terminal::TerminalSize>,
        base_colors: TerminalColorProfile,
        progress: crate::progress::ProgressObserver,
    ) -> Result<Self, DaemonError> {
        use voytty_diagnostics::{Event, Kind, Level, Operation, Outcome, Stage};
        let operation = Operation::default();
        voytty_diagnostics::record(
            Event::new(Kind::ConnectionStarted)
                .operation(&operation)
                .connection(operation.id()),
        );
        let result = async move {
            let deadline = crate::protocol::startup_deadline(target);
            let (session_id, session_name) = match selector {
                Some(SessionSelector::Id(session_id)) => (Some(session_id.into()), String::new()),
                Some(SessionSelector::Name(name)) => (None, name.to_string()),
                None => (None, String::new()),
            };
            let resume_view_id = if target.device_id().is_some() {
                Some(random_resume_view_id()?)
            } else {
                None
            };
            let request_id = 1;
            let bytes = encode_message(
                WireKind::TerminalAttachRequest,
                request_id,
                u32::try_from(DEFAULT_DEADLINE.as_millis()).unwrap_or(u32::MAX),
                &v2::TerminalAttachRequest {
                    accept_zlib_frames: target.device_id().is_some(),
                    // Remote paths can buffer far more than they deliver; bound
                    // undelivered live frames so a requested viewport is not
                    // queued behind obsolete output.
                    acknowledge_surface_frames: target.device_id().is_some(),
                    base_colors: Some(base_colors.clone().into()),
                    target: Some(resolved_target_wire(target)),
                    session_id,
                    takeover,
                    session_name,
                    create_main,
                    viewport: viewport.map(Into::into),
                    resume_view_id: resume_view_id.map(Into::into),
                    known_revision: None,
                },
            )
            .map_err(protocol_error)?;
            let mut transport = tokio::time::timeout_at(deadline, connector.open(target))
                .await
                .map_err(|_| crate::protocol::control_timeout())??;
            // Connection setup may use the startup budget, but an admitted
            // Session operation still gets at most five seconds and never
            // extends the original absolute deadline.
            let deadline = deadline.min(crate::protocol::control_deadline());
            progress.report(voytty_core::connection_progress::ConnectionStage::RequestingSession);
            if let Err(error) = transport.write_until(&bytes, deadline).await {
                return Err(if create_main {
                    create_main_outcome_unknown()
                } else {
                    error
                });
            }

            // The outer result owns post-write ambiguity. The inner result is
            // reserved for a decoded, correlated ServiceError, which is already a
            // definitive result and must retain its exact domain category.
            progress.report(
                voytty_core::connection_progress::ConnectionStage::ReceivingTerminalSnapshot,
            );
            let response = tokio::time::timeout_at(deadline, async {
            let mut pre_snapshot_states = Vec::new();
            let mut pre_snapshot_paths = Vec::new();
            let initial_snapshot = loop {
                let frame = match transport.read_item().await? {
                    AttachmentTransportItem::Session(frame) => frame,
                    AttachmentTransportItem::Path(path) => {
                        check_initial_capacity(
                            pre_snapshot_paths.len() + pre_snapshot_states.len(),
                        )?;
                        pre_snapshot_paths.push(path);
                        continue;
                    }
                };
                if frame.kind == WireKind::ServiceErrorResponse {
                    if frame.request_id != request_id {
                        return Err(malformed("initial terminal error correlation mismatch"));
                    }
                    return Ok(Err(service_error(&frame)?));
                }
                if frame.kind == WireKind::TerminalTransportStateEvent {
                    if target.device_id().is_some() {
                        return Err(malformed(
                            "remote target Session stream carried a same-UID-only transport state",
                        ));
                    }
                    let state: v2::TerminalTransportStateEvent = frame
                        .decode_message(WireKind::TerminalTransportStateEvent)
                        .map_err(protocol_error)?;
                    v2::TerminalTransportState::try_from(state.state)
                        .map_err(|_| malformed("unknown terminal transport state"))?;
                    check_initial_capacity(pre_snapshot_paths.len() + pre_snapshot_states.len())?;
                    pre_snapshot_states.push(state);
                    continue;
                }
                if frame.request_id != request_id
                    || frame.kind != WireKind::TerminalSemanticSnapshot
                {
                    return Err(malformed("initial terminal snapshot correlation mismatch"));
                }
                let snapshot: v2::TerminalSemanticSnapshot = frame
                    .decode_message(WireKind::TerminalSemanticSnapshot)
                    .map_err(protocol_error)?;
                break voytty_proto::terminal_surface_snapshot_from_message(snapshot)
                    .map_err(protocol_error)?;
            };
            let (session_id, attachment_id, initial_snapshot) = initial_snapshot;
            let terminal_rows = u32::from(initial_snapshot.surface.size.rows);
            for state in &pre_snapshot_states {
                let state_attachment: AttachmentId = state
                    .attachment_id
                    .clone()
                    .ok_or_else(|| malformed("transport state omitted attachment_id"))?
                    .try_into()
                    .map_err(protocol_error)?;
                if state_attachment != attachment_id {
                    return Err(malformed("transport state attachment_id mismatch"));
                }
            }
            let pending_transport_events =
                changed_tunnel_path_events_after_unknown(attachment_id, pre_snapshot_paths);
            Ok(Ok(Self {
                reconnect_diagnostic: None,
                last_diagnostic_path: None,
                transport,
                write_error: None,
                takeover_deadline: None,
                deferred: VecDeque::new(),
                pending_transport_events,
                connector,
                session_id,
                attachment_id,
                target,
                resume_view_id,
                applied_revision: Arc::new(AtomicU64::new(initial_snapshot.revision.get())),
                latest_viewport: initial_snapshot.surface.size,
                reconnect_pending: false,
                reconnect_attempt: Default::default(),
                reconnect_retry: 0,
                retry_after: None,
                resize_after_ack: false,
                colors_after_ack: false,
                force_full_sync: false,
                latest_colors: base_colors,
                color_sequence: 0,
                initial_snapshot: Some(initial_snapshot),
                terminal_rows,
                next_request_id: request_id + 1,
                operation_lease: None,
                next_operation_sequence: 1,
                pending_takeover_request_id: None,
                pending_history_window: None,
                surface_frame_receipts: target.device_id().is_some(),
                surface_frames_received: 0,
                surface_frames_reported: 0,
            }))
        })
        .await;

            match response {
                Ok(Ok(result)) => {
                    if result.is_ok() {
                        progress.report(
                        voytty_core::connection_progress::ConnectionStage::TerminalSnapshotReceived,
                    );
                    }
                    result
                }
                Ok(Err(_)) if create_main => Err(create_main_outcome_unknown()),
                Ok(Err(error)) => Err(error),
                Err(_) if create_main => Err(create_main_outcome_unknown()),
                Err(_) => Err(DaemonError::new(
                    DomainErrorKind::DeadlineExceeded,
                    "timed out waiting for initial terminal snapshot",
                )),
            }
        }
        .await;
        let event = match &result {
            Ok(client) => Event::new(Kind::ConnectionCompleted)
                .outcome(Outcome::Success)
                .stage(Stage::Synchronizing)
                .session(client.session_id)
                .attachment(client.attachment_id),
            Err(error) => Event::new(Kind::ConnectionFailed)
                .error(error.kind())
                .level(if error.kind() == DomainErrorKind::Cancelled {
                    Level::Info
                } else {
                    Level::Warn
                })
                .outcome(if error.kind() == DomainErrorKind::Cancelled {
                    Outcome::Cancelled
                } else {
                    Outcome::Failed
                }),
        };
        voytty_diagnostics::record(event.operation(&operation).connection(operation.id()));
        result
    }
}
