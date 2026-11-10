//! Explicit upgrades reuse NAT traversal without reconnecting or bypassing its throttle.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use tracing_subscriber::{Layer, layer::Context, prelude::*};

use super::{Endpoint, presets};
use crate::{EndpointAddr, RelayConfig, RelayMode, SecretKey, TransportAddr};

#[derive(Clone, Default)]
struct NatRounds(Arc<AtomicUsize>);

impl<S: tracing::Subscriber> Layer<S> for NatRounds {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        if event.metadata().target() == "iroh::_events::qnt::init" {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl NatRounds {
    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn voytty_direct_path_request_reuses_and_throttles_nat_traversal() {
    let rounds = NatRounds::default();
    let _subscriber =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(rounds.clone()));
    let (_, url, relay) = crate::test_utils::run_relay_server_with(false)
        .await
        .unwrap();
    // Both peers advertise candidates but have no UDP transport. Thus the first
    // traversal round completes without a direct path, leaving a working relay.
    let builder = |port| {
        Endpoint::builder(presets::Minimal)
            .clear_ip_transports()
            .external_addr(([127, 0, 0, 1], port).into())
            .ca_tls_config(crate::tls::CaTlsConfig::insecure_skip_verify())
            .relay_mode(RelayMode::Custom(
                RelayConfig::new(url.clone(), None).into(),
            ))
    };
    let host = builder(12341)
        .alpns(vec![b"upgrade-test".to_vec()])
        .bind()
        .await
        .unwrap();
    let client = builder(12342).bind().await.unwrap();
    assert!(
        !client.request_direct_path(host.id()),
        "unknown peers must not be dialed"
    );
    tokio::time::timeout(Duration::from_secs(5), host.online())
        .await
        .unwrap();
    let (outgoing, incoming) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            client.connect(
                EndpointAddr::new(host.id()).with_relay_url(url),
                b"upgrade-test"
            ),
            async { host.accept().await.unwrap().await.unwrap() },
        )
    })
    .await
    .unwrap();
    let outgoing = outgoing.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while rounds.count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial NAT traversal must have run");
    assert!(outgoing.paths().iter().any(|path| path.is_selected()
        && matches!(path.remote_addr(), TransportAddr::Relay(_))));

    // Let the initial five-second throttle expire, far before the 60-second
    // periodic check. The explicit request must initiate a fresh NAT round.
    tokio::time::sleep(Duration::from_millis(5100)).await;
    let before = rounds.count();
    for _ in 0..8 {
        assert!(client.request_direct_path(host.id()));
    }
    client.remote_info(host.id()).await.unwrap(); // actor inbox barrier
    assert_eq!(
        rounds.count(),
        before + 1,
        "requests must share the existing throttle"
    );

    // The existing QUIC connection still carries traffic while direct fails.
    let (mut send, _) = outgoing.open_bi().await.unwrap();
    send.write_all(b"relay remains usable").await.unwrap();
    send.finish().unwrap();
    let (_, mut recv) = incoming.accept_bi().await.unwrap();
    assert_eq!(recv.read_to_end(64).await.unwrap(), b"relay remains usable");
    assert!(outgoing.close_reason().is_none());
    tokio::time::timeout(Duration::from_secs(6), async {
        while rounds.count() == before + 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("throttled requests must retain their scheduled attempt");
    assert_eq!(
        rounds.count(),
        before + 2,
        "duplicate requests must coalesce"
    );
    tokio::join!(client.close(), host.close());
    assert!(!client.request_direct_path(host.id()));
    relay.shutdown().await.unwrap();
}

#[tokio::test]
async fn voytty_direct_path_request_keeps_an_existing_direct_connection() {
    let rounds = NatRounds::default();
    let _subscriber =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(rounds.clone()));
    let builder = || {
        Endpoint::builder(presets::Minimal)
            .clear_ip_transports()
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
    };
    let host = builder()
        .alpns(vec![b"upgrade-test".to_vec()])
        .bind()
        .await
        .unwrap();
    let client = builder().bind().await.unwrap();
    let address = EndpointAddr::new(host.id()).with_ip_addr(host.bound_sockets()[0]);
    let (outgoing, incoming) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(client.connect(address, b"upgrade-test"), async {
            host.accept().await.unwrap().await.unwrap()
        })
    })
    .await
    .unwrap();
    let outgoing = outgoing.unwrap();
    assert!(
        outgoing
            .paths()
            .iter()
            .any(|path| path.is_selected() && path.is_ip())
    );
    client.remote_info(host.id()).await.unwrap();
    let before = rounds.count();
    assert!(client.request_direct_path(host.id()));
    client.remote_info(host.id()).await.unwrap();
    assert_eq!(rounds.count(), before);
    assert!(!client.request_direct_path(SecretKey::generate().public()));
    assert!(outgoing.close_reason().is_none() && incoming.close_reason().is_none());
    tokio::join!(client.close(), host.close());
}
