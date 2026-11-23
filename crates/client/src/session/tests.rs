use super::*;
use crate::protocol::DEFAULT_DEADLINE;
use voytty_core::{DeviceId, SessionSelector};
use voytty_proto::FrameDecoder;

impl SessionClient {
    #[cfg(test)]
    pub(crate) fn negotiate_surface_frame_receipts_for_test(&mut self) {
        self.surface_frame_receipts = true;
    }

    #[cfg(test)]
    pub(crate) fn terminal_driver_test_pair(
        target: ResolvedSessionTarget,
        session_id: SessionId,
        attachment_id: AttachmentId,
    ) -> (Self, TestStream) {
        let (stream, peer) =
            TestStream::pair().expect("test-private terminal driver Unix stream pair");
        (
            Self {
                color_sequence: Default::default(),
                latest_colors: Default::default(),

                reconnect_diagnostic: None,
                last_diagnostic_path: None,
                transport: AttachmentTransport::new(UnixAttachmentTransport::Direct {
                    stream,
                    decoder: FrameDecoder::new(),
                    queued: VecDeque::new(),
                }),
                write_error: None,
                takeover_deadline: None,
                deferred: VecDeque::new(),
                pending_transport_events: VecDeque::new(),
                connector: Arc::new(UnixAttachmentConnector::new(PathBuf::new())),
                session_id,
                attachment_id,
                target,
                resume_view_id: None,
                applied_revision: Arc::new(AtomicU64::new(1)),
                latest_viewport: voytty_core::terminal::TerminalSize::new(24, 80),
                reconnect_pending: false,
                reconnect_attempt: Default::default(),
                reconnect_retry: 0,
                retry_after: None,
                resize_after_ack: false,
                colors_after_ack: false,
                force_full_sync: false,
                initial_snapshot: Some(TerminalSurfaceSnapshot {
                    revision: Revision::new(1),
                    surface: voytty_core::terminal::TerminalSurface {
                        application_title: String::new(),
                        colors: Default::default(),

                        size: voytty_core::terminal::TerminalSize::new(24, 80),
                        active_screen: voytty_core::terminal::ActiveScreen::Main,
                        rows: (0..24)
                            .map(|_| voytty_core::terminal::TerminalSurfaceRow {
                                cells: vec![voytty_core::terminal::TerminalCell::default(); 80],
                                wrapped: false,
                            })
                            .collect(),
                        cursor: voytty_core::terminal::TerminalCursor {
                            presentation: Default::default(),
                            row: 0,
                            column: 0,
                            visible: true,
                            style: voytty_core::terminal::TerminalStyle::default(),
                        },
                        modes: voytty_core::terminal::TerminalModes::default(),
                        scroll_metrics: Some(voytty_core::terminal::TerminalScrollMetrics {
                            epoch: Revision::ZERO,
                            revision: Revision::new(1),
                            offset_from_bottom: 0,
                            max_offset_from_bottom: 0,
                            viewport_rows: 24,
                        }),
                    },
                }),
                terminal_rows: 24,
                next_request_id: 2,
                operation_lease: None,
                next_operation_sequence: 1,
                pending_takeover_request_id: None,
                pending_history_window: None,
                surface_frame_receipts: false,
                surface_frames_received: 0,
                surface_frames_reported: 0,
            },
            peer,
        )
    }
}

#[cfg(test)]
use crate::desktop_transport::{
    RemoteDaemonRestarter, UnixAttachmentConnector, UnixAttachmentTransport,
};
#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::{future::Future, pin::Pin};
#[cfg(test)]
impl SessionClient {
    /// Opens one already-resolved local or remote view for the high-level
    /// command runtime without exposing the socket or target token to the CLI.
    #[cfg(test)]
    pub async fn connect_resolved(
        socket: impl AsRef<Path>,
        target: ResolvedSessionTarget,
        selector: Option<SessionSelector>,
        create_main: bool,
        takeover: bool,
        viewport: Option<voytty_core::terminal::TerminalSize>,
    ) -> Result<Self, DaemonError> {
        Self::connect_inner(
            socket.as_ref(),
            target,
            selector,
            create_main,
            takeover,
            viewport,
            TerminalColorProfile::default(),
        )
        .await
    }

    /// Attaches to the daemon-lifetime default `main` session, creating it if absent.
    pub async fn connect_main(
        socket: impl AsRef<Path>,
        viewport: Option<voytty_core::terminal::TerminalSize>,
    ) -> Result<Self, DaemonError> {
        Self::connect_inner(
            socket.as_ref(),
            ResolvedSessionTarget::local(),
            None,
            true,
            false,
            viewport,
            TerminalColorProfile::default(),
        )
        .await
    }

    /// Attaches to an existing session selected by stable ID or exact name.
    pub async fn connect_session(
        socket: impl AsRef<Path>,
        selector: SessionSelector,
        takeover: bool,
        viewport: Option<voytty_core::terminal::TerminalSize>,
    ) -> Result<Self, DaemonError> {
        Self::connect_inner(
            socket.as_ref(),
            ResolvedSessionTarget::local(),
            Some(selector),
            false,
            takeover,
            viewport,
            TerminalColorProfile::default(),
        )
        .await
    }

    /// Opens one frontend-owned remote default view through an opaque daemon tunnel.
    #[doc(hidden)]
    pub async fn connect_remote_main(
        socket: impl AsRef<Path>,
        target: DeviceId,
        viewport: Option<voytty_core::terminal::TerminalSize>,
    ) -> Result<Self, DaemonError> {
        Self::connect_inner(
            socket.as_ref(),
            ResolvedSessionTarget::device(target),
            None,
            true,
            false,
            viewport,
            TerminalColorProfile::default(),
        )
        .await
    }

    /// Opens one frontend-owned remote named/ID view through an opaque daemon tunnel.
    #[doc(hidden)]
    pub async fn connect_remote_session(
        socket: impl AsRef<Path>,
        target: DeviceId,
        selector: SessionSelector,
        takeover: bool,
        viewport: Option<voytty_core::terminal::TerminalSize>,
    ) -> Result<Self, DaemonError> {
        Self::connect_inner(
            socket.as_ref(),
            ResolvedSessionTarget::device(target),
            Some(selector),
            false,
            takeover,
            viewport,
            TerminalColorProfile::default(),
        )
        .await
    }

    pub async fn connect_resolved_with_colors(
        socket: impl AsRef<Path>,
        target: ResolvedSessionTarget,
        selector: Option<SessionSelector>,
        create_main: bool,
        takeover: bool,
        viewport: Option<voytty_core::terminal::TerminalSize>,
        base_colors: TerminalColorProfile,
    ) -> Result<Self, DaemonError> {
        Self::connect_inner(
            socket.as_ref(),
            target,
            selector,
            create_main,
            takeover,
            viewport,
            base_colors,
        )
        .await
    }

    async fn connect_inner(
        socket: &Path,
        target: ResolvedSessionTarget,
        selector: Option<SessionSelector>,
        create_main: bool,
        takeover: bool,
        viewport: Option<voytty_core::terminal::TerminalSize>,
        base_colors: TerminalColorProfile,
    ) -> Result<Self, DaemonError> {
        Self::connect(
            Arc::new(UnixAttachmentConnector::new(socket)),
            target,
            selector,
            create_main,
            takeover,
            viewport,
            base_colors,
        )
        .await
    }
}

use crate::desktop_transport::{read_frame_parts, read_tunnel_first};
use crate::framing::read_first;
use crate::protocol::resolved_target_from_wire;
use std::sync::Mutex as StdMutex;
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn detach_waits_for_host_stream_end_before_reporting_completion() {
    use tokio::io::AsyncReadExt;
    let attachment_id = AttachmentId::from_array([2; 16]);
    let (mut client, mut peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(DeviceId::from_array([3; 32])),
        SessionId::from_array([1; 16]),
        attachment_id,
    );
    let mut detached = tokio::spawn(async move { client.detach().await });
    let first = read_first(&mut peer).await.expect("explicit detach frame");
    let detach: v2::TerminalDetach = first
        .frame
        .decode_message(WireKind::TerminalDetach)
        .expect("terminal detach");
    assert_eq!(detach.attachment_id, Some(attachment_id.into()));
    let mut remaining = [0_u8; 1];
    assert_eq!(peer.read(&mut remaining).await.expect("client FIN"), 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut detached)
            .await
            .is_err(),
        "finishing the client write half does not prove host release"
    );
    peer.shutdown().await.expect("host finishes after release");
    tokio::time::timeout(Duration::from_secs(1), detached)
        .await
        .expect("host completion unblocks detach")
        .expect("detach task")
        .expect("completed detach");
}

#[tokio::test]
async fn detach_preserves_host_rejection_instead_of_accepting_write_finish() {
    let (mut client, mut peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        SessionId::from_array([1; 16]),
        AttachmentId::from_array([2; 16]),
    );
    let detached = tokio::spawn(async move { client.detach().await });
    let first = read_first(&mut peer).await.expect("detach request");
    peer.write_all(
        &encode_message(
            WireKind::ServiceErrorResponse,
            first.frame.request_id,
            0,
            &v2::ServiceError {
                code: DomainErrorKind::Unauthorized.code().to_owned(),
                message: "authorization revoked".to_owned(),
            },
        )
        .expect("typed detach rejection"),
    )
    .await
    .expect("deliver rejection before host end");
    let error = detached
        .await
        .expect("detach task")
        .expect_err("a finished write half cannot mask the host failure");
    assert_eq!(error.kind(), DomainErrorKind::Unauthorized);
}

#[tokio::test]
async fn detach_host_end_wait_keeps_the_existing_control_deadline() {
    let (mut client, mut peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        SessionId::from_array([1; 16]),
        AttachmentId::from_array([2; 16]),
    );
    let detached = tokio::spawn(async move { client.detach().await });
    let first = read_first(&mut peer).await.expect("detach request");
    assert_eq!(first.frame.kind, WireKind::TerminalDetach);
    let error = tokio::time::timeout(DEFAULT_DEADLINE + Duration::from_secs(1), detached)
        .await
        .expect("detach stays bounded without a host end")
        .expect("detach task")
        .expect_err("no host completion was received");
    assert_eq!(error.kind(), DomainErrorKind::DeadlineExceeded);
    drop(peer);
}

#[test]
fn takeover_token_debug_redacts_operation_and_session_identity() {
    let retry_owner = LocalTakeoverRetryToken {
        operation_id: OperationId {
            lease: OperationLease {
                daemon_incarnation: voytty_core::DaemonIncarnation::from_array(
                    *b"TAKEOVER_TOKEN_1",
                ),
                ordinal: 8_675_309,
            },
            sequence: 2_434_117,
        },
        session_id: SessionId::from_array(*b"SESSION_TOKEN_01"),
    };
    assert_eq!(
        format!("{retry_owner:?}"),
        "LocalTakeoverRetryToken([REDACTED])"
    );
}

#[tokio::test]
async fn lease_wait_preserves_retirement_before_eof() {
    use tokio::io::AsyncReadExt;
    let attachment_id = AttachmentId::from_array([2; 16]);
    let (mut client, mut peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(DeviceId::from_array([3; 32])),
        SessionId::from_array([1; 16]),
        attachment_id,
    );
    let server = tokio::spawn(async move {
        let request = read_first(&mut peer).await.expect("lease request");
        assert_eq!(request.frame.kind, WireKind::SessionOperationLeaseRequest);
        assert!(request.queued.is_empty());
        let retired = encode_message(
            WireKind::TerminalLeaseLost,
            0,
            0,
            &v2::TerminalLeaseLost {
                attachment_id: Some(attachment_id.into()),
                generation: 2,
            },
        )
        .expect("authoritative retirement");
        peer.write_all(&retired).await.expect("send retirement");
        peer.shutdown().await.expect("end retired attachment");
        let mut remaining = Vec::new();
        peer.read_to_end(&mut remaining)
            .await
            .expect("client releases retired stream");
        assert!(
            remaining.is_empty(),
            "no takeover may follow a retired attachment"
        );
    });
    let error = client
        .begin_takeover()
        .await
        .expect_err("retired before obtaining an operation lease");
    drop(client);
    server.await.expect("retirement server");
    assert_eq!(error.kind(), DomainErrorKind::LeaseLost);
}

#[tokio::test]
async fn lease_wait_never_submits_takeover_after_retirement_and_late_reply() {
    let attachment_id = AttachmentId::from_array([2; 16]);
    let (mut client, mut peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(DeviceId::from_array([3; 32])),
        SessionId::from_array([1; 16]),
        attachment_id,
    );
    let server = tokio::spawn(async move {
        let request = read_first(&mut peer).await.expect("lease request");
        assert_eq!(request.frame.kind, WireKind::SessionOperationLeaseRequest);
        let mut bytes = encode_message(
            WireKind::TerminalLeaseLost,
            0,
            0,
            &v2::TerminalLeaseLost {
                attachment_id: Some(attachment_id.into()),
                generation: 2,
            },
        )
        .expect("retirement");
        bytes.extend(
            encode_message(
                WireKind::SessionOperationLeaseResponse,
                request.frame.request_id,
                0,
                &v2::SessionOperationLeaseResponse {
                    lease: Some(
                        OperationLease {
                            daemon_incarnation: voytty_core::DaemonIncarnation::from_array([4; 16]),
                            ordinal: 1,
                        }
                        .into(),
                    ),
                },
            )
            .expect("late lease reply"),
        );
        peer.write_all(&bytes)
            .await
            .expect("coalesced retirement and lease");
        tokio::time::timeout(Duration::from_secs(1), read_first(&mut peer))
            .await
            .expect("retired stream releases promptly")
            .ok()
            .map(|first| first.frame.kind)
    });
    let error = client
        .begin_takeover()
        .await
        .expect_err("retirement precedes late lease");
    assert_eq!(error.kind(), DomainErrorKind::LeaseLost);
    assert!(client.operation_lease.is_none());
    assert!(client.pending_takeover_request_id.is_none());
    assert!(!client.reconnect_pending());
    assert_eq!(
        client
            .write_input(b"retired input".to_vec())
            .await
            .expect_err("input remains fenced")
            .kind(),
        DomainErrorKind::LeaseLost
    );
    drop(client);
    assert_eq!(
        server.await.expect("late lease server"),
        None,
        "no mutation follows retirement"
    );
}

#[tokio::test]
async fn lease_wait_validates_retirement_identity_and_header() {
    for (attachment, request_id, deadline_ms) in [
        (None, 0, 0),
        (Some(AttachmentId::from_array([9; 16])), 0, 0),
        (Some(AttachmentId::from_array([2; 16])), 2, 0),
        (Some(AttachmentId::from_array([2; 16])), 0, 100),
    ] {
        let (mut client, mut peer) = SessionClient::terminal_driver_test_pair(
            ResolvedSessionTarget::local(),
            SessionId::from_array([1; 16]),
            AttachmentId::from_array([2; 16]),
        );
        let server = tokio::spawn(async move {
            read_first(&mut peer).await.expect("lease request");
            peer.write_all(
                &encode_message(
                    WireKind::TerminalLeaseLost,
                    request_id,
                    deadline_ms,
                    &v2::TerminalLeaseLost {
                        attachment_id: attachment.map(Into::into),
                        generation: 2,
                    },
                )
                .expect("invalid retirement fixture"),
            )
            .await
            .expect("send invalid retirement");
        });
        let error = client
            .begin_takeover()
            .await
            .expect_err("invalid retirement rejected");
        server.await.expect("invalid retirement server");
        assert_eq!(error.kind(), DomainErrorKind::MalformedFrame);
        assert!(client.pending_takeover_request_id.is_none());
    }
}

#[tokio::test(start_paused = true)]
async fn silent_lease_expires_but_an_idle_attachment_has_no_control_deadline() {
    let (mut client, peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        SessionId::from_array([1; 16]),
        AttachmentId::from_array([2; 16]),
    );
    let started = tokio::time::Instant::now();
    assert_eq!(
        client
            .begin_takeover()
            .await
            .expect_err("silent lease must expire")
            .kind(),
        DomainErrorKind::DeadlineExceeded
    );
    assert_eq!(tokio::time::Instant::now() - started, DEFAULT_DEADLINE);
    assert!(matches!(client.transport, AttachmentTransport::Closed));
    assert!(
        client.pending_takeover_request_id.is_none(),
        "no takeover was submitted"
    );
    drop(peer);

    let (mut idle, _peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        SessionId::from_array([1; 16]),
        AttachmentId::from_array([2; 16]),
    );
    assert!(
        tokio::time::timeout(DEFAULT_DEADLINE * 2, idle.read_next_event())
            .await
            .is_err()
    );
    assert!(!matches!(idle.transport, AttachmentTransport::Closed));
}

#[tokio::test]
async fn lease_wait_bounds_unrelated_frames_by_count_and_bytes() {
    for (count, payload_bytes) in [(9, 16), (2, 5 * 1024 * 1024)] {
        let (mut client, mut peer) = SessionClient::terminal_driver_test_pair(
            ResolvedSessionTarget::local(),
            SessionId::from_array([1; 16]),
            AttachmentId::from_array([2; 16]),
        );
        let sender = tokio::spawn(async move {
            // The lease waiter must bound envelopes before retaining or interpreting
            // any unsolicited content, including invalid content from a faulty peer.
            let frame = encode_message(
                WireKind::TerminalSemanticSnapshot,
                0,
                0,
                &v2::TerminalInput {
                    attachment_id: None,
                    bytes: vec![b'x'; payload_bytes],
                    ..v2::TerminalInput::default()
                },
            )
            .expect("bounded content envelope");
            for _ in 0..count {
                if peer.write_all(&frame).await.is_err() {
                    break;
                }
            }
            peer
        });
        let error = client
            .begin_takeover()
            .await
            .expect_err("unrelated content must stay bounded");
        assert_eq!(error.kind(), DomainErrorKind::ResourceExhausted);
        assert!(matches!(client.transport, AttachmentTransport::Closed));
        assert!(client.deferred.is_empty());
        drop(sender.await.expect("bounded sender"));
    }
}

#[tokio::test(start_paused = true)]
async fn blocked_partial_write_expires_and_its_epoch_is_never_reused() {
    use tokio::io::AsyncReadExt;
    let (mut client, mut peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        SessionId::from_array([1; 16]),
        AttachmentId::from_array([2; 16]),
    );
    let error = client
        .write_input(vec![b'x'; 900_000])
        .await
        .expect_err("non-reading peer must not hold the owner");
    assert_eq!(error.kind(), DomainErrorKind::DeadlineExceeded);
    assert!(matches!(client.transport, AttachmentTransport::Closed));
    assert_eq!(
        client
            .write_input(b"later".to_vec())
            .await
            .expect_err("failed epoch cannot write again"),
        error
    );
    let mut received = Vec::new();
    peer.read_to_end(&mut received)
        .await
        .expect("timed-out epoch releases socket");
    assert!(!received.is_empty());
    assert!(
        received.len() < 900_000,
        "fixture must interrupt a partial frame"
    );
    let mut decoder = FrameDecoder::new();
    assert!(
        decoder
            .feed(&received)
            .expect("valid frame prefix")
            .is_empty()
    );
    assert!(
        decoder.finish().is_err(),
        "no later command repairs or follows the partial frame"
    );
}

#[tokio::test(start_paused = true)]
async fn unanswered_sent_takeover_expires_as_unknown_without_replay() {
    let (mut client, _peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        SessionId::from_array([1; 16]),
        AttachmentId::from_array([2; 16]),
    );
    client.operation_lease = Some(OperationLease {
        daemon_incarnation: voytty_core::DaemonIncarnation::from_array([3; 16]),
        ordinal: 1,
    });
    client
        .begin_takeover()
        .await
        .expect("takeover was submitted");
    assert_eq!(
        client
            .read_next_event()
            .await
            .expect_err("missing takeover response is ambiguous")
            .kind(),
        DomainErrorKind::OperationOutcomeUnknown
    );
    assert!(matches!(client.transport, AttachmentTransport::Closed));
    assert!(!client.reconnect_pending());
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_partial_write_drops_the_epoch_before_another_command() {
    use tokio::io::AsyncReadExt;
    let (mut client, mut peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        SessionId::from_array([1; 16]),
        AttachmentId::from_array([2; 16]),
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(10),
            client.write_input(vec![b'x'; 900_000])
        )
        .await
        .is_err()
    );
    assert!(matches!(client.transport, AttachmentTransport::Closed));
    assert!(client.write_input(b"later".to_vec()).await.is_err());
    let mut bytes = Vec::new();
    peer.read_to_end(&mut bytes)
        .await
        .expect("cancelled write releases socket");
    assert!(!bytes.is_empty() && bytes.len() < 900_000);
}

#[tokio::test]
async fn lease_wait_also_bounds_tunnel_path_sidebands() {
    let (mut client, peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(DeviceId::from_array([3; 32])),
        SessionId::from_array([1; 16]),
        AttachmentId::from_array([2; 16]),
    );
    let (stream, replacement_peer) = TestStream::pair().expect("tunnel sideband fixture");
    drop(peer);
    let mut peer = replacement_peer;
    client.transport = tunnel_transport(stream);
    for rtt_ms in 0..9 {
        peer.write_all(&tunnel_envelope(
            WireKind::LocalSessionTunnelPath,
            &v2::LocalSessionTunnelPath {
                path: v2::TerminalConnectionPath::Direct as i32,
                rtt_ms: Some(rtt_ms),
            },
        ))
        .await
        .expect("bounded sideband fixture");
    }
    assert_eq!(
        client
            .begin_takeover()
            .await
            .expect_err("sidebands must not grow while awaiting lease")
            .kind(),
        DomainErrorKind::ResourceExhausted
    );
    assert!(matches!(client.transport, AttachmentTransport::Closed));
    assert!(client.pending_transport_events.is_empty());
}

struct FailingRemoteDaemonRestarter {
    calls: Arc<AtomicU64>,
}

#[tokio::test(start_paused = true)]
async fn occupied_resume_stops_instead_of_reclaiming_after_the_new_controller_detaches() {
    struct OccupiedConnector(Arc<AtomicU64>);
    impl AttachmentConnector for OccupiedConnector {
        fn open(
            &self,
            _target: ResolvedSessionTarget,
        ) -> crate::transport::TransportFuture<'_, Result<AttachmentTransport, DaemonError>>
        {
            self.0.fetch_add(1, Ordering::AcqRel);
            Box::pin(async {
                Err(DaemonError::new(
                    DomainErrorKind::SessionOccupied,
                    "another view owns the session",
                ))
            })
        }
    }
    let (mut client, old_peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(DeviceId::from_array([0xda; 32])),
        SessionId::from_array([0xdb; 16]),
        AttachmentId::from_array([0xdc; 16]),
    );
    drop(old_peer);
    let attempts = Arc::new(AtomicU64::new(0));
    client.connector = Arc::new(OccupiedConnector(Arc::clone(&attempts)));
    client.resume_view_id = Some(ResumeViewId::from_array([0xdd; 16]));
    client.enter_reconnecting();
    let error = tokio::time::timeout(Duration::from_secs(1), client.reconnect_remote())
        .await
        .expect("a different controller is a definitive recovery outcome")
        .expect_err("resume cannot replace its controller");
    assert_eq!(error.kind(), DomainErrorKind::SessionOccupied);
    assert_eq!(
        attempts.load(Ordering::Acquire),
        1,
        "no background occupied retry remains"
    );
    assert_eq!(client.session_id, SessionId::from_array([0xdb; 16]));
}

impl RemoteDaemonRestarter for FailingRemoteDaemonRestarter {
    fn ensure_running<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), DaemonError>> + Send + 'a>> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        Box::pin(async {
            Err(DaemonError::new(
                DomainErrorKind::DaemonStartTimeout,
                "injected viewer-daemon restart failure",
            ))
        })
    }
}

struct AcceptingRemoteDaemonRestarter {
    socket: PathBuf,
    target: DeviceId,
    new_attachment: AttachmentId,
    calls: Arc<AtomicU64>,
    server: StdMutex<Option<tokio::task::JoinHandle<v2::TerminalAttachRequest>>>,
}

impl RemoteDaemonRestarter for AcceptingRemoteDaemonRestarter {
    fn ensure_running<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), DaemonError>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::AcqRel);
            let listener = TestListener::bind(&self.socket).map_err(|error| {
                DaemonError::new(
                    DomainErrorKind::DaemonStartTimeout,
                    format!("unable to bind replacement viewer-daemon fixture: {error}"),
                )
            })?;
            let target = self.target;
            let new_attachment = self.new_attachment;
            let task = tokio::spawn(async move {
                serve_replacement_tunnel_once(listener, target, new_attachment).await
            });
            let mut server = self.server.lock().map_err(|_| {
                DaemonError::new(
                    DomainErrorKind::IdentityStateMismatch,
                    "replacement viewer-daemon fixture task lock is poisoned",
                )
            })?;
            if server.replace(task).is_some() {
                return Err(DaemonError::new(
                    DomainErrorKind::IdentityStateMismatch,
                    "replacement viewer-daemon fixture was started more than once",
                ));
            }
            Ok(())
        })
    }
}

struct DelayedStartup {
    setup: Duration,
    response: Duration,
    writes: Arc<AtomicU64>,
}

impl AttachmentConnector for DelayedStartup {
    fn open(
        &self,
        _target: ResolvedSessionTarget,
    ) -> crate::transport::TransportFuture<'_, Result<AttachmentTransport, DaemonError>> {
        Box::pin(async move {
            tokio::time::sleep(self.setup).await;
            Ok(AttachmentTransport::new(DelayedSnapshot {
                response: self.response,
                writes: self.writes.clone(),
            }))
        })
    }
}

struct DelayedSnapshot {
    response: Duration,
    writes: Arc<AtomicU64>,
}

impl crate::transport::AttachmentIo for DelayedSnapshot {
    fn queued_session_count(&self) -> usize {
        0
    }
    fn write<'a>(
        &'a mut self,
        bytes: &'a [u8],
    ) -> crate::transport::TransportFuture<'a, Result<(), DaemonError>> {
        Box::pin(async move {
            let frame = FrameDecoder::new()
                .feed(bytes)
                .expect("attach frame")
                .remove(0);
            assert_eq!(
                frame.deadline_ms, 5_000,
                "host operation budget stays bounded"
            );
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn read(
        &mut self,
    ) -> crate::transport::TransportFuture<'_, Result<AttachmentTransportItem, DaemonError>> {
        Box::pin(async move {
            tokio::time::sleep(self.response).await;
            let bytes = encode_message(
                WireKind::TerminalSemanticSnapshot,
                1,
                0,
                &semantic_snapshot(
                    SessionId::from_array([1; 16]),
                    AttachmentId::from_array([2; 16]),
                ),
            )
            .expect("snapshot");
            Ok(AttachmentTransportItem::Session(
                FrameDecoder::new()
                    .feed(&bytes)
                    .expect("snapshot frame")
                    .remove(0),
            ))
        })
    }
    fn shutdown(&mut self) -> crate::transport::TransportFuture<'_, Result<(), DaemonError>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test(start_paused = true)]
async fn remote_resume_survives_cancelled_reads_during_slow_setup_and_attach() {
    let (mut client, _peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(DeviceId::from_array([7; 32])),
        SessionId::from_array([1; 16]),
        AttachmentId::from_array([3; 16]),
    );
    let writes = Arc::new(AtomicU64::new(0));
    client.connector = Arc::new(DelayedStartup {
        setup: Duration::from_secs(7),
        response: Duration::from_secs(4),
        writes: Arc::clone(&writes),
    });
    client.resume_view_id = Some(ResumeViewId::from_array([4; 16]));
    client.next_request_id = 1;
    client.enter_reconnecting();
    let started = tokio::time::Instant::now();
    for _ in 0..9 {
        assert!(
            tokio::time::timeout(Duration::from_secs(1), client.reconnect_remote())
                .await
                .is_err()
        );
        client
            .resize(voytty_core::terminal::TerminalSize::new(40, 100))
            .await
            .expect("retain resize");
    }
    tokio::time::timeout(Duration::from_secs(3), client.reconnect_remote())
        .await
        .expect("original resume progresses")
        .expect("resumed");
    assert_eq!(started.elapsed(), Duration::from_secs(11));
    assert_eq!(writes.load(Ordering::Acquire), 1);
    assert!(client.resize_after_ack);
    assert_eq!(
        client.latest_viewport,
        voytty_core::terminal::TerminalSize::new(40, 100)
    );
}

#[tokio::test(start_paused = true)]
async fn startup_budget_allows_slow_remote_setup_but_caps_total_and_ready_operations() {
    let remote = ResolvedSessionTarget::device(DeviceId::from_array([7; 32]));
    for (target, setup, response, expected_seconds, succeeds, expected_writes) in [
        (remote, 6, 1, 7, true, 1),
        (remote, 6, 4, 10, true, 1),
        (remote, 20, 0, 15, false, 0),
        (remote, 12, 4, 15, false, 1),
        (remote, 0, 6, 5, false, 1),
        (ResolvedSessionTarget::local(), 6, 0, 5, false, 0),
    ] {
        let writes = Arc::new(AtomicU64::new(0));
        let connector = Arc::new(DelayedStartup {
            setup: Duration::from_secs(setup),
            response: Duration::from_secs(response),
            writes: writes.clone(),
        });
        let started = tokio::time::Instant::now();
        let result = SessionClient::connect(
            connector,
            target,
            None,
            false,
            false,
            None,
            Default::default(),
        )
        .await;
        assert_eq!(
            result.is_ok(),
            succeeds,
            "setup={setup}, response={response}"
        );
        if let Err(error) = result {
            assert_eq!(error.kind(), DomainErrorKind::DeadlineExceeded);
        }
        assert!(
            started
                .elapsed()
                .abs_diff(Duration::from_secs(expected_seconds))
                < Duration::from_millis(10)
        );
        assert_eq!(writes.load(Ordering::SeqCst), expected_writes);
    }
}

#[tokio::test(start_paused = true)]
async fn startup_budget_cancellation_after_five_seconds_never_submits_an_attach() {
    let writes = Arc::new(AtomicU64::new(0));
    let connector = Arc::new(DelayedStartup {
        setup: Duration::from_secs(10),
        response: Duration::ZERO,
        writes: writes.clone(),
    });
    let connecting = tokio::spawn(SessionClient::connect(
        connector,
        ResolvedSessionTarget::device(DeviceId::from_array([7; 32])),
        None,
        true,
        false,
        None,
        Default::default(),
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(6)).await;
    assert!(
        !connecting.is_finished(),
        "cold setup survives the old five-second cutoff"
    );
    connecting.abort();
    assert!(
        connecting
            .await
            .expect_err("cancelled startup")
            .is_cancelled()
    );
    tokio::time::advance(Duration::from_secs(20)).await;
    assert_eq!(writes.load(Ordering::SeqCst), 0);
}

fn semantic_snapshot(
    session_id: SessionId,
    attachment_id: AttachmentId,
) -> v2::TerminalSemanticSnapshot {
    voytty_proto::terminal_surface_snapshot_message(
        session_id,
        attachment_id,
        TerminalSurfaceSnapshot {
            revision: Revision::new(1),
            surface: voytty_core::terminal::TerminalSurface {
                application_title: String::new(),
                colors: Default::default(),

                size: voytty_core::terminal::TerminalSize::new(24, 80),
                active_screen: voytty_core::terminal::ActiveScreen::Main,
                rows: (0..24)
                    .map(|_| voytty_core::terminal::TerminalSurfaceRow {
                        cells: vec![voytty_core::terminal::TerminalCell::default(); 80],
                        wrapped: false,
                    })
                    .collect(),
                cursor: voytty_core::terminal::TerminalCursor {
                    presentation: Default::default(),
                    row: 0,
                    column: 0,
                    visible: true,
                    style: Default::default(),
                },
                modes: voytty_core::terminal::TerminalModes::default(),
                scroll_metrics: Some(voytty_core::terminal::TerminalScrollMetrics {
                    epoch: Revision::ZERO,
                    revision: Revision::new(1),
                    offset_from_bottom: 0,
                    max_offset_from_bottom: 0,
                    viewport_rows: 24,
                }),
            },
        },
    )
}

fn tunnel_transport(stream: TestStream) -> AttachmentTransport {
    AttachmentTransport::new(UnixAttachmentTransport::tunnel_after_first(stream, None))
}

fn tunnel_envelope<Message: prost::Message>(kind: WireKind, message: &Message) -> Vec<u8> {
    encode_message(kind, 0, 0, message).expect("encode bounded tunnel envelope")
}

async fn read_tunneled_session_command(
    stream: &mut TestStream,
    decoder: &mut FrameDecoder,
    queued: &mut VecDeque<DecodedFrame>,
) -> DecodedFrame {
    let envelope = read_frame_parts(stream, decoder, queued)
        .await
        .expect("read frontend tunnel envelope");
    assert_eq!(envelope.kind, WireKind::LocalSessionTunnelData);
    assert_eq!(envelope.request_id, 0);
    assert_eq!(envelope.deadline_ms, 0);
    let data: v2::LocalSessionTunnelData = envelope
        .decode_message(WireKind::LocalSessionTunnelData)
        .expect("decode frontend tunnel Data");
    let mut session_decoder = FrameDecoder::new();
    let mut frames = session_decoder
        .feed(&data.bytes)
        .expect("decode frontend Session command");
    assert_eq!(frames.len(), 1);
    session_decoder
        .finish()
        .expect("frontend Data contains one complete Session command");
    frames.remove(0)
}

async fn serve_replacement_tunnel_once(
    listener: TestListener,
    target: DeviceId,
    new_attachment: AttachmentId,
) -> v2::TerminalAttachRequest {
    let (mut stream, _) = listener
        .accept()
        .await
        .expect("replacement viewer daemon accepts one tunnel");
    serve_replacement_tunnel_stream(&mut stream, target, new_attachment).await
}

async fn serve_replacement_tunnel_stream(
    stream: &mut TestStream,
    target: DeviceId,
    new_attachment: AttachmentId,
) -> v2::TerminalAttachRequest {
    let first = read_first(stream)
        .await
        .expect("replacement viewer daemon reads tunnel Open");
    assert_eq!(first.frame.kind, WireKind::LocalSessionTunnelOpenRequest);
    let open: v2::LocalSessionTunnelOpenRequest = first
        .frame
        .decode_message(WireKind::LocalSessionTunnelOpenRequest)
        .expect("decode replacement tunnel Open");
    assert_eq!(
        DeviceId::try_from(open.target_device_id.expect("replacement Open target"))
            .expect("valid replacement Open target"),
        target
    );
    stream
        .write_all(
            &encode_message(
                WireKind::LocalSessionTunnelOpened,
                first.frame.request_id,
                0,
                &v2::LocalSessionTunnelOpened {
                    remote_capabilities: None,
                    protocol_version: voytty_proto::LOCAL_SESSION_TUNNEL_VERSION,
                },
            )
            .expect("encode replacement tunnel Opened"),
        )
        .await
        .expect("write replacement tunnel Opened");

    let mut envelope_decoder = first.decoder;
    let mut queued_envelopes = first.queued;
    let data = read_frame_parts(stream, &mut envelope_decoder, &mut queued_envelopes)
        .await
        .expect("read replacement tunneled resume request");
    assert_eq!(data.kind, WireKind::LocalSessionTunnelData);
    let data: v2::LocalSessionTunnelData = data
        .decode_message(WireKind::LocalSessionTunnelData)
        .expect("decode replacement tunneled resume bytes");
    let mut session_decoder = FrameDecoder::new();
    let mut session_frames = session_decoder
        .feed(&data.bytes)
        .expect("decode replacement inner resume frame");
    assert_eq!(session_frames.len(), 1);
    session_decoder
        .finish()
        .expect("replacement Data contains one complete resume frame");
    let attach_frame = session_frames.remove(0);
    assert_eq!(attach_frame.kind, WireKind::TerminalAttachRequest);
    let request_id = attach_frame.request_id;
    let attach: v2::TerminalAttachRequest = attach_frame
        .decode_message(WireKind::TerminalAttachRequest)
        .expect("decode replacement resume request");
    assert!(attach.accept_zlib_frames);
    assert!(attach.acknowledge_surface_frames);
    let viewport = voytty_core::terminal::TerminalSize::try_from(
        attach
            .viewport
            .expect("replacement resume advertises its latest viewport"),
    )
    .expect("replacement resume viewport is valid");
    let known_revision = attach
        .known_revision
        .expect("replacement resume advertises an applied revision");
    let delta = TerminalSurfaceDelta {
        application_title: String::new(),
        colors: Default::default(),

        from_revision: Revision::new(known_revision),
        to_revision: Revision::new(
            known_revision
                .checked_add(1)
                .expect("replacement revision fits"),
        ),
        size: viewport,
        active_screen: voytty_core::terminal::ActiveScreen::Main,
        row_patches: Vec::new(),
        cursor: voytty_core::terminal::TerminalCursor {
            presentation: Default::default(),
            row: 0,
            column: 0,
            visible: true,
            style: Default::default(),
        },
        modes: voytty_core::terminal::TerminalModes::default(),
        scroll_metrics: Some(voytty_core::terminal::TerminalScrollMetrics {
            epoch: Revision::ZERO,
            revision: Revision::new(known_revision + 1),
            offset_from_bottom: 0,
            max_offset_from_bottom: 0,
            viewport_rows: viewport.rows,
        }),
    };
    let inner = encode_message(
        WireKind::TerminalSemanticDelta,
        request_id,
        0,
        &voytty_proto::terminal_surface_delta_message(new_attachment, delta),
    )
    .expect("encode replacement resume delta");
    stream
        .write_all(&tunnel_envelope(
            WireKind::LocalSessionTunnelData,
            &v2::LocalSessionTunnelData { bytes: inner },
        ))
        .await
        .expect("write replacement resume delta");
    attach
}

#[tokio::test]
async fn tunnel_adapter_surfaces_path_before_a_split_inner_frame_and_preserves_coalescing() {
    let attachment_id = AttachmentId::from_array([0x21; AttachmentId::LENGTH]);
    let mut inner = encode_message(
        WireKind::TerminalTransportStateEvent,
        0,
        0,
        &v2::TerminalTransportStateEvent {
            attachment_id: Some(attachment_id.into()),
            state: v2::TerminalTransportState::Active as i32,
        },
    )
    .expect("encode first inner Session frame");
    inner.extend_from_slice(
        &encode_message(
            WireKind::TerminalSyncRequired,
            0,
            0,
            &v2::TerminalSyncRequired {
                attachment_id: Some(attachment_id.into()),
                latest_revision: 9,
            },
        )
        .expect("encode second inner Session frame"),
    );

    let (client_stream, mut daemon_stream) =
        TestStream::pair().expect("create same-process tunnel fixture");
    let mut transport = tunnel_transport(client_stream);
    let mut envelopes = tunnel_envelope(
        WireKind::LocalSessionTunnelData,
        &v2::LocalSessionTunnelData {
            bytes: inner[..1].to_vec(),
        },
    );
    envelopes.extend_from_slice(&tunnel_envelope(
        WireKind::LocalSessionTunnelPath,
        &v2::LocalSessionTunnelPath {
            path: v2::TerminalConnectionPath::Direct as i32,
            rtt_ms: Some(7),
        },
    ));
    envelopes.extend_from_slice(&tunnel_envelope(
        WireKind::LocalSessionTunnelData,
        &v2::LocalSessionTunnelData {
            bytes: inner[1..].to_vec(),
        },
    ));
    daemon_stream
        .write_all(&envelopes)
        .await
        .expect("write split and coalesced tunnel envelopes");

    let AttachmentTransportItem::Path(path) = transport
        .read_item()
        .await
        .expect("path is observable without awaiting a complete inner frame")
    else {
        panic!("path sideband must surface immediately");
    };
    assert_eq!(
        v2::TerminalConnectionPath::try_from(path.path),
        Ok(v2::TerminalConnectionPath::Direct)
    );
    assert_eq!(path.rtt_ms, Some(7));

    let AttachmentTransportItem::Session(first) = transport
        .read_item()
        .await
        .expect("split inner frame is reassembled")
    else {
        panic!("expected first Session frame");
    };
    assert_eq!(first.kind, WireKind::TerminalTransportStateEvent);
    let AttachmentTransportItem::Session(second) = transport
        .read_item()
        .await
        .expect("coalesced inner frame remains queued")
    else {
        panic!("expected second Session frame");
    };
    assert_eq!(second.kind, WireKind::TerminalSyncRequired);
}

#[tokio::test]
async fn tunnel_adapter_rejects_zero_data_and_incomplete_inner_eof() {
    let (client_stream, mut daemon_stream) =
        TestStream::pair().expect("create zero-data tunnel fixture");
    let mut transport = tunnel_transport(client_stream);
    daemon_stream
        .write_all(&tunnel_envelope(
            WireKind::LocalSessionTunnelData,
            &v2::LocalSessionTunnelData { bytes: Vec::new() },
        ))
        .await
        .expect("write invalid zero-byte Data envelope");
    let error = match transport.read_item().await {
        Err(error) => error,
        Ok(_) => panic!("zero-byte tunnel Data is invalid"),
    };
    assert_eq!(error.kind(), DomainErrorKind::MalformedFrame);

    let (client_stream, mut daemon_stream) =
        TestStream::pair().expect("create incomplete-inner tunnel fixture");
    let mut transport = tunnel_transport(client_stream);
    let incomplete_inner = encode_message(
        WireKind::TerminalSyncRequired,
        0,
        0,
        &v2::TerminalSyncRequired {
            attachment_id: Some(AttachmentId::from_array([0x31; 16]).into()),
            latest_revision: 1,
        },
    )
    .expect("encode an inner frame to truncate");
    let mut envelopes = tunnel_envelope(
        WireKind::LocalSessionTunnelData,
        &v2::LocalSessionTunnelData {
            bytes: incomplete_inner[..1].to_vec(),
        },
    );
    envelopes.extend_from_slice(&tunnel_envelope(
        WireKind::LocalSessionTunnelHalfClose,
        &v2::LocalSessionTunnelHalfClose {},
    ));
    envelopes.extend_from_slice(&tunnel_envelope(
        WireKind::LocalSessionTunnelClosed,
        &v2::LocalSessionTunnelClosed {
            reason: v2::LocalSessionTunnelCloseReason::RemoteEof as i32,
        },
    ));
    daemon_stream
        .write_all(&envelopes)
        .await
        .expect("write incomplete inner stream then terminal envelopes");
    let error = match transport.read_item().await {
        Err(error) => error,
        Ok(_) => panic!("Closed must validate the complete inner Session stream"),
    };
    assert_eq!(error.kind(), DomainErrorKind::MalformedFrame);

    let (client_stream, mut daemon_stream) =
        TestStream::pair().expect("create interrupted-inner tunnel fixture");
    let mut transport = tunnel_transport(client_stream);
    let mut envelopes = tunnel_envelope(
        WireKind::LocalSessionTunnelData,
        &v2::LocalSessionTunnelData {
            bytes: incomplete_inner[..1].to_vec(),
        },
    );
    envelopes.extend_from_slice(&tunnel_envelope(
        WireKind::LocalSessionTunnelClosed,
        &v2::LocalSessionTunnelClosed {
            reason: v2::LocalSessionTunnelCloseReason::TransportLost as i32,
        },
    ));
    daemon_stream
        .write_all(&envelopes)
        .await
        .expect("write partial inner frame then transport loss");
    let error = match transport.read_item().await {
        Err(error) => error,
        Ok(_) => panic!("transport loss must terminate the interrupted Session epoch"),
    };
    assert_eq!(
        error.kind(),
        DomainErrorKind::TransportUnavailable,
        "partial inner bytes from a lost epoch must not mask reconnect eligibility"
    );
}

#[tokio::test]
async fn tunnel_adapter_rejects_data_after_half_close() {
    let (client_stream, mut daemon_stream) =
        TestStream::pair().expect("create half-close tunnel fixture");
    let mut transport = tunnel_transport(client_stream);
    let mut envelopes = tunnel_envelope(
        WireKind::LocalSessionTunnelHalfClose,
        &v2::LocalSessionTunnelHalfClose {},
    );
    envelopes.extend_from_slice(&tunnel_envelope(
        WireKind::LocalSessionTunnelData,
        &v2::LocalSessionTunnelData { bytes: vec![0x01] },
    ));
    daemon_stream
        .write_all(&envelopes)
        .await
        .expect("write Data after HalfClose");
    let error = match transport.read_item().await {
        Err(error) => error,
        Ok(_) => panic!("Data after HalfClose is a tunnel protocol violation"),
    };
    assert_eq!(error.kind(), DomainErrorKind::MalformedFrame);
}

#[tokio::test]
async fn direct_and_tunnel_adapters_share_one_session_trace_and_command_interpreter() {
    let target_device = DeviceId::from_array([0x35; DeviceId::LENGTH]);
    let session_id = SessionId::from_array([0x36; SessionId::LENGTH]);
    let attachment_id = AttachmentId::from_array([0x37; AttachmentId::LENGTH]);
    let (mut direct, mut direct_target) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        session_id,
        attachment_id,
    );
    let (mut tunneled, stale_direct_target) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(target_device),
        session_id,
        attachment_id,
    );
    drop(stale_direct_target);
    let (tunnel_stream, mut tunnel_daemon) =
        TestStream::pair().expect("create paired tunnel stream");
    tunneled.transport = tunnel_transport(tunnel_stream);
    tunneled.resume_view_id = Some(ResumeViewId::from_array([0x38; ResumeViewId::LENGTH]));

    assert!(!direct.is_remote());
    assert!(tunneled.is_remote());
    assert_eq!(direct.session_id(), tunneled.session_id());
    assert_eq!(direct.attachment_id(), tunneled.attachment_id());

    let delta = TerminalSurfaceDelta {
        application_title: String::new(),
        colors: Default::default(),

        from_revision: Revision::new(1),
        to_revision: Revision::new(2),
        size: voytty_core::terminal::TerminalSize::new(24, 80),
        active_screen: voytty_core::terminal::ActiveScreen::Main,
        row_patches: Vec::new(),
        cursor: voytty_core::terminal::TerminalCursor {
            presentation: Default::default(),
            row: 3,
            column: 7,
            visible: true,
            style: Default::default(),
        },
        modes: voytty_core::terminal::TerminalModes::default(),
        scroll_metrics: Some(voytty_core::terminal::TerminalScrollMetrics {
            epoch: Revision::ZERO,
            revision: Revision::new(2),
            offset_from_bottom: 0,
            max_offset_from_bottom: 4,
            viewport_rows: 24,
        }),
    };
    let clipboard = TerminalClipboardWrite::new("paired clipboard".to_owned())
        .expect("valid paired clipboard effect");
    let mut target_trace = encode_message(
        WireKind::TerminalSemanticDelta,
        0,
        0,
        &voytty_proto::terminal_surface_delta_message(attachment_id, delta.clone()),
    )
    .expect("encode paired delta");
    target_trace.extend_from_slice(
        &encode_message(
            WireKind::TerminalClipboardWrite,
            0,
            0,
            &voytty_proto::terminal_clipboard_write_message(attachment_id, clipboard),
        )
        .expect("encode paired clipboard effect"),
    );
    target_trace.extend_from_slice(
        &encode_message(
            WireKind::TerminalSyncRequired,
            0,
            0,
            &v2::TerminalSyncRequired {
                attachment_id: Some(attachment_id.into()),
                latest_revision: 3,
            },
        )
        .expect("encode paired sync requirement"),
    );
    direct_target
        .write_all(&target_trace)
        .await
        .expect("write direct target trace");
    tunnel_daemon
        .write_all(&tunnel_envelope(
            WireKind::LocalSessionTunnelData,
            &v2::LocalSessionTunnelData {
                bytes: target_trace,
            },
        ))
        .await
        .expect("write tunneled target trace");

    // The target has emitted the ordinary delta, but the frontend submits
    // resize before consuming it. Framing/batching must not promote it to a
    // reconnect barrier on either adapter.
    let viewport = voytty_core::terminal::TerminalSize::new(31, 97);
    direct
        .snapshot_applied(Revision::new(1))
        .await
        .expect("direct acknowledgement");
    tunneled
        .snapshot_applied(Revision::new(1))
        .await
        .expect("tunneled acknowledgement");
    direct.resize(viewport).await.expect("direct resize");
    tunneled.resize(viewport).await.expect("tunneled resize");

    for expected_kind in ["delta", "clipboard", "sync-required"] {
        let direct_event = direct
            .read_next_event()
            .await
            .unwrap_or_else(|error| panic!("direct {expected_kind} event failed: {error}"));
        let tunnel_event = tunneled
            .read_next_event()
            .await
            .unwrap_or_else(|error| panic!("tunnel {expected_kind} event failed: {error}"));
        if expected_kind == "delta" {
            assert!(matches!(direct_event, LocalAttachmentEvent::Delta(_)));
            direct.applied_revision.store(2, Ordering::Release);
            tunneled.applied_revision.store(2, Ordering::Release);
        }
        assert_eq!(
            direct_event, tunnel_event,
            "the shared Session interpreter diverged for {expected_kind}"
        );
    }

    direct
        .write_input(b"paired-input".to_vec())
        .await
        .expect("direct input");
    tunneled
        .write_input(b"paired-input".to_vec())
        .await
        .expect("tunneled input");
    direct
        .request_sync(Revision::new(2))
        .await
        .expect("direct sync request");
    tunneled
        .request_sync(Revision::new(2))
        .await
        .expect("tunneled sync request");

    let mut direct_decoder = FrameDecoder::new();
    let mut direct_queued = VecDeque::new();
    let mut tunnel_decoder = FrameDecoder::new();
    let mut tunnel_queued = VecDeque::new();
    for expected_kind in [
        WireKind::TerminalSnapshotApplied,
        WireKind::TerminalResize,
        WireKind::TerminalInput,
        WireKind::TerminalSyncRequest,
    ] {
        let direct_command =
            read_frame_parts(&mut direct_target, &mut direct_decoder, &mut direct_queued)
                .await
                .expect("read direct Session command");
        let tunnel_command = read_tunneled_session_command(
            &mut tunnel_daemon,
            &mut tunnel_decoder,
            &mut tunnel_queued,
        )
        .await;
        assert_eq!(direct_command.kind, expected_kind);
        assert_eq!(
            direct_command, tunnel_command,
            "route adapter changed the target-visible {expected_kind:?} command"
        );
    }

    assert_eq!(direct.latest_viewport, viewport);
    assert_eq!(tunneled.latest_viewport, viewport);
    assert_eq!(direct.terminal_rows, tunneled.terminal_rows);
    assert_eq!(direct.terminal_rows, 24);
    assert_eq!(
        direct.applied_revision.load(Ordering::Acquire),
        tunneled.applied_revision.load(Ordering::Acquire)
    );
    assert_eq!(direct.applied_revision.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn shared_peer_loss_keeps_each_frontend_resume_checkpoint_independent() {
    let target_device = DeviceId::from_array([0x39; DeviceId::LENGTH]);
    let first_view = ResumeViewId::from_array([0x3a; ResumeViewId::LENGTH]);
    let second_view = ResumeViewId::from_array([0x3b; ResumeViewId::LENGTH]);
    let first_attachment = AttachmentId::from_array([0x3c; AttachmentId::LENGTH]);
    let second_attachment = AttachmentId::from_array([0x3d; AttachmentId::LENGTH]);
    let first_viewport = voytty_core::terminal::TerminalSize::new(23, 79);
    let second_viewport = voytty_core::terminal::TerminalSize::new(41, 119);
    let (mut first, stale_first_peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(target_device),
        SessionId::from_array([0x3e; SessionId::LENGTH]),
        first_attachment,
    );
    let (mut second, stale_second_peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(target_device),
        SessionId::from_array([0x3f; SessionId::LENGTH]),
        second_attachment,
    );
    drop((stale_first_peer, stale_second_peer));
    let (first_stream, mut first_daemon) =
        TestStream::pair().expect("create first peer-loss tunnel");
    let (second_stream, mut second_daemon) =
        TestStream::pair().expect("create second peer-loss tunnel");
    first.transport = tunnel_transport(first_stream);
    second.transport = tunnel_transport(second_stream);
    first.resume_view_id = Some(first_view);
    second.resume_view_id = Some(second_view);
    first.applied_revision.store(7, Ordering::Release);
    second.applied_revision.store(13, Ordering::Release);
    first.latest_viewport = first_viewport;
    second.latest_viewport = second_viewport;

    let lost = tunnel_envelope(
        WireKind::LocalSessionTunnelClosed,
        &v2::LocalSessionTunnelClosed {
            reason: v2::LocalSessionTunnelCloseReason::TransportLost as i32,
        },
    );
    first_daemon
        .write_all(&lost)
        .await
        .expect("report first tunnel peer loss");
    second_daemon
        .write_all(&lost)
        .await
        .expect("report second tunnel peer loss");

    for (client, attachment) in [
        (&mut first, first_attachment),
        (&mut second, second_attachment),
    ] {
        let LocalAttachmentEvent::TransportState(state) = client
            .read_next_event()
            .await
            .expect("each frontend independently enters reconnect")
        else {
            panic!("peer loss did not produce a reconnect transition");
        };
        assert_eq!(
            v2::TerminalTransportState::try_from(state.state),
            Ok(v2::TerminalTransportState::Reconnecting)
        );
        assert_eq!(
            AttachmentId::try_from(state.attachment_id.expect("reconnecting attachment ID"))
                .expect("valid reconnecting attachment ID"),
            attachment
        );
        assert!(client.reconnect_pending());
    }

    assert_eq!(first.resume_view_id, Some(first_view));
    assert_eq!(second.resume_view_id, Some(second_view));
    assert_ne!(first.resume_view_id, second.resume_view_id);
    assert_ne!(first.session_id, second.session_id);
    assert_eq!(first.applied_revision.load(Ordering::Acquire), 7);
    assert_eq!(second.applied_revision.load(Ordering::Acquire), 13);
    assert_eq!(first.latest_viewport, first_viewport);
    assert_eq!(second.latest_viewport, second_viewport);
}

#[tokio::test]
async fn reconnecting_frontends_resume_independently_through_one_viewer_listener() {
    let temporary = tempfile::tempdir().expect("create multi-resume fixture directory");
    let socket = temporary.path().join("viewer.sock");
    let listener = TestListener::bind(&socket).expect("bind viewer IPC listener");
    let target_device = DeviceId::from_array([0x40; DeviceId::LENGTH]);
    let first_session = SessionId::from_array([0x41; SessionId::LENGTH]);
    let second_session = SessionId::from_array([0x42; SessionId::LENGTH]);
    let first_view = ResumeViewId::from_array([0x43; ResumeViewId::LENGTH]);
    let second_view = ResumeViewId::from_array([0x44; ResumeViewId::LENGTH]);
    let first_new_attachment = AttachmentId::from_array([0x45; AttachmentId::LENGTH]);
    let second_new_attachment = AttachmentId::from_array([0x46; AttachmentId::LENGTH]);
    let first_viewport = voytty_core::terminal::TerminalSize::new(27, 83);
    let second_viewport = voytty_core::terminal::TerminalSize::new(43, 127);

    let server = tokio::spawn(async move {
        let (mut first_stream, _) = listener
            .accept()
            .await
            .expect("accept first replacement tunnel");
        let (mut second_stream, _) = listener
            .accept()
            .await
            .expect("accept second replacement tunnel");
        tokio::join!(
            serve_replacement_tunnel_stream(&mut first_stream, target_device, first_new_attachment,),
            serve_replacement_tunnel_stream(
                &mut second_stream,
                target_device,
                second_new_attachment,
            )
        )
    });

    let (mut first, first_old_peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(target_device),
        first_session,
        AttachmentId::from_array([0x47; AttachmentId::LENGTH]),
    );
    let (mut second, second_old_peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(target_device),
        second_session,
        AttachmentId::from_array([0x48; AttachmentId::LENGTH]),
    );
    drop((first_old_peer, second_old_peer));
    first.connector = Arc::new(UnixAttachmentConnector::new(socket.clone()));
    second.connector = Arc::new(UnixAttachmentConnector::new(socket));
    first.resume_view_id = Some(first_view);
    second.resume_view_id = Some(second_view);
    first.applied_revision.store(19, Ordering::Release);
    second.applied_revision.store(31, Ordering::Release);
    first.latest_viewport = first_viewport;
    second.latest_viewport = second_viewport;
    first.reconnect_pending = true;
    second.reconnect_pending = true;

    let (first_result, second_result) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(first.reconnect_remote(), second.reconnect_remote())
    })
    .await
    .expect("both frontend resumes stay bounded");
    first_result.expect("first frontend resumes");
    second_result.expect("second frontend resumes");

    let requests = server.await.expect("multi-resume fixture joins");
    let mut first_assigned_attachment = None;
    let mut second_assigned_attachment = None;
    for (request, assigned_attachment) in [
        (requests.0, first_new_attachment),
        (requests.1, second_new_attachment),
    ] {
        let actual_session = SessionId::try_from(request.session_id.expect("resume Session ID"))
            .expect("valid resume Session ID");
        let (expected_view, expected_revision, expected_viewport) =
            if actual_session == first_session {
                first_assigned_attachment = Some(assigned_attachment);
                (first_view, 19, first_viewport)
            } else {
                assert_eq!(actual_session, second_session);
                second_assigned_attachment = Some(assigned_attachment);
                (second_view, 31, second_viewport)
            };
        assert_eq!(
            ResumeViewId::try_from(request.resume_view_id.expect("resume view ID"))
                .expect("valid resume view ID"),
            expected_view
        );
        assert_eq!(request.known_revision, Some(expected_revision));
        assert_eq!(
            voytty_core::terminal::TerminalSize::try_from(
                request.viewport.expect("resume viewport")
            )
            .expect("valid resume viewport"),
            expected_viewport
        );
    }

    for (client, expected_attachment, expected_from, expected_viewport) in [
        (
            &mut first,
            first_assigned_attachment.expect("first frontend received an attachment"),
            19,
            first_viewport,
        ),
        (
            &mut second,
            second_assigned_attachment.expect("second frontend received an attachment"),
            31,
            second_viewport,
        ),
    ] {
        let LocalAttachmentEvent::TransportState(state) = client
            .read_next_event()
            .await
            .expect("replacement epoch enters synchronization")
        else {
            panic!("replacement epoch omitted synchronization state");
        };
        assert_eq!(
            AttachmentId::try_from(state.attachment_id.expect("new attachment ID"))
                .expect("valid new attachment ID"),
            expected_attachment
        );
        assert_eq!(
            v2::TerminalTransportState::try_from(state.state),
            Ok(v2::TerminalTransportState::Synchronizing)
        );
        let LocalAttachmentEvent::ConnectionStatus(path) = client
            .read_next_event()
            .await
            .expect("replacement epoch resets its path")
        else {
            panic!("replacement epoch omitted its path reset");
        };
        assert_eq!(
            v2::TerminalConnectionPath::try_from(path.path),
            Ok(v2::TerminalConnectionPath::Unknown)
        );
        assert_eq!(path.rtt_ms, None);
        let LocalAttachmentEvent::ResumeDelta(delta) = client
            .read_next_event()
            .await
            .expect("replacement epoch resumes from its own checkpoint")
        else {
            panic!("replacement epoch omitted its resume delta");
        };
        assert_eq!(delta.from_revision, Revision::new(expected_from));
        assert_eq!(delta.to_revision, Revision::new(expected_from + 1));
        assert_eq!(delta.size, expected_viewport);
    }

    assert_eq!(first.session_id, first_session);
    assert_eq!(second.session_id, second_session);
    assert_eq!(first.resume_view_id, Some(first_view));
    assert_eq!(second.resume_view_id, Some(second_view));
    assert_ne!(first.attachment_id, second.attachment_id);
}

#[tokio::test]
async fn startup_progress_precedes_opened_and_preserves_coalesced_tunnel_bytes() {
    use crate::progress::ProgressObserver;
    use voytty_core::connection_progress::ConnectionStage;
    for chunk_size in [1, 4096] {
        let (observer, history) = ProgressObserver::channel(|_| {});
        let stages = [
            ConnectionStage::LookingUpAddress,
            ConnectionStage::ConnectingSecurely,
            ConnectionStage::SessionChannelReady,
        ];
        let mut bytes = Vec::new();
        for stage in stages {
            bytes.extend(
                encode_message(
                    WireKind::LocalConnectionProgress,
                    1,
                    0,
                    &voytty_proto::connection_stage_to_message(stage)
                        .expect("tunnel progress fixture"),
                )
                .expect("tunnel progress fixture"),
            );
        }
        bytes.extend(
            encode_message(
                WireKind::LocalSessionTunnelOpened,
                1,
                0,
                &v2::LocalSessionTunnelOpened {
                    remote_capabilities: None,
                    protocol_version: voytty_proto::LOCAL_SESSION_TUNNEL_VERSION,
                },
            )
            .expect("tunnel progress fixture"),
        );
        bytes.extend(tunnel_envelope(
            WireKind::LocalSessionTunnelPath,
            &v2::LocalSessionTunnelPath {
                path: v2::TerminalConnectionPath::Direct as i32,
                rtt_ms: Some(3),
            },
        ));
        let (mut client, mut daemon) = TestStream::pair().expect("tunnel progress fixture");
        let send = async {
            for chunk in bytes.chunks(chunk_size) {
                daemon
                    .write_all(chunk)
                    .await
                    .expect("tunnel progress fixture");
                tokio::task::yield_now().await;
            }
        };
        let receive = async {
            let first = read_tunnel_first(&mut client, observer)
                .await
                .expect("tunnel progress fixture");
            assert_eq!(first.frame.kind, WireKind::LocalSessionTunnelOpened);
            let mut transport = AttachmentTransport::new(
                UnixAttachmentTransport::tunnel_after_first(client, Some(first)),
            );
            assert!(
                matches!(transport.read_item().await.expect("tunnel progress fixture"), AttachmentTransportItem::Path(path) if path.path == v2::TerminalConnectionPath::Direct as i32 && path.rtt_ms == Some(3))
            );
        };
        tokio::join!(send, receive);
        assert_eq!(
            history
                .borrow()
                .after(0)
                .map(|(_, event)| event.stage)
                .collect::<Vec<_>>(),
            stages
        );
    }
}

#[tokio::test]
async fn interrupted_outer_tunnel_frames_remain_retryable() {
    let opened = encode_message(
        WireKind::LocalSessionTunnelOpened,
        1,
        0,
        &v2::LocalSessionTunnelOpened {
            remote_capabilities: None,
            protocol_version: voytty_proto::LOCAL_SESSION_TUNNEL_VERSION,
        },
    )
    .expect("encode Opened envelope");
    let (mut client_stream, mut daemon_stream) =
        TestStream::pair().expect("create interrupted-Open fixture");
    daemon_stream
        .write_all(&opened[..1])
        .await
        .expect("write partial Opened envelope");
    daemon_stream
        .shutdown()
        .await
        .expect("interrupt Opened envelope");
    let error = match read_tunnel_first(
        &mut client_stream,
        crate::progress::ProgressObserver::default(),
    )
    .await
    {
        Err(error) => error,
        Ok(_) => panic!("partial Opened envelope cannot establish a tunnel"),
    };
    assert_eq!(error.kind(), DomainErrorKind::TransportUnavailable);

    let data = tunnel_envelope(
        WireKind::LocalSessionTunnelData,
        &v2::LocalSessionTunnelData { bytes: vec![0x01] },
    );
    let (client_stream, mut daemon_stream) =
        TestStream::pair().expect("create interrupted-envelope fixture");
    let mut transport = tunnel_transport(client_stream);
    daemon_stream
        .write_all(&data[..1])
        .await
        .expect("write partial tunnel envelope");
    daemon_stream
        .shutdown()
        .await
        .expect("interrupt tunnel envelope");
    let error = match transport.read_item().await {
        Err(error) => error,
        Ok(_) => panic!("partial outer envelope cannot produce a Session item"),
    };
    assert_eq!(error.kind(), DomainErrorKind::TransportUnavailable);
}

#[tokio::test]
async fn remote_reconnect_reuses_view_and_session_with_latest_frontend_checkpoint() {
    reconnect_checkpoint_fixture().await;
}

async fn reconnect_checkpoint_fixture() {
    let temporary = tempfile::tempdir().expect("create reconnect fixture directory");
    let socket = temporary.path().join("daemon.sock");
    let listener = TestListener::bind(&socket).expect("bind reconnect fixture IPC");
    let target_device = DeviceId::from_array([0x51; DeviceId::LENGTH]);
    let session_id = SessionId::from_array([0x52; SessionId::LENGTH]);
    let old_attachment = AttachmentId::from_array([0x53; AttachmentId::LENGTH]);
    let new_attachment = AttachmentId::from_array([0x54; AttachmentId::LENGTH]);
    let resume_view_id = ResumeViewId::from_array([0x55; ResumeViewId::LENGTH]);
    let viewport = voytty_core::terminal::TerminalSize::new(31, 97);

    let mut colors = TerminalColorProfile::default();
    colors.values[1] = voytty_core::terminal::TerminalColorValue::Rgb(17, 18, 19);
    colors.appearance = voytty_core::terminal::TerminalAppearance::Dark;
    let expected_colors = colors.clone();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept reconnect tunnel");
        let first = read_first(&mut stream).await.expect("read tunnel Open");
        assert_eq!(first.frame.kind, WireKind::LocalSessionTunnelOpenRequest);
        let open: v2::LocalSessionTunnelOpenRequest = first
            .frame
            .decode_message(WireKind::LocalSessionTunnelOpenRequest)
            .expect("decode tunnel Open");
        assert_eq!(
            open.protocol_version,
            voytty_proto::LOCAL_SESSION_TUNNEL_VERSION
        );
        let opened_target: DeviceId = open
            .target_device_id
            .expect("tunnel Open has a target")
            .try_into()
            .expect("tunnel target is valid");
        assert_eq!(opened_target, target_device);

        let mut outbound = encode_message(
            WireKind::LocalSessionTunnelOpened,
            first.frame.request_id,
            0,
            &v2::LocalSessionTunnelOpened {
                remote_capabilities: None,
                protocol_version: voytty_proto::LOCAL_SESSION_TUNNEL_VERSION,
            },
        )
        .expect("encode tunnel Opened");
        for path in [
            v2::LocalSessionTunnelPath {
                path: v2::TerminalConnectionPath::Unknown as i32,
                rtt_ms: None,
            },
            v2::LocalSessionTunnelPath {
                path: v2::TerminalConnectionPath::Unknown as i32,
                rtt_ms: None,
            },
            v2::LocalSessionTunnelPath {
                path: v2::TerminalConnectionPath::Direct as i32,
                rtt_ms: Some(6),
            },
            v2::LocalSessionTunnelPath {
                path: v2::TerminalConnectionPath::Direct as i32,
                rtt_ms: Some(6),
            },
        ] {
            outbound.extend_from_slice(&tunnel_envelope(WireKind::LocalSessionTunnelPath, &path));
        }
        stream
            .write_all(&outbound)
            .await
            .expect("write tunnel Opened and initial path");

        let mut envelope_decoder = first.decoder;
        let mut queued_envelopes = first.queued;
        let data = read_frame_parts(&mut stream, &mut envelope_decoder, &mut queued_envelopes)
            .await
            .expect("read tunneled resume request");
        assert_eq!(data.kind, WireKind::LocalSessionTunnelData);
        let data: v2::LocalSessionTunnelData = data
            .decode_message(WireKind::LocalSessionTunnelData)
            .expect("decode tunneled resume bytes");
        let mut session_decoder = FrameDecoder::new();
        let mut session_frames = session_decoder
            .feed(&data.bytes)
            .expect("decode inner resume frame");
        assert_eq!(session_frames.len(), 1);
        let attach = session_frames.remove(0);
        assert_eq!(attach.kind, WireKind::TerminalAttachRequest);
        let request_id = attach.request_id;
        let attach: v2::TerminalAttachRequest = attach
            .decode_message(WireKind::TerminalAttachRequest)
            .expect("decode inner resume request");
        assert_eq!(
            TerminalColorProfile::try_from(
                attach
                    .base_colors
                    .clone()
                    .expect("resume retains observations")
            )
            .expect("valid resume observations"),
            expected_colors
        );
        let attached_session: SessionId = attach
            .session_id
            .expect("resume request keeps the Session ID")
            .try_into()
            .expect("resume Session ID is valid");
        assert_eq!(attached_session, session_id);
        let attached_view: ResumeViewId = attach
            .resume_view_id
            .expect("resume request keeps the view ID")
            .try_into()
            .expect("resume view ID is valid");
        assert_eq!(attached_view, resume_view_id);
        assert_eq!(attach.known_revision, Some(11));
        let requested_viewport: voytty_core::terminal::TerminalSize = attach
            .viewport
            .expect("resume request keeps the latest viewport")
            .try_into()
            .expect("resume viewport is valid");
        assert_eq!(requested_viewport, viewport);
        let attached_target =
            resolved_target_from_wire(attach.target).expect("resume target selector is valid");
        assert_eq!(attached_target.device_id(), Some(target_device));

        let delta = voytty_core::terminal::TerminalSurfaceDelta {
            application_title: String::new(),
            colors: Default::default(),

            from_revision: Revision::new(11),
            to_revision: Revision::new(12),
            size: viewport,
            active_screen: voytty_core::terminal::ActiveScreen::Main,
            row_patches: Vec::new(),
            cursor: voytty_core::terminal::TerminalCursor {
                presentation: Default::default(),
                row: 0,
                column: 0,
                visible: true,
                style: Default::default(),
            },
            modes: voytty_core::terminal::TerminalModes::default(),
            scroll_metrics: Some(voytty_core::terminal::TerminalScrollMetrics {
                epoch: Revision::ZERO,
                revision: Revision::new(12),
                offset_from_bottom: 0,
                max_offset_from_bottom: 0,
                viewport_rows: viewport.rows,
            }),
        };
        let inner = encode_message(
            WireKind::TerminalSemanticDelta,
            request_id,
            0,
            &voytty_proto::terminal_surface_delta_message(new_attachment, delta),
        )
        .expect("encode resume delta");
        stream
            .write_all(&tunnel_envelope(
                WireKind::LocalSessionTunnelData,
                &v2::LocalSessionTunnelData { bytes: inner },
            ))
            .await
            .expect("write tunneled resume delta");
    });

    let (mut client, old_peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(target_device),
        session_id,
        old_attachment,
    );
    drop(old_peer);
    client.connector = Arc::new(UnixAttachmentConnector::new(socket));
    client.resume_view_id = Some(resume_view_id);
    client
        .applied_revision
        .store(11, std::sync::atomic::Ordering::Release);
    client.latest_viewport = viewport;
    client.enter_reconnecting();
    client.pending_transport_events.clear(); // The fixture observes the replacement epoch below.
    client
        .update_colors(colors.clone())
        .await
        .expect("colors remain retained across a closed transport");
    client
        .reconnect_remote()
        .await
        .expect("frontend resumes through a replacement tunnel");
    assert_eq!(client.session_id, session_id);
    assert_eq!(client.resume_view_id, Some(resume_view_id));
    assert_eq!(client.attachment_id, new_attachment);
    assert_eq!(client.latest_viewport, viewport);
    assert_eq!(client.latest_colors, colors);
    assert_eq!(
        client.color_sequence, 0,
        "new attachment begins a fresh observation sequence"
    );
    assert_eq!(
        client
            .applied_revision
            .load(std::sync::atomic::Ordering::Acquire),
        11,
        "the resume delta is not advertised as applied before the UI installs it"
    );

    let state = client
        .read_next_event()
        .await
        .expect("replacement epoch begins synchronizing");
    let LocalAttachmentEvent::TransportState(state) = state else {
        panic!("replacement epoch must begin with transport state");
    };
    assert_eq!(
        v2::TerminalTransportState::try_from(state.state),
        Ok(v2::TerminalTransportState::Synchronizing)
    );
    assert_eq!(
        AttachmentId::try_from(state.attachment_id.expect("state attachment ID"))
            .expect("state attachment ID is valid"),
        new_attachment
    );
    let LocalAttachmentEvent::ConnectionStatus(path) = client
        .read_next_event()
        .await
        .expect("replacement epoch exposes its frontend-owned path reset")
    else {
        panic!("replacement path reset must precede its update");
    };
    assert_eq!(
        v2::TerminalConnectionPath::try_from(path.path),
        Ok(v2::TerminalConnectionPath::Unknown)
    );
    assert_eq!(path.rtt_ms, None);
    let LocalAttachmentEvent::ConnectionStatus(path) = client
        .read_next_event()
        .await
        .expect("replacement epoch exposes its collected path sample")
    else {
        panic!("replacement path sample must precede its update");
    };
    assert_eq!(
        v2::TerminalConnectionPath::try_from(path.path),
        Ok(v2::TerminalConnectionPath::Direct)
    );
    assert_eq!(path.rtt_ms, Some(6));
    let LocalAttachmentEvent::ResumeDelta(delta) = client
        .read_next_event()
        .await
        .expect("replacement epoch exposes its contiguous delta")
    else {
        panic!("replacement epoch should resume with a delta");
    };
    assert_eq!(delta.from_revision, Revision::new(11));
    assert_eq!(delta.to_revision, Revision::new(12));
    server.await.expect("reconnect fixture server joins");
}

#[test]
fn diagnostics_correlate_real_reconnect_and_coalesce_healthy_route_samples() {
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "session::tests::diagnostics_reconnect_child",
            "--ignored",
        ])
        .output()
        .expect("isolated recorder fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[tokio::test]
#[ignore = "isolated shared-client recorder fixture"]
async fn diagnostics_reconnect_child() {
    #[derive(Default)]
    struct Memory(std::sync::Mutex<Vec<Vec<u8>>>);
    impl voytty_diagnostics::Sink for Memory {
        fn append(&self, _: bool, bytes: &[u8]) -> std::io::Result<()> {
            self.0.lock().expect("records").push(bytes.into());
            Ok(())
        }
        fn control(&self) -> std::io::Result<voytty_diagnostics::Control> {
            Ok(voytty_diagnostics::Control::disabled())
        }
    }
    let sink = Arc::new(Memory::default());
    let recorder = voytty_diagnostics::Recorder::new(sink.clone()).expect("recorder");
    assert!(voytty_diagnostics::install(recorder.clone()));
    reconnect_checkpoint_fixture().await;
    let (mut client, _peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        SessionId::from_array([0x61; 16]),
        AttachmentId::from_array([0x62; 16]),
    );
    for rtt in 1..=100 {
        client
            .pending_transport_events
            .push_back(LocalAttachmentEvent::ConnectionStatus(
                v2::TerminalConnectionStatusEvent {
                    attachment_id: Some(client.attachment_id.into()),
                    path: v2::TerminalConnectionPath::Direct as i32,
                    rtt_ms: Some(rtt),
                },
            ));
        client.read_next_event().await.expect("healthy sample");
    }
    assert!(recorder.flush(Duration::from_secs(2)));
    let records: Vec<_> = sink
        .0
        .lock()
        .expect("records")
        .iter()
        .map(|bytes| voytty_diagnostics::Record::decode(bytes).expect("safe record"))
        .collect();
    use voytty_diagnostics::Kind;
    let start = records
        .iter()
        .find(|r| r.event == Kind::ReconnectStarted)
        .expect("start");
    let completed = records
        .iter()
        .find(|r| r.event == Kind::ReconnectCompleted)
        .expect("completed");
    assert_eq!(start.fields.operation_id, completed.fields.operation_id);
    assert_eq!(start.fields.session_id, completed.fields.session_id);
    assert_ne!(start.fields.attachment_id, completed.fields.attachment_id);
    assert!(completed.fields.elapsed_ms.is_some());
    assert_eq!(
        records
            .iter()
            .filter(|r| r.event == Kind::ReconnectStarted)
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|r| r.event == Kind::RouteChanged
                && r.fields.session_id == Some(client.session_id.to_string()))
            .count(),
        1
    );
    assert!(
        records
            .iter()
            .all(|r| r.level != voytty_diagnostics::Level::Debug)
    );
}

#[tokio::test]
async fn remote_reconnect_ensures_stopped_viewer_daemon_but_local_view_does_not() {
    let temporary = tempfile::tempdir().expect("create stopped-daemon fixture directory");
    let target_device = DeviceId::from_array([0x57; DeviceId::LENGTH]);
    let calls = Arc::new(AtomicU64::new(0));
    let restarter: Arc<dyn RemoteDaemonRestarter> = Arc::new(FailingRemoteDaemonRestarter {
        calls: Arc::clone(&calls),
    });

    let (mut remote, remote_peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(target_device),
        SessionId::from_array([0x58; SessionId::LENGTH]),
        AttachmentId::from_array([0x59; AttachmentId::LENGTH]),
    );
    drop(remote_peer);
    remote.connector = Arc::new(
        UnixAttachmentConnector::new(temporary.path().join("stopped-daemon.sock"))
            .with_remote_restarter(Arc::clone(&restarter)),
    );
    remote.resume_view_id = Some(ResumeViewId::from_array([0x5a; ResumeViewId::LENGTH]));
    remote.reconnect_pending = true;

    let error = remote
        .reconnect_remote()
        .await
        .expect_err("a failed viewer-daemon launch is surfaced");
    assert_eq!(error.kind(), DomainErrorKind::DaemonStartTimeout);
    assert_eq!(calls.load(Ordering::Acquire), 1);

    let local_connector = UnixAttachmentConnector::new(temporary.path().join("stopped-local.sock"))
        .with_remote_restarter(restarter);
    let error = local_connector
        .open(ResolvedSessionTarget::local())
        .await
        .err()
        .expect("local daemon remains stopped");
    assert_eq!(
        error.kind(),
        DomainErrorKind::DaemonStopped,
        "a local Session cannot be resumed into a new daemon incarnation"
    );
    assert_eq!(calls.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn stopped_viewer_restart_opens_a_tunnel_and_resumes_the_same_frontend_state() {
    let temporary = tempfile::tempdir().expect("create restart-resume fixture directory");
    let socket = temporary.path().join("replacement-daemon.sock");
    let target_device = DeviceId::from_array([0x5d; DeviceId::LENGTH]);
    let session_id = SessionId::from_array([0x5e; SessionId::LENGTH]);
    let old_attachment = AttachmentId::from_array([0x5f; AttachmentId::LENGTH]);
    let new_attachment = AttachmentId::from_array([0x60; AttachmentId::LENGTH]);
    let resume_view_id = ResumeViewId::from_array([0x61; ResumeViewId::LENGTH]);
    let viewport = voytty_core::terminal::TerminalSize::new(33, 101);
    let calls = Arc::new(AtomicU64::new(0));
    let restarter = Arc::new(AcceptingRemoteDaemonRestarter {
        socket: socket.clone(),
        target: target_device,
        new_attachment,
        calls: Arc::clone(&calls),
        server: StdMutex::new(None),
    });

    let (mut client, old_peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(target_device),
        session_id,
        old_attachment,
    );
    drop(old_peer);
    let restart_capability: Arc<dyn RemoteDaemonRestarter> = restarter.clone();
    client.connector =
        Arc::new(UnixAttachmentConnector::new(socket).with_remote_restarter(restart_capability));
    client.resume_view_id = Some(resume_view_id);
    client.applied_revision.store(17, Ordering::Release);
    client.latest_viewport = viewport;
    client.reconnect_pending = true;

    tokio::time::timeout(Duration::from_secs(2), client.reconnect_remote())
        .await
        .expect("restart and replacement tunnel stay bounded")
        .expect("replacement viewer daemon accepts the resumed Session");
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert_eq!(client.session_id, session_id);
    assert_eq!(client.resume_view_id, Some(resume_view_id));
    assert_eq!(client.attachment_id, new_attachment);
    assert_eq!(client.latest_viewport, viewport);
    assert_eq!(client.applied_revision.load(Ordering::Acquire), 17);

    let server = restarter
        .server
        .lock()
        .expect("replacement server task lock")
        .take()
        .expect("restart hook installed one replacement server");
    let attach = server
        .await
        .expect("replacement viewer-daemon fixture joins");
    assert!(!attach.create_main);
    assert!(!attach.takeover);
    assert_eq!(attach.known_revision, Some(17));
    assert_eq!(
        SessionId::try_from(attach.session_id.expect("resumed Session ID"))
            .expect("valid resumed Session ID"),
        session_id
    );
    assert_eq!(
        ResumeViewId::try_from(attach.resume_view_id.expect("resumed view ID"))
            .expect("valid resumed view ID"),
        resume_view_id
    );
    assert_eq!(
        voytty_core::terminal::TerminalSize::try_from(attach.viewport.expect("resumed viewport"))
            .expect("valid resumed viewport"),
        viewport
    );
    assert_eq!(
        resolved_target_from_wire(attach.target)
            .expect("valid resumed target")
            .device_id(),
        Some(target_device)
    );

    let LocalAttachmentEvent::TransportState(state) = client
        .read_next_event()
        .await
        .expect("replacement epoch starts synchronizing")
    else {
        panic!("replacement epoch omitted synchronization state");
    };
    assert_eq!(
        v2::TerminalTransportState::try_from(state.state),
        Ok(v2::TerminalTransportState::Synchronizing)
    );
    let LocalAttachmentEvent::ConnectionStatus(path) = client
        .read_next_event()
        .await
        .expect("replacement epoch resets its connection observation")
    else {
        panic!("replacement epoch omitted its unknown path reset");
    };
    assert_eq!(
        v2::TerminalConnectionPath::try_from(path.path),
        Ok(v2::TerminalConnectionPath::Unknown)
    );
    assert_eq!(path.rtt_ms, None);
    let LocalAttachmentEvent::ResumeDelta(delta) = client
        .read_next_event()
        .await
        .expect("replacement epoch returns a contiguous delta")
    else {
        panic!("replacement epoch omitted its contiguous delta");
    };
    assert_eq!(delta.from_revision, Revision::new(17));
    assert_eq!(delta.to_revision, Revision::new(18));
    assert_eq!(delta.size, viewport);
}

#[tokio::test]
async fn remote_reconnect_resolves_pending_history_once_before_transition() {
    let (mut client, _peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(DeviceId::from_array([0x61; DeviceId::LENGTH])),
        SessionId::from_array([0x62; SessionId::LENGTH]),
        AttachmentId::from_array([0x63; AttachmentId::LENGTH]),
    );
    let query = TerminalHistoryWindowQuery {
        anchor: voytty_core::terminal::TerminalHistoryWindowAnchor {
            epoch: Revision::new(7),
            revision: Revision::new(9),
            max_offset_from_bottom: 20,
            viewport: voytty_core::terminal::TerminalSize::new(24, 80),
        },
        target_offset_from_bottom: 4,
        older_margin_rows: 8,
        newer_margin_rows: 0,
    };
    client.pending_history_window = Some((41, query));
    client.applied_revision.store(11, Ordering::Release);
    client.enter_reconnecting();

    assert!(client.pending_history_window.is_none());
    assert!(matches!(
        client.pending_transport_events.pop_front(),
        Some(LocalAttachmentEvent::HistoryWindow(
            TerminalSurfaceHistoryWindowResult::HistoryGap { epoch, revision }
        )) if epoch == Revision::new(7) && revision == Revision::new(11)
    ));
    assert!(matches!(
        client.pending_transport_events.pop_front(),
        Some(LocalAttachmentEvent::TransportState(state))
            if v2::TerminalTransportState::try_from(state.state)
                == Ok(v2::TerminalTransportState::Reconnecting)
    ));
    assert!(client.pending_transport_events.is_empty());
    client.enter_reconnecting();
    assert!(
        client.pending_transport_events.is_empty(),
        "repeated failure in one reconnect epoch must not duplicate outcomes"
    );
}

#[tokio::test]
async fn local_history_window_client_allows_only_one_outstanding_request() {
    let (mut client, _peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        SessionId::from_array([0x43; 16]),
        AttachmentId::from_array([0x44; 16]),
    );
    let query = TerminalHistoryWindowQuery {
        anchor: voytty_core::terminal::TerminalHistoryWindowAnchor {
            epoch: Revision::new(1),
            revision: Revision::new(2),
            max_offset_from_bottom: 12,
            viewport: voytty_core::terminal::TerminalSize::new(4, 10),
        },
        target_offset_from_bottom: 1,
        older_margin_rows: 8,
        newer_margin_rows: 0,
    };
    client
        .request_history_window(query)
        .await
        .expect("first bounded window request");
    let error = client
        .request_history_window(query)
        .await
        .expect_err("second window request must await correlation");
    assert_eq!(error.kind(), DomainErrorKind::ResourceExhausted);
}

#[test]
fn local_session_end_debug_never_formats_the_signal_text() {
    let event = LocalAttachmentEvent::SessionEnded(v2::TerminalSessionEnded {
        session_id: Some(SessionId::from_array([0x90; SessionId::LENGTH]).into()),
        attachment_id: Some(AttachmentId::from_array([0x91; AttachmentId::LENGTH]).into()),
        reason: v2::TerminalSessionEndReason::NaturalExit as i32,
        exit_code: 1,
        signal: "SENSITIVE_LOCAL_SIGNAL_SENTINEL".to_owned(),
    });
    let debug = format!("{event:?}");
    assert!(debug.contains("has_signal: true"));
    assert!(!debug.contains("SENSITIVE_LOCAL_SIGNAL_SENTINEL"));
}

#[tokio::test]
async fn local_attachment_consumes_validated_transport_state_over_unix_duplex() {
    let session_id = SessionId::from_array([0x92; SessionId::LENGTH]);
    let attachment_id = AttachmentId::from_array([0x93; AttachmentId::LENGTH]);
    let (mut client, mut daemon_stream) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        session_id,
        attachment_id,
    );
    let event = v2::TerminalTransportStateEvent {
        attachment_id: Some(attachment_id.into()),
        state: v2::TerminalTransportState::Reconnecting as i32,
    };
    daemon_stream
        .write_all(
            &encode_message(WireKind::TerminalTransportStateEvent, 0, 0, &event)
                .expect("bounded transport-state event"),
        )
        .await
        .expect("write transport-state event");
    match client
        .read_event(Duration::from_secs(1))
        .await
        .expect("validated transport-state event")
    {
        LocalAttachmentEvent::TransportState(actual) => assert_eq!(actual, event),
        event => panic!("unexpected local attachment event: {event:?}"),
    }

    let invalid = v2::TerminalTransportStateEvent {
        attachment_id: Some(attachment_id.into()),
        state: i32::MAX,
    };
    daemon_stream
        .write_all(
            &encode_message(WireKind::TerminalTransportStateEvent, 0, 0, &invalid)
                .expect("bounded invalid transport-state event"),
        )
        .await
        .expect("write invalid transport-state event");
    assert_eq!(
        client
            .read_event(Duration::from_secs(1))
            .await
            .expect_err("unknown transport state is rejected")
            .kind(),
        DomainErrorKind::MalformedFrame
    );
}

#[tokio::test]
async fn remote_session_stream_rejects_same_uid_transport_projections() {
    let target = ResolvedSessionTarget::device(DeviceId::from_array([0x94; DeviceId::LENGTH]));
    let session_id = SessionId::from_array([0x95; SessionId::LENGTH]);
    let attachment_id = AttachmentId::from_array([0x96; AttachmentId::LENGTH]);
    let (mut client, mut target_stream) =
        SessionClient::terminal_driver_test_pair(target, session_id, attachment_id);
    target_stream
        .write_all(
            &encode_message(
                WireKind::TerminalTransportStateEvent,
                0,
                0,
                &v2::TerminalTransportStateEvent {
                    attachment_id: Some(attachment_id.into()),
                    state: v2::TerminalTransportState::Active as i32,
                },
            )
            .expect("encode same-UID-only transport state"),
        )
        .await
        .expect("write invalid remote Session transport state");
    assert_eq!(
        client
            .read_event(Duration::from_secs(1))
            .await
            .expect_err("remote normal-ALPN Session rejects transport state")
            .kind(),
        DomainErrorKind::MalformedFrame
    );

    let (mut client, mut target_stream) =
        SessionClient::terminal_driver_test_pair(target, session_id, attachment_id);
    target_stream
        .write_all(
            &encode_message(
                WireKind::TerminalConnectionStatusEvent,
                0,
                0,
                &v2::TerminalConnectionStatusEvent {
                    attachment_id: Some(attachment_id.into()),
                    path: v2::TerminalConnectionPath::Direct as i32,
                    rtt_ms: Some(3),
                },
            )
            .expect("encode same-UID-only connection status"),
        )
        .await
        .expect("write invalid remote Session connection status");
    assert_eq!(
        client
            .read_event(Duration::from_secs(1))
            .await
            .expect_err("remote normal-ALPN Session rejects connection status")
            .kind(),
        DomainErrorKind::MalformedFrame
    );
}

#[tokio::test]
async fn notification_events_require_unsolicited_identity_on_both_routes() {
    let session = SessionId::from_array([0xb1; 16]);
    let id = AttachmentId::from_array([0xb2; 16]);
    let wrong = AttachmentId::from_array([0xb3; 16]);
    for target in [
        ResolvedSessionTarget::local(),
        ResolvedSessionTarget::device(voytty_core::DeviceId::from_array([0xb4; 32])),
    ] {
        for (request, deadline, recipient, valid) in [
            (0, 0, id, true),
            (1, 0, id, false),
            (0, 1, id, false),
            (0, 0, wrong, false),
        ] {
            let (mut client, mut stream) =
                SessionClient::terminal_driver_test_pair(target, session, id);
            let value = TerminalNotification::osc777("标题".into(), "结果;body".into())
                .expect("valid notification");
            let message = voytty_proto::terminal_notification_message(recipient, value.clone());
            stream
                .write_all(
                    &encode_message(WireKind::TerminalNotification, request, deadline, &message)
                        .expect("encode"),
                )
                .await
                .expect("write");
            let event = client.read_event(Duration::from_secs(1)).await;
            if valid {
                assert!(
                    matches!(event, Ok(LocalAttachmentEvent::Notification(received)) if received == value)
                );
            } else {
                assert_eq!(
                    event.expect_err("invalid notification").kind(),
                    DomainErrorKind::MalformedFrame
                );
            }
        }
    }
}

#[tokio::test]
async fn local_attachment_clipboard_event_requires_zero_request_and_exact_identity() {
    let session_id = SessionId::from_array([0xa2; SessionId::LENGTH]);
    let attachment_id = AttachmentId::from_array([0xa3; AttachmentId::LENGTH]);
    let (mut client, mut daemon_stream) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        session_id,
        attachment_id,
    );
    let write =
        TerminalClipboardWrite::new("typed clipboard".to_owned()).expect("valid clipboard fixture");
    let event = voytty_proto::terminal_clipboard_write_message(attachment_id, write);
    daemon_stream
        .write_all(
            &encode_message(WireKind::TerminalClipboardWrite, 0, 0, &event)
                .expect("bounded clipboard event"),
        )
        .await
        .expect("write clipboard event");
    let LocalAttachmentEvent::ClipboardWrite(write) = client
        .read_event(Duration::from_secs(1))
        .await
        .expect("validated clipboard event")
    else {
        panic!("expected typed clipboard event");
    };
    assert_eq!(write.as_str(), "typed clipboard");

    let (mut client, mut daemon_stream) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        session_id,
        attachment_id,
    );
    daemon_stream
        .write_all(
            &encode_message(WireKind::TerminalClipboardWrite, 7, 0, &event)
                .expect("nonzero request fixture"),
        )
        .await
        .expect("write nonzero request fixture");
    assert_eq!(
        client
            .read_event(Duration::from_secs(1))
            .await
            .expect_err("nonzero clipboard request id")
            .kind(),
        DomainErrorKind::MalformedFrame
    );

    let (mut client, mut daemon_stream) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        session_id,
        attachment_id,
    );
    let wrong = voytty_proto::terminal_clipboard_write_message(
        AttachmentId::from_array([0xa4; AttachmentId::LENGTH]),
        TerminalClipboardWrite::new("wrong owner".to_owned()).expect("valid wrong-owner fixture"),
    );
    daemon_stream
        .write_all(
            &encode_message(WireKind::TerminalClipboardWrite, 0, 0, &wrong)
                .expect("wrong identity fixture"),
        )
        .await
        .expect("write wrong identity fixture");
    assert_eq!(
        client
            .read_event(Duration::from_secs(1))
            .await
            .expect_err("wrong clipboard attachment")
            .kind(),
        DomainErrorKind::MalformedFrame
    );
}

#[tokio::test]
async fn local_attachment_discards_stale_pre_snapshot_transport_states() {
    let temporary = tempfile::tempdir().expect("temporary socket root");
    let socket_path = temporary.path().join("terminal.sock");
    let listener = TestListener::bind(&socket_path).expect("local listener");
    let session_id = SessionId::from_array([0x94; SessionId::LENGTH]);
    let attachment_id = AttachmentId::from_array([0x95; AttachmentId::LENGTH]);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept local view");
        let states = [
            v2::TerminalTransportState::Preparing,
            v2::TerminalTransportState::Synchronizing,
        ];
        for state in states {
            stream
                .write_all(
                    &encode_message(
                        WireKind::TerminalTransportStateEvent,
                        0,
                        0,
                        &v2::TerminalTransportStateEvent {
                            attachment_id: Some(attachment_id.into()),
                            state: state as i32,
                        },
                    )
                    .expect("bounded pre-snapshot state"),
                )
                .await
                .expect("write pre-snapshot state");
        }
        stream
            .write_all(
                &encode_message(
                    WireKind::TerminalSemanticSnapshot,
                    1,
                    0,
                    &semantic_snapshot(session_id, attachment_id),
                )
                .expect("bounded initial snapshot"),
            )
            .await
            .expect("write initial snapshot");
        stream
            .write_all(
                &encode_message(
                    WireKind::TerminalTransportStateEvent,
                    0,
                    0,
                    &v2::TerminalTransportStateEvent {
                        attachment_id: Some(attachment_id.into()),
                        state: v2::TerminalTransportState::Active as i32,
                    },
                )
                .expect("bounded post-snapshot state"),
            )
            .await
            .expect("write post-snapshot state");
    });

    let mut client = SessionClient::connect_resolved(
        &socket_path,
        ResolvedSessionTarget::local(),
        None,
        true,
        false,
        None,
    )
    .await
    .expect("connect through pre-snapshot states");
    assert_eq!(
        client
            .take_initial_snapshot()
            .expect("initial snapshot")
            .revision,
        Revision::new(1)
    );
    let event = client
        .read_event(Duration::from_secs(1))
        .await
        .expect("post-snapshot state");
    assert!(matches!(
        event,
        LocalAttachmentEvent::TransportState(state)
            if state.state == v2::TerminalTransportState::Active as i32
    ));
    server.await.expect("server task");
}

#[tokio::test]
async fn create_main_post_submit_response_loss_is_outcome_unknown_for_every_target() {
    let remote = DeviceId::from_array([0x96; DeviceId::LENGTH]);
    for target in [
        ResolvedSessionTarget::local(),
        ResolvedSessionTarget::device(remote),
    ] {
        let error = run_fake_create_main(target, FakeCreateMainReply::DropAfterSubmit)
            .await
            .expect_err("post-submit response loss has an unknown create outcome");
        assert_eq!(error.kind(), DomainErrorKind::OperationOutcomeUnknown);
    }
}

#[tokio::test]
async fn create_main_preserves_exact_snapshot_and_correlated_error_for_every_target() {
    let remote = DeviceId::from_array([0x97; DeviceId::LENGTH]);
    for (index, target) in [
        ResolvedSessionTarget::local(),
        ResolvedSessionTarget::device(remote),
    ]
    .into_iter()
    .enumerate()
    {
        let session_id = SessionId::from_array(
            [0x98 + u8::try_from(index).expect("two target fixtures fit u8"); SessionId::LENGTH],
        );
        let attachment_id = AttachmentId::from_array(
            [0xa8 + u8::try_from(index).expect("two target fixtures fit u8"); AttachmentId::LENGTH],
        );
        let client = run_fake_create_main(
            target,
            FakeCreateMainReply::Snapshot {
                session_id,
                attachment_id,
            },
        )
        .await
        .expect("a complete correlated snapshot is an exact committed result");
        assert_eq!(client.session_id(), session_id);
        assert_eq!(client.attachment_id(), attachment_id);
        assert_eq!(client.is_remote(), !target.is_local());

        let error = run_fake_create_main(
            target,
            FakeCreateMainReply::ServiceError {
                request_id: 1,
                kind: DomainErrorKind::SessionOccupied,
                detail: "exact occupied fixture",
            },
        )
        .await
        .expect_err("a correlated typed service error is definitive");
        assert_eq!(error.kind(), DomainErrorKind::SessionOccupied);
        assert_eq!(error.detail(), "exact occupied fixture");
    }
}

#[tokio::test]
async fn create_main_prewrite_failure_stays_definitive_and_wrong_error_id_is_unknown() {
    let temporary = tempfile::tempdir().expect("temporary missing socket root");
    let remote = DeviceId::from_array([0x9a; DeviceId::LENGTH]);
    for target in [
        ResolvedSessionTarget::local(),
        ResolvedSessionTarget::device(remote),
    ] {
        let error = SessionClient::connect_resolved(
            temporary.path().join("missing.sock"),
            target,
            None,
            true,
            false,
            None,
        )
        .await
        .expect_err("connect failure occurs before any request write");
        assert_eq!(error.kind(), DomainErrorKind::DaemonStopped);

        let error = run_fake_create_main(
            target,
            FakeCreateMainReply::ServiceError {
                request_id: 2,
                kind: DomainErrorKind::SessionOccupied,
                detail: "uncorrelated occupied fixture",
            },
        )
        .await
        .expect_err("an uncorrelated service error cannot prove the create outcome");
        assert_eq!(error.kind(), DomainErrorKind::OperationOutcomeUnknown);
    }
}

#[tokio::test(start_paused = true)]
async fn initial_attachment_deadline_covers_states_before_the_snapshot() {
    let create_main = run_fake_initial_state_stall(true).await;
    assert_eq!(
        create_main.kind(),
        DomainErrorKind::OperationOutcomeUnknown,
        "a stalled post-write create cannot be reported as definitively absent"
    );

    let existing = run_fake_initial_state_stall(false).await;
    assert_eq!(
        existing.kind(),
        DomainErrorKind::DeadlineExceeded,
        "an existing-session attach retains its bounded transport failure"
    );
}

#[derive(Clone, Copy)]
enum FakeCreateMainReply {
    DropAfterSubmit,
    ServiceError {
        request_id: u64,
        kind: DomainErrorKind,
        detail: &'static str,
    },
    Snapshot {
        session_id: SessionId,
        attachment_id: AttachmentId,
    },
}

async fn run_fake_initial_state_stall(create_main: bool) -> DaemonError {
    let temporary = tempfile::tempdir().expect("temporary initial-state stall fixture");
    let socket = temporary.path().join("initial-state-stall.sock");
    let listener = TestListener::bind(&socket).expect("bind fake local daemon");
    let attachment_id = AttachmentId::from_array([0x9b; AttachmentId::LENGTH]);
    let (state_written_sender, state_written_receiver) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept attach request");
        let first = read_first(&mut stream)
            .await
            .expect("decode complete attach request");
        assert_eq!(first.frame.kind, WireKind::TerminalAttachRequest);
        stream
            .write_all(
                &encode_message(
                    WireKind::TerminalTransportStateEvent,
                    0,
                    0,
                    &v2::TerminalTransportStateEvent {
                        attachment_id: Some(attachment_id.into()),
                        state: v2::TerminalTransportState::Preparing as i32,
                    },
                )
                .expect("bounded pre-snapshot state"),
            )
            .await
            .expect("write pre-snapshot state");
        let _ = state_written_sender.send(());
        std::future::pending::<()>().await;
    });

    let client_socket = socket.clone();
    let client = tokio::spawn(async move {
        SessionClient::connect_resolved(
            &client_socket,
            ResolvedSessionTarget::local(),
            None,
            create_main,
            false,
            None,
        )
        .await
    });
    state_written_receiver
        .await
        .expect("server crossed the complete pre-snapshot-state write barrier");
    let result = tokio::time::timeout(
        DEFAULT_DEADLINE
            .checked_mul(2)
            .expect("fixture watchdog deadline fits Duration"),
        client,
    )
    .await
    .expect("the production initial-response deadline fires before the fixture watchdog")
    .expect("initial-state client task")
    .expect_err("the initial snapshot never arrived");
    server.abort();
    let _ = server.await;
    result
}

async fn run_fake_create_main(
    target: ResolvedSessionTarget,
    reply: FakeCreateMainReply,
) -> Result<SessionClient, DaemonError> {
    run_fake_create_main_after(target, reply, Duration::ZERO).await
}

#[tokio::test]
async fn desktop_tunnel_startup_budget_survives_six_second_connection_setup() {
    let started = tokio::time::Instant::now();
    run_fake_create_main_after(
        ResolvedSessionTarget::device(DeviceId::from_array([3; 32])),
        FakeCreateMainReply::Snapshot {
            session_id: SessionId::from_array([1; 16]),
            attachment_id: AttachmentId::from_array([2; 16]),
        },
        Duration::from_secs(6),
    )
    .await
    .expect("the actual Unix tunnel adapter must also allow slow startup");
    assert!(started.elapsed() >= Duration::from_secs(6));
    assert!(started.elapsed() < crate::protocol::REMOTE_STARTUP_DEADLINE);
}

async fn run_fake_create_main_after(
    target: ResolvedSessionTarget,
    reply: FakeCreateMainReply,
    setup_delay: Duration,
) -> Result<SessionClient, DaemonError> {
    let temporary = tempfile::tempdir().expect("temporary create-main fixture");
    let socket = temporary.path().join("create-main.sock");
    let listener = TestListener::bind(&socket).expect("bind fake local daemon");
    let expected_target = target.device_id();
    let (submitted_tx, submitted_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept create-main request");
        let first = tokio::time::timeout(Duration::from_secs(2), read_first(&mut stream))
            .await
            .expect("create-main request reached the write boundary")
            .expect("decode create-main request");
        let (attach, tunneled) = if let Some(expected_target) = expected_target {
            assert_eq!(first.frame.kind, WireKind::LocalSessionTunnelOpenRequest);
            assert_eq!(first.frame.deadline_ms, 15_000);
            tokio::time::sleep(setup_delay).await;
            let open: v2::LocalSessionTunnelOpenRequest = first
                .frame
                .decode_message(WireKind::LocalSessionTunnelOpenRequest)
                .expect("decode create-main tunnel Open");
            assert_eq!(
                DeviceId::try_from(open.target_device_id.expect("Open target"))
                    .expect("valid Open target"),
                expected_target
            );
            stream
                .write_all(
                    &encode_message(
                        WireKind::LocalSessionTunnelOpened,
                        first.frame.request_id,
                        0,
                        &v2::LocalSessionTunnelOpened {
                            remote_capabilities: None,
                            protocol_version: voytty_proto::LOCAL_SESSION_TUNNEL_VERSION,
                        },
                    )
                    .expect("bounded tunnel Opened"),
                )
                .await
                .expect("write tunnel Opened");

            let mut decoder = first.decoder;
            let mut queued = first.queued;
            let data = read_frame_parts(&mut stream, &mut decoder, &mut queued)
                .await
                .expect("read tunneled create-main request");
            assert_eq!(data.kind, WireKind::LocalSessionTunnelData);
            let data: v2::LocalSessionTunnelData = data
                .decode_message(WireKind::LocalSessionTunnelData)
                .expect("decode tunneled create-main bytes");
            let mut session_decoder = FrameDecoder::new();
            let mut frames = session_decoder
                .feed(&data.bytes)
                .expect("decode tunneled create-main Session frame");
            assert_eq!(frames.len(), 1);
            (frames.remove(0), true)
        } else {
            (first.frame, false)
        };
        assert_eq!(attach.kind, WireKind::TerminalAttachRequest);
        assert_eq!(
            resolved_target_from_wire(
                attach
                    .decode_message::<v2::TerminalAttachRequest>(WireKind::TerminalAttachRequest)
                    .expect("attach request")
                    .target
            )
            .expect("valid target")
            .device_id(),
            expected_target
        );
        let request: v2::TerminalAttachRequest = attach
            .decode_message(WireKind::TerminalAttachRequest)
            .expect("decode create-main payload");
        assert!(request.create_main);
        assert_eq!(request.accept_zlib_frames, expected_target.is_some());
        assert_eq!(
            request.acknowledge_surface_frames,
            expected_target.is_some(),
            "only remote paths bound undelivered surface frames"
        );
        submitted_tx
            .send(())
            .expect("observe request only after a complete frame was read");
        release_rx
            .await
            .expect("release fake response after submission barrier");

        match reply {
            FakeCreateMainReply::DropAfterSubmit => {}
            FakeCreateMainReply::ServiceError {
                request_id,
                kind,
                detail,
            } => {
                let response = encode_message(
                    WireKind::ServiceErrorResponse,
                    request_id,
                    0,
                    &v2::ServiceError {
                        code: kind.code().to_owned(),
                        message: detail.to_owned(),
                    },
                )
                .expect("bounded typed create-main error");
                stream
                    .write_all(&tunnel_data_if_needed(response, tunneled))
                    .await
                    .expect("write typed create-main error");
            }
            FakeCreateMainReply::Snapshot {
                session_id,
                attachment_id,
            } => {
                let response = encode_message(
                    WireKind::TerminalSemanticSnapshot,
                    1,
                    0,
                    &semantic_snapshot(session_id, attachment_id),
                )
                .expect("bounded committed create-main snapshot");
                stream
                    .write_all(&tunnel_data_if_needed(response, tunneled))
                    .await
                    .expect("write committed create-main snapshot");
            }
        }
        let _ = stream.shutdown().await;
    });

    let client_socket = socket.clone();
    let client = tokio::spawn(async move {
        SessionClient::connect_resolved(client_socket, target, None, true, false, None).await
    });
    tokio::time::timeout(setup_delay + Duration::from_secs(2), submitted_rx)
        .await
        .expect("create-main submission barrier is bounded")
        .expect("fake server reports create-main submission");
    release_tx
        .send(())
        .expect("release response-loss or exact-result fixture");
    let result = tokio::time::timeout(Duration::from_secs(2), client)
        .await
        .expect("create-main client completion is bounded")
        .expect("create-main client task");
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("fake create-main server completion is bounded")
        .expect("fake create-main server task");
    result
}

fn tunnel_data_if_needed(bytes: Vec<u8>, tunneled: bool) -> Vec<u8> {
    if tunneled {
        tunnel_envelope(
            WireKind::LocalSessionTunnelData,
            &v2::LocalSessionTunnelData { bytes },
        )
    } else {
        bytes
    }
}
