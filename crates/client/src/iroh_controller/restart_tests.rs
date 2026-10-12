//! A kept connection meets a host that was killed without a QUIC close and
//! returned with the same identity, or a live host that answers slowly.
use super::*;
use crate::handshake::{read_one_frame, write_handshake_message};
use iroh::tls::CaTlsConfig;
use prost::Message;
use std::{
    net::Ipv4Addr,
    sync::atomic::{AtomicU64, AtomicUsize},
    time::Duration,
};
use tokio_util::task::AbortOnDropHandle;
use zterm_core::ConnectionWelcome;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Return {
    SameRelay,
    OtherRelay,
    NewDirectPort,
}

/// Signed lookup whose advertised Relay follows the host, like Pkarr.
#[derive(Debug)]
struct MovableLookup {
    relay: Arc<StdMutex<iroh::RelayUrl>>,
    resolves: Arc<AtomicUsize>,
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
        self.resolves.fetch_add(1, Ordering::SeqCst);
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
        Some(Box::pin(futures_util::stream::once(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(item)
        })))
    }
}

async fn spawn_relay() -> (iroh_relay::server::Server, iroh::RelayUrl) {
    let mut config = iroh_relay::server::ServerConfig::default();
    config.relay = Some(iroh_relay::server::testing::relay_config());
    let server = iroh_relay::server::Server::spawn(config)
        .await
        .expect("isolated loopback relay");
    let url = format!("https://{}", server.https_addr().expect("relay HTTPS"))
        .parse()
        .expect("relay URL");
    (server, url)
}

/// Another URL for the same Relay whose TCP traffic is delayed in each
/// direction by `one_way` milliseconds, which a test may change, like netem
/// on one side's Relay connection. The Relay forwards by endpoint ID, so
/// peers may reach it through different URLs.
async fn delayed_relay(
    server: &iroh_relay::server::Server,
    one_way: Arc<AtomicU64>,
) -> (AbortOnDropHandle<()>, iroh::RelayUrl) {
    let target = server.https_addr().expect("relay HTTPS");
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("delay proxy");
    let url = format!("https://{}", listener.local_addr().expect("proxy address"))
        .parse()
        .expect("proxy URL");
    let proxy = tokio::spawn(async move {
        while let Ok((inbound, _)) = listener.accept().await {
            let one_way = Arc::clone(&one_way);
            tokio::spawn(async move {
                let Ok(outbound) = tokio::net::TcpStream::connect(target).await else {
                    return;
                };
                let _ = inbound.set_nodelay(true);
                let _ = outbound.set_nodelay(true);
                let (inbound_read, inbound_write) = inbound.into_split();
                let (outbound_read, outbound_write) = outbound.into_split();
                tokio::join!(
                    delay_line(inbound_read, outbound_write, &one_way),
                    delay_line(outbound_read, inbound_write, &one_way),
                );
            });
        }
    });
    (AbortOnDropHandle::new(proxy), url)
}

/// Forwards one direction in order after a fixed delay, without throttling.
async fn delay_line(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    one_way: &AtomicU64,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let read = async move {
        let mut buffer = [0_u8; 16 * 1024];
        while let Ok(read @ 1..) = from.read(&mut buffer).await {
            let due =
                tokio::time::Instant::now() + Duration::from_millis(one_way.load(Ordering::SeqCst));
            if sender.send((due, buffer[..read].to_vec())).is_err() {
                break;
            }
        }
    };
    let write = async move {
        while let Some((due, bytes)) = receiver.recv().await {
            tokio::time::sleep_until(due).await;
            if to.write_all(&bytes).await.is_err() {
                break;
            }
        }
        let _ = to.shutdown().await;
    };
    tokio::join!(read, write);
}

#[derive(Clone, Default)]
struct Served {
    accepted: Arc<AtomicUsize>,
    greeted: Arc<AtomicUsize>,
    queries: Arc<AtomicUsize>,
}

async fn serve(host: Endpoint, served: Served, reply_delay: Duration) {
    let mut handlers = tokio::task::JoinSet::new();
    while let Some(incoming) = host.accept().await {
        let served = served.clone();
        handlers.spawn(async move {
            let Ok(connection) = incoming.await else {
                return;
            };
            served.accepted.fetch_add(1, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(15);
            let Ok((mut send, mut recv)) = connection.accept_bi().await else {
                return;
            };
            if read_one_frame(&mut recv, 16384, deadline).await.is_err() {
                return;
            }
            served.greeted.fetch_add(1, Ordering::SeqCst);
            let welcome = ConnectionWelcome::new(
                zterm_proto::WIRE_MAJOR,
                Capabilities::from_bits_retain(
                    Capabilities::SESSION_SERVICE | Capabilities::TERMINAL_SERVICE,
                ),
                "restart host",
                "test",
                "test",
                AuthGeneration::new(1).expect("generation"),
            )
            .expect("Welcome");
            if write_handshake_message(
                &mut send,
                WireKind::ConnectionWelcome,
                &v2::ConnectionWelcome::from(&welcome),
                deadline,
            )
            .await
            .is_err()
            {
                return;
            }
            let _ = send.finish();
            while let Ok((mut send, mut recv)) = connection.accept_bi().await {
                let Ok(frame) =
                    read_one_frame(&mut recv, 16384, Instant::now() + Duration::from_secs(5)).await
                else {
                    continue;
                };
                served.queries.fetch_add(1, Ordering::SeqCst);
                // The service is slow; QUIC still acknowledges the request.
                tokio::time::sleep(reply_delay).await;
                let bytes = zterm_proto::encode_message(
                    WireKind::SessionListResponse,
                    frame.request_id,
                    0,
                    &v2::SessionListResponse {
                        sessions: Vec::new(),
                    },
                )
                .expect("reply");
                let _ = send.write_all(&bytes).await;
                let _ = send.finish();
            }
        });
    }
}

/// One host incarnation. Killing it abandons its runtime, so no QUIC close
/// is written, like a killed process or VM.
struct KillableHost {
    kill: tokio::sync::oneshot::Sender<()>,
    thread: std::thread::JoinHandle<()>,
    served: Served,
}

impl KillableHost {
    async fn spawn(
        secret: SecretKey,
        relay: iroh::RelayUrl,
        direct: bool,
        reply_delay: Duration,
    ) -> Self {
        let (ready, online) = tokio::sync::oneshot::channel();
        let (kill, killed) = tokio::sync::oneshot::channel::<()>();
        let served = Served::default();
        let serving = served.clone();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("killable host runtime");
            runtime.block_on(async move {
                let mut builder = Endpoint::builder(presets::Minimal)
                    .secret_key(secret)
                    .persistent_stateless_reset(true)
                    .ca_tls_config(CaTlsConfig::insecure_skip_verify())
                    .relay_mode(RelayMode::Custom(
                        iroh::RelayConfig::new(relay, None).into(),
                    ))
                    .clear_ip_transports()
                    .alpns(vec![ZTERM_ALPN.to_vec()]);
                if direct {
                    builder = builder
                        .bind_addr((Ipv4Addr::LOCALHOST, 0))
                        .expect("host loopback");
                }
                let host = builder.bind().await.expect("host");
                tokio::time::timeout(Duration::from_secs(10), host.online())
                    .await
                    .expect("host relay online");
                tokio::spawn(serve(host.clone(), serving, reply_delay));
                ready.send(()).expect("report host online");
                let _ = killed.await;
                std::mem::forget(host);
            });
            runtime.shutdown_background();
        });
        tokio::time::timeout(Duration::from_secs(15), online)
            .await
            .expect("killable host online")
            .expect("killable host reports readiness");
        Self {
            kill,
            thread,
            served,
        }
    }

    async fn kill(self) {
        let _ = self.kill.send(());
        tokio::task::spawn_blocking(move || self.thread.join())
            .await
            .expect("join host")
            .expect("host thread");
    }
}

struct Client {
    controller: IrohController,
    remote: DeviceId,
    secret: SecretKey,
    advertised: Arc<StdMutex<iroh::RelayUrl>>,
    resolves: Arc<AtomicUsize>,
}

impl Client {
    /// The production controller profile also persists its stateless reset key.
    async fn new(relays: &[&iroh::RelayUrl], direct: bool) -> Self {
        let advertised = Arc::new(StdMutex::new(relays[0].clone()));
        let resolves = Arc::new(AtomicUsize::new(0));
        let mut client = Endpoint::builder(presets::Minimal)
            .persistent_stateless_reset(true)
            .ca_tls_config(CaTlsConfig::insecure_skip_verify())
            .relay_mode(RelayMode::Custom(
                relays
                    .iter()
                    .map(|relay| iroh::RelayConfig::new((*relay).clone(), None))
                    .collect(),
            ))
            .clear_ip_transports()
            .address_lookup(MovableLookup {
                relay: Arc::clone(&advertised),
                resolves: Arc::clone(&resolves),
            });
        if direct {
            client = client
                .bind_addr((Ipv4Addr::LOCALHOST, 0))
                .expect("client loopback");
        }
        let controller =
            IrohController::new(client.bind().await.expect("client"), "kept").expect("controller");
        let secret = SecretKey::generate();
        let remote = device_from_endpoint_id(secret.public());
        controller
            .remember_host(
                remote,
                vec![RelayHint::new(relays[0].to_string()).expect("verified route")],
            )
            .expect("known host");
        Self {
            controller,
            remote,
            secret,
            advertised,
            resolves,
        }
    }

    async fn list(&self) -> Result<DecodedFrame, ClientError> {
        self.list_within(crate::protocol::DEFAULT_DEADLINE).await
    }

    async fn list_within(&self, deadline: Duration) -> Result<DecodedFrame, ClientError> {
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
                deadline,
                startup_deadline: crate::protocol::REMOTE_STARTUP_DEADLINE,
                mutation_or_lease_retry: false,
            })
            .await
    }

    async fn kept(&self) -> ControllerConnectionInfo {
        self.controller
            .connection_info(self.remote)
            .await
            .expect("observation")
            .expect("kept connection")
    }
}

/// The kept connection's request reaches nobody when the host returns on
/// another Relay. The first query must still finish inside its unchanged
/// five-second budget on one fresh connection, which the next query reuses.
/// A nonzero `host_delay` slows each direction of the host's Relay traffic.
async fn first_query_after_host_returns(returned: Return, host_delay: Duration) {
    let (old_server, old_relay) = spawn_relay().await;
    let (new_server, new_relay) = spawn_relay().await;
    // Hosts reach the Relays directly unless their traffic is delayed.
    let (proxies, old_host_relay, new_host_relay) = if host_delay.is_zero() {
        (Vec::new(), old_relay.clone(), new_relay.clone())
    } else {
        let one_way = Arc::new(AtomicU64::new(
            u64::try_from(host_delay.as_millis()).expect("delay in milliseconds"),
        ));
        let (old_proxy, old_url) = delayed_relay(&old_server, Arc::clone(&one_way)).await;
        let (new_proxy, new_url) = delayed_relay(&new_server, one_way).await;
        (vec![old_proxy, new_proxy], old_url, new_url)
    };
    let direct = returned == Return::NewDirectPort;
    let client = Client::new(&[&old_relay, &new_relay], direct).await;
    let old_host = KillableHost::spawn(
        client.secret.clone(),
        old_host_relay.clone(),
        direct,
        Duration::ZERO,
    )
    .await;
    client.list().await.expect("original query");
    let kept_rtt = client.kept().await.rtt_ms;
    if direct {
        let waited = Instant::now();
        while client.kept().await.path != "direct" {
            assert!(waited.elapsed() < Duration::from_secs(10), "no direct path");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    old_host.kill().await;
    // A real restart takes seconds; the host returns with the same identity.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (relay, host_relay) = if returned == Return::OtherRelay {
        (new_relay.clone(), new_host_relay)
    } else {
        (old_relay.clone(), old_host_relay)
    };
    *client
        .advertised
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = relay;
    let new_host =
        KillableHost::spawn(client.secret.clone(), host_relay, direct, Duration::ZERO).await;
    assert!(
        !client.kept().await.closed,
        "the query must meet an apparently open connection"
    );

    let started = Instant::now();
    let first = client.list().await;
    let elapsed = started.elapsed();
    let next = client.list().await;
    let accepted = new_host.served.accepted.load(Ordering::SeqCst);
    let queries = new_host.served.queries.load(Ordering::SeqCst);
    eprintln!(
        "{returned:?} kept_rtt={kept_rtt}ms: first={elapsed:?} {:?}; accepted={accepted} queries={queries}",
        first.as_ref().err().map(ClientError::kind),
    );
    new_host.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), client.controller.shutdown()).await;
    drop(proxies);
    old_server.shutdown().await.expect("old relay shutdown");
    new_server.shutdown().await.expect("new relay shutdown");
    first.expect("first query on the kept connection");
    assert!(elapsed < crate::protocol::DEFAULT_DEADLINE, "{elapsed:?}");
    next.expect("next query");
    assert_eq!(
        accepted, 1,
        "one fresh connection, reused by the next query"
    );
    assert_eq!(queries, 2, "both queries reach the returned host");
}

#[tokio::test]
async fn first_query_on_kept_connection_recovers_when_host_returns_on_the_same_relay() {
    first_query_after_host_returns(Return::SameRelay, Duration::ZERO).await;
}

#[tokio::test]
async fn first_query_on_kept_connection_recovers_when_host_returns_on_another_relay() {
    first_query_after_host_returns(Return::OtherRelay, Duration::ZERO).await;
}

#[tokio::test]
async fn first_query_on_kept_connection_recovers_when_host_returns_on_a_new_direct_port() {
    first_query_after_host_returns(Return::NewDirectPort, Duration::ZERO).await;
}

/// The replacement dial can look the host up before it publishes its new
/// Relay and then never connect. The retry after the window still dials
/// afresh, as without a replacement, and uses whichever connects first.
#[tokio::test]
async fn first_query_recovers_when_its_replacement_dial_saw_only_a_stale_route() {
    let (old_server, old_relay) = spawn_relay().await;
    let (new_server, new_relay) = spawn_relay().await;
    let client = Client::new(&[&old_relay, &new_relay], false).await;
    let old_host = KillableHost::spawn(
        client.secret.clone(),
        old_relay.clone(),
        false,
        Duration::ZERO,
    )
    .await;
    client.list().await.expect("original query");
    old_host.kill().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let new_host = KillableHost::spawn(
        client.secret.clone(),
        new_relay.clone(),
        false,
        Duration::ZERO,
    )
    .await;

    // The controller's route lookup and Iroh's own lookup for the replacement
    // both still see the old Relay; the host publishes before the window ends.
    let before = client.resolves.load(Ordering::SeqCst);
    let publish = async {
        let waited = Instant::now();
        while client.resolves.load(Ordering::SeqCst) < before + 2 {
            assert!(
                waited.elapsed() < Duration::from_secs(5),
                "no replacement lookup"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let published = waited.elapsed();
        *client
            .advertised
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = new_relay.clone();
        published
    };
    let started = Instant::now();
    let (first, published) = tokio::join!(
        async {
            let first = client.list().await;
            (first, started.elapsed())
        },
        publish,
    );
    let (first, elapsed) = first;
    let next = client.list().await;
    let queries = new_host.served.queries.load(Ordering::SeqCst);
    eprintln!(
        "stale replacement: published={published:?} first={elapsed:?} {:?}; accepted={} queries={queries}",
        first.as_ref().err().map(ClientError::kind),
        new_host.served.accepted.load(Ordering::SeqCst),
    );
    new_host.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), client.controller.shutdown()).await;
    old_server.shutdown().await.expect("old relay shutdown");
    new_server.shutdown().await.expect("new relay shutdown");
    first.expect("first query on the kept connection");
    assert!(elapsed < crate::protocol::DEFAULT_DEADLINE, "{elapsed:?}");
    next.expect("next query");
    assert_eq!(queries, 2, "both queries reach the returned host once");
}

/// About 760 ms of path RTT, where the acknowledgement window reaches its
/// cap and the field run (R10) still timed out after the new connection.
#[tokio::test]
async fn first_query_on_a_high_rtt_kept_connection_recovers_when_host_returns_on_another_relay() {
    first_query_after_host_returns(Return::OtherRelay, Duration::from_millis(380)).await;
}

/// A terminal attachment keeps its own stream on the kept connection. When
/// the host returns on another Relay, nothing on that connection is ever
/// acknowledged again, so the attachment is as unreachable as the request.
/// The first query must still recover in its budget, and the attachment must
/// learn of the failure then instead of at the QUIC idle timeout.
#[tokio::test]
async fn first_query_on_a_kept_connection_with_an_attachment_recovers_when_host_returns_on_another_relay()
 {
    let (old_server, old_relay) = spawn_relay().await;
    let (new_server, new_relay) = spawn_relay().await;
    let client = Client::new(&[&old_relay, &new_relay], false).await;
    let old_host = KillableHost::spawn(
        client.secret.clone(),
        old_relay.clone(),
        false,
        Duration::ZERO,
    )
    .await;
    client.list().await.expect("original query");
    let peer = client
        .controller
        .peer(client.remote, None)
        .expect("known host");
    let mut attachment = client
        .controller
        .service_stream(
            client.remote,
            &peer,
            Instant::now() + crate::protocol::DEFAULT_DEADLINE,
        )
        .await
        .expect("attachment stream on the kept connection");
    old_host.kill().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    *client
        .advertised
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = new_relay.clone();
    let new_host = KillableHost::spawn(
        client.secret.clone(),
        new_relay.clone(),
        false,
        Duration::ZERO,
    )
    .await;
    assert!(
        !client.kept().await.closed,
        "the query must meet an apparently open connection"
    );

    let started = Instant::now();
    let first = client.list().await;
    let elapsed = started.elapsed();
    let attachment_failure = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Err(error) = attachment.read().await {
                break error;
            }
        }
    })
    .await;
    let next = client.list().await;
    let accepted = new_host.served.accepted.load(Ordering::SeqCst);
    let queries = new_host.served.queries.load(Ordering::SeqCst);
    eprintln!(
        "attachment: first={elapsed:?} {:?}; attachment={:?}; accepted={accepted} queries={queries}",
        first.as_ref().err().map(ClientError::kind),
        attachment_failure.as_ref().map(ClientError::kind),
    );
    drop(attachment);
    new_host.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), client.controller.shutdown()).await;
    old_server.shutdown().await.expect("old relay shutdown");
    new_server.shutdown().await.expect("new relay shutdown");
    first.expect("first query on the kept connection");
    assert!(elapsed < crate::protocol::DEFAULT_DEADLINE, "{elapsed:?}");
    assert_eq!(
        attachment_failure
            .expect("the attachment sees its connection retired")
            .kind(),
        DomainErrorKind::TransportUnavailable,
        "a retryable failure lets the attachment reconnect"
    );
    next.expect("next query");
    assert_eq!(
        accepted, 1,
        "one fresh connection, reused by the next query"
    );
    assert_eq!(queries, 2, "both queries reach the returned host");
}

/// A live host acknowledges the request even while its service takes longer
/// than the acknowledgement window, so the kept connection is not replaced.
#[tokio::test]
async fn slow_live_host_keeps_its_connection_past_the_acknowledgement_window() {
    let (server, relay) = spawn_relay().await;
    let client = Client::new(&[&relay], false).await;
    let reply_delay = Duration::from_millis(2500);
    let host = KillableHost::spawn(client.secret.clone(), relay, false, reply_delay).await;
    let started = Instant::now();
    client.list().await.expect("original query");
    let cold = started.elapsed();
    let started = Instant::now();
    let kept = client.list().await;
    let elapsed = started.elapsed();
    let accepted = host.served.accepted.load(Ordering::SeqCst);
    let queries = host.served.queries.load(Ordering::SeqCst);
    eprintln!(
        "slow live host: cold={cold:?} kept={elapsed:?} {:?}; accepted={accepted} queries={queries}",
        kept.as_ref().err().map(ClientError::kind),
    );
    host.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), client.controller.shutdown()).await;
    server.shutdown().await.expect("relay shutdown");
    kept.expect("slow answer on the kept connection");
    assert!(elapsed >= reply_delay, "{elapsed:?}");
    assert_eq!(accepted, 1, "the kept connection is not replaced");
    assert_eq!(queries, 2, "neither request is sent twice");
}

/// A request that fails only because a live host answers after its deadline
/// was still acknowledged. The attachment sharing that connection keeps it.
#[tokio::test]
async fn late_answer_beside_an_attachment_keeps_the_attachment_connection() {
    let (server, relay) = spawn_relay().await;
    let client = Client::new(&[&relay], false).await;
    let host = KillableHost::spawn(
        client.secret.clone(),
        relay,
        false,
        Duration::from_millis(2500),
    )
    .await;
    client.list().await.expect("original query");
    let peer = client
        .controller
        .peer(client.remote, None)
        .expect("known host");
    let mut attachment = client
        .controller
        .service_stream(
            client.remote,
            &peer,
            Instant::now() + crate::protocol::DEFAULT_DEADLINE,
        )
        .await
        .expect("attachment stream on the kept connection");
    let late = client.list_within(Duration::from_secs(2)).await;
    let closed = client
        .controller
        .connection_info(client.remote)
        .await
        .expect("observation")
        .is_none_or(|kept| kept.closed);
    let attachment_failure = tokio::time::timeout(Duration::from_millis(1500), async {
        loop {
            if let Err(error) = attachment.read().await {
                break error;
            }
        }
    })
    .await;
    let accepted = host.served.accepted.load(Ordering::SeqCst);
    eprintln!(
        "late answer beside attachment: {:?}; closed={closed} attachment={:?}; accepted={accepted}",
        late.as_ref().err().map(ClientError::kind),
        attachment_failure.as_ref().map(ClientError::kind),
    );
    drop(attachment);
    host.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), client.controller.shutdown()).await;
    server.shutdown().await.expect("relay shutdown");
    assert_eq!(
        late.expect_err("the answer misses the shorter deadline")
            .kind(),
        DomainErrorKind::DeadlineExceeded
    );
    assert!(!closed, "the attachment's connection is kept");
    assert!(attachment_failure.is_err(), "the attachment stays readable");
    assert_eq!(accepted, 1, "no replacement connection was greeted");
}

/// A live host whose acknowledgement arrives after half the window, but
/// within it, also keeps its connection. The replacement dialed meanwhile
/// sends no Hello, so the host does not retire the kept connection, and it is
/// closed unused.
#[tokio::test]
async fn late_acknowledgement_keeps_the_connection_and_discards_its_replacement() {
    let (server, relay) = spawn_relay().await;
    let one_way = Arc::new(AtomicU64::new(0));
    let (proxy, host_relay) = delayed_relay(&server, Arc::clone(&one_way)).await;
    let client = Client::new(&[&relay], false).await;
    // Long enough that the replacement finishes its QUIC handshake first.
    let reply_delay = Duration::from_millis(2500);
    let host = KillableHost::spawn(client.secret.clone(), host_relay, false, reply_delay).await;
    client.list().await.expect("original query");
    let kept_rtt = client.kept().await.rtt_ms;
    // The kept path slows to about 700 ms RTT: the window stays at its one
    // second minimum and the acknowledgement arrives after its midpoint.
    one_way.store(350, Ordering::SeqCst);
    let started = Instant::now();
    let kept = client.list().await;
    let elapsed = started.elapsed();
    let replaced = host.served.accepted.load(Ordering::SeqCst);
    // The RTT estimate may still lag the slower path, so the next request
    // can dial and discard its own replacement too; it must still be greeted
    // only on the kept connection.
    let next = client.list().await;
    let accepted = host.served.accepted.load(Ordering::SeqCst);
    let greeted = host.served.greeted.load(Ordering::SeqCst);
    let queries = host.served.queries.load(Ordering::SeqCst);
    eprintln!(
        "late acknowledgement kept_rtt={kept_rtt}ms: kept={elapsed:?} {:?}; replaced={replaced} accepted={accepted} greeted={greeted} queries={queries}",
        kept.as_ref().err().map(ClientError::kind),
    );
    host.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), client.controller.shutdown()).await;
    drop(proxy);
    server.shutdown().await.expect("relay shutdown");
    kept.expect("answer on the kept connection");
    next.expect("next query");
    assert!(elapsed >= reply_delay, "{elapsed:?}");
    assert_eq!(replaced, 2, "the first request dialed one replacement");
    assert_eq!(greeted, 1, "only the kept connection sent Hello");
    assert_eq!(queries, 3, "no request is sent twice");
}
