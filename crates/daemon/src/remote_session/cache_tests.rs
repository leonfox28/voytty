//! Real TLS/Hello/Welcome regressions for the broker unary and tunnel adapters. A local
//! HTTPS relay keeps these independent of public routing and platform UDP.
use super::*;
use crate::authorization::AuthorizationRegistry;
use crate::connection_broker::{
    ConnectionIdentity, InboundAuthenticatedStream, RemoteServiceHandler,
    RemoteServiceHandlerFuture,
};
use crate::store::{StateStore, StoreActor};
use crate::transport::VOYTTY_ALPN;
use iroh::endpoint::{Connection, RecvStream, SendStream, presets};
use iroh::{Endpoint, RelayMode, tls::CaTlsConfig};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
#[cfg(unix)]
use tokio::net::UnixListener as TestListener;
use voytty_client::handshake::{read_one_frame, write_handshake_message};
use voytty_core::{AuthGeneration, Capabilities, ConnectionWelcome, DeviceAlias, TransportLimits};
#[cfg(windows)]
use voytty_platform::local::AsyncListener as TestListener;
use voytty_platform::user_state::UserPaths;
use voytty_proto::{WireKind, encode_message, v2};

const ECHO_REQUEST: u64 = 10_000;
const ERROR_REQUEST: u64 = 20_000;
// Each case owns a complete TLS relay and endpoint pair. Keep unrelated fixture
// lifecycles from competing for the bounded operations being measured; streams
// within a fixture still run concurrently, including the live-sibling cases.
static FIXTURE_SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

fn remove_fixture_socket(socket: &std::path::Path) {
    #[cfg(unix)]
    std::fs::remove_file(socket).expect("remove fixture socket");
    // Direct named-pipe listeners have no filesystem entry. The production
    // daemon's ownership marker is separately exercised by its lifecycle test.
    #[cfg(windows)]
    assert!(!socket.try_exists().expect("inspect fixture socket"));
}

fn list_request(target: DeviceId, id: u64) -> Vec<u8> {
    encode_message(
        WireKind::SessionListRequest,
        id,
        0,
        &v2::SessionListRequest {
            target: Some(v2::TargetSelector {
                target: Some(v2::target_selector::Target::Device(target.into())),
            }),
        },
    )
    .expect("request")
}

fn reply(id: u64) -> Vec<u8> {
    encode_message(
        WireKind::SessionListResponse,
        id,
        0,
        &v2::SessionListResponse {
            sessions: Vec::new(),
        },
    )
    .expect("reply")
}

struct EchoHandler;

impl RemoteServiceHandler for EchoHandler {
    fn handle_service_stream(
        &self,
        stream: InboundAuthenticatedStream,
        _deadline: Instant,
    ) -> RemoteServiceHandlerFuture {
        Box::pin(async move {
            let (mut send, mut recv) = stream.into_parts();
            while let Ok(frame) =
                read_one_frame(&mut recv, 16384, Instant::now() + Duration::from_secs(30)).await
            {
                assert_eq!(frame.request_id, ECHO_REQUEST);
                if send.write_all(&reply(frame.request_id)).await.is_err() {
                    break;
                }
            }
            Ok(())
        })
    }
}

struct Fixture {
    broker: ConnectionBroker,
    endpoint: Endpoint,
    host: Endpoint,
    remote: DeviceId,
    relay: iroh_relay::server::Server,
    serving: tokio::task::JoinHandle<()>,
    connections: Arc<AtomicUsize>,
    attachments: Arc<AtomicUsize>,
    accepted: tokio::sync::mpsc::UnboundedReceiver<Connection>,
    store: StoreActor,
    _state: tempfile::TempDir,
    _fixture_slot: tokio::sync::SemaphorePermit<'static>,
}

impl Fixture {
    async fn new() -> Self {
        let fixture_slot = FIXTURE_SLOT.acquire().await.expect("fixture slot");
        let mut config = iroh_relay::server::ServerConfig::default();
        config.relay = Some(iroh_relay::server::testing::relay_config());
        let relay = iroh_relay::server::Server::spawn(config)
            .await
            .expect("relay");
        let url: iroh::RelayUrl = format!("https://{}", relay.https_addr().expect("HTTPS"))
            .parse()
            .expect("URL");
        let builder = || {
            Endpoint::builder(presets::Minimal)
                .ca_tls_config(CaTlsConfig::insecure_skip_verify())
                .relay_mode(RelayMode::Custom(
                    iroh::RelayConfig::new(url.clone(), None).into(),
                ))
                .clear_ip_transports()
        };
        let host = builder()
            .alpns(vec![VOYTTY_ALPN.to_vec()])
            .bind()
            .await
            .expect("host");
        let endpoint = tokio::time::timeout(Duration::from_secs(5), async {
            let endpoint = builder().bind().await.expect("controller");
            tokio::join!(host.online(), endpoint.online());
            endpoint
        })
        .await
        .expect("both relay endpoints online");
        let remote = DeviceId::from_array(*host.id().as_bytes());
        let local = DeviceId::from_array(*endpoint.id().as_bytes());
        let state = tempfile::tempdir().expect("isolated state");
        let home = state.path().join("home");
        std::fs::create_dir(&home).expect("home");
        let paths = UserPaths::for_test(
            voytty_platform::account::EffectiveAccount::current()
                .expect("current account")
                .uid(),
            home.clone(),
            home.join(".voytty"),
            state.path().join("run"),
        );
        paths
            .prepare_state_directories()
            .expect("state directories");
        let mut database = StateStore::open(&paths).expect("database");
        database
            .upsert_known_device(
                remote,
                &DeviceAlias::new("host").expect("alias"),
                "host",
                None,
            )
            .expect("known host");
        database
            .authorize_device(remote, "host", 1)
            .expect("reverse authorization");
        let authorization = AuthorizationRegistry::new();
        authorization
            .preload(database.list_authorizations().expect("grants"))
            .expect("preload");
        let store = StoreActor::start(database).expect("store actor");
        let identity = ConnectionIdentity::new(
            local,
            "controller",
            "test",
            "test",
            Capabilities::from_bits_retain(
                Capabilities::SESSION_SERVICE | Capabilities::TERMINAL_SERVICE,
            ),
        )
        .expect("identity");
        let broker = ConnectionBroker::for_test(
            endpoint.clone(),
            identity,
            store.handle(),
            authorization,
            TransportLimits::default(),
        )
        .expect("broker");
        broker.set_test_route(remote, host.addr()).expect("route");
        broker
            .install_remote_service_handler(Arc::new(EchoHandler))
            .expect("reverse service");
        let connections = Arc::new(AtomicUsize::new(0));
        let attachments = Arc::new(AtomicUsize::new(0));
        let attaches = attachments.clone();
        let count = connections.clone();
        let accepting = host.clone();
        let (sender, accepted) = tokio::sync::mpsc::unbounded_channel();
        let serving = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            while let Some(incoming) = accepting.accept().await {
                let index = count.fetch_add(1, Ordering::SeqCst);
                let sender = sender.clone();
                let attaches = attaches.clone();
                handlers.spawn(async move {
                    let connection = incoming.await.expect("TLS");
                    let (mut send, mut recv) = connection.accept_bi().await.expect("Hello stream");
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let hello = read_one_frame(&mut recv, 16384, deadline)
                        .await
                        .expect("Hello");
                    assert_eq!(hello.kind, WireKind::ConnectionHello);
                    let welcome = ConnectionWelcome::new(
                        voytty_proto::WIRE_MAJOR,
                        Capabilities::from_bits_retain(
                            Capabilities::SESSION_SERVICE | Capabilities::TERMINAL_SERVICE,
                        ),
                        "host",
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
                    .expect("Welcome write");
                    send.finish().expect("Welcome finish");
                    sender
                        .send(connection.clone())
                        .expect("accepted connection");
                    let ordinary = Arc::new(AtomicUsize::new(0));
                    let mut streams = tokio::task::JoinSet::new();
                    while let Ok((mut send, mut recv)) = connection.accept_bi().await {
                        let connection = connection.clone();
                        let ordinary = ordinary.clone();
                        let attaches = attaches.clone();
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
                                let attach = frame.kind == WireKind::TerminalAttachRequest;
                                assert!(attach || frame.kind == WireKind::SessionListRequest);
                                if attach {
                                    attaches.fetch_add(1, Ordering::SeqCst);
                                }
                                let echo = frame.request_id == ECHO_REQUEST;
                                let error = frame.request_id == ERROR_REQUEST;
                                if !echo
                                    && !error
                                    && index == 0
                                    && ordinary.fetch_add(1, Ordering::SeqCst) > 0
                                {
                                    connection.closed().await;
                                    return;
                                }
                                let bytes = if error || attach {
                                    encode_message(
                                        WireKind::ServiceErrorResponse,
                                        frame.request_id,
                                        0,
                                        &v2::ServiceError {
                                            code: DomainErrorKind::DeadlineExceeded
                                                .code()
                                                .to_owned(),
                                            message: "service deadline".to_owned(),
                                        },
                                    )
                                    .expect("service error")
                                } else {
                                    reply(frame.request_id)
                                };
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
            broker,
            endpoint,
            host,
            remote,
            relay,
            serving,
            connections,
            attachments,
            accepted,
            store,
            _state: state,
            _fixture_slot: fixture_slot,
        }
    }

    async fn query(&self, id: u64) -> Result<DecodedFrame, DaemonError> {
        RemoteUnaryClient::new(Arc::new(BrokerRemoteUnaryTransport {
            broker: self.broker.clone(),
        }))
        .execute_preencoded(
            self.remote,
            id,
            &list_request(self.remote, id),
            Instant::now() + Duration::from_secs(5),
        )
        .await
    }

    async fn fail_query(&self) {
        assert_eq!(
            self.query(2)
                .await
                .expect_err("unresponsive request expires")
                .kind(),
            DomainErrorKind::DeadlineExceeded
        );
    }

    fn listen_for_tunnel(
        &self,
    ) -> (
        std::path::PathBuf,
        tokio::task::JoinHandle<Result<(), DaemonError>>,
    ) {
        let socket = self._state.path().join("tunnel.sock");
        let listener = TestListener::bind(&socket).expect("tunnel listener");
        let broker = self.broker.clone();
        let remote = self.remote;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("frontend");
            let first = crate::session_wire::read_first(&mut stream)
                .await
                .expect("tunnel Open");
            crate::remote_tunnel::serve_remote_session_tunnel(
                &broker,
                remote,
                stream,
                first,
                crate::session_wire::SessionWireLimits::default(),
                Instant::now() + Duration::from_secs(5),
            )
            .await
        });
        (socket, server)
    }

    async fn attach_through_tunnel(&self) -> DaemonError {
        let (socket, server) = self.listen_for_tunnel();
        let error = tokio::time::timeout(
            Duration::from_secs(6),
            crate::client::SessionClient::connect_remote_main(&socket, self.remote, None),
        )
        .await
        .expect("frontend keeps the original five-second deadline")
        .expect_err("fixture withholds or rejects attachment");
        let _ = tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("frontend closure releases the tunnel")
            .expect("tunnel task");
        remove_fixture_socket(&socket);
        error
    }

    async fn fail_tunnel(&self) {
        assert_eq!(
            self.attach_through_tunnel().await.kind(),
            DomainErrorKind::OperationOutcomeUnknown,
            "a lost create result stays uncertain and is never replayed"
        );
    }

    async fn close(self) {
        self.serving.abort();
        let _ = self.serving.await;
        self.broker.quiesce().await;
        tokio::join!(self.endpoint.close(), self.host.close());
        self.relay.shutdown().await.expect("relay shutdown");
        self.store.shutdown();
    }
}

async fn echo(send: &mut SendStream, recv: &mut RecvStream, remote: DeviceId) {
    send.write_all(&list_request(remote, ECHO_REQUEST))
        .await
        .expect("echo write");
    let frame = read_one_frame(recv, 16384, Instant::now() + Duration::from_secs(1))
        .await
        .expect("live stream must still exchange data");
    assert_eq!(frame.request_id, ECHO_REQUEST);
}

/// A retired idle connection's retry adopts the transport that an
/// unacknowledged request already dialed, instead of dialing again.
#[tokio::test]
async fn retired_connection_retry_adopts_the_replacement_transport() {
    let fixture = Fixture::new().await;
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
    let replacement = demand.dial_replacement(deadline);
    while !replacement.is_finished() {
        assert!(Instant::now() < deadline, "replacement dial did not end");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        fixture.connections.load(Ordering::SeqCst),
        2,
        "the replacement reached the host"
    );
    demand.retire_failed_candidate(old, Some(replacement), false);
    fixture
        .query(3)
        .await
        .expect("the retry uses the replacement");
    assert_eq!(
        fixture.connections.load(Ordering::SeqCst),
        2,
        "no further connection was dialed"
    );
    drop(demand);
    fixture.close().await;
}

#[tokio::test]
async fn failed_idle_query_reconnects_and_late_failure_cannot_retire_replacement() {
    let fixture = Fixture::new().await;
    fixture.query(1).await.expect("warm query");
    let old = fixture
        .broker
        .peer_observation(fixture.remote)
        .await
        .primary
        .expect("primary");
    fixture.fail_query().await;
    fixture
        .query(3)
        .await
        .expect("next independent query uses a fresh connection");
    let demand = fixture
        .broker
        .demand(fixture.remote, Instant::now() + Duration::from_secs(5))
        .await
        .expect("demand");
    demand.retire_failed_candidate(old, None, false);
    fixture
        .query(4)
        .await
        .expect("late failure preserves replacement");
    assert_eq!(fixture.connections.load(Ordering::SeqCst), 2);
    drop(demand);
    fixture.close().await;
}

async fn preserves_live_stream(inbound: bool, tunnel: bool) {
    let mut fixture = Fixture::new().await;
    fixture.query(1).await.expect("warm query");
    let connection = fixture.accepted.recv().await.expect("accepted");
    let demand = fixture
        .broker
        .demand(fixture.remote, Instant::now() + Duration::from_secs(5))
        .await
        .expect("demand");
    let mut outbound = if inbound {
        None
    } else {
        Some(
            demand
                .open_bi(
                    StreamPurpose::Service,
                    Instant::now() + Duration::from_secs(5),
                )
                .await
                .expect("held outbound stream"),
        )
    };
    let mut reverse = if inbound {
        Some(connection.open_bi().await.expect("reverse stream"))
    } else {
        None
    };
    let (send, recv) = match (&mut outbound, &mut reverse) {
        (Some(stream), _) => (&mut stream.send, &mut stream.recv),
        (_, Some((send, recv))) => (send, recv),
        _ => unreachable!(),
    };
    echo(send, recv, fixture.remote).await;
    if tunnel {
        fixture.fail_tunnel().await;
    } else {
        fixture.fail_query().await;
    }
    echo(send, recv, fixture.remote).await;
    assert_eq!(fixture.connections.load(Ordering::SeqCst), 1);
    send.finish().expect("finish live stream");
    drop(outbound);
    drop(reverse);
    tokio::time::timeout(Duration::from_secs(1), async {
        while fixture
            .broker
            .peer_observation(fixture.remote)
            .await
            .active_stream_count
            != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("released handler");
    if tunnel {
        fixture.fail_tunnel().await;
    } else {
        fixture.fail_query().await;
    }
    fixture
        .query(3)
        .await
        .expect("idle failed connection now replaced");
    assert_eq!(fixture.connections.load(Ordering::SeqCst), 2);
    drop(demand);
    fixture.close().await;
}

#[tokio::test]
async fn query_timeout_preserves_another_outbound_stream() {
    preserves_live_stream(false, false).await;
}

#[tokio::test]
async fn query_timeout_preserves_an_inbound_handler() {
    preserves_live_stream(true, false).await;
}

/// A failed request whose connection acknowledged nothing retires it even
/// beside another stream, which cannot reach the host either.
#[tokio::test]
async fn silent_failed_candidate_is_retired_beside_a_live_stream() {
    let fixture = Fixture::new().await;
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
    let mut held = demand
        .open_bi(StreamPurpose::Service, deadline)
        .await
        .expect("held outbound stream");
    echo(&mut held.send, &mut held.recv, fixture.remote).await;
    demand.retire_failed_candidate(old, None, true);
    let ended = tokio::time::timeout(Duration::from_secs(1), held.recv.read(&mut [0_u8; 1]))
        .await
        .expect("the held stream ends promptly");
    assert!(
        ended.is_err(),
        "the held stream reports its connection lost"
    );
    fixture
        .query(3)
        .await
        .expect("the next query dials a fresh connection");
    assert_eq!(fixture.connections.load(Ordering::SeqCst), 2);
    drop(held);
    drop(demand);
    fixture.close().await;
}

#[tokio::test]
async fn expired_half_close_releases_tunnel_and_retires_unanswered_idle_candidate() {
    use voytty_client::transport::AttachmentConnector;
    let fixture = Fixture::new().await;
    fixture.query(1).await.expect("warm connection");
    let (socket, mut server) = fixture.listen_for_tunnel();
    let connector = crate::client::UnixAttachmentConnector::new(&socket);
    let mut transport = connector
        .open(voytty_client::model::ResolvedSessionTarget::device(
            fixture.remote,
        ))
        .await
        .expect("tunnel");
    transport
        .write_session_bytes(&list_request(fixture.remote, 2))
        .await
        .expect("send query");
    let error = transport
        .shutdown()
        .await
        .expect_err("silent host cannot finish detach");
    assert_eq!(
        error.kind(),
        DomainErrorKind::DeadlineExceeded,
        "keep the original two-second control deadline"
    );
    let joined = tokio::time::timeout(Duration::from_secs(3), &mut server).await;
    server.abort();
    assert!(
        joined.is_ok(),
        "expired frontend drain must release its broker stream"
    );
    joined
        .expect("tunnel completed")
        .expect("tunnel task did not panic")
        .expect_err("closed frontend is reported by the pump");
    remove_fixture_socket(&socket);
    let next = fixture.query(3).await;
    let connections = fixture.connections.load(Ordering::SeqCst);
    fixture.close().await;
    next.expect("next independent query uses a new connection");
    assert_eq!(
        connections, 2,
        "unanswered idle candidate is retired after pump exit"
    );
}

#[tokio::test]
async fn unanswered_idle_tunnel_reconnects_without_replaying_attach() {
    let fixture = Fixture::new().await;
    fixture.query(1).await.expect("warm connection");
    fixture.fail_tunnel().await;
    let next = fixture.query(3).await;
    let connections = fixture.connections.load(Ordering::SeqCst);
    let attachments = fixture.attachments.load(Ordering::SeqCst);
    fixture.close().await;
    next.expect("next independent query uses a new connection after unanswered tunnel");
    assert_eq!(connections, 2);
    assert_eq!(attachments, 1, "the uncertain create is not replayed");
}

#[tokio::test]
async fn unanswered_tunnel_preserves_another_outbound_stream() {
    preserves_live_stream(false, true).await;
}

#[tokio::test]
async fn unanswered_tunnel_preserves_an_inbound_handler() {
    preserves_live_stream(true, true).await;
}

#[tokio::test]
async fn tunnel_service_error_preserves_idle_connection() {
    let fixture = Fixture::new().await;
    assert_eq!(
        fixture.attach_through_tunnel().await.kind(),
        DomainErrorKind::DeadlineExceeded,
        "a correlated host error remains definitive"
    );
    fixture
        .query(ECHO_REQUEST)
        .await
        .expect("same connection works");
    assert_eq!(fixture.connections.load(Ordering::SeqCst), 1);
    fixture.close().await;
}

#[tokio::test]
async fn tunnel_cancelled_before_sending_preserves_idle_connection() {
    use voytty_client::transport::AttachmentConnector;
    let fixture = Fixture::new().await;
    fixture.query(1).await.expect("warm connection");
    let (socket, server) = fixture.listen_for_tunnel();
    let connector = crate::client::UnixAttachmentConnector::new(&socket);
    let transport = connector
        .open(voytty_client::model::ResolvedSessionTarget::device(
            fixture.remote,
        ))
        .await
        .expect("open without sending Session bytes");
    drop(transport);
    let _ = tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .expect("cancelled frontend releases tunnel")
        .expect("tunnel task");
    fixture
        .query(ECHO_REQUEST)
        .await
        .expect("same connection works");
    assert_eq!(fixture.connections.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.attachments.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test]
async fn framed_service_deadline_is_not_a_transport_failure() {
    let fixture = Fixture::new().await;
    let response = fixture
        .query(ERROR_REQUEST)
        .await
        .expect("framed service response");
    assert_eq!(response.kind, WireKind::ServiceErrorResponse);
    fixture.query(1).await.expect("same connection still works");
    assert_eq!(fixture.connections.load(Ordering::SeqCst), 1);
    fixture.close().await;
}
