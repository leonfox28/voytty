//! Close ownership regressions using real TLS and in-memory delivery only.

use std::{sync::Arc, time::Duration};

use futures_util::poll;

use crate::{
    Endpoint, SecretKey,
    endpoint::{Connection, presets},
    test_utils::{reset_transport::ResetTransport, test_transport::TestNetwork},
};

/// Independent packet delay with bounded task ownership. No OS network sockets.
#[derive(Debug)]
struct DelayedTransport {
    inner: Arc<dyn crate::endpoint::transports::CustomTransport>,
    delay: Duration,
    tasks: Arc<std::sync::Mutex<tokio::task::JoinSet<()>>>,
}

#[derive(Debug)]
struct DelayedEndpoint {
    inner: Box<dyn crate::endpoint::transports::CustomEndpoint>,
    delay: Duration,
    tasks: Arc<std::sync::Mutex<tokio::task::JoinSet<()>>>,
}

#[derive(Debug)]
struct DelayedSender {
    inner: Arc<dyn crate::endpoint::transports::CustomSender>,
    delay: Duration,
    tasks: Arc<std::sync::Mutex<tokio::task::JoinSet<()>>>,
}

impl crate::endpoint::transports::CustomTransport for DelayedTransport {
    fn bind(&self) -> std::io::Result<Box<dyn crate::endpoint::transports::CustomEndpoint>> {
        Ok(Box::new(DelayedEndpoint {
            inner: self.inner.bind()?,
            delay: self.delay,
            tasks: self.tasks.clone(),
        }))
    }
}

impl crate::endpoint::transports::CustomEndpoint for DelayedEndpoint {
    fn watch_local_addrs(&self) -> n0_watcher::Direct<Vec<iroh_base::CustomAddr>> {
        self.inner.watch_local_addrs()
    }

    fn create_sender(&self) -> Arc<dyn crate::endpoint::transports::CustomSender> {
        Arc::new(DelayedSender {
            inner: self.inner.create_sender(),
            delay: self.delay,
            tasks: self.tasks.clone(),
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut std::task::Context,
        bufs: &mut [std::io::IoSliceMut<'_>],
        metas: &mut [noq_udp::RecvMeta],
        infos: &mut [crate::endpoint::transports::RecvInfo],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.inner.poll_recv(cx, bufs, metas, infos)
    }
}

impl crate::endpoint::transports::CustomSender for DelayedSender {
    fn is_valid_send_addr(&self, addr: &iroh_base::CustomAddr) -> bool {
        self.inner.is_valid_send_addr(addr)
    }

    fn poll_send(
        &self,
        _cx: &mut std::task::Context,
        dst: &iroh_base::CustomAddr,
        src: Option<&iroh_base::CustomAddr>,
        transmit: &crate::endpoint::transports::Transmit<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let inner = self.inner.clone();
        let dst = dst.clone();
        let src = src.cloned();
        let contents = transmit.contents.to_vec();
        let segment_size = transmit.segment_size;
        let ecn = transmit.ecn;
        let delay = self.delay;
        self.tasks.lock().unwrap().spawn(async move {
            tokio::time::sleep(delay).await;
            let transmit = crate::endpoint::transports::Transmit {
                contents: &contents,
                segment_size,
                ecn,
            };
            std::future::poll_fn(|cx| inner.poll_send(cx, &dst, src.as_ref(), &transmit))
                .await
                .unwrap();
        });
        std::task::Poll::Ready(Ok(()))
    }
}

async fn slow_peer_close(block_reply: bool) {
    let network = TestNetwork::new();
    let host_key = SecretKey::generate();
    let client_key = SecretKey::generate();
    let tasks = Arc::new(std::sync::Mutex::new(tokio::task::JoinSet::new()));
    let host_gate = Arc::new(ResetTransport::new(
        network.create_transport(host_key.public()).unwrap(),
    ));
    let client_gate = Arc::new(ResetTransport::new(
        network.create_transport(client_key.public()).unwrap(),
    ));
    let builder = |key, gate: Arc<ResetTransport>| {
        Endpoint::builder(presets::Minimal)
            .secret_key(key)
            .clear_ip_transports()
            .clear_relay_transports()
            .address_lookup(network.address_lookup())
            .add_custom_transport(Arc::new(DelayedTransport {
                inner: gate,
                delay: Duration::from_secs(1),
                tasks: tasks.clone(),
            }))
            .alpns(vec![b"close-test".to_vec()])
    };
    let host = builder(host_key, host_gate).bind().await.unwrap();
    let client = builder(client_key, client_gate.clone())
        .bind()
        .await
        .unwrap();
    let (outgoing, incoming) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(client.connect(host.id(), b"close-test"), async {
            host.accept().await.unwrap().await.unwrap()
        })
    })
    .await
    .unwrap();
    let outgoing = outgoing.unwrap();
    let ((), ()) = tokio::join!(
        async {
            let (mut send, mut recv) = outgoing.open_bi().await.unwrap();
            send.write_all(b"ready").await.unwrap();
            send.finish().unwrap();
            assert_eq!(recv.read_to_end(32).await.unwrap(), b"ready");
        },
        async {
            let (mut send, mut recv) = incoming.accept_bi().await.unwrap();
            assert_eq!(recv.read_to_end(32).await.unwrap(), b"ready");
            send.write_all(b"ready").await.unwrap();
            send.finish().unwrap();
        }
    );
    let rtt = incoming.rtt(noq_proto::PathId::ZERO).unwrap();
    assert!(rtt >= Duration::from_millis(1800), "observed RTT: {rtt:?}");
    if block_reply {
        client_gate.block_sends(true);
    }
    incoming.close(42u32.into(), b"shutdown proof");
    let inner = host.inner().unwrap();
    let sock = inner.sock.clone();
    let start = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(5), host.close()).await;
    println!(
        "close: block_reply={block_reply} rtt={rtt:?} elapsed={:?} result={result:?}",
        start.elapsed()
    );
    assert!(
        matches!(outgoing.close_reason(), Some(noq::ConnectionError::ApplicationClosed(close))
        if close.error_code == 42u32.into() && close.reason == b"shutdown proof"[..]),
        "the peer must receive the application close code before teardown"
    );
    if block_reply {
        assert!(
            result.is_err(),
            "no reply plus high PTO must preserve the caller's timeout"
        );
        assert!(!host.is_closed());
        assert!(poll!(Box::pin(inner.noq_endpoint().wait_all_draining())).is_pending());
        assert!(!sock.shutdown.at_endpoint_closed.is_cancelled());
        assert!(
            client_gate.dropped() > 0,
            "the close response was suppressed"
        );
        // The timeout cancelled the first close. A retry must still wait for QUIC,
        // rather than silently returning success because shutdown has started.
        assert!(poll!(Box::pin(host.close())).is_pending());
    } else {
        result.expect("a responsive slow peer closes within the original five seconds");
        assert!(host.is_closed());
    }
    drop(inner);
    drop(host);
    assert!(sock.shutdown.at_endpoint_closed.is_cancelled());
    assert!(sock.is_closed());
    client_gate.block_sends(false);
    client.close().await;
    let mut pending_packets = std::mem::take(&mut *tasks.lock().unwrap());
    pending_packets.shutdown().await;
}

#[tokio::test]
async fn zterm_close_slow_peer_preserves_close_code_within_deadline() {
    slow_peer_close(false).await;
}

#[tokio::test]
async fn zterm_close_slow_unresponsive_peer_keeps_deadline_failure() {
    slow_peer_close(true).await;
}

async fn pair() -> (
    Endpoint,
    Endpoint,
    Connection,
    Connection,
    Arc<ResetTransport>,
) {
    let network = TestNetwork::new();
    let host_key = SecretKey::generate();
    let client_key = SecretKey::generate();
    let gate = Arc::new(ResetTransport::new(
        network.create_transport(host_key.public()).unwrap(),
    ));
    let builder = || {
        Endpoint::builder(presets::Minimal)
            .clear_ip_transports()
            .clear_relay_transports()
            .address_lookup(network.address_lookup())
    };
    let host = builder()
        .secret_key(host_key)
        .add_custom_transport(gate.clone())
        .alpns(vec![b"close-test".to_vec()])
        .bind()
        .await
        .unwrap();
    let client = builder()
        .secret_key(client_key.clone())
        .add_custom_transport(network.create_transport(client_key.public()).unwrap())
        .bind()
        .await
        .unwrap();
    let (outgoing, incoming) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(client.connect(host.id(), b"close-test"), async {
            host.accept().await.unwrap().await.unwrap()
        })
    })
    .await
    .unwrap();
    (host, client, incoming, outgoing.unwrap(), gate)
}

#[tokio::test]
async fn zterm_close_cancelled_can_resume() {
    let (host, client, _incoming, _outgoing, gate) = pair().await;
    gate.block_sends(true);
    {
        let close = host.close();
        tokio::pin!(close);
        assert!(poll!(&mut close).is_pending());
    }
    assert!(!host.is_closed());
    tokio::time::timeout(Duration::from_secs(5), host.close())
        .await
        .expect("resumed close finishes within the original deadline");
    let host_closed = host.is_closed();
    client.close().await;
    assert!(
        host_closed,
        "a resumed close must finish the actual shutdown"
    );
}

#[tokio::test]
async fn zterm_close_concurrent_call_waits_for_shutdown() {
    let (host, client, _incoming, _outgoing, gate) = pair().await;
    gate.block_sends(true);
    let first = host.close();
    tokio::pin!(first);
    assert!(poll!(&mut first).is_pending());
    let second = host.close();
    tokio::pin!(second);
    let second_pending = poll!(&mut second).is_pending();
    tokio::time::timeout(Duration::from_secs(5), &mut first)
        .await
        .expect("first close finishes");
    if second_pending {
        second.await;
    }
    client.close().await;
    assert!(
        second_pending,
        "concurrent close must not report premature success"
    );
}

#[tokio::test]
async fn zterm_close_drop_after_cancel_aborts_remaining_tasks() {
    let (host, client, incoming, _outgoing, gate) = pair().await;
    gate.block_sends(true);
    let sock = host.inner().unwrap().sock.clone();
    let runtime = host.inner().unwrap().runtime.clone();
    let (dropped, released) = tokio::sync::oneshot::channel();
    struct NotifyDrop(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for NotifyDrop {
        fn drop(&mut self) {
            let _ = self.0.take().unwrap().send(());
        }
    }
    let guard = NotifyDrop(Some(dropped));
    noq::Runtime::spawn(
        &*runtime,
        Box::pin(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        }),
    );
    {
        let close = host.close();
        tokio::pin!(close);
        assert!(poll!(&mut close).is_pending());
    }
    drop(incoming);
    drop(host);
    client.close().await;
    assert!(sock.shutdown.at_endpoint_closed.is_cancelled());
    assert!(
        sock.is_closed(),
        "last owner must abort a cancelled shutdown"
    );
    tokio::time::timeout(Duration::from_secs(1), released)
        .await
        .expect("runtime tasks release their owned resources")
        .unwrap();
}

#[tokio::test]
async fn zterm_close_cancelled_actor_wait_retains_join_handle() {
    let network = TestNetwork::new();
    let key = SecretKey::generate();
    let host = Endpoint::builder(presets::Minimal)
        .secret_key(key.clone())
        .clear_ip_transports()
        .clear_relay_transports()
        .add_custom_transport(network.create_transport(key.public()).unwrap())
        .bind()
        .await
        .unwrap();
    let inner = host.inner().unwrap();
    // Replace only the actor with a deterministic slow cleanup task; QUIC is idle.
    let (finish, finished) = tokio::sync::oneshot::channel();
    let task = n0_future::task::spawn(async move {
        finished.await.unwrap();
    });
    let actor = task.abort_handle();
    let old = inner
        .actor_task
        .lock()
        .await
        .replace(n0_future::task::AbortOnDropHandle::new(task));
    let old = old.unwrap();
    old.abort();
    let _ = old.await;
    {
        let close = host.close();
        tokio::pin!(close);
        assert!(poll!(&mut close).is_pending());
        assert!(inner.sock.shutdown.at_endpoint_closed.is_cancelled());
    }
    tokio::task::yield_now().await;
    assert!(
        !actor.is_finished(),
        "cancelling the wait must retain actor ownership"
    );
    finish.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), host.close())
        .await
        .unwrap();
    assert!(actor.is_finished());
    assert!(host.is_closed());
}
