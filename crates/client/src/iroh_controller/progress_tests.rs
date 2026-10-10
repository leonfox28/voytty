//! Real TLS/QUIC with in-memory packets: partial reads survive cancellation.
use super::*;
use std::time::Duration;

#[tokio::test]
async fn stream_progress_distinguishes_partial_prefix_body_and_complete_frame() {
    let network = iroh::test_utils::test_transport::TestNetwork::new();
    let builder = |key: SecretKey| {
        Endpoint::builder(presets::Minimal)
            .clear_ip_transports()
            .clear_relay_transports()
            .add_custom_transport(
                network
                    .create_transport(key.public())
                    .expect("in-memory QUIC fixture"),
            )
            .address_lookup(network.address_lookup())
            .secret_key(key)
    };
    let host = builder(SecretKey::generate())
        .alpns(vec![b"stream-progress".to_vec()])
        .bind()
        .await
        .expect("in-memory QUIC fixture");
    let client = builder(SecretKey::generate())
        .bind()
        .await
        .expect("in-memory QUIC fixture");
    let (outgoing, incoming) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(client.connect(host.id(), b"stream-progress"), async {
            host.accept()
                .await
                .expect("in-memory QUIC fixture")
                .await
                .expect("in-memory QUIC fixture")
        })
    })
    .await
    .expect("in-memory QUIC fixture");
    let outgoing = outgoing.expect("in-memory QUIC fixture");
    let (send, recv) = outgoing.open_bi().await.expect("in-memory QUIC fixture");
    let mut stream = IrohSessionIo {
        diagnostic: zterm_diagnostics::Operation::default(),
        connection: outgoing,
        last_stream_sample: None,
        read_bytes: 0,
        decoded_frames: 0,
        last_path: None,
        send,
        recv,
        decoder: FrameDecoder::new(),
        queued: VecDeque::new(),
        _permit: Arc::new(Semaphore::new(1))
            .acquire_owned()
            .await
            .expect("in-memory QUIC fixture"),
    };
    stream
        .write(b"start")
        .await
        .expect("in-memory QUIC fixture");
    let (mut host_send, mut host_recv) =
        incoming.accept_bi().await.expect("in-memory QUIC fixture");
    let message = v2::ServiceError {
        code: "unavailable".into(),
        message: "PRIVATE_FRAME_TEXT".repeat(2048),
    };
    let bytes = zterm_proto::encode_message(WireKind::ServiceErrorResponse, 7, 0, &message)
        .expect("in-memory QUIC fixture");
    assert!(bytes[0] & 0x80 != 0 && bytes[1] & 0x80 != 0);
    let total = bytes.len() as u64;
    let host_task = tokio::spawn(async move {
        let mut command = [0; 5];
        host_recv
            .read_exact(&mut command)
            .await
            .expect("in-memory QUIC fixture");
        assert_eq!(&command, b"start");
        host_send
            .write_all(&bytes[..2])
            .await
            .expect("in-memory QUIC fixture");
        host_recv
            .read_exact(&mut command)
            .await
            .expect("in-memory QUIC fixture");
        assert_eq!(&command, b"next!");
        host_send
            .write_all(&bytes[2..1026])
            .await
            .expect("in-memory QUIC fixture");
        host_recv
            .read_exact(&mut command)
            .await
            .expect("in-memory QUIC fixture");
        assert_eq!(&command, b"next!");
        host_send
            .write_all(&bytes[1026..])
            .await
            .expect("in-memory QUIC fixture");
        host_send
    });
    for expected in [2, 1026] {
        let incomplete = tokio::time::timeout(Duration::from_millis(100), async {
            loop {
                if let AttachmentTransportItem::Session(_) =
                    stream.read().await.expect("in-memory QUIC fixture")
                {
                    panic!("incomplete frame must not escape the decoder");
                }
            }
        })
        .await;
        assert!(incomplete.is_err());
        let progress = stream.stream_progress();
        assert_eq!(progress.read_bytes, expected);
        assert_eq!(progress.buffered_bytes, expected);
        assert_eq!(progress.decoded_frames, 0);
        assert_eq!(progress.queued_frames, 0);
        assert!(progress.connection_stream_frames > 0);
        // Writing a command between cancelled reads must retain both the
        // incomplete prefix and body in this exact stream's decoder.
        stream
            .write(b"next!")
            .await
            .expect("in-memory QUIC fixture");
    }
    let frame = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let AttachmentTransportItem::Session(frame) =
                stream.read().await.expect("in-memory QUIC fixture")
            {
                break frame;
            }
        }
    })
    .await
    .expect("in-memory QUIC fixture");
    assert_eq!(frame.request_id, 7);
    assert_eq!(
        frame
            .decode_message::<v2::ServiceError>(WireKind::ServiceErrorResponse)
            .expect("in-memory QUIC fixture"),
        message
    );
    let progress = stream.stream_progress();
    assert_eq!(progress.read_bytes, total);
    assert_eq!(progress.buffered_bytes, 0);
    assert_eq!(progress.decoded_frames, 1);
    assert_eq!(progress.queued_frames, 0);
    let mut host_send = host_task.await.expect("in-memory QUIC fixture");
    host_send.finish().expect("finish the original stream");
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match stream.read().await {
                Ok(AttachmentTransportItem::Path(_)) => {}
                Ok(AttachmentTransportItem::Session(_)) => panic!("only one frame was sent"),
                Err(error) => break error,
            }
        }
    })
    .await
    .expect("observe EOF");
    assert_eq!(ended.kind(), DomainErrorKind::Cancelled);
    assert_eq!(
        stream.stream_progress().read_bytes,
        total,
        "closed QUIC receive state cannot turn known consumed bytes into zero"
    );
    drop(host_send);
    drop(stream);
    tokio::join!(client.close(), host.close());
}
