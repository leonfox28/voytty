use super::*;
use crate::unary::SessionUnaryClient;
use voytty_core::{DaemonIncarnation, OperationLease, SessionId};

#[derive(Debug)]
struct FixtureLookup {
    relay: iroh::RelayUrl,
    stalled: bool,
    delay: std::time::Duration,
}

impl iroh::address_lookup::AddressLookup for FixtureLookup {
    fn resolve(
        &self,
        remote: iroh::EndpointId,
    ) -> Option<
        futures_util::stream::BoxStream<
            'static,
            Result<iroh::address_lookup::Item, iroh::address_lookup::Error>,
        >,
    > {
        if self.stalled {
            return Some(Box::pin(futures_util::stream::pending()));
        }
        let item = iroh::address_lookup::Item::new(
            iroh::EndpointAddr::new(remote)
                .with_relay_url(self.relay.clone())
                .into(),
            "stale-route-fixture",
            None,
        );
        let delay = self.delay;
        Some(Box::pin(futures_util::stream::once(async move {
            tokio::time::sleep(delay).await;
            Ok(item)
        })))
    }
}

#[derive(Clone, Copy)]
enum FreshRoute {
    Stale,
    Stalled,
    Working,
}

#[tokio::test]
async fn unusable_fresh_route_cannot_starve_a_working_verified_route() {
    route_fallback_fixture(true, FreshRoute::Stale).await;
}

#[tokio::test]
async fn unusable_fresh_route_cannot_starve_a_working_pair_ticket_route() {
    route_fallback_fixture(false, FreshRoute::Stale).await;
}

#[tokio::test]
async fn cold_verified_route_does_not_wait_for_a_stalled_lookup() {
    let elapsed = route_fallback_fixture(true, FreshRoute::Stalled).await;
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "a working cold cache was held behind discovery for {elapsed:?}"
    );
}

#[tokio::test]
async fn cold_pair_ticket_does_not_wait_for_a_stalled_lookup() {
    let elapsed = route_fallback_fixture(false, FreshRoute::Stalled).await;
    assert!(elapsed < std::time::Duration::from_secs(1));
}

#[tokio::test]
async fn cold_stale_cache_still_uses_concurrent_fresh_lookup() {
    route_fallback_fixture(true, FreshRoute::Working).await;
}

async fn route_fallback_fixture(verified_cache: bool, fresh: FreshRoute) -> std::time::Duration {
    use iroh::tls::CaTlsConfig;
    use std::time::Duration;
    let alpn = if verified_cache {
        VOYTTY_ALPN
    } else {
        VOYTTY_PAIR_ALPN
    };
    let mut relay_config = iroh_relay::server::ServerConfig::default();
    relay_config.relay = Some(iroh_relay::server::testing::relay_config());
    let server = iroh_relay::server::Server::spawn(relay_config)
        .await
        .expect("isolated loopback relay");
    let good: iroh::RelayUrl = format!(
        "https://{}",
        server.https_addr().expect("relay HTTPS address")
    )
    .parse()
    .expect("loopback relay URL");
    // An unrelated live relay has no route to this host. This models a
    // correctly signed but stale advertised home, not an identity mismatch.
    let mut stale_config = iroh_relay::server::ServerConfig::default();
    stale_config.relay = Some(iroh_relay::server::testing::relay_config());
    let stale_server = iroh_relay::server::Server::spawn(stale_config)
        .await
        .expect("unrelated relay");
    let stale: iroh::RelayUrl = format!(
        "https://{}",
        stale_server.https_addr().expect("stale HTTPS address")
    )
    .parse()
    .expect("stale relay URL");
    let host = Endpoint::builder(presets::Minimal)
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .relay_mode(RelayMode::Custom(
            iroh::RelayConfig::new(good.clone(), None).into(),
        ))
        .clear_ip_transports()
        .alpns(vec![alpn.to_vec()])
        .bind()
        .await
        .expect("relay-only host");
    tokio::time::timeout(Duration::from_secs(5), host.online())
        .await
        .expect("host online");
    let endpoint = Endpoint::builder(presets::Minimal)
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .relay_mode(RelayMode::Custom(
            iroh::RelayConfig::new(good.clone(), None).into(),
        ))
        .clear_ip_transports()
        .address_lookup(FixtureLookup {
            relay: if matches!(fresh, FreshRoute::Working) {
                good.clone()
            } else {
                stale.clone()
            },
            stalled: matches!(fresh, FreshRoute::Stalled),
            delay: Duration::from_millis(100),
        })
        .bind()
        .await
        .expect("relay-only client");
    let controller = IrohController::new(endpoint, "route fallback test").expect("controller");
    let accepting = host.clone();
    let accepted = tokio::spawn(async move {
        accepting
            .accept()
            .await
            .expect("incoming test connection")
            .await
            .expect("authenticated transport")
    });
    let started = Instant::now();
    let good_hint = RelayHint::new(good.to_string()).expect("validated fallback route");
    let known_hint = if matches!(fresh, FreshRoute::Working) {
        RelayHint::new(stale.to_string()).expect("stale cached route")
    } else {
        good_hint.clone()
    };
    let result = controller
        .dial(
            device_from_endpoint_id(host.id()),
            if verified_cache {
                vec![known_hint.clone()]
            } else {
                vec![]
            },
            if verified_cache {
                vec![]
            } else {
                vec![known_hint]
            },
            alpn,
            started + crate::protocol::DEFAULT_DEADLINE,
        )
        .await;
    let elapsed = started.elapsed();
    let connected = result.is_ok();
    let verified_route = result.as_ref().ok().and_then(|(_, relay)| relay.clone());
    eprintln!(
        "verified fallback result: elapsed={elapsed:?} connected={connected} error={:?}",
        result.as_ref().err().map(|error| error.kind())
    );
    if let Ok((connection, _)) = result.as_ref() {
        connection.close(VarInt::from_u32(0), b"fixture complete");
    }
    accepted.abort();
    let _ = accepted.await;
    let _ = tokio::time::timeout(Duration::from_secs(5), controller.shutdown()).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), host.close()).await;
    server.shutdown().await.expect("relay shutdown");
    stale_server.shutdown().await.expect("stale relay shutdown");
    assert!(
        connected,
        "working verified fallback was starved for {elapsed:?}: {}",
        result.expect_err("failed dial").kind().code()
    );
    assert_eq!(
        verified_route,
        Some(good_hint),
        "cache only the selected working relay"
    );
    elapsed
}

/// Signed lookup whose advertised Relay follows the host, like Pkarr.
#[derive(Debug)]
struct MovableLookup {
    relay: Arc<StdMutex<iroh::RelayUrl>>,
    delay: std::time::Duration,
}

impl iroh::address_lookup::AddressLookup for MovableLookup {
    fn resolve(
        &self,
        remote: iroh::EndpointId,
    ) -> Option<
        futures_util::stream::BoxStream<
            'static,
            Result<iroh::address_lookup::Item, iroh::address_lookup::Error>,
        >,
    > {
        let relay = self
            .relay
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let item = iroh::address_lookup::Item::new(
            iroh::EndpointAddr::new(remote).with_relay_url(relay).into(),
            "movable-route-fixture",
            None,
        );
        let delay = self.delay;
        Some(Box::pin(futures_util::stream::once(async move {
            tokio::time::sleep(delay).await;
            Ok(item)
        })))
    }
}

async fn relay_host(secret: SecretKey, relay: iroh::RelayUrl) -> Endpoint {
    let host = Endpoint::builder(presets::Minimal)
        .secret_key(secret)
        .ca_tls_config(iroh::tls::CaTlsConfig::insecure_skip_verify())
        .relay_mode(RelayMode::Custom(
            iroh::RelayConfig::new(relay, None).into(),
        ))
        .clear_ip_transports()
        .alpns(vec![VOYTTY_ALPN.to_vec()])
        .bind()
        .await
        .expect("relay-only host");
    tokio::time::timeout(std::time::Duration::from_secs(5), host.online())
        .await
        .expect("host relay online");
    host
}

async fn hold_incoming(host: Endpoint) {
    let mut held = tokio::task::JoinSet::new();
    while let Some(incoming) = host.accept().await {
        held.spawn(async move {
            if let Ok(connection) = incoming.await {
                connection.closed().await;
            }
        });
    }
}

/// A host incarnation that disappears without a QUIC close, like a killed
/// process or VM. Its runtime is abandoned, so no close frame is written.
async fn spawn_killable_host(
    secret: SecretKey,
    relay: iroh::RelayUrl,
) -> (
    tokio::sync::oneshot::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    let (ready, online) = tokio::sync::oneshot::channel();
    let (kill, killed) = tokio::sync::oneshot::channel::<()>();
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("killable host runtime");
        runtime.block_on(async move {
            let host = relay_host(secret, relay).await;
            tokio::spawn(hold_incoming(host.clone()));
            ready.send(()).expect("report host online");
            let _ = killed.await;
            std::mem::forget(host);
        });
        runtime.shutdown_background();
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), online)
        .await
        .expect("killable host online")
        .expect("killable host reports readiness");
    (kill, thread)
}

/// The host restarts abruptly and returns on another Relay. A faster stale
/// lookup can still advertise the old Relay, as DNS did in the field.
async fn moved_host_fixture(old_connection_open: bool, stale_lookup_first: bool) {
    use std::time::Duration;
    let spawn_relay = || async {
        let mut config = iroh_relay::server::ServerConfig::default();
        config.relay = Some(iroh_relay::server::testing::relay_config());
        let server = iroh_relay::server::Server::spawn(config)
            .await
            .expect("isolated loopback relay");
        let url: iroh::RelayUrl = format!("https://{}", server.https_addr().expect("relay HTTPS"))
            .parse()
            .expect("relay URL");
        (server, url)
    };
    let (old_server, old_relay) = spawn_relay().await;
    let (new_server, new_relay) = spawn_relay().await;
    let advertised = Arc::new(StdMutex::new(old_relay.clone()));
    let mut client = Endpoint::builder(presets::Minimal)
        .ca_tls_config(iroh::tls::CaTlsConfig::insecure_skip_verify())
        .relay_mode(RelayMode::Custom(
            [
                iroh::RelayConfig::new(old_relay.clone(), None),
                iroh::RelayConfig::new(new_relay.clone(), None),
            ]
            .into_iter()
            .collect(),
        ))
        .clear_ip_transports()
        .address_lookup(MovableLookup {
            relay: Arc::clone(&advertised),
            delay: Duration::from_millis(300),
        });
    if stale_lookup_first {
        client = client.address_lookup(FixtureLookup {
            relay: old_relay.clone(),
            stalled: false,
            delay: Duration::from_millis(20),
        });
    }
    let controller = IrohController::new(client.bind().await.expect("client"), "moved host")
        .expect("controller");
    let secret = SecretKey::generate();
    let remote = device_from_endpoint_id(secret.public());
    let verified = RelayHint::new(old_relay.to_string()).expect("verified route");

    let (kill, old_host) = spawn_killable_host(secret.clone(), old_relay.clone()).await;
    let (old_connection, _) = controller
        .dial(
            remote,
            vec![verified.clone()],
            Vec::new(),
            VOYTTY_ALPN,
            Instant::now() + crate::protocol::REMOTE_STARTUP_DEADLINE,
        )
        .await
        .expect("original connection");
    // A real restart takes seconds; the original connection's own lookup
    // (300 ms here) has finished long before the host returns elsewhere.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = kill.send(());
    tokio::task::spawn_blocking(move || old_host.join())
        .await
        .expect("join old host")
        .expect("old host thread");
    if !old_connection_open {
        old_connection.close(VarInt::from_u32(0), b"idle timeout model");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    *advertised
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = new_relay.clone();
    let host = relay_host(secret, new_relay).await;
    let holding = tokio::spawn(hold_incoming(host.clone()));

    let started = Instant::now();
    let result = controller
        .dial(
            remote,
            vec![verified],
            Vec::new(),
            VOYTTY_ALPN,
            started + crate::protocol::REMOTE_STARTUP_DEADLINE,
        )
        .await;
    let elapsed = started.elapsed();
    let error = result.as_ref().err().map(ClientError::kind);
    eprintln!(
        "moved host: old_connection_open={old_connection_open} \
         stale_lookup_first={stale_lookup_first} elapsed={elapsed:?} error={error:?}"
    );
    drop(result);
    drop(old_connection);
    holding.abort();
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(controller.shutdown(), host.close());
    })
    .await;
    old_server.shutdown().await.expect("old relay shutdown");
    new_server.shutdown().await.expect("new relay shutdown");
    assert_eq!(error, None, "moved host was not found after {elapsed:?}");
    // The old connection's idle timeout (30 s on Relay) must not gate it.
    assert!(
        elapsed < crate::protocol::DEFAULT_DEADLINE,
        "moved host took {elapsed:?}"
    );
}

#[tokio::test]
async fn moved_host_is_found_after_the_old_connection_closed_despite_a_stale_first_lookup() {
    moved_host_fixture(false, true).await;
}

#[tokio::test]
async fn moved_host_is_found_while_the_old_connection_is_open_without_a_stale_lookup() {
    moved_host_fixture(true, false).await;
}

#[tokio::test]
async fn moved_host_is_found_while_the_old_connection_is_open_despite_a_stale_first_lookup() {
    moved_host_fixture(true, true).await;
}

async fn exchange_test_greeting(
    client: &Endpoint,
    host: &Endpoint,
    address: iroh::EndpointAddr,
) -> Result<(Connection, Connection), ClientError> {
    let deadline = Instant::now() + crate::protocol::DEFAULT_DEADLINE;
    let incoming = async {
        let connection = host
            .accept()
            .await
            .expect("incoming connection")
            .await
            .expect("authenticated incoming transport");
        let (mut send, mut receive) = connection.accept_bi().await.expect("greeting stream");
        let mut byte = [0];
        receive.read_exact(&mut byte).await.expect("read greeting");
        send.write_all(&byte).await.expect("write greeting reply");
        send.finish().expect("finish greeting reply");
        connection
    };
    let outgoing = async {
        let connection = connect_transport(client, address, VOYTTY_ALPN, deadline).await?;
        let (mut send, mut receive) = connection.open_bi().await.expect("outgoing greeting");
        send.write_all(b"x").await.expect("send greeting");
        send.finish().expect("finish greeting");
        let mut reply = [0];
        receive
            .read_exact(&mut reply)
            .await
            .expect("receive greeting reply");
        assert_eq!(&reply, b"x");
        Ok::<_, ClientError>(connection)
    };
    timeout_until(deadline, async {
        let (remote, local) = tokio::join!(incoming, outgoing);
        local.map(|local| (local, remote))
    })
    .await?
}

#[tokio::test]
async fn new_endpoint_port_with_same_identity_can_exchange_while_old_connection_survives() {
    use std::{net::Ipv4Addr, time::Duration};
    async fn endpoint(secret: SecretKey) -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .secret_key(secret)
            .relay_mode(RelayMode::Disabled)
            .clear_ip_transports()
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("loopback socket")
            .alpns(vec![VOYTTY_ALPN.to_vec()])
            .bind()
            .await
            .expect("isolated endpoint")
    }
    let host = endpoint(SecretKey::generate()).await;
    let identity = SecretKey::generate();
    let old = endpoint(identity.clone()).await;
    let address = iroh::EndpointAddr::new(host.id()).with_ip_addr(host.bound_sockets()[0]);
    let old_connections = exchange_test_greeting(&old, &host, address.clone())
        .await
        .expect("original live connection");
    let new = endpoint(identity).await;
    assert_ne!(old.bound_sockets(), new.bound_sockets());
    let started = Instant::now();
    let result = exchange_test_greeting(&new, &host, address).await;
    eprintln!(
        "same identity/new port result: elapsed={:?} error={:?}",
        started.elapsed(),
        result.as_ref().err().map(|error| error.kind())
    );
    drop(old_connections);
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(old.close(), new.close(), host.close());
    })
    .await;
    result.expect("old transport must not starve the new endpoint's greeting");
}

#[tokio::test]
async fn fresh_relay_endpoint_can_greet_while_same_identity_keeps_an_old_direct_path() {
    use iroh::tls::CaTlsConfig;
    use std::{net::Ipv4Addr, time::Duration};
    let mut config = iroh_relay::server::ServerConfig::default();
    config.relay = Some(iroh_relay::server::testing::relay_config());
    let server = iroh_relay::server::Server::spawn(config)
        .await
        .expect("isolated relay");
    let relay: iroh::RelayUrl = format!("https://{}", server.https_addr().expect("relay HTTPS"))
        .parse()
        .expect("relay URL");
    let builder = |secret| {
        Endpoint::builder(presets::Minimal)
            .secret_key(secret)
            .ca_tls_config(CaTlsConfig::insecure_skip_verify())
            .relay_mode(RelayMode::Custom(
                iroh::RelayConfig::new(relay.clone(), None).into(),
            ))
            .clear_ip_transports()
            .alpns(vec![VOYTTY_ALPN.to_vec()])
    };
    let host = builder(SecretKey::generate())
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .expect("host loopback")
        .bind()
        .await
        .expect("host endpoint");
    tokio::time::timeout(Duration::from_secs(5), host.online())
        .await
        .expect("host relay online");
    let identity = SecretKey::generate();
    let old = builder(identity.clone())
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .expect("old loopback")
        .bind()
        .await
        .expect("old direct endpoint");
    tokio::time::timeout(Duration::from_secs(5), old.online())
        .await
        .expect("old relay registration is live before restart");
    let direct = iroh::EndpointAddr::new(host.id()).with_ip_addr(host.bound_sockets()[0]);
    let old_connections = exchange_test_greeting(&old, &host, direct)
        .await
        .expect("original direct connection");
    assert!(
        old_connections
            .1
            .paths()
            .iter()
            .any(|path| path.is_selected() && matches!(path.remote_addr(), TransportAddr::Ip(_)))
    );
    // A restarted process can only use Relay while the host still retains
    // its previous incarnation's selected Direct path. No prewarming of the
    // new endpoint, and the old connection deliberately remains alive.
    let new = builder(identity)
        .bind()
        .await
        .expect("fresh relay-only endpoint");
    let started = Instant::now();
    let result = exchange_test_greeting(
        &new,
        &host,
        iroh::EndpointAddr::new(host.id()).with_relay_url(relay),
    )
    .await;
    eprintln!(
        "old direct/new relay greeting: elapsed={:?} error={:?}",
        started.elapsed(),
        result.as_ref().err().map(|error| error.kind())
    );
    let selected_relay = result.as_ref().is_ok_and(|(connection, _)| {
        connection
            .paths()
            .iter()
            .any(|path| path.is_selected() && matches!(path.remote_addr(), TransportAddr::Relay(_)))
    });
    drop(old_connections);
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(old.close(), new.close(), host.close());
    })
    .await;
    server.shutdown().await.expect("relay shutdown");
    result.expect("retained Direct path must not starve a fresh Relay handshake");
    assert!(selected_relay, "the new endpoint must actually use Relay");
}

struct Replies {
    frames: StdMutex<VecDeque<DecodedFrame>>,
    requests: StdMutex<Vec<WireKind>>,
}
impl SessionUnaryTransport for Replies {
    fn request(
        &self,
        request: SessionUnaryRequest,
    ) -> TransportFuture<'_, Result<DecodedFrame, ClientError>> {
        self.requests
            .lock()
            .expect("request capture lock")
            .push(request.request_kind);
        let frame = self
            .frames
            .lock()
            .expect("reply fixture lock")
            .pop_front()
            .expect("expected unary request");
        Box::pin(async move { session_unary_result(frame) })
    }
}
fn frame(kind: WireKind, message: &impl prost::Message) -> DecodedFrame {
    let bytes = voytty_proto::encode_message(kind, 1, 0, message).expect("encode fixture response");
    FrameDecoder::new()
        .feed(&bytes)
        .expect("decode fixture response")
        .pop()
        .expect("one fixture frame")
}
fn error(kind: DomainErrorKind) -> DecodedFrame {
    frame(
        WireKind::ServiceErrorResponse,
        &v2::ServiceError {
            code: kind.code().to_owned(),
            message: "PRIVATE_REMOTE_DIAGNOSTIC".to_owned(),
        },
    )
}
#[tokio::test]
async fn restarted_daemon_error_retires_unary_lease_without_replaying_mutation() {
    let lease = |incarnation| {
        frame(
            WireKind::SessionOperationLeaseResponse,
            &v2::SessionOperationLeaseResponse {
                lease: Some(
                    OperationLease {
                        daemon_incarnation: DaemonIncarnation::from_array([incarnation; 16]),
                        ordinal: 1,
                    }
                    .into(),
                ),
            },
        )
    };
    let transport = Replies {
        frames: StdMutex::new(VecDeque::from([
            lease(1),
            error(DomainErrorKind::OperationOutcomeUnknown),
            lease(2),
            error(DomainErrorKind::SessionNotFound),
        ])),
        requests: StdMutex::new(Vec::new()),
    };
    let client = SessionUnaryClient::default();
    let target = ResolvedSessionTarget::device(DeviceId::from_array([7; 32]));
    let session = SessionId::from_array([1; 16]);
    let first = client
        .close_session_at(&transport, target, session)
        .await
        .expect_err("definitive fixture service failure");
    assert_eq!(first.kind(), DomainErrorKind::OperationOutcomeUnknown);
    assert!(!first.detail().contains("PRIVATE_REMOTE_DIAGNOSTIC"));
    assert_eq!(
        *transport.requests.lock().expect("request capture lock"),
        vec![
            WireKind::SessionOperationLeaseRequest,
            WireKind::SessionCloseRequest
        ]
    );
    // Only a new explicit call acquires the new daemon's lease. Its definitive
    // service failure remains typed rather than being decoded as Session data.
    let second = client
        .close_session_at(&transport, target, session)
        .await
        .expect_err("definitive fixture service failure");
    assert_eq!(second.kind(), DomainErrorKind::SessionNotFound);
    assert_eq!(
        *transport.requests.lock().expect("request capture lock"),
        vec![
            WireKind::SessionOperationLeaseRequest,
            WireKind::SessionCloseRequest,
            WireKind::SessionOperationLeaseRequest,
            WireKind::SessionCloseRequest
        ]
    );
    for kind in [
        DomainErrorKind::Unauthorized,
        DomainErrorKind::SessionOccupied,
    ] {
        assert_eq!(
            session_unary_result(error(kind))
                .expect_err("typed service error")
                .kind(),
            kind
        );
    }
}

#[test]
fn path_sample_hysteresis_suppresses_small_rtt_jitter_and_emits_real_changes() {
    let direct = v2::TerminalConnectionPath::Direct as i32;
    let relay = v2::TerminalConnectionPath::Relay as i32;

    // Initial observation always emits.
    assert!(should_emit_path_sample(None, (relay, Some(10))));

    // Low-RTT jitter below 5 ms is suppressed; >= 5 ms emits.
    assert!(!should_emit_path_sample(
        Some((direct, Some(10))),
        (direct, Some(11))
    ));
    assert!(!should_emit_path_sample(
        Some((direct, Some(10))),
        (direct, Some(14))
    ));
    assert!(!should_emit_path_sample(
        Some((direct, Some(10))),
        (direct, Some(6))
    ));
    assert!(should_emit_path_sample(
        Some((direct, Some(10))),
        (direct, Some(15))
    ));
    assert!(should_emit_path_sample(
        Some((direct, Some(10))),
        (direct, Some(5))
    ));

    // High-RTT jitter below 10% is suppressed; >= 10% emits.
    assert!(!should_emit_path_sample(
        Some((relay, Some(200))),
        (relay, Some(219))
    ));
    assert!(should_emit_path_sample(
        Some((relay, Some(200))),
        (relay, Some(220))
    ));

    // Path class transitions and RTT availability transitions always emit.
    assert!(should_emit_path_sample(
        Some((relay, Some(10))),
        (direct, Some(10))
    ));
    assert!(should_emit_path_sample(
        Some((direct, None)),
        (direct, Some(10))
    ));
    assert!(should_emit_path_sample(
        Some((direct, Some(10))),
        (direct, None)
    ));
    assert!(!should_emit_path_sample(
        Some((direct, None)),
        (direct, None)
    ));
}

#[tokio::test(start_paused = true)]
async fn retry_backoff_doubles_to_cap_and_stops_on_cancel_or_deadline() {
    use std::time::Duration;

    let cancelled = CancellationToken::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut backoff = OPEN_BI_RETRY_INITIAL;
    let expected_delays = [20_u64, 40, 80, 160, 320, 500, 500];
    for expected_ms in expected_delays {
        let before = tokio::time::Instant::now();
        wait_retry_backoff(&cancelled, deadline, &mut backoff)
            .await
            .expect("backoff step completes before deadline");
        assert_eq!(
            tokio::time::Instant::now() - before,
            Duration::from_millis(expected_ms)
        );
    }

    // Cancellation interrupts backoff immediately.
    let trigger = cancelled.clone();
    let stopper = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        trigger.cancel();
    });
    let before = tokio::time::Instant::now();
    let error = wait_retry_backoff(&cancelled, deadline, &mut backoff)
        .await
        .expect_err("cancelled backoff must stop");
    assert_eq!(error.kind(), DomainErrorKind::Cancelled);
    assert_eq!(
        tokio::time::Instant::now() - before,
        Duration::from_millis(5)
    );
    stopper.await.expect("stopper");

    // Reaching deadline returns DeadlineExceeded without further retries.
    let active = CancellationToken::new();
    let short_deadline = Instant::now() + Duration::from_millis(10);
    let mut large_backoff = OPEN_BI_RETRY_MAX;
    let error = wait_retry_backoff(&active, short_deadline, &mut large_backoff)
        .await
        .expect_err("expired backoff must time out");
    assert_eq!(error.kind(), DomainErrorKind::DeadlineExceeded);
}
