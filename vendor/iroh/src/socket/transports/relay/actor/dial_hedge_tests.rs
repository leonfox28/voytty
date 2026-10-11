//! A relay dial that makes no progress must not hold every new connection until
//! the dial deadline when a fresh attempt to the same relay would succeed.

use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use n0_future::time::{self, Instant};

use super::{CONNECT_TIMEOUT, DIAL_HEDGE_DELAY, hedged};

type Attempt = Pin<Box<dyn Future<Output = Result<u8, &'static str>> + Send>>;

/// `None` never completes, like a TLS handshake that receives nothing.
fn attempt(id: u8, outcome: Option<(Duration, bool)>) -> Attempt {
    Box::pin(async move {
        let Some((after, succeeds)) = outcome else {
            return std::future::pending().await;
        };
        time::sleep(after).await;
        if succeeds { Ok(id) } else { Err("failed") }
    })
}

/// Runs `first` and a counted second attempt; returns the result, its time and
/// how many second attempts were started.
async fn race(
    first: Option<(Duration, bool)>,
    second: Option<(Duration, bool)>,
) -> (Result<u8, &'static str>, Duration, usize) {
    let started = Arc::new(AtomicUsize::new(0));
    let counted = started.clone();
    let begin = Instant::now();
    let result = hedged(
        attempt(1, first),
        move || {
            counted.fetch_add(1, Ordering::SeqCst);
            attempt(2, second)
        },
        DIAL_HEDGE_DELAY,
    )
    .await;
    (result, begin.elapsed(), started.load(Ordering::SeqCst))
}

#[tokio::test(start_paused = true)]
async fn zterm_relay_dial_hedge_recovers_a_stalled_first_attempt() {
    let fast = Duration::from_millis(100);
    let (result, elapsed, hedges) = race(None, Some((fast, true))).await;
    assert_eq!(result, Ok(2));
    assert_eq!(hedges, 1);
    assert_eq!(elapsed, DIAL_HEDGE_DELAY + fast);
    assert!(elapsed < CONNECT_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn zterm_relay_dial_fast_attempt_does_not_hedge() {
    let (result, _, hedges) = race(Some((Duration::from_millis(500), true)), None).await;
    assert_eq!(result, Ok(1));
    assert_eq!(hedges, 0);
}

#[tokio::test(start_paused = true)]
async fn zterm_relay_dial_early_error_is_returned_without_hedging() {
    let (result, elapsed, hedges) = race(Some((Duration::from_millis(100), false)), None).await;
    assert_eq!(result, Err("failed"));
    assert_eq!(hedges, 0);
    assert_eq!(elapsed, Duration::from_millis(100));
}

#[tokio::test(start_paused = true)]
async fn zterm_relay_dial_hedge_does_not_cancel_a_progressing_attempt() {
    let slow = Duration::from_millis(3_500);
    let (result, elapsed, hedges) = race(Some((slow, true)), None).await;
    assert_eq!(result, Ok(1));
    assert_eq!(hedges, 1);
    assert_eq!(elapsed, slow);
}

#[tokio::test(start_paused = true)]
async fn zterm_relay_dial_failed_attempt_waits_for_the_other() {
    // The hedge fails quickly; the original attempt still connects.
    let (result, elapsed, _) = race(
        Some((Duration::from_secs(4), true)),
        Some((Duration::from_millis(50), false)),
    )
    .await;
    assert_eq!((result, elapsed), (Ok(1), Duration::from_secs(4)));
    // The original attempt fails after the hedge started; the hedge connects.
    let (result, elapsed, _) = race(
        Some((Duration::from_millis(3_200), false)),
        Some((Duration::from_millis(800), true)),
    )
    .await;
    assert_eq!(
        (result, elapsed),
        (Ok(2), DIAL_HEDGE_DELAY + Duration::from_millis(800))
    );
}

// Socket-based integration gate, like `cold_relay_tls_first_request_survives_slow_relay_connect`.
#[tokio::test]
async fn stalled_first_relay_tls_is_hedged_before_the_dial_deadline() {
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::task::AbortOnDropHandle;

    use crate::{
        Endpoint, EndpointAddr, RelayConfig, RelayMode, RelayUrl, endpoint::presets,
        tls::CaTlsConfig,
    };

    let (_, relay_url, relay) = crate::test_utils::run_relay_server_with(false)
        .await
        .unwrap();
    let relay_addr = relay.https_addr().unwrap();
    // A transparent proxy that accepts the first TCP connection and never connects
    // it onward, so its TLS handshake receives nothing. Later connections forward.
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let proxied_url: RelayUrl = format!("https://{}", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let proxy = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut forwards = tokio::task::JoinSet::new();
        let mut stalled = Vec::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut downstream, _) = accepted.unwrap();
                    if stalled.is_empty() {
                        stalled.push(downstream);
                        continue;
                    }
                    forwards.spawn(async move {
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
        .alpns(vec![b"stalled-relay-test".to_vec()])
        .bind()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), host.online())
        .await
        .unwrap();
    let client = Endpoint::builder(presets::Minimal)
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .relay_mode(RelayMode::Custom(
            RelayConfig::new(proxied_url.clone(), None).into(),
        ))
        .clear_ip_transports()
        .bind()
        .await
        .unwrap();
    let accepting = host.clone();
    let receiver = AbortOnDropHandle::new(tokio::spawn(async move {
        let connection = accepting.accept().await.unwrap().await.unwrap();
        let (mut send, mut recv) = connection.accept_bi().await.unwrap();
        let mut request = [0; 5];
        recv.read_exact(&mut request).await.unwrap();
        send.write_all(b"reply").await.unwrap();
        send.finish().unwrap();
        send.stopped().await.unwrap();
        connection.closed().await;
    }));
    // The product's whole first-request budget; only the elapsed time is asserted.
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let connection = client
            .connect(
                EndpointAddr::new(host.id()).with_relay_url(proxied_url),
                b"stalled-relay-test",
            )
            .await
            .unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        send.write_all(b"query").await.unwrap();
        send.finish().unwrap();
        let mut reply = [0; 5];
        recv.read_exact(&mut reply).await.unwrap();
        connection.close(0u32.into(), b"done");
    })
    .await;
    let elapsed = started.elapsed();
    eprintln!("stalled first relay TLS, first request: {elapsed:?}, {result:?}");
    receiver.abort();
    let _ = receiver.await;
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(client.close(), host.close());
    })
    .await;
    proxy.abort();
    let _ = proxy.await;
    relay.shutdown().await.unwrap();
    result.expect("first request within the 15 s budget");
    assert!(
        elapsed < CONNECT_TIMEOUT,
        "first request waited for the dead dial's deadline: {elapsed:?}"
    );
}
