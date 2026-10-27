use super::*;
use crate::handshake::{read_one_frame, write_handshake_message};
use iroh::tls::CaTlsConfig;
use prost::Message;
use std::{sync::atomic::AtomicUsize, time::Duration};
use voytty_core::ConnectionWelcome;

const ECHO_REQUEST: u64 = 10_000;

struct Fixture {
    controller: IrohController,
    host: Endpoint,
    remote: DeviceId,
    relay: iroh_relay::server::Server,
    serving: tokio::task::JoinHandle<()>,
    connections: Arc<AtomicUsize>,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_handshake_delay(Duration::ZERO).await
    }

    async fn with_handshake_delay(handshake_delay: Duration) -> Self {
        Self::with_setup_delays(Duration::ZERO, handshake_delay).await
    }

    async fn with_setup_delays(transport_delay: Duration, handshake_delay: Duration) -> Self {
        let mut config = iroh_relay::server::ServerConfig::default();
        config.relay = Some(iroh_relay::server::testing::relay_config());
        let relay = iroh_relay::server::Server::spawn(config)
            .await
            .expect("local relay");
        let url: iroh::RelayUrl = format!("https://{}", relay.https_addr().expect("HTTPS"))
            .parse()
            .expect("local relay URL");
        let builder = || {
            Endpoint::builder(presets::Minimal)
                .ca_tls_config(CaTlsConfig::insecure_skip_verify())
                .relay_mode(RelayMode::Custom(
                    iroh::RelayConfig::new(url.clone(), None).into(),
                ))
                .clear_ip_transports()
        };
        let host = builder()
            .alpns(vec![VOYTTY_ALPN.to_vec(), VOYTTY_PAIR_ALPN.to_vec()])
            .bind()
            .await
            .expect("host");
        tokio::time::timeout(Duration::from_secs(5), host.online())
            .await
            .expect("online");
        let controller =
            IrohController::new(builder().bind().await.expect("endpoint"), "cache test")
                .expect("controller");
        let remote = device_from_endpoint_id(host.id());
        controller
            .remember_host(
                remote,
                vec![RelayHint::new(url.to_string()).expect("route")],
            )
            .expect("known host");
        let connections = Arc::new(AtomicUsize::new(0));
        let count = connections.clone();
        let accepting = host.clone();
        let client = controller.device_id();
        let serving = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            while let Some(incoming) = accepting.accept().await {
                let index = count.fetch_add(1, Ordering::SeqCst);
                handlers.spawn(async move {
                    tokio::time::sleep(transport_delay).await;
                    let connection = incoming.await.expect("authenticated connection");
                    assert_eq!(device_from_endpoint_id(connection.remote_id()), client);
                    if connection.alpn() == VOYTTY_PAIR_ALPN {
                        connection.closed().await;
                        return;
                    }
                    let deadline = Instant::now() + Duration::from_secs(15);
                    let (mut send, mut recv) = connection.accept_bi().await.expect("Hello stream");
                    let hello = read_one_frame(&mut recv, 16384, deadline)
                        .await
                        .expect("Hello");
                    assert_eq!(hello.kind, WireKind::ConnectionHello);
                    tokio::time::sleep(handshake_delay).await;
                    let welcome = ConnectionWelcome::new(
                        voytty_proto::WIRE_MAJOR,
                        Capabilities::from_bits_retain(
                            Capabilities::SESSION_SERVICE | Capabilities::TERMINAL_SERVICE,
                        ),
                        "test host",
                        "test",
                        "test",
                        AuthGeneration::new(1).expect("generation"),
                    )
                    .expect("Welcome");
                    write_handshake_message(
                        &mut send,
                        WireKind::ConnectionWelcome,
                        &v2::ConnectionWelcome::from(&welcome),
                        deadline,
                    )
                    .await
                    .expect("send Welcome");
                    send.finish().expect("finish Welcome");
                    let ordinary = Arc::new(AtomicUsize::new(0));
                    let mut streams = tokio::task::JoinSet::new();
                    while let Ok((mut send, mut recv)) = connection.accept_bi().await {
                        let connection = connection.clone();
                        let ordinary = ordinary.clone();
                        streams.spawn(async move {
                            loop {
                                let Ok(frame) = read_one_frame(
                                    &mut recv,
                                    16384,
                                    Instant::now() + Duration::from_secs(30),
                                )
                                .await
                                else {
                                    return;
                                };
                                assert_eq!(frame.kind, WireKind::SessionListRequest);
                                let echo = frame.request_id == ECHO_REQUEST;
                                if !echo
                                    && index == 0
                                    && ordinary.fetch_add(1, Ordering::SeqCst) > 0
                                {
                                    // Application requests on this connection stop
                                    // receiving replies, while QUIC and an optional
                                    // independent stream remain alive.
                                    connection.closed().await;
                                    return;
                                }
                                let bytes = voytty_proto::encode_message(
                                    WireKind::SessionListResponse,
                                    frame.request_id,
                                    0,
                                    &v2::SessionListResponse {
                                        sessions: Vec::new(),
                                    },
                                )
                                .expect("reply");
                                if send.write_all(&bytes).await.is_err() {
                                    return;
                                }
                                if !echo {
                                    let _ = send.finish();
                                    return;
                                }
                            }
                        });
                    }
                });
            }
        });
        Self {
            controller,
            host,
            remote,
            relay,
            serving,
            connections,
        }
    }

    async fn list(&self) -> Result<DecodedFrame, ClientError> {
        self.controller
            .request(SessionUnaryRequest {
                target: ResolvedSessionTarget::device(self.remote),
                request_kind: WireKind::SessionListRequest,
                response_kind: WireKind::SessionListResponse,
                payload: v2::SessionListRequest {
                    target: Some(v2::TargetSelector {
                        target: Some(v2::target_selector::Target::Device(self.remote.into())),
                    }),
                }
                .encode_to_vec(),
                deadline: Duration::from_secs(5),
                startup_deadline: crate::protocol::REMOTE_STARTUP_DEADLINE,
                mutation_or_lease_retry: false,
            })
            .await
    }

    async fn close(self) {
        self.serving.abort();
        let _ = self.serving.await;
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(self.controller.shutdown(), self.host.close());
        })
        .await;
        self.relay.shutdown().await.expect("relay shutdown");
    }
}

async fn echo(stream: &mut IrohSessionIo) {
    let bytes = voytty_proto::encode_message(
        WireKind::SessionListRequest,
        ECHO_REQUEST,
        0,
        &v2::SessionListRequest { target: None },
    )
    .expect("stream probe");
    stream
        .send
        .write_all(&bytes)
        .await
        .expect("live stream write");
    let frame = read_one_frame(
        &mut stream.recv,
        16384,
        Instant::now() + Duration::from_secs(1),
    )
    .await
    .expect("another stream must still exchange data after the query timeout");
    assert_eq!(frame.request_id, ECHO_REQUEST);
}

#[tokio::test]
async fn startup_budget_slow_authentication_succeeds_and_cached_query_still_expires_in_five_seconds()
 {
    let fixture = Fixture::with_handshake_delay(Duration::from_secs(6)).await;
    let client = crate::unary::SessionUnaryClient::default();
    let target = ResolvedSessionTarget::device(fixture.remote);
    let started = Instant::now();
    let first = client.list_sessions_at(&fixture.controller, target).await;
    let cold_elapsed = started.elapsed();
    let started = Instant::now();
    let second = client.list_sessions_at(&fixture.controller, target).await;
    let warm_elapsed = started.elapsed();
    let connections = fixture.connections.load(Ordering::SeqCst);
    fixture.close().await;
    first.expect("six-second authentication fits the initial budget");
    assert!(
        cold_elapsed >= Duration::from_secs(6) && cold_elapsed < Duration::from_secs(15),
        "{cold_elapsed:?}"
    );
    assert_eq!(
        second
            .expect_err("cached silent query still expires")
            .kind(),
        DomainErrorKind::DeadlineExceeded
    );
    assert!(
        warm_elapsed >= Duration::from_secs(5) && warm_elapsed < Duration::from_secs(6),
        "{warm_elapsed:?}"
    );
    assert_eq!(
        connections, 1,
        "an expired operation cannot acquire a fresh startup budget for its retry"
    );
}

#[tokio::test]
async fn startup_budget_transport_can_take_more_than_ten_seconds() {
    let fixture = Fixture::with_setup_delays(Duration::from_secs(11), Duration::from_secs(1)).await;
    let started = Instant::now();
    let result = fixture.list().await;
    let elapsed = started.elapsed();
    fixture.close().await;
    eprintln!(
        "slow normal transport: {elapsed:?}, result={:?}",
        result.as_ref().err()
    );
    result.expect("eleven-second transport plus authentication fits the startup budget");
    assert!(elapsed >= Duration::from_secs(12) && elapsed < Duration::from_secs(15));
}

#[tokio::test]
async fn startup_budget_transport_and_authentication_share_fifteen_seconds() {
    let fixture = Fixture::with_setup_delays(Duration::from_secs(11), Duration::from_secs(6)).await;
    let started = Instant::now();
    let result = fixture.list().await;
    let elapsed = started.elapsed();
    fixture.close().await;
    eprintln!(
        "total normal startup: {elapsed:?}, result={:?}",
        result.as_ref().err()
    );
    assert_eq!(
        result
            .expect_err("setup cannot renew the total deadline")
            .kind(),
        DomainErrorKind::DeadlineExceeded
    );
    assert!(elapsed >= Duration::from_secs(15) && elapsed < Duration::from_secs(16));
}

#[tokio::test]
async fn startup_budget_transport_change_preserves_pairing_attempt_limit() {
    let fixture = Fixture::with_setup_delays(Duration::from_secs(11), Duration::ZERO).await;
    let peer = fixture
        .controller
        .peer(fixture.remote, None)
        .expect("known host");
    let routes = peer.routes.lock().expect("route cache lock").clone();
    let started = Instant::now();
    let result = fixture
        .controller
        .dial(
            fixture.remote,
            routes,
            Vec::new(),
            VOYTTY_PAIR_ALPN,
            started + crate::protocol::REMOTE_STARTUP_DEADLINE,
        )
        .await;
    let elapsed = started.elapsed();
    if let Ok((connection, _)) = &result {
        connection.close(VarInt::from_u32(0), b"fixture complete");
    }
    fixture.close().await;
    assert_eq!(
        result
            .expect_err("pairing retains its ten-second attempt cap")
            .kind(),
        DomainErrorKind::DeadlineExceeded
    );
    assert!(elapsed >= Duration::from_secs(10) && elapsed < Duration::from_secs(11));
}

#[tokio::test]
async fn failed_idle_unary_connection_is_not_reused_by_the_next_query() {
    let fixture = Fixture::new().await;
    fixture
        .list()
        .await
        .expect("first query populates the authenticated cache");
    let error = fixture
        .list()
        .await
        .expect_err("unanswered request expires");
    assert_eq!(error.kind(), DomainErrorKind::DeadlineExceeded);
    let next = fixture.list().await;
    let connections = fixture.connections.load(Ordering::SeqCst);
    fixture.close().await;
    next.expect("next independent query must authenticate a fresh connection");
    assert_eq!(connections, 2);
}

#[tokio::test]
async fn failed_query_preserves_another_live_stream_until_it_is_released() {
    let fixture = Fixture::new().await;
    fixture.list().await.expect("warm authenticated cache");
    let peer = fixture.controller.peer(fixture.remote, None).expect("peer");
    let mut held = fixture
        .controller
        .service_stream(
            fixture.remote,
            &peer,
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .expect("another admitted stream");
    echo(&mut held).await;
    let error = fixture
        .list()
        .await
        .expect_err("unanswered concurrent request expires");
    assert_eq!(error.kind(), DomainErrorKind::DeadlineExceeded);
    echo(&mut held).await;
    assert_eq!(fixture.connections.load(Ordering::SeqCst), 1);
    drop(held);
    let error = fixture
        .list()
        .await
        .expect_err("unanswered request on now-idle connection expires");
    assert_eq!(error.kind(), DomainErrorKind::DeadlineExceeded);
    let next = fixture.list().await;
    let connections = fixture.connections.load(Ordering::SeqCst);
    fixture.close().await;
    next.expect("a failed idle connection is retired after the other stream is released");
    assert_eq!(connections, 2);
}
