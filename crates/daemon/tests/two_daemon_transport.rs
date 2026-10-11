//! Linux-only real-Iroh evidence for two task-private daemon/network owners.
//!
//! Developer workstations compile this target with `cargo test --no-run`.
//! The real loopback case is ignored outside Linux before any Endpoint can bind.
//! Persisted owner config is `OfficialN0`; runtime evidence here is only
//! relay-disabled loopback/direct Iroh, not an official-n0 Relay traversal.

use std::time::{Duration, Instant};

use zterm_core::{AuthorizationStatus, DomainErrorKind, TransportLimits};
use zterm_daemon::connection_broker::{AuthenticatedBiStream, StreamPurpose};
use zterm_daemon::network::NetworkObserver;
use zterm_proto::{FrameDecoder, WireKind, encode_message, v2};

#[path = "support/two_daemon_fixture.rs"]
mod two_daemon_fixture;
use two_daemon_fixture::PreparedDaemonOwner;

async fn wait_for_primary_observation(observer: NetworkObserver, deadline: Instant) {
    // Peer state and the public snapshot are published separately. Wait on
    // the channel being asserted, without extending the operation's deadline.
    let mut receiver = observer.subscribe();
    loop {
        if receiver.borrow_and_update().primary_connection_count == 1 {
            return;
        }
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), receiver.changed())
            .await
            .expect("public primary observation deadline")
            .expect("network observation publisher remains live");
    }
}

async fn normal_setup_with_delays(
    transport_delay: Duration,
    hello_delay: Duration,
) -> (bool, Duration) {
    assert_linux_before_bind();
    let host = PreparedDaemonOwner::new("slow-host");
    let controller = PreparedDaemonOwner::new("slow-controller");
    let host_id = host.device_id();
    let client_id = controller.device_id();
    let host = host
        .bind_with_setup_delays(
            &[(client_id, "slow-controller")],
            &[],
            TransportLimits::default(),
            transport_delay,
            hello_delay,
        )
        .await;
    let controller = controller
        .bind(
            &[],
            &[(host_id, "slow-host", "slow-host")],
            TransportLimits::default(),
        )
        .await;
    controller
        .broker()
        .set_test_route(host_id, host.direct_address())
        .expect("direct fixture route");
    let started = Instant::now();
    let deadline = started + zterm_client::protocol::REMOTE_STARTUP_DEADLINE;
    let demand = controller
        .broker()
        .demand(host_id, deadline)
        .await
        .expect("known host demand");
    let result = demand.confirm_authorization(deadline).await;
    let elapsed = started.elapsed();
    let success = result.is_ok();
    eprintln!(
        "normal setup transport={transport_delay:?} hello={hello_delay:?}: elapsed={elapsed:?}, error={:?}",
        result.as_ref().err()
    );
    drop(demand);
    controller.shutdown().await;
    host.shutdown().await;
    (success, elapsed)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "real Iroh loopback is Linux CI only"
)]
async fn startup_budget_normal_transport_survives_eleven_seconds() {
    let (success, elapsed) =
        normal_setup_with_delays(Duration::from_secs(11), Duration::from_secs(1)).await;
    assert!(
        success,
        "normal transport must not be cut off at ten seconds"
    );
    assert!(elapsed >= Duration::from_secs(12) && elapsed < Duration::from_secs(15));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "real Iroh loopback is Linux CI only"
)]
async fn startup_budget_normal_authentication_survives_six_seconds() {
    let (success, elapsed) = normal_setup_with_delays(Duration::ZERO, Duration::from_secs(6)).await;
    assert!(
        success,
        "authentication uses the startup deadline before the operation timer begins"
    );
    assert!(elapsed >= Duration::from_secs(6) && elapsed < Duration::from_secs(15));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "real Iroh loopback is Linux CI only"
)]
async fn startup_budget_normal_setup_still_has_one_total_deadline() {
    let (success, elapsed) =
        normal_setup_with_delays(Duration::from_secs(8), Duration::from_secs(8)).await;
    assert!(
        !success,
        "separate setup phases cannot renew the total deadline"
    );
    assert!(elapsed >= Duration::from_secs(15) && elapsed < Duration::from_secs(16));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "real Iroh loopback is Linux CI only"
)]
async fn restarted_owner_replaces_stale_primary_and_rejects_late_old_handshake() {
    use zterm_core::{Capabilities, ConnectionAttemptId};
    use zterm_daemon::connection_broker::ConnectionIdentity;
    assert_linux_before_bind();
    let host = PreparedDaemonOwner::new("host");
    let controller = PreparedDaemonOwner::new("controller");
    let controller_id = controller.device_id();
    let identity = || {
        ConnectionIdentity::new(
            controller_id,
            "controller",
            "test",
            "linux",
            Capabilities::from_bits_retain(Capabilities::SESSION_SERVICE),
        )
        .expect("identity")
    };
    let old_identity = identity();
    let new_identity = identity();
    let host = host
        .bind(
            &[(controller_id, "controller")],
            &[],
            TransportLimits::default(),
        )
        .await;
    let controller = controller.bind(&[], &[], TransportLimits::default()).await;
    let old = controller
        .connect_with_identity(
            &host,
            &old_identity,
            ConnectionAttemptId::from_array([1; 16]),
        )
        .await
        .expect("old Hello");
    host.wait_for_primary(controller_id, Instant::now() + Duration::from_secs(5))
        .await
        .expect("old primary");
    // A larger attempt must win over a dead previous incarnation, while the
    // ordinary lexicographic arbitration within one incarnation stays intact.
    let fresh = controller
        .connect_with_identity(
            &host,
            &new_identity,
            ConnectionAttemptId::from_array([250; 16]),
        )
        .await
        .expect("restart Hello");
    tokio::time::timeout(Duration::from_secs(3), old.closed())
        .await
        .expect("stale primary retired");
    assert!(
        fresh.close_reason().is_none(),
        "restart connection was incorrectly discarded"
    );
    let late_old = controller
        .connect_with_identity(
            &host,
            &old_identity,
            ConnectionAttemptId::from_array([2; 16]),
        )
        .await;
    assert!(
        late_old.is_err(),
        "a delayed retired incarnation must not displace the new one"
    );
    assert!(fresh.close_reason().is_none());
    let (mut send, mut recv) = fresh.open_bi().await.expect("fresh business stream");
    let request = encode_message(
        WireKind::SessionListRequest,
        701,
        0,
        &v2::SessionListRequest::default(),
    )
    .expect("request");
    send.write_all(&request).await.expect("write request");
    send.finish().expect("finish request");
    let frame = zterm_client::handshake::read_one_frame(
        &mut recv,
        16 * 1024,
        Instant::now() + Duration::from_secs(5),
    )
    .await
    .expect("fresh service reply");
    assert_eq!(frame.request_id, 701);
    fresh.close(0_u32.into(), b"test complete");
    controller.shutdown().await;
    host.shutdown().await;
}

/// A reverse direction needs its own Welcome even when its new connection
/// loses arbitration to an already active connection from the smaller ID.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "real Iroh loopback is Linux CI only"
)]
async fn reverse_confirmation_survives_losing_duplicate_arbitration() {
    assert_linux_before_bind();
    let first = PreparedDaemonOwner::new("first");
    let second = PreparedDaemonOwner::new("second");
    let (lower, higher) = if first.device_id() < second.device_id() {
        (first, second)
    } else {
        (second, first)
    };
    let lower_id = lower.device_id();
    let higher_id = higher.device_id();
    let limits = TransportLimits::default();
    let lower = lower
        .bind(
            &[(higher_id, "higher")],
            &[(higher_id, "higher", "higher")],
            limits,
        )
        .await;
    let higher = higher
        .bind(
            &[(lower_id, "lower")],
            &[(lower_id, "lower", "lower")],
            limits,
        )
        .await;
    lower
        .broker()
        .set_test_route(higher_id, higher.direct_address())
        .expect("lower route");
    higher
        .broker()
        .set_test_route(lower_id, lower.direct_address())
        .expect("higher route");
    let deadline = Instant::now() + Duration::from_secs(15);
    let forward = lower
        .broker()
        .demand(higher_id, deadline)
        .await
        .expect("forward demand");
    forward
        .confirm_authorization(deadline)
        .await
        .expect("forward confirmation");
    higher
        .wait_for_primary(lower_id, deadline)
        .await
        .expect("inbound primary");
    let primary = higher
        .broker()
        .peer_observation(lower_id)
        .await
        .primary
        .expect("primary");
    let reverse = higher
        .broker()
        .demand(lower_id, deadline)
        .await
        .expect("reverse demand");
    reverse
        .confirm_authorization(deadline)
        .await
        .expect("reverse confirmation must receive Welcome before duplicate close");
    assert_eq!(
        higher.broker().peer_observation(lower_id).await.primary,
        Some(primary)
    );
    let mut stream = reverse
        .open_bi(StreamPurpose::Service, deadline)
        .await
        .expect("reverse service stream");
    complete_fallback_service_exchange(&mut stream, 703).await;
    drop(stream);
    drop(reverse);
    drop(forward);
    higher.shutdown().await;
    lower.shutdown().await;
}

#[test]
fn production_cli_has_no_state_override_argument() {
    let cli = include_str!("../../cli/src/lib.rs");
    let main = include_str!("../../cli/src/main.rs");
    for forbidden in [
        "--state-dir",
        "--state-path",
        "--identity-path",
        "--socket-path",
        "ZTERM_HOME",
        "UserPaths::for_test",
    ] {
        assert!(
            !cli.contains(forbidden) && !main.contains(forbidden),
            "production CLI must not expose task-private state override {forbidden}"
        );
    }
    assert!(cli.contains("internal_daemon"));
    assert!(main.contains("run_internal_daemon"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "real Iroh loopback is Linux CI only"
)]
async fn two_daemon_owners_reuse_endpoint_for_pair_and_normal_confirmation() {
    assert_linux_before_bind();
    let prepared_a = PreparedDaemonOwner::new("host-a");
    let prepared_b = PreparedDaemonOwner::new("controller-b");
    let a_id = prepared_a.device_id();
    let b_id = prepared_b.device_id();
    let limits = TransportLimits::default();

    let a = prepared_a
        .bind(&[(b_id, "controller-b")], &[], limits)
        .await;
    let b = prepared_b
        .bind(&[], &[(a_id, "host-a", "host-a")], limits)
        .await;
    let a_sockets = a.bound_sockets();
    let b_sockets = b.bound_sockets();
    assert!(!a_sockets.is_empty());
    assert!(!b_sockets.is_empty());
    assert!(
        a_id != b_id,
        "real-Iroh owners must have distinct task-private identities"
    );

    assert!(a.committed_is_official_n0());
    assert!(b.committed_is_official_n0());
    let profile_before = b.committed_profile_summary();

    let before_pair_a = a.broker().observe().snapshot();
    let before_pair_b = b.broker().observe().snapshot();
    assert_eq!(before_pair_a.primary_connection_count, 0);
    assert_eq!(before_pair_b.primary_connection_count, 0);
    let pair_deadline = Instant::now() + Duration::from_secs(15);
    b.probe_pair_connection(&a, pair_deadline)
        .await
        .expect("full pair-ALPN TLS over the controller's sole Endpoint");

    let after_pair_a = a.broker().observe().snapshot();
    let after_pair_b = b.broker().observe().snapshot();
    assert_eq!(after_pair_a.authenticated_connection_count, 0);
    assert_eq!(after_pair_a.primary_connection_count, 0);
    assert_eq!(after_pair_a.active_stream_count, 0);
    assert_eq!(after_pair_b.authenticated_connection_count, 0);
    assert_eq!(after_pair_b.primary_connection_count, 0);
    assert_eq!(after_pair_b.active_stream_count, 0);
    assert!(
        a.bound_sockets() == a_sockets,
        "host task-private Endpoint socket ownership changed unexpectedly"
    );
    assert!(
        b.bound_sockets() == b_sockets,
        "controller task-private Endpoint socket ownership changed unexpectedly"
    );

    b.broker()
        .set_test_route(a_id, a.direct_address())
        .expect("task-private direct normal route");
    let normal_deadline = Instant::now() + Duration::from_secs(15);
    let first = b
        .broker()
        .demand(a_id, normal_deadline)
        .await
        .expect("durable normal confirmation demand");
    let first_confirmation = first
        .confirm_authorization(normal_deadline)
        .await
        .expect("normal authorization confirmation");
    assert!(
        first_confirmation.remote() == a_id,
        "normal confirmation must authenticate the expected task-private owner"
    );
    assert!(first_confirmation.verified_relay().is_none());
    let first_primary = b
        .broker()
        .peer_observation(a_id)
        .await
        .primary
        .expect("first promoted primary");

    // A durable demand joins the already-promoted connection. Confirmation
    // selects Welcome state only and therefore opens no application stream.
    let reused = b
        .broker()
        .demand(a_id, normal_deadline)
        .await
        .expect("durable known-device demand");
    let second_confirmation = reused
        .confirm_authorization(normal_deadline)
        .await
        .expect("reused normal authorization confirmation");
    assert!(
        second_confirmation.remote() == first_confirmation.remote()
            && second_confirmation.generation() == first_confirmation.generation()
            && second_confirmation.verified_relay().is_none(),
        "reused normal confirmation must preserve the authenticated direct owner and generation"
    );
    let second_observation = b.broker().peer_observation(a_id).await;
    assert!(
        second_observation.primary == Some(first_primary),
        "the second demand must reuse the first promoted primary"
    );
    assert_eq!(second_observation.candidate_count, 1);
    assert_eq!(second_observation.active_stream_count, 0);
    a.wait_for_primary(b_id, normal_deadline)
        .await
        .expect("host observes the same normal primary");
    wait_for_primary_observation(a.broker().observe(), normal_deadline).await;
    wait_for_primary_observation(b.broker().observe(), normal_deadline).await;
    assert_eq!(a.broker().observe().snapshot().primary_connection_count, 1);
    assert_eq!(b.broker().observe().snapshot().primary_connection_count, 1);

    let mut first_service = first
        .open_bi(StreamPurpose::Service, normal_deadline)
        .await
        .expect("open first service stream on the promoted primary");
    let mut second_service = reused
        .open_bi(StreamPurpose::Service, normal_deadline)
        .await
        .expect("open second service stream on the same promoted primary");
    assert_eq!(first_service.candidate(), first_primary);
    assert_eq!(second_service.candidate(), first_primary);
    let two_streams = b.broker().peer_observation(a_id).await;
    assert_eq!(two_streams.primary, Some(first_primary));
    assert_eq!(two_streams.candidate_count, 1);
    assert_eq!(two_streams.active_stream_count, 2);
    assert_eq!(b.broker().observe().snapshot().primary_connection_count, 1);

    complete_fallback_service_exchange(&mut first_service, 701).await;
    drop(first_service);
    let one_stream = b.broker().peer_observation(a_id).await;
    assert_eq!(one_stream.primary, Some(first_primary));
    assert_eq!(one_stream.candidate_count, 1);
    assert_eq!(one_stream.active_stream_count, 1);
    complete_fallback_service_exchange(&mut second_service, 702).await;
    drop(second_service);
    let no_streams = b.broker().peer_observation(a_id).await;
    assert_eq!(no_streams.primary, Some(first_primary));
    assert_eq!(no_streams.candidate_count, 1);
    assert_eq!(no_streams.active_stream_count, 0);
    assert_eq!(b.broker().observe().snapshot().primary_connection_count, 1);

    let host_auth = a.authorization_snapshot(b_id);
    assert_eq!(host_auth.status, AuthorizationStatus::Authorized);
    assert_eq!(host_auth.generation, first_confirmation.generation());
    assert_eq!(
        b.authorization_snapshot(a_id).status,
        AuthorizationStatus::None,
        "one-way normal transport must not grant reverse authorization"
    );
    assert!(
        b.known_device(a_id, normal_deadline)
            .expect("controller keeps host in its address book")
            .route_cache
            .is_none(),
        "a task-private direct fixture route must never persist a direct IP"
    );

    let profile_after = b.committed_profile_summary();
    assert!(
        profile_after == profile_before,
        "persistent infrastructure selection changed during loopback transport"
    );
    for socket in b_sockets {
        assert!(
            !format!("{:?}", b.broker().observe().snapshot()).contains(&socket.to_string()),
            "redacted network observation must not expose a direct address"
        );
    }

    drop(first);
    drop(reused);
    b.shutdown().await;
    a.shutdown().await;
}

async fn complete_fallback_service_exchange(stream: &mut AuthenticatedBiStream, request_id: u64) {
    let request = encode_message(
        WireKind::SessionListRequest,
        request_id,
        1_000,
        &v2::SessionListRequest {
            target: Some(v2::TargetSelector {
                target: Some(v2::target_selector::Target::Local(true)),
            }),
        },
    )
    .expect("encode valid service request");
    stream
        .send
        .write_all(&request)
        .await
        .expect("write one service request");
    stream.send.finish().expect("finish one service request");
    let response = stream
        .recv
        .read_to_end(zterm_proto::MAX_FRAME_BYTES)
        .await
        .expect("read one service response");
    let mut decoder = FrameDecoder::new();
    let mut frames = decoder
        .feed(&response)
        .expect("decode one service response");
    decoder
        .finish()
        .expect("service response ends on a complete frame");
    assert_eq!(frames.len(), 1);
    let frame = frames.remove(0);
    assert_eq!(frame.kind, WireKind::ServiceErrorResponse);
    assert_eq!(frame.request_id, request_id);
    let error: v2::ServiceError = frame
        .decode_message(WireKind::ServiceErrorResponse)
        .expect("decode fallback service response");
    assert_eq!(error.code, DomainErrorKind::ServiceNotImplemented.code());
}

fn assert_linux_before_bind() {
    #[cfg(not(target_os = "linux"))]
    panic!("real Iroh loopback tests must fail before bind outside Linux");
}
