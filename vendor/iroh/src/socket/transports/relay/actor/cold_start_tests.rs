//! Cold relay startup must not discard a packet that has only just been queued.

use std::future::pending;

use iroh_relay::tls::{CaTlsConfig, default_provider};

use super::*;

#[tokio::test(start_paused = true)]
async fn voytty_cold_relay_keeps_recent_datagram_across_cleanup_tick() {
    let url: RelayUrl = "https://relay.invalid".parse().unwrap();
    let (_prio_tx, prio_rx) = mpsc::channel(8);
    let (_inbox_tx, inbox_rx) = mpsc::channel(8);
    let (send_tx, send_rx) = mpsc::channel(8);
    let (recv_tx, _recv_rx) = mpsc::channel(8);
    let stop = CancellationToken::new();
    let mut actor = ActiveRelayActor::new(ActiveRelayActorOptions {
        url: url.clone(),
        prio_inbox_: prio_rx,
        inbox: inbox_rx,
        relay_datagrams_send: send_rx,
        relay_datagrams_recv: recv_tx,
        connection_opts: RelayConnectionOptions {
            secret_key: SecretKey::from_bytes(&[0x39; 32]),
            dns_resolver: DnsResolver::new(),
            proxy_url: None,
            prefer_ipv6: Arc::new(AtomicBool::new(false)),
            tls_config: CaTlsConfig::insecure_skip_verify()
                .client_config(default_provider())
                .unwrap(),
            auth_token: None,
        },
        stop_token: stop.clone(),
        metrics: Default::default(),
        my_relay: Default::default(),
    });
    let dialing = tokio::spawn(async move {
        assert!(actor.run_dialing_with(pending()).await.is_none());
        actor
    });
    // The injected dial never completes and cannot open any network socket.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(2_900)).await;
    send_tx
        .send(RelaySendItem {
            queued_at: Instant::now(),
            remote_endpoint: SecretKey::from_bytes(&[0x3a; 32]).public(),
            url,
            datagrams: Datagrams::from(b"recent initial"),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop.cancel();
    let mut actor = dialing.await.unwrap();
    let mut retained = Vec::new();
    let result = tokio::time::timeout(
        Duration::from_millis(1),
        actor.relay_datagrams_send.recv_many(&mut retained, 8),
    )
    .await;
    assert_eq!(
        result.ok(),
        Some(1),
        "the three-second cleanup discarded a datagram only 200 ms old"
    );
    assert_eq!(retained[0].datagrams, Datagrams::from(b"recent initial"));
}

fn packet(data: &'static [u8]) -> RelaySendItem {
    RelaySendItem {
        queued_at: Instant::now(),
        remote_endpoint: SecretKey::from_bytes(&[0x3a; 32]).public(),
        url: "https://relay.invalid".parse().unwrap(),
        datagrams: Datagrams::from(data),
    }
}

#[tokio::test(start_paused = true)]
async fn voytty_cold_relay_expires_only_old_packets_and_preserves_order() {
    let (sender, receiver) = mpsc::channel(4);
    let mut queue = PendingDatagrams::from(receiver);
    sender.send(packet(b"expired")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(2_900)).await;
    sender.send(packet(b"recent one")).await.unwrap();
    sender.send(packet(b"recent two")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    queue.discard_expired();
    drop(sender);
    let mut retained = Vec::new();
    assert_eq!(queue.recv_many(&mut retained, 4).await, 2);
    assert_eq!(retained[0].datagrams, Datagrams::from(b"recent one"));
    assert_eq!(retained[1].datagrams, Datagrams::from(b"recent two"));
    assert_eq!(queue.recv_many(&mut retained, 4).await, 0);
}

#[tokio::test(start_paused = true)]
async fn voytty_cold_relay_rechecks_packet_age_after_backoff() {
    let (sender, receiver) = mpsc::channel(4);
    let mut queue = PendingDatagrams::from(receiver);
    sender.send(packet(b"formerly recent")).await.unwrap();
    queue.discard_expired();
    // A reconnect backoff can exceed the TTL without running a cleanup tick.
    tokio::time::sleep(UNDELIVERABLE_DATAGRAM_TIMEOUT).await;
    sender.send(packet(b"new retry")).await.unwrap();
    drop(sender);
    let mut retained = Vec::new();
    assert_eq!(queue.recv_many(&mut retained, 4).await, 1);
    assert_eq!(retained[0].datagrams, Datagrams::from(b"new retry"));
    assert_eq!(queue.recv_many(&mut retained, 4).await, 0);
}

// Socket-based integration gate; run on the isolated Linux test VM. The voytty_
// filter above remains usable on a Mac that refuses incoming test connections.
#[tokio::test]
async fn cold_relay_tls_first_request_survives_slow_relay_connect() {
    use crate::{Endpoint, EndpointAddr, RelayConfig, RelayMode, endpoint::presets};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::task::AbortOnDropHandle;

    let (_, relay_url, relay) = crate::test_utils::run_relay_server_with(false)
        .await
        .unwrap();
    let relay_addr = relay.https_addr().unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let delayed_url: RelayUrl = format!("https://{}", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let proxy = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut forwards = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut downstream, _) = accepted.unwrap();
                    forwards.spawn(async move {
                        tokio::time::sleep(Duration::from_millis(3_300)).await;
                        let mut upstream = TcpStream::connect(relay_addr).await.unwrap();
                        let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
                    });
                }
                _ = forwards.join_next(), if !forwards.is_empty() => {}
            }
        }
    }));
    let host = Endpoint::builder(presets::Minimal)
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .relay_mode(RelayMode::Custom(RelayConfig::new(relay_url, None).into()))
        .clear_ip_transports()
        .alpns(vec![b"cold-relay-test".to_vec()])
        .bind()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), host.online())
        .await
        .unwrap();
    let client = Endpoint::builder(presets::Minimal)
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .relay_mode(RelayMode::Custom(
            RelayConfig::new(delayed_url.clone(), None).into(),
        ))
        .clear_ip_transports()
        // Place a real QUIC retransmit before the three-second queue cleanup.
        // This only controls the fixture's PTO schedule, not the 5s request limit.
        .transport_config(
            crate::endpoint::QuicTransportConfig::builder()
                .initial_rtt(Duration::from_millis(300))
                .build(),
        )
        .bind()
        .await
        .unwrap();
    let accepting = host.clone();
    let receiver = AbortOnDropHandle::new(tokio::spawn(async move {
        let connection = accepting.accept().await.unwrap().await.unwrap();
        let (mut send, mut recv) = connection.accept_bi().await.unwrap();
        let mut request = [0; 5];
        recv.read_exact(&mut request).await.unwrap();
        assert_eq!(&request, b"query");
        send.write_all(b"reply").await.unwrap();
        send.finish().unwrap();
        send.stopped().await.unwrap();
        connection.closed().await;
    }));
    let started = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let connection = client
            .connect(
                EndpointAddr::new(host.id()).with_relay_url(delayed_url),
                b"cold-relay-test",
            )
            .await
            .unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        send.write_all(b"query").await.unwrap();
        send.finish().unwrap();
        let mut reply = [0; 5];
        recv.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"reply");
        connection.close(0u32.into(), b"done");
    })
    .await;
    eprintln!(
        "cold relay first request: {:?}, {result:?}",
        started.elapsed()
    );
    receiver.abort();
    let _ = receiver.await;
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(client.close(), host.close());
    })
    .await;
    proxy.abort();
    let _ = proxy.await;
    relay.shutdown().await.unwrap();
    result.expect("a fresh QUIC retransmit must survive the unrelated queue cleanup tick");
}
