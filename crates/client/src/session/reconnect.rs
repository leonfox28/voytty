//! Remote attachment reconnect state machine and backoff scheduling.

use std::sync::{Arc, atomic::Ordering};
use std::time::Duration;

use ring::rand::{SecureRandom, SystemRandom};
use voytty_core::terminal::TerminalColorProfile;
use voytty_core::{AttachmentId, DomainErrorKind, ResumeViewId};
use voytty_proto::{WireKind, encode_message, v2};

use super::{
    LocalAttachmentEvent, SessionClient, changed_tunnel_path_events_after_unknown,
    check_initial_capacity, is_retryable_remote_reconnect, reconnect_history_gap,
    terminal_transport_state,
};
use crate::error::ClientError as DaemonError;
use crate::protocol::{
    DEFAULT_DEADLINE, malformed, protocol_error, resolved_target_wire, resource_error,
    service_error,
};
use crate::transport::{AttachmentTransport, AttachmentTransportItem};

pub(super) struct ReconnectedAttachment {
    requested_viewport: voytty_core::terminal::TerminalSize,
    requested_colors: TerminalColorProfile,
    transport: AttachmentTransport,
    attachment_id: AttachmentId,
    initial: LocalAttachmentEvent,
    paths: Vec<v2::LocalSessionTunnelPath>,
    terminal_size: voytty_core::terminal::TerminalSize,
}

impl SessionClient {
    pub(super) async fn reconnect_remote(&mut self) -> Result<(), DaemonError> {
        let resume_view_id = self.resume_view_id.ok_or_else(|| {
            malformed("only a remote terminal view may reconnect through a tunnel")
        })?;
        loop {
            if let Some(deadline) = self.retry_after {
                tokio::time::sleep_until(deadline).await;
                self.retry_after = None;
            }
            if self.reconnect_attempt.is_empty() {
                self.start_reconnect_attempt(resume_view_id)?;
            }
            let result = self
                .reconnect_attempt
                .join_next()
                .await
                .expect("one remote reconnect attempt")
                .map_err(|_| malformed("remote reconnect worker ended unexpectedly"))?;
            match result {
                Ok(reconnected) => {
                    if let LocalAttachmentEvent::ResumeDelta(delta) = &reconnected.initial
                        && (self.force_full_sync
                            || delta.from_revision.get()
                                != self.applied_revision.load(Ordering::Acquire))
                    {
                        self.force_full_sync = true;
                        continue;
                    }
                    self.reconnect_retry = 0;
                    self.color_sequence = 0;
                    self.resize_after_ack = self.latest_viewport != reconnected.requested_viewport;
                    self.colors_after_ack = self.latest_colors != reconnected.requested_colors;
                    if let Some(operation) = self.reconnect_diagnostic.take() {
                        voytty_diagnostics::record(
                            voytty_diagnostics::Event::new(
                                voytty_diagnostics::Kind::ReconnectCompleted,
                            )
                            .session(self.session_id)
                            .attachment(reconnected.attachment_id)
                            .operation(&operation)
                            .stage(voytty_diagnostics::Stage::Synchronizing)
                            .outcome(voytty_diagnostics::Outcome::Success),
                        );
                    }
                    self.transport = reconnected.transport;
                    self.reset_surface_frame_receipts();
                    self.write_error = None;
                    self.takeover_deadline = None;
                    self.attachment_id = reconnected.attachment_id;
                    self.terminal_rows = u32::from(reconnected.terminal_size.rows);
                    if !self.resize_after_ack {
                        self.latest_viewport = reconnected.terminal_size;
                    }
                    self.deferred.clear();
                    self.operation_lease = None;
                    self.next_operation_sequence = 1;
                    self.pending_history_window = None;
                    self.pending_takeover_request_id = None;
                    self.reconnect_pending = false;
                    self.force_full_sync = false;
                    self.pending_transport_events
                        .push_back(LocalAttachmentEvent::TransportState(
                            terminal_transport_state(
                                self.attachment_id,
                                v2::TerminalTransportState::Synchronizing,
                            ),
                        ));
                    self.pending_transport_events.push_back(
                        LocalAttachmentEvent::ConnectionStatus(v2::TerminalConnectionStatusEvent {
                            attachment_id: Some(self.attachment_id.into()),
                            path: v2::TerminalConnectionPath::Unknown as i32,
                            rtt_ms: None,
                        }),
                    );
                    self.pending_transport_events
                        .extend(changed_tunnel_path_events_after_unknown(
                            self.attachment_id,
                            reconnected.paths,
                        ));
                    self.pending_transport_events.push_back(reconnected.initial);
                    return Ok(());
                }
                Err(error) if is_retryable_remote_reconnect(error.kind()) => {
                    let base_ms = (250u64 << self.reconnect_retry.min(6)).min(8_000);
                    let mut jitter = [0_u8; 1];
                    let _ = SystemRandom::new().fill(&mut jitter);
                    let delay =
                        Duration::from_millis(base_ms + base_ms * u64::from(jitter[0]) / 1024);
                    self.retry_after = Some(tokio::time::Instant::now() + delay);
                    self.reconnect_retry = self.reconnect_retry.saturating_add(1);
                }
                Err(error) => {
                    if let Some(operation) = &self.reconnect_diagnostic {
                        voytty_diagnostics::record(
                            voytty_diagnostics::Event::new(
                                voytty_diagnostics::Kind::ReconnectFailed,
                            )
                            .session(self.session_id)
                            .operation(operation)
                            .level(voytty_diagnostics::Level::Warn)
                            .error(error.kind()),
                        );
                    }
                    return Err(error);
                }
            }
        }
    }

    fn start_reconnect_attempt(&mut self, resume_view_id: ResumeViewId) -> Result<(), DaemonError> {
        let request_id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| resource_error("terminal reconnect request ID exhausted"))?;
        let requested_colors = self.latest_colors.clone();
        let requested_viewport = self.latest_viewport;
        let request = v2::TerminalAttachRequest {
            accept_zlib_frames: true,
            acknowledge_surface_frames: true,
            base_colors: Some(requested_colors.clone().into()),
            target: Some(resolved_target_wire(self.target)),
            session_id: Some(self.session_id.into()),
            takeover: false,
            session_name: String::new(),
            create_main: false,
            viewport: Some(requested_viewport.into()),
            resume_view_id: Some(resume_view_id.into()),
            known_revision: (!self.force_full_sync)
                .then(|| self.applied_revision.load(Ordering::Acquire)),
        };
        let bytes = encode_message(
            WireKind::TerminalAttachRequest,
            request_id,
            u32::try_from(DEFAULT_DEADLINE.as_millis()).unwrap_or(u32::MAX),
            &request,
        )
        .map_err(protocol_error)?;
        let connector = Arc::clone(&self.connector);
        let target = self.target;
        let expected_session = self.session_id;
        let known_revision = request.known_revision;
        self.reconnect_attempt.spawn(async move {
            let mut transport = tokio::time::timeout_at(
                crate::protocol::startup_deadline(target),
                connector.open(target),
            )
            .await
            .map_err(|_| crate::protocol::control_timeout())??;
            let deadline = crate::protocol::control_deadline();
            transport.write_until(&bytes, deadline).await?;

            tokio::time::timeout_at(deadline, async {
                let mut paths = Vec::new();
                loop {
                    let frame = match transport.read_item().await? {
                        AttachmentTransportItem::Path(path) => {
                            check_initial_capacity(paths.len())?;
                            paths.push(path);
                            continue;
                        }
                        AttachmentTransportItem::Session(frame) => frame,
                    };
                    if frame.request_id != request_id {
                        return Err(malformed(
                            "remote resume initial update correlation mismatch",
                        ));
                    }
                    if frame.kind == WireKind::ServiceErrorResponse {
                        return Err(service_error(&frame)?);
                    }
                    let (attachment_id, initial, terminal_size) = match frame.kind {
                        WireKind::TerminalSemanticSnapshot => {
                            let snapshot: v2::TerminalSemanticSnapshot = frame
                                .decode_message(WireKind::TerminalSemanticSnapshot)
                                .map_err(protocol_error)?;
                            let (session_id, attachment_id, snapshot) =
                                voytty_proto::terminal_surface_snapshot_from_message(snapshot)
                                    .map_err(protocol_error)?;
                            if session_id != expected_session {
                                return Err(malformed(
                                    "remote resume snapshot changed the frozen session identity",
                                ));
                            }
                            let size = snapshot.surface.size;
                            (
                                attachment_id,
                                LocalAttachmentEvent::Snapshot(snapshot),
                                size,
                            )
                        }
                        WireKind::TerminalSemanticDelta => {
                            let delta: v2::TerminalSemanticDelta = frame
                                .decode_message(WireKind::TerminalSemanticDelta)
                                .map_err(protocol_error)?;
                            let (attachment_id, delta) =
                                voytty_proto::terminal_surface_delta_from_message(delta)
                                    .map_err(protocol_error)?;
                            if Some(delta.from_revision.get()) != known_revision {
                                return Err(malformed(
                                    "remote resume delta does not continue the applied revision",
                                ));
                            }
                            let size = delta.size;
                            (
                                attachment_id,
                                LocalAttachmentEvent::ResumeDelta(delta),
                                size,
                            )
                        }
                        _ => {
                            return Err(malformed(
                                "remote resume began with an invalid Session update",
                            ));
                        }
                    };
                    return Ok(ReconnectedAttachment {
                        requested_colors,
                        requested_viewport,
                        transport,
                        attachment_id,
                        initial,
                        paths,
                        terminal_size,
                    });
                }
            })
            .await
            .map_err(|_| {
                DaemonError::new(
                    DomainErrorKind::DeadlineExceeded,
                    "timed out waiting for remote resume state",
                )
            })?
        });
        Ok(())
    }

    pub(super) fn enter_reconnecting(&mut self) {
        if self.reconnect_pending {
            return;
        }
        let diagnostic = voytty_diagnostics::Operation::default();
        voytty_diagnostics::record(
            voytty_diagnostics::Event::new(voytty_diagnostics::Kind::ReconnectStarted)
                .session(self.session_id)
                .attachment(self.attachment_id)
                .operation(&diagnostic),
        );
        self.reconnect_diagnostic = Some(diagnostic);
        self.last_diagnostic_path = None;
        self.transport.invalidate();
        self.reset_surface_frame_receipts();
        self.write_error = Some(crate::protocol::attachment_cancelled());
        let history_gap = self.pending_history_window.take().map(|(_, query)| {
            reconnect_history_gap(query, self.applied_revision.load(Ordering::Acquire))
        });
        self.reconnect_pending = true;
        self.pending_transport_events.clear();
        self.deferred.clear();
        self.operation_lease = None;
        self.next_operation_sequence = 1;
        self.pending_takeover_request_id = None;
        if let Some(gap) = history_gap {
            self.pending_transport_events
                .push_back(LocalAttachmentEvent::HistoryWindow(gap));
        }
        self.pending_transport_events
            .push_back(LocalAttachmentEvent::TransportState(
                terminal_transport_state(
                    self.attachment_id,
                    v2::TerminalTransportState::Reconnecting,
                ),
            ));
    }
}
