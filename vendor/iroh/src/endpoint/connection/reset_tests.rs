//! Restart recovery on a nonzero path after path zero has been retired.

use std::{sync::Arc, time::Duration};

use crate::{
    Endpoint, SecretKey,
    endpoint::presets,
    test_utils::{reset_transport::ResetTransport, test_transport::TestNetwork},
};

#[tokio::test]
async fn zterm_restart_reset_recovers_after_path_zero_is_retired() {
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
            .persistent_stateless_reset(true)
    };
    let bind_host = || {
        builder()
            .secret_key(host_key.clone())
            .add_custom_transport(gate.clone())
            .alpns(vec![b"reset-test".to_vec()])
            .bind()
    };
    let host = bind_host().await.unwrap();
    let client = builder()
        .secret_key(client_key.clone())
        .add_custom_transport(network.create_transport(client_key.public()).unwrap())
        .bind()
        .await
        .unwrap();
    let (outgoing, incoming) = tokio::join!(client.connect(host.id(), b"reset-test"), async {
        host.accept().await.unwrap().await.unwrap()
    });
    let outgoing = outgoing.unwrap();
    // A real stream round-trip ensures the handshake and CID advertisements
    // are processed before opening the additional multipath path.
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
    let initial = outgoing.inner.path(noq_proto::PathId::ZERO).unwrap();
    let path = tokio::time::timeout(
        Duration::from_secs(5),
        outgoing.inner.open_path(
            initial.network_path().unwrap(),
            noq_proto::PathStatus::Available,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(path.id(), noq_proto::PathId::ZERO);
    initial.close().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while outgoing.inner.path(noq_proto::PathId::ZERO).is_some()
            || incoming.inner.path(noq_proto::PathId::ZERO).is_some()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial path discarded on both endpoints");

    gate.block_sends(true);
    host.close().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(host.is_closed());
    assert!(gate.dropped() > 0);
    assert!(
        outgoing.close_reason().is_none(),
        "old connection still appears live"
    );
    let replacement = bind_host().await.unwrap();
    gate.block_sends(false);
    path.ping().unwrap();
    let reset = tokio::time::timeout(Duration::from_secs(5), outgoing.closed()).await;
    tokio::join!(client.close(), replacement.close());
    assert!(
        matches!(reset, Ok(noq::ConnectionError::Reset)),
        "expected a recognized reset on the remaining nonzero path: {reset:?}"
    );
}
