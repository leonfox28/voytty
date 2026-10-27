//! A writable relay must still consume ready peer traffic during a send backlog.

use std::{
    pin::Pin,
    sync::{Mutex, atomic::AtomicUsize},
    task::{Context, Poll},
};

use iroh_relay::tls::{CaTlsConfig, default_provider};

use super::*;

const OUTGOING: usize = SEND_DATAGRAM_BATCH_SIZE * 10;
const RECEIVE_LIMIT: usize = OUTGOING * 10;

/// Immediate writes model an uncongested local TCP socket. Stop at a finite
/// number of writes so a biased actor cannot hide starvation behind a timeout.
struct WritableRelay {
    sent: Arc<AtomicUsize>,
    stop: CancellationToken,
    pong: Arc<Mutex<Option<[u8; 8]>>>,
    fail_after_backlog: bool,
}

impl n0_future::Sink<ClientToRelayMsg> for WritableRelay {
    type Error = RunError;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), RunError>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, item: ClientToRelayMsg) -> Result<(), RunError> {
        if let ClientToRelayMsg::Ping(data) = item {
            *self.pong.lock().unwrap() = Some(data);
        }
        if matches!(item, ClientToRelayMsg::Datagrams { .. })
            && self.sent.fetch_add(1, Ordering::SeqCst) + 1 == OUTGOING
        {
            if self.fail_after_backlog {
                return Err(e!(RunError::SendTimeout));
            }
            self.stop.cancel();
        }
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), RunError>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), RunError>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn voytty_relay_backlog_keeps_both_directions_progressing() {
    exercise_ready_io(false, false).await;
}

#[tokio::test]
async fn voytty_relay_backlog_consumes_ready_pong() {
    exercise_ready_io(false, true).await;
}

#[tokio::test]
async fn voytty_relay_cancel_preempts_ready_traffic() {
    exercise_ready_io(true, false).await;
}

async fn exercise_ready_io(cancelled: bool, fail_after_backlog: bool) {
    let url: RelayUrl = "https://relay.invalid".parse().unwrap();
    let remote = SecretKey::from_bytes(&[0x62; 32]).public();
    let (_prio_tx, prio_rx) = mpsc::channel(8);
    let (inbox_tx, inbox_rx) = mpsc::channel(8);
    let (send_tx, send_rx) = mpsc::channel(OUTGOING);
    let (recv_tx, mut recv_rx) = mpsc::channel(OUTGOING);
    let stop = CancellationToken::new();
    let mut actor = ActiveRelayActor::new(ActiveRelayActorOptions {
        url: url.clone(),
        prio_inbox_: prio_rx,
        inbox: inbox_rx,
        relay_datagrams_send: send_rx,
        relay_datagrams_recv: recv_tx,
        connection_opts: RelayConnectionOptions {
            secret_key: SecretKey::from_bytes(&[0x61; 32]),
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
    for _ in 0..OUTGOING {
        send_tx
            .try_send(RelaySendItem {
                queued_at: Instant::now(),
                remote_endpoint: remote,
                url: url.clone(),
                datagrams: Datagrams::from(b"outgoing"),
            })
            .unwrap();
    }
    let received = Arc::new(AtomicUsize::new(0));
    let pong = Arc::new(Mutex::new(None));
    let stream = n0_future::stream::poll_fn({
        let received = received.clone();
        let stop = stop.clone();
        let pong = pong.clone();
        move |_| {
            // Also terminate a naive receive-first fix that starves all sends.
            if received.fetch_add(1, Ordering::SeqCst) + 1 == RECEIVE_LIMIT {
                stop.cancel();
            }
            if let Some(data) = pong.lock().unwrap().take() {
                return Poll::Ready(Some(Ok(RelayToClientMsg::Pong(data))));
            }
            Poll::Ready(Some(Ok(RelayToClientMsg::Datagrams {
                remote_endpoint_id: remote,
                datagrams: Datagrams::from(b"incoming"),
            })))
        }
    });
    let sent = Arc::new(AtomicUsize::new(0));
    let sink = WritableRelay {
        sent: sent.clone(),
        stop: stop.clone(),
        pong,
        fail_after_backlog,
    };
    if cancelled {
        stop.cancel();
    }
    // Trigger the real health-check path without depending on a sub-millisecond
    // timer tick in this entirely ready, socket-free fixture.
    let local_addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
    inbox_tx
        .try_send(ActiveRelayMessage::CheckConnection {
            local_ips: vec![local_addr.ip()],
        })
        .unwrap();
    let result = actor.run_connected_io(stream, sink, Some(local_addr)).await;
    if fail_after_backlog {
        assert_eq!(
            result.unwrap_err().diagnostic(),
            (12, 10),
            "the ready pong must establish relay liveness during the send backlog"
        );
    } else {
        result.unwrap();
    }
    if cancelled {
        assert_eq!(sent.load(Ordering::SeqCst), 0);
        assert_eq!(received.load(Ordering::SeqCst), 0);
        assert!(recv_rx.try_recv().is_err());
    } else {
        assert_eq!(sent.load(Ordering::SeqCst), OUTGOING);
        assert!(received.load(Ordering::SeqCst) < RECEIVE_LIMIT);
        assert!(
            recv_rx.try_recv().is_ok(),
            "ready received datagrams were starved throughout ten outbound batches"
        );
    }
}
