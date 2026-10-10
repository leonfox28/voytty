//! Shared real TLS/Hello/Session query fixture for production endpoint profiles.
//! Only temporary identities and the in-memory transport participate in faults.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use iroh::test_utils::{reset_transport::ResetTransport, test_transport::TestNetwork};
use iroh::{
    Endpoint, SecretKey,
    endpoint::{Builder, Connection},
};
use zterm_client::{
    handshake::{read_one_frame, write_handshake_message},
    iroh_controller::{IrohController, ZTERM_ALPN},
    model::ResolvedSessionTarget,
    unary::{SessionUnaryRequest, SessionUnaryTransport},
};
use zterm_core::{AuthGeneration, Capabilities, ConnectionWelcome, DeviceId, RelayHint};
use zterm_proto::{FrameDecoder, WireKind, encode_message, v2};

async fn list(
    controller: &IrohController,
    remote: DeviceId,
) -> Result<(), zterm_client::error::ClientError> {
    let bytes = encode_message(
        WireKind::SessionListRequest,
        0,
        0,
        &v2::SessionListRequest {
            target: Some(v2::TargetSelector {
                target: Some(v2::target_selector::Target::Device(remote.into())),
            }),
        },
    )
    .expect("request encoding");
    let payload = FrameDecoder::new()
        .feed(&bytes)
        .expect("request frame")
        .pop()
        .expect("request")
        .payload;
    controller
        .request(SessionUnaryRequest {
            target: ResolvedSessionTarget::device(remote),
            request_kind: WireKind::SessionListRequest,
            response_kind: WireKind::SessionListResponse,
            payload,
            deadline: Duration::from_secs(5),
            // Keep the original five-second restart regression as a stricter check.
            startup_deadline: Duration::from_secs(5),
            mutation_or_lease_retry: false,
        })
        .await?;
    Ok(())
}

async fn serve(connection: Connection, expected: DeviceId, requests: Arc<AtomicUsize>) {
    assert_eq!(connection.remote_id().as_bytes(), expected.as_bytes());
    let deadline = Instant::now() + Duration::from_secs(5);
    let (mut send, mut recv) = connection.accept_bi().await.expect("Hello stream");
    let hello = read_one_frame(&mut recv, 16384, deadline)
        .await
        .expect("Hello");
    let _: v2::ConnectionHello = hello
        .decode_message(WireKind::ConnectionHello)
        .expect("Hello message");
    let welcome = ConnectionWelcome::new(
        zterm_proto::WIRE_MAJOR,
        Capabilities::from_bits_retain(
            Capabilities::SESSION_SERVICE | Capabilities::TERMINAL_SERVICE,
        ),
        "restart test",
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
    send.finish().expect("Welcome FIN");
    while let Ok((mut send, mut recv)) = connection.accept_bi().await {
        let frame = read_one_frame(&mut recv, 16384, Instant::now() + Duration::from_secs(5))
            .await
            .expect("query");
        assert_eq!(frame.kind, WireKind::SessionListRequest);
        requests.fetch_add(1, Ordering::SeqCst);
        let bytes = encode_message(
            WireKind::SessionListResponse,
            frame.request_id,
            0,
            &v2::SessionListResponse {
                sessions: Vec::new(),
            },
        )
        .expect("response");
        send.write_all(&bytes).await.expect("response write");
        send.finish().expect("response FIN");
    }
}

fn listen(
    host: &Endpoint,
    client: DeviceId,
    accepts: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
) -> tokio::task::JoinHandle<()> {
    let listener = host.clone();
    tokio::spawn(async move {
        let mut handlers = tokio::task::JoinSet::new();
        while let Some(incoming) = listener.accept().await {
            let connection = incoming.await.expect("TLS");
            accepts.fetch_add(1, Ordering::SeqCst);
            handlers.spawn(serve(connection, client, requests.clone()));
        }
        while let Some(result) = handlers.join_next().await {
            result.expect("host task");
        }
    })
}

pub async fn first_query_recovers(make_builder: impl Fn(SecretKey) -> Builder) {
    let network = TestNetwork::new();
    let host_key = SecretKey::generate();
    let client_key = SecretKey::generate();
    let gate = Arc::new(ResetTransport::new(
        network
            .create_transport(host_key.public())
            .expect("host transport"),
    ));
    // Keep the production reset/QUIC/identity settings. Replace only external
    // delivery and discovery; the inert route below cannot send network traffic.
    let builder = |key| {
        make_builder(key)
            .clear_ip_transports()
            .clear_relay_transports()
            .clear_address_lookup()
            .clear_addr_filter()
            .address_lookup(network.address_lookup())
    };
    let bind_host = || {
        builder(host_key.clone())
            .add_custom_transport(gate.clone())
            .alpns(vec![ZTERM_ALPN.to_vec()])
            .bind()
    };
    let host = bind_host().await.expect("host");
    let client = builder(client_key.clone())
        .add_custom_transport(
            network
                .create_transport(client_key.public())
                .expect("client transport"),
        )
        .bind()
        .await
        .expect("client");
    let controller = IrohController::new(client, "restart test").expect("controller");
    let remote = DeviceId::from_array(*host.id().as_bytes());
    controller
        .remember_host(
            remote,
            vec![RelayHint::new("https://127.0.0.1:9").expect("inert hint")],
        )
        .expect("remember host");
    let accepts = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let serving = listen(
        &host,
        controller.device_id(),
        accepts.clone(),
        requests.clone(),
    );
    list(&controller, remote)
        .await
        .expect("original connection");

    gate.block_sends(true);
    host.close().await;
    serving.await.expect("old endpoint handlers exit");
    assert!(
        host.is_closed(),
        "only one live Endpoint owns this identity"
    );
    // Drain the old transport's queued close datagrams before reusing its test
    // address. This belongs to fault setup, outside the unchanged query budget.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        gate.dropped() > 0,
        "the fixture suppresses actual close datagrams"
    );
    let retained = controller
        .connection_info(remote)
        .await
        .expect("observation")
        .expect("cached connection");
    assert!(
        !retained.closed,
        "query must encounter an apparently open stale connection"
    );

    let host = bind_host().await.expect("replacement Endpoint");
    let serving = listen(
        &host,
        controller.device_id(),
        accepts.clone(),
        requests.clone(),
    );
    gate.block_sends(false);
    let started = Instant::now();
    let first = list(&controller, remote).await;
    let elapsed = started.elapsed();
    let first_accepts = accepts.load(Ordering::SeqCst);
    let first_requests = requests.load(Ordering::SeqCst);
    eprintln!(
        "first cached query after Endpoint restart: elapsed={elapsed:?}, error={:?}, accepts={first_accepts}, requests={first_requests}",
        first.as_ref().err().map(|e| e.kind())
    );
    let next = list(&controller, remote).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(controller.shutdown(), host.close());
    })
    .await
    .expect("bounded cleanup");
    serving.await.expect("replacement handlers exit");

    first.expect("the first query recovers inside its original deadline");
    assert!(elapsed < Duration::from_secs(5));
    assert_eq!(
        first_accepts, 2,
        "the same query obtains one new authenticated connection"
    );
    assert_eq!(
        first_requests, 2,
        "one original and one recovered query reach the host"
    );
    next.expect("next independent query");
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        2,
        "healthy replacement is reused"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 3);
}
