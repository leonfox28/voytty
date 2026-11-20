use std::future::{Future, poll_fn};
use std::sync::Arc;

use iroh::{RelayUrl, TransportAddr};
use tokio::sync::oneshot;
use voytty_core::{AuthorizationSnapshot, ConnectionAttemptId, DeviceDisplayName, TransportLimits};

use super::*;
use crate::authorization::AuthorizationRegistry;
use crate::network::NetworkReporter;
use crate::store::DeviceAuthorization;

#[cfg(any(unix, windows))]
struct NoopRemoteServiceHandler;

#[cfg(any(unix, windows))]
impl RemoteServiceHandler for NoopRemoteServiceHandler {
    fn handle_service_stream(
        &self,
        _stream: InboundAuthenticatedStream,
        _first_frame_deadline: Instant,
    ) -> RemoteServiceHandlerFuture {
        Box::pin(async { Ok(()) })
    }
}

pub(super) fn device(byte: u8) -> DeviceId {
    DeviceId::from_array([byte; 32])
}

fn key(initiator: u8, attempt: u8) -> ConnectionCandidateKey {
    ConnectionCandidateKey::new(
        device(initiator),
        ConnectionAttemptId::from_array([attempt; 16]),
    )
}

pub(super) fn relay(url: &str) -> RelayHint {
    RelayHint::new(url).expect("test relay is valid")
}

#[test]
fn retired_owner_handshakes_are_bounded_and_cannot_replace_the_current_owner() {
    let first = ConnectionIncarnation::from_array([1; 16]);
    let next = ConnectionIncarnation::from_array([2; 16]);
    let third = ConnectionIncarnation::from_array([3; 16]);
    let now = Instant::now();
    let window = Duration::from_secs(15);
    let mut owners = PeerIncarnation::default();
    assert!(owners.observe(first, now, window, 1).expect("first"));
    assert!(!owners.observe(first, now, window, 1).expect("same owner"));
    assert!(owners.observe(next, now, window, 1).expect("restart"));
    assert_eq!(
        owners
            .observe(first, now, window, 1)
            .expect_err("late old Hello")
            .kind(),
        DomainErrorKind::TransportUnavailable
    );
    assert_eq!(owners.current, Some(next));
    assert_eq!(
        owners
            .observe(third, now, window, 1)
            .expect_err("bounded churn")
            .kind(),
        DomainErrorKind::ResourceExhausted
    );
    assert_eq!(owners.current, Some(next));
    assert!(
        owners
            .observe(third, now + window, window, 1)
            .expect("expired handshake window")
    );
    assert_eq!(owners.retired.len(), 1);
}

#[cfg(any(unix, windows))]
#[test]
fn selected_path_rtt_rounds_to_nearest_millisecond_and_clamps() {
    assert_eq!(round_rtt_millis(Duration::from_micros(499)), 0);
    assert_eq!(round_rtt_millis(Duration::from_micros(500)), 1);
    assert_eq!(round_rtt_millis(Duration::from_micros(1_499)), 1);
    assert_eq!(round_rtt_millis(Duration::from_micros(1_500)), 2);
    assert_eq!(round_rtt_millis(Duration::MAX), u32::MAX);
}

fn authorization(
    device_id: DeviceId,
    status: AuthorizationStatus,
    generation: u64,
) -> DeviceAuthorization {
    DeviceAuthorization {
        device_id,
        display_name: DeviceDisplayName::new("peer").expect("display name"),
        status,
        generation: AuthGeneration::new(generation).expect("non-zero generation"),
        paired_at_unix: 1,
        revoked_at_unix: (status == AuthorizationStatus::Revoked).then_some(2),
        last_seen_at_unix: None,
    }
}

#[test]
fn candidate_registry_converges_from_opposite_registration_orders() {
    let low = key(1, 1);
    let high = key(2, 1);

    let mut high_first = CandidateRegistry::default();
    high_first.register(high, "high").expect("register high");
    assert!(matches!(
        high_first.mark_ready_and_decide(high),
        CandidateDecision::Promoted(ref losers) if losers.is_empty()
    ));
    high_first.register(low, "low").expect("register low");
    assert!(matches!(
        high_first.mark_ready_and_decide(low),
        CandidateDecision::Promoted(ref losers) if losers == &["high"]
    ));
    assert_eq!(high_first.primary(), Some(low));
    assert_eq!(high_first.len(), 1);

    let mut low_first = CandidateRegistry::default();
    low_first.register(low, "low").expect("register low");
    low_first.register(high, "high").expect("register high");
    assert!(matches!(
        low_first.mark_ready_and_decide(high),
        CandidateDecision::Wait
    ));
    assert!(matches!(
        low_first.mark_ready_and_decide(low),
        CandidateDecision::Promoted(ref losers) if losers == &["high"]
    ));
    assert_eq!(low_first.primary(), Some(low));
    assert_eq!(low_first.len(), 1);

    assert_eq!(high_first.primary(), low_first.primary());
    assert!(high_first.register(low, "collision").is_err());
}

#[test]
fn candidate_loser_cleanup_and_peer_close_leave_no_redial_triggering_ghosts() {
    let low = key(1, 1);
    let high = key(2, 1);
    let mut candidates = CandidateRegistry::default();
    candidates.register(low, "low").expect("register low");
    candidates.register(high, "high").expect("register high");
    assert!(matches!(
        candidates.mark_ready_and_decide(high),
        CandidateDecision::Wait
    ));
    assert!(
        !peer_needs_outbound_attempt(&candidates, None, |_| false),
        "a provisional duplicate candidate suppresses redial"
    );

    assert_eq!(candidates.remove(low), Some("low"));
    assert!(matches!(
        candidates.mark_ready_and_decide(high),
        CandidateDecision::Promoted(ref losers) if losers.is_empty()
    ));
    assert_eq!(candidates.primary(), Some(high));
    assert!(
        peer_needs_outbound_attempt(&candidates, None, |_| false),
        "an inbound primary needs one outbound acceptance proof"
    );
    assert!(
        !peer_needs_outbound_attempt(
            &candidates,
            Some(AuthGeneration::new(1).expect("generation")),
            |_| false,
        ),
        "a duplicate loser does not redial after proving acceptance"
    );

    assert_eq!(candidates.take_all(), vec!["high"]);
    assert_eq!(candidates.primary(), None);
    assert!(candidates.is_empty());

    let lifecycle = BrokerLifecycle::default();
    assert!(!lifecycle.is_quiescing());
    lifecycle.begin_quiesce();
    assert!(lifecycle.is_quiescing());
}

#[test]
fn demand_bookkeeping_is_checked_singleflight_and_drops_transient_routes() {
    let demand = DemandState::new();
    let first_route = relay("https://first.example");
    let second_route = relay("https://second.example");

    let (previous, first_lease) = demand
        .acquire(vec![first_route.clone()], 2)
        .expect("first demand");
    assert_eq!(previous, 0);
    let (previous, second_lease) = demand
        .acquire(vec![first_route.clone(), second_route.clone()], 2)
        .expect("second demand");
    assert_eq!(previous, 1);
    assert_eq!(demand.count(), 2);
    assert_eq!(
        demand.routes(),
        vec![first_route.clone(), second_route.clone()]
    );

    let overflow = demand
        .acquire(vec![relay("https://third.example")], 2)
        .expect_err("unique route bound is enforced");
    assert_eq!(overflow.kind(), DomainErrorKind::ResourceExhausted);
    assert_eq!(demand.count(), 2, "failed acquisition rolls back count");

    demand.release(&first_lease);
    assert_eq!(demand.count(), 1);
    assert_eq!(
        demand.routes(),
        vec![first_route.clone(), second_route.clone()]
    );
    demand.release(&second_lease);
    assert_eq!(demand.count(), 0);
    assert!(demand.routes().is_empty());

    let mut peer = PeerState::default();
    assert!(peer.claim_dial_worker());
    assert!(!peer.claim_dial_worker(), "one peer has one dial worker");
    peer.terminal_error = Some(DaemonError::new(DomainErrorKind::Unauthorized, "terminal"));
    peer.begin_demand_cycle(1);
    assert!(peer.terminal_error.is_some());
    peer.begin_demand_cycle(0);
    assert!(
        peer.terminal_error.is_none(),
        "a new user demand retries once"
    );
}

#[test]
fn broker_peer_and_connection_admission_are_bounded_without_sockets() {
    let limits = TransportLimits {
        max_pending_dials: 2,
        max_remote_connections: 2,
        max_stream_handlers_global: 2,
        max_open_stream_queue_per_connection: 2,
        max_bi_streams_per_connection: 2,
        max_stream_handlers_per_connection: 2,
        ..TransportLimits::default()
    };
    let broker = BrokerAdmission::new(limits);
    let peer = PeerAdmission::new(limits);
    let connection = CandidateAdmission::new(limits);

    assert_two_permits_then_full(&broker.pending_dials);
    assert_two_permits_then_full(&broker.authenticated_connections);
    assert_two_permits_then_full(&broker.global_stream_handlers);
    assert_two_permits_then_full(&peer.open_queue);
    assert_two_permits_then_full(&connection.streams);
    assert_two_permits_then_full(&connection.handlers);
}

#[test]
#[cfg(any(unix, windows))]
fn remote_service_handler_installs_once_without_transport_ownership() {
    let slot = RemoteServiceHandlerSlot::default();
    assert!(!slot.is_installed());
    slot.install(Arc::new(NoopRemoteServiceHandler))
        .expect("first handler installation");
    assert!(slot.is_installed());
    assert!(slot.get().is_some());
    let error = slot
        .install(Arc::new(NoopRemoteServiceHandler))
        .expect_err("handler ownership cannot be replaced after installation");
    assert_eq!(error.kind(), DomainErrorKind::IdentityStateMismatch);
}

#[tokio::test]
async fn service_handler_admission_quiesce_and_reclamation_use_raii_permits() {
    let lifecycle = BrokerLifecycle::default();
    let connection = Arc::new(Semaphore::new(1));
    let global = Arc::new(Semaphore::new(1));
    let permits = try_admit_service_handler(&lifecycle, &connection, &global)
        .expect("first service handler is admitted");
    assert_eq!(connection.available_permits(), 0);
    assert_eq!(global.available_permits(), 0);
    assert_eq!(
        try_admit_service_handler(&lifecycle, &connection, &global)
            .expect_err("connection-local handler capacity is bounded")
            .kind(),
        DomainErrorKind::ResourceExhausted
    );

    let wait_global = Arc::clone(&global);
    let (first_poll, first_poll_observation) = oneshot::channel();
    let waiting = tokio::spawn(async move {
        let wait = tokio::task::unconstrained(wait_for_service_handlers_until(
            wait_global,
            1,
            Instant::now() + Duration::from_secs(1),
        ));
        tokio::pin!(wait);
        let mut observer = Some(first_poll);
        poll_fn(|context| {
            let result = wait.as_mut().poll(context);
            if let Some(observer) = observer.take() {
                let _ = observer.send(result.is_pending());
            }
            result
        })
        .await
    });
    assert!(
        first_poll_observation
            .await
            .expect("handler reclamation first-poll result"),
        "reclamation waits while the admitted handler owns its permit"
    );
    drop(permits);
    waiting
        .await
        .expect("handler reclamation task")
        .expect("released handler permit is reclaimed");
    assert_eq!(connection.available_permits(), 1);
    assert_eq!(global.available_permits(), 1);

    lifecycle.begin_quiesce();
    let error = try_admit_service_handler(&lifecycle, &connection, &global)
        .expect_err("new service admission fails closed after quiesce");
    assert_eq!(error.kind(), DomainErrorKind::TransportUnavailable);
    assert_eq!(connection.available_permits(), 1);
    assert_eq!(global.available_permits(), 1);
}

#[tokio::test]
async fn service_handler_panic_releases_only_its_stream_permits() {
    let lifecycle = BrokerLifecycle::default();
    let connection = Arc::new(Semaphore::new(1));
    let global = Arc::new(Semaphore::new(1));
    let permits = try_admit_service_handler(&lifecycle, &connection, &global)
        .expect("panic fixture handler admission");
    let mut handlers = JoinSet::new();
    handlers.spawn(async move {
        let _permits = permits;
        panic!("injected stream-local handler panic");
    });
    let joined = handlers.join_next().await.expect("one handler task joined");
    assert!(joined.is_err(), "the injected handler task panicked");
    assert!(!lifecycle.is_quiescing());
    assert_eq!(connection.available_permits(), 1);
    assert_eq!(global.available_permits(), 1);
    drop(
        try_admit_service_handler(&lifecycle, &connection, &global)
            .expect("another independent handler remains admissible"),
    );
}

fn assert_two_permits_then_full(semaphore: &Arc<Semaphore>) {
    let first = Arc::clone(semaphore)
        .try_acquire_owned()
        .expect("first permit");
    let second = Arc::clone(semaphore)
        .try_acquire_owned()
        .expect("second permit");
    assert!(
        Arc::clone(semaphore).try_acquire_owned().is_err(),
        "third permit must be rejected"
    );
    drop((first, second));
    assert_eq!(semaphore.available_permits(), 2);
}

#[test]
fn per_peer_stream_observation_tracks_raii_lifetimes() {
    let local = device(0x31);
    let (reporter, observer) = NetworkReporter::initializing(local);
    let metrics = Arc::new(BrokerMetrics {
        authenticated: AtomicUsize::new(0),
        primary: AtomicUsize::new(0),
        streams: AtomicUsize::new(0),
        paths: Mutex::new(BTreeMap::new()),
        reporter,
    });
    let peer_streams = Arc::new(AtomicUsize::new(0));
    let first = StreamMetricGuard::new(Arc::clone(&metrics), Arc::clone(&peer_streams))
        .expect("first stream");
    let second = StreamMetricGuard::new(Arc::clone(&metrics), Arc::clone(&peer_streams))
        .expect("second stream");
    assert_eq!(peer_streams.load(Ordering::Acquire), 2);
    assert_eq!(observer.snapshot().active_stream_count, 2);

    drop(first);
    assert_eq!(peer_streams.load(Ordering::Acquire), 1);
    assert_eq!(observer.snapshot().active_stream_count, 1);
    drop(second);
    assert_eq!(peer_streams.load(Ordering::Acquire), 0);
    assert_eq!(observer.snapshot().active_stream_count, 0);
}

#[tokio::test]
async fn malformed_and_oversized_first_frames_are_stream_local() {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut malformed = &[0x80, 0x00][..];
    let malformed_error = read_one_frame(&mut malformed, voytty_proto::MAX_FRAME_BYTES, deadline)
        .await
        .expect_err("non-canonical frame prefix is malformed");
    assert_eq!(malformed_error.kind(), DomainErrorKind::MalformedFrame);

    let valid_bytes = encode_message(
        WireKind::SessionListRequest,
        1,
        0,
        &v2::SessionListRequest { target: None },
    )
    .expect("bounded service frame");
    let mut oversized = valid_bytes.as_slice();
    let oversized_error = read_one_frame(&mut oversized, 1, deadline)
        .await
        .expect_err("injected one-byte body ceiling rejects the service frame");
    assert_eq!(oversized_error.kind(), DomainErrorKind::FrameTooLarge);

    let mut healthy_peer = valid_bytes.as_slice();
    let frame = read_one_frame(&mut healthy_peer, voytty_proto::MAX_FRAME_BYTES, deadline)
        .await
        .expect("another peer's frame remains readable");
    assert_eq!(frame.kind, WireKind::SessionListRequest);
    assert_eq!(frame.request_id, 1);
}

#[tokio::test]
async fn stalled_first_frame_deadline_is_stream_local() {
    let (_sender, mut stalled) = tokio::io::duplex(64);
    let stalled_error = read_one_frame(&mut stalled, voytty_proto::MAX_FRAME_BYTES, Instant::now())
        .await
        .expect_err("elapsed first-frame deadline cannot wait for input");
    assert_eq!(stalled_error.kind(), DomainErrorKind::DeadlineExceeded);

    let healthy_bytes = encode_message(
        WireKind::SessionListRequest,
        2,
        0,
        &v2::SessionListRequest { target: None },
    )
    .expect("bounded service frame");
    let mut healthy_peer = healthy_bytes.as_slice();
    let frame = read_one_frame(
        &mut healthy_peer,
        voytty_proto::MAX_FRAME_BYTES,
        Instant::now() + Duration::from_secs(1),
    )
    .await
    .expect("a stalled peer does not delay another peer");
    assert_eq!(frame.kind, WireKind::SessionListRequest);
    assert_eq!(frame.request_id, 2);
}

#[test]
fn retry_categories_separate_transient_from_terminal_failures() {
    for kind in [
        DomainErrorKind::AddressUnavailable,
        DomainErrorKind::TransportUnavailable,
        DomainErrorKind::DeadlineExceeded,
        DomainErrorKind::ResourceExhausted,
        DomainErrorKind::StoreUnavailable,
    ] {
        assert!(is_retryable(kind), "{} should retry", kind.code());
    }
    for kind in [
        DomainErrorKind::Unauthorized,
        DomainErrorKind::AuthorizationRevoked,
        DomainErrorKind::WireMajorMismatch,
        DomainErrorKind::Cancelled,
        DomainErrorKind::IdentityStateMismatch,
    ] {
        assert!(!is_retryable(kind), "{} must be terminal", kind.code());
    }
}

#[test]
fn pair_tls_identity_mismatch_is_terminal_while_route_failure_can_fall_back() {
    let expected = device(0x51);
    validate_pair_tls_metadata(expected, expected, VOYTTY_PAIR_ALPN)
        .expect("exact pair TLS metadata");

    let wrong_id = validate_pair_tls_metadata(expected, device(0x52), VOYTTY_PAIR_ALPN)
        .expect_err("ticket host must equal the TLS identity");
    assert_eq!(wrong_id.kind(), DomainErrorKind::Unauthorized);
    assert!(!should_try_next_pair_route(
        &wrong_id,
        Instant::now() + Duration::from_secs(1)
    ));

    let wrong_alpn = validate_pair_tls_metadata(expected, expected, VOYTTY_ALPN)
        .expect_err("normal ALPN is never a pair connection");
    assert_eq!(wrong_alpn.kind(), DomainErrorKind::WireMajorMismatch);
    assert!(!should_try_next_pair_route(
        &wrong_alpn,
        Instant::now() + Duration::from_secs(1)
    ));

    let route_failure = transport_unavailable("fixture route failed");
    assert!(should_try_next_pair_route(
        &route_failure,
        Instant::now() + Duration::from_secs(1)
    ));
}

#[test]
fn pair_dial_and_confirmation_source_contracts_are_registry_and_stream_free() {
    let source = concat!(
        include_str!("../connection_broker.rs"),
        include_str!("dial.rs"),
        include_str!("demand.rs")
    );
    let pair_start = source
        .find("pub(crate) async fn connect_pair_transient")
        .expect("pair dial method");
    let pair_end = source[pair_start..]
        .find("/// Wraps one fully awaited inbound pair connection")
        .map(|offset| pair_start + offset)
        .expect("pair dial method end");
    let pair_body = &source[pair_start..pair_end];
    assert!(pair_body.contains("VOYTTY_PAIR_ALPN"));
    assert!(pair_body.contains("connect_transport("));
    for forbidden in [
        "peer_slot(",
        "register_candidate(",
        "persist_verified",
        "insert_relay",
    ] {
        assert!(
            !pair_body.contains(forbidden),
            "pair dial must not use normal-registry/profile operation {forbidden}"
        );
    }

    let confirmation_start = source
        .find("async fn wait_for_confirmed_primary")
        .expect("confirmation selection method");
    let confirmation_end = source[confirmation_start..]
        .find("/// Opens a business stream")
        .map(|offset| confirmation_start + offset)
        .expect("confirmation method end");
    let confirmation_body = &source[confirmation_start..confirmation_end];
    assert!(!confirmation_body.contains(concat!(".open", "_bi(")));
    assert!(!source.contains(concat!("into_0", "rtt")));
}

#[test]
fn normal_connections_apply_bi_and_zero_uni_limits_before_handshake() {
    let source = concat!(
        include_str!("../connection_broker.rs"),
        include_str!("dial.rs"),
        include_str!("demand.rs")
    );
    let accept_start = source
        .find("pub async fn accept_normal")
        .expect("normal accept method");
    let accept_end = source[accept_start..]
        .find("/// Replaces the current Endpoint")
        .map(|offset| accept_start + offset)
        .expect("normal accept method end");
    let accept_body = &source[accept_start..accept_end];
    assert!(
        accept_body
            .find("configure_normal_connection_limits")
            .expect("incoming QUIC limits")
            < accept_body
                .find("run_inbound_handshake")
                .expect("incoming application handshake")
    );

    let dial_start = source
        .find("async fn dial_once")
        .expect("normal dial method");
    let dial_end = source[dial_start..]
        .find("async fn dial_routes")
        .map(|offset| dial_start + offset)
        .expect("normal dial method end");
    let dial_body = &source[dial_start..dial_end];
    assert!(
        dial_body
            .find("configure_normal_connection_limits")
            .expect("outgoing QUIC limits")
            < dial_body
                .find("run_outbound_handshake")
                .expect("outgoing application handshake")
    );

    let helper_start = source
        .find("fn configure_normal_connection_limits")
        .expect("QUIC limit helper");
    let helper_end = source[helper_start..]
        .find("fn protocol_error")
        .map(|offset| helper_start + offset)
        .expect("QUIC limit helper end");
    let helper_body = &source[helper_start..helper_end];
    assert!(helper_body.contains("set_max_concurrent_bi_streams"));
    assert!(helper_body.contains("set_max_concurrent_uni_streams(VarInt::from_u32(0))"));
}

#[tokio::test]
async fn inbound_admission_is_generic_directional_and_generation_exact() {
    let authorized = device(0x41);
    let revoked = device(0x42);
    let unknown = device(0x43);
    let registry = AuthorizationRegistry::new();
    registry
        .preload(vec![
            authorization(authorized, AuthorizationStatus::Authorized, 1),
            authorization(revoked, AuthorizationStatus::Revoked, 2),
        ])
        .expect("preload registry");

    let admitted =
        admit_inbound_before_payload(&registry, authorized).expect("authorized before payload");
    assert_eq!(
        receiver_generation_for_stream(&registry, authorized, Some(admitted.snapshot.generation),)
            .expect("stream uses the receiver-owned accepted generation"),
        admitted.snapshot.generation
    );
    let revoked_error =
        admit_inbound_before_payload(&registry, revoked).expect_err("revoked before payload");
    let unknown_error =
        admit_inbound_before_payload(&registry, unknown).expect_err("unknown before payload");
    assert_eq!(revoked_error.kind(), DomainErrorKind::Unauthorized);
    assert_eq!(unknown_error.kind(), DomainErrorKind::Unauthorized);
    assert_eq!(revoked_error.detail(), unknown_error.detail());

    // Outbound known-device state is deliberately absent from this gate:
    // only receiver-owned device_auth can admit a reverse service stream.
    assert_eq!(
        registry.snapshot(unknown).expect("unknown snapshot"),
        AuthorizationSnapshot::none()
    );

    {
        let mut write = registry
            .authorize_guard(authorized)
            .await
            .expect("authorization writer");
        write
            .publish(AuthorizationSnapshot {
                status: AuthorizationStatus::Authorized,
                generation: AuthGeneration::new(2).expect("generation two"),
            })
            .expect("advance generation");
    }
    let stale = recheck_inbound_admission(&registry, authorized, admitted.snapshot)
        .expect_err("generation changed before candidate publication");
    assert_eq!(stale.kind(), DomainErrorKind::Unauthorized);
    let stale_stream =
        receiver_generation_for_stream(&registry, authorized, Some(admitted.snapshot.generation))
            .expect_err("an old inbound connection cannot adopt a new generation");
    assert_eq!(stale_stream.kind(), DomainErrorKind::Unauthorized);
    assert_eq!(
        receiver_generation_for_stream(&registry, authorized, None)
            .expect("an outbound candidate checks current reverse direction"),
        AuthGeneration::new(2).expect("generation two")
    );
}

#[test]
fn path_projection_never_turns_direct_ip_into_persistable_route() {
    let direct = TransportAddr::Ip("127.0.0.1:4242".parse().expect("socket address"));
    assert_eq!(classify_transport_addr(&direct), (PathKind::Direct, None));

    let relay_url: RelayUrl = "https://relay.example".parse().expect("relay URL parses");
    let relay_path = TransportAddr::Relay(relay_url);
    let (kind, persistable) = classify_transport_addr(&relay_path);
    assert_eq!(kind, PathKind::Relay);
    assert_eq!(
        persistable.as_ref().map(RelayHint::as_str),
        Some("https://relay.example/")
    );
}

#[cfg(any(unix, windows))]
/// A retired idle connection's retry adopts the transport that an
/// unacknowledged request already dialed, instead of dialing again.
#[tokio::test]
async fn retired_connection_retry_adopts_the_replacement_transport() {
    let fixture = crate::remote_session::cache_tests::Fixture::with_limits(TransportLimits {
        max_pending_dials: 1,
        ..TransportLimits::default()
    })
    .await;
    fixture.query(1).await.expect("warm query");
    let old = fixture
        .broker
        .peer_observation(fixture.remote)
        .await
        .primary
        .expect("primary");
    let deadline = Instant::now() + Duration::from_secs(5);
    let demand = fixture
        .broker
        .demand(fixture.remote, deadline)
        .await
        .expect("demand");
    let replacement = demand.dial_replacement(deadline).expect("admitted");
    while !replacement.is_finished() {
        assert!(Instant::now() < deadline, "replacement dial did not end");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        fixture.connections.load(Ordering::SeqCst),
        2,
        "the replacement reached the host"
    );
    // Another operation takes the only free dial permit once the speculative
    // TLS dial finishes. Adoption still works even with no capacity for fresh.
    let occupied = fixture
        .broker
        .dial_permit()
        .expect("sole pending dial permit");
    let (release, wait) = oneshot::channel();
    let replacement = ReplacementTransport::dial(deadline, async move {
        wait.await.expect("release pending replacement");
        replacement.race(pending()).await
    });
    let held = demand.slot.state.lock().await;
    demand.retire_failed_candidate(old, Some(replacement), false);
    assert_eq!(held.candidates.primary(), Some(old));
    drop(held);
    assert_ne!(
        demand.slot.state.lock().await.candidates.primary(),
        Some(old)
    );
    timeout_until(deadline, async {
        while demand.slot.state.lock().await.replacement.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker adopts pending replacement at capacity");
    release
        .send(())
        .expect("adopted replacement still in flight");
    fixture
        .query(3)
        .await
        .expect("the retry uses the replacement");
    assert_eq!(
        fixture.connections.load(Ordering::SeqCst),
        2,
        "no further connection was dialed"
    );
    drop(occupied);
    drop(demand);
    fixture.close().await;
}
