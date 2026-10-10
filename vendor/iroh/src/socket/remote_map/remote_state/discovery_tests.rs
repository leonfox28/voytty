//! Fresh discovery results must not wait for an unrelated QUIC loss timer.

use std::time::Duration;

use n0_future::{StreamExt, boxed::BoxStream, time::Instant};

use crate::{
    Endpoint, EndpointAddr, EndpointId, SecretKey, TransportAddr,
    address_lookup::{AddressLookup, EndpointInfo, Error, Item},
    endpoint::presets,
    test_utils::test_transport::{TestNetwork, to_custom_addr},
};

#[derive(Debug)]
struct DelayedLookup(EndpointAddr);

impl AddressLookup for DelayedLookup {
    fn resolve(&self, endpoint_id: EndpointId) -> Option<BoxStream<Result<Item, Error>>> {
        assert_eq!(endpoint_id, self.0.id);
        let addr = self.0.clone();
        Some(
            n0_future::stream::once_future(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(Item::new(EndpointInfo::from(addr), "delayed-test", None))
            })
            .boxed(),
        )
    }
}

#[tokio::test]
async fn zterm_fresh_discovery_does_not_wait_for_initial_retransmission() {
    let network = TestNetwork::new();
    let host_key = SecretKey::generate();
    let client_key = SecretKey::generate();
    let host = Endpoint::builder(presets::Minimal)
        .secret_key(host_key.clone())
        .clear_ip_transports()
        .add_custom_transport(network.create_transport(host_key.public()).unwrap())
        .alpns(vec![b"fresh-route-test".to_vec()])
        .bind()
        .await
        .unwrap();
    let client = Endpoint::builder(presets::Minimal)
        .secret_key(client_key.clone())
        .clear_ip_transports()
        .add_custom_transport(network.create_transport(client_key.public()).unwrap())
        .address_lookup(DelayedLookup(
            EndpointAddr::new(host.id())
                .with_addrs([TransportAddr::Custom(to_custom_addr(host.id()))]),
        ))
        .bind()
        .await
        .unwrap();
    let stale = EndpointAddr::new(host.id()).with_addrs([TransportAddr::Custom(to_custom_addr(
        SecretKey::generate().public(),
    ))]);
    let (result, elapsed) = first_request(&host, &client, stale).await;
    eprintln!("fresh discovery first request: {elapsed:?}, {result:?}");
    result.unwrap();
    // The route arrives at 100ms. The unchanged default first PTO is ~1s;
    // allow generous scheduling headroom while detecting that avoidable wait.
    assert!(elapsed < Duration::from_millis(750), "waited for QUIC PTO");
}

fn pending_state() -> super::State {
    let mut state = super::RemoteStateActor::new(
        SecretKey::from_bytes(&[0x72; 32]).public(),
        n0_watcher::Watchable::new(Default::default()).watch(),
        Default::default(),
        Default::default(),
        Default::default(),
        std::sync::Arc::new(
            crate::socket::biased_rtt_path_selector::BiasedRttPathSelector::default(),
        ),
    )
    .state;
    state.address_lookup_stream = Some(n0_future::stream::pending().boxed());
    state
}

fn initial() -> super::OwnedTransmit {
    super::OwnedTransmit {
        ecn: None,
        contents: bytes::Bytes::from_static(&[0xc0, 0, 0, 0, 1, 0, 0]),
        segment_size: None,
    }
}

fn discovered(
    id: EndpointId,
    name: &'static str,
    addr: TransportAddr,
) -> Option<Result<Item, crate::address_lookup::AddressLookupFailed>> {
    Some(Ok(Item::new(
        EndpointInfo::from(EndpointAddr::new(id).with_addrs([addr])),
        name,
        None,
    )))
}

#[tokio::test]
async fn zterm_initial_fanout_checks_identity_and_only_new_addresses() {
    use super::{MAX_DATAGRAM_SEND_TASKS, TransportsSender};
    let mut state = pending_state();
    let (sender, mut receiver) = TransportsSender::with_bounded_relay(4, std::iter::empty());
    state.initial_transmits.remember(&sender, &initial());
    let mut tasks = n0_future::FuturesUnorderedBounded::new(MAX_DATAGRAM_SEND_TASKS);
    let addr = TransportAddr::Relay("https://new-relay.invalid".parse().unwrap());
    state.handle_address_lookup_item(
        discovered(SecretKey::generate().public(), "wrong", addr.clone()),
        &mut tasks,
    );
    assert!(state.paths.is_empty());
    assert!(tasks.is_empty());
    state.handle_address_lookup_item(
        discovered(state.endpoint_id, "first", addr.clone()),
        &mut tasks,
    );
    while tasks.next().await.is_some() {}
    assert_eq!(
        n0_future::future::now_or_never(receiver.next()),
        Some(Some(()))
    );
    state.handle_address_lookup_item(
        discovered(state.endpoint_id, "duplicate provider", addr.clone()),
        &mut tasks,
    );
    let (tx, _rx) = tokio::sync::oneshot::channel();
    state.handle_msg_resolve_remote([addr].into(), tx, &mut tasks);
    assert!(tasks.is_empty());
    assert_eq!(n0_future::future::now_or_never(receiver.next()), None);
}

#[tokio::test(start_paused = true)]
async fn zterm_initial_fanout_skips_expired_and_local_paths() {
    use super::{MAX_DATAGRAM_SEND_TASKS, TransportsSender};
    let mut state = pending_state();
    let (sender, _receiver) = TransportsSender::with_bounded_relay(4, std::iter::empty());
    let mut tasks = n0_future::FuturesUnorderedBounded::new(MAX_DATAGRAM_SEND_TASKS);
    state.initial_transmits.remember(&sender, &initial());
    let local = "127.0.0.1:4567".parse().unwrap();
    state.local_direct_addrs = n0_watcher::Watchable::new(
        [crate::socket::DirectAddr {
            addr: local,
            typ: crate::socket::DirectAddrType::Local,
        }]
        .into(),
    )
    .watch();
    state.handle_address_lookup_item(
        discovered(state.endpoint_id, "local", TransportAddr::Ip(local)),
        &mut tasks,
    );
    assert!(tasks.is_empty());
    tokio::time::sleep(super::DATAGRAM_SEND_TIMEOUT).await;
    state.handle_address_lookup_item(
        discovered(
            state.endpoint_id,
            "late",
            TransportAddr::Relay("https://late.invalid".parse().unwrap()),
        ),
        &mut tasks,
    );
    assert!(tasks.is_empty());
    assert_eq!(state.initial_transmits.expires_at(), None);
}

#[tokio::test]
async fn zterm_initial_fanout_stops_after_lookup_or_path_selection() {
    use super::{MAX_DATAGRAM_SEND_TASKS, TransportsSender, transports};
    let mut state = pending_state();
    let (sender, _receiver) = TransportsSender::with_bounded_relay(4, std::iter::empty());
    let mut tasks = n0_future::FuturesUnorderedBounded::new(MAX_DATAGRAM_SEND_TASKS);
    state.initial_transmits.remember(&sender, &initial());
    state.selected_path = Some(transports::FourTuple::from_remote(transports::Addr::Relay(
        "https://selected.invalid".parse().unwrap(),
        state.endpoint_id,
    )));
    state.handle_address_lookup_item(
        discovered(
            state.endpoint_id,
            "unused",
            TransportAddr::Relay("https://unused.invalid".parse().unwrap()),
        ),
        &mut tasks,
    );
    assert!(tasks.is_empty());
    state.selected_path = None;
    state.handle_address_lookup_item(None, &mut tasks);
    assert!(state.address_lookup_stream.is_none());
    assert_eq!(state.initial_transmits.expires_at(), None);
    let (tx, _rx) = tokio::sync::oneshot::channel();
    state.handle_msg_resolve_remote(
        [TransportAddr::Relay(
            "https://later.invalid".parse().unwrap(),
        )]
        .into(),
        tx,
        &mut tasks,
    );
    assert!(tasks.is_empty());
}

async fn first_request(
    host: &Endpoint,
    client: &Endpoint,
    address: EndpointAddr,
) -> (Result<(), tokio::time::error::Elapsed>, Duration) {
    let accepting = host.clone();
    let server = tokio::spawn(async move {
        let conn = accepting.accept().await.unwrap().await.unwrap();
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        assert_eq!(recv.read_to_end(16).await.unwrap(), b"query");
        send.write_all(b"reply").await.unwrap();
        send.finish().unwrap();
        conn.closed().await;
    });
    let start = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let conn = client.connect(address, b"fresh-route-test").await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        send.write_all(b"query").await.unwrap();
        send.finish().unwrap();
        assert_eq!(recv.read_to_end(16).await.unwrap(), b"reply");
        conn.close(0u32.into(), b"done");
    })
    .await;
    let elapsed = start.elapsed();
    server.abort();
    let _ = server.await;
    tokio::join!(client.close(), host.close());
    (result, elapsed)
}

// Socket-based regression: run in the disposable Linux lab, not on the Mac.
#[tokio::test]
async fn fresh_discovery_slow_tls_relay_first_request_keeps_original_deadline() {
    use iroh_relay::tls::CaTlsConfig;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::task::AbortOnDropHandle;

    use crate::{RelayConfig, RelayMode, RelayUrl};

    let (_, relay_url, relay) = crate::test_utils::run_relay_server_with(false)
        .await
        .unwrap();
    let relay_addr = relay.https_addr().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let delayed_url: RelayUrl = format!("https://{}", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let (dial_tx, mut dial_rx) = tokio::sync::mpsc::channel(16);
    let proxy = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut forwards = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut downstream, _) = accepted.unwrap();
                    dial_tx.try_send(Instant::now()).unwrap();
                    forwards.spawn(async move {
                        tokio::time::sleep(Duration::from_millis(4_100)).await;
                        let mut upstream = TcpStream::connect(relay_addr).await.unwrap();
                        let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
                    });
                }
                _ = forwards.join_next(), if !forwards.is_empty() => {}
            }
        }
    }));
    // A listening but unresponsive old relay. It must not be the healthy relay's
    // alias: the old route must never accidentally deliver the Initial.
    let old = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let old_url: RelayUrl = format!("https://{}", old.local_addr().unwrap())
        .parse()
        .unwrap();
    let host = Endpoint::builder(presets::Minimal)
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .relay_mode(RelayMode::Custom(RelayConfig::new(relay_url, None).into()))
        .clear_ip_transports()
        .alpns(vec![b"fresh-route-test".to_vec()])
        .bind()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), host.online())
        .await
        .unwrap();
    let client = Endpoint::builder(presets::Minimal)
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .relay_mode(RelayMode::Custom(
            RelayConfig::new(old_url.clone(), None).into(),
        ))
        .clear_ip_transports()
        .address_lookup(DelayedLookup(
            EndpointAddr::new(host.id()).with_relay_url(delayed_url),
        ))
        .bind()
        .await
        .unwrap();
    let start = Instant::now();
    let (result, elapsed) = first_request(
        &host,
        &client,
        EndpointAddr::new(host.id()).with_relay_url(old_url),
    )
    .await;
    let dial_after = dial_rx.try_recv().unwrap().saturating_duration_since(start);
    proxy.abort();
    let _ = proxy.await;
    relay.shutdown().await.unwrap();
    eprintln!("discovered relay dial: {dial_after:?}; first request: {elapsed:?}, {result:?}");
    result.expect("a discovered relay must start before the next QUIC retransmission");
}
