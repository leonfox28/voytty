//! A kept connection meets a host that was killed without a QUIC close and
//! returned with the same identity, or a live host that answers slowly.
use super::*;
use crate::handshake::{read_one_frame, write_handshake_message};
use iroh::tls::CaTlsConfig;
use prost::Message;
use std::{net::Ipv4Addr, sync::atomic::AtomicUsize, time::Duration};
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

#[derive(Clone, Default)]
struct Served {
    accepted: Arc<AtomicUsize>,
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
                tokio::time::timeout(Duration::from_secs(5), host.online())
                    .await
                    .expect("host relay online");
                tokio::spawn(serve(host.clone(), serving, reply_delay));
                ready.send(()).expect("report host online");
                let _ = killed.await;
                std::mem::forget(host);
            });
            runtime.shutdown_background();
        });
        tokio::time::timeout(Duration::from_secs(10), online)
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
}

impl Client {
    /// The production controller profile also persists its stateless reset key.
    async fn new(relays: &[&iroh::RelayUrl], direct: bool) -> Self {
        let advertised = Arc::new(StdMutex::new(relays[0].clone()));
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
                deadline: crate::protocol::DEFAULT_DEADLINE,
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
async fn first_query_after_host_returns(returned: Return) {
    let (old_server, old_relay) = spawn_relay().await;
    let (new_server, new_relay) = spawn_relay().await;
    let direct = returned == Return::NewDirectPort;
    let client = Client::new(&[&old_relay, &new_relay], direct).await;
    let old_host = KillableHost::spawn(
        client.secret.clone(),
        old_relay.clone(),
        direct,
        Duration::ZERO,
    )
    .await;
    client.list().await.expect("original query");
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
    let relay = if returned == Return::OtherRelay {
        new_relay.clone()
    } else {
        old_relay.clone()
    };
    *client
        .advertised
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = relay.clone();
    let new_host = KillableHost::spawn(client.secret.clone(), relay, direct, Duration::ZERO).await;
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
        "{returned:?}: first={elapsed:?} {:?}; accepted={accepted} queries={queries}",
        first.as_ref().err().map(ClientError::kind),
    );
    new_host.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), client.controller.shutdown()).await;
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
    first_query_after_host_returns(Return::SameRelay).await;
}

#[tokio::test]
async fn first_query_on_kept_connection_recovers_when_host_returns_on_another_relay() {
    first_query_after_host_returns(Return::OtherRelay).await;
}

#[tokio::test]
async fn first_query_on_kept_connection_recovers_when_host_returns_on_a_new_direct_port() {
    first_query_after_host_returns(Return::NewDirectPort).await;
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
