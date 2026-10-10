//! Hidden local pairing-client framing and retry acceptance tests.
//!
//! These tests bind only task-private Unix-domain sockets and use the pure
//! in-memory pairing manager to mint tickets. They never create an Iroh
//! Endpoint, bind UDP, perform DNS, or contact public infrastructure.

#![cfg(unix)]

use std::path::PathBuf;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroize::{Zeroize, Zeroizing};
use zterm_core::{
    AuthGeneration, AuthorizationStatus, DEFAULT_PAIR_TTL_SECONDS, DeviceAlias, DeviceDisplayName,
    DeviceId, DeviceSummary, EphemeralOperationId, PairFingerprint, RelayHint, TransportLimits,
};
use zterm_daemon::local_ipc::LocalPairingClient;
use zterm_daemon::pairing::{PairOfferRequest, PairTicketText, PairingManager};
use zterm_proto::{DecodedFrame, FrameDecoder, WireKind, encode_message, v2};

const HOST: DeviceId = DeviceId::from_array([0x31; 32]);

#[tokio::test]
async fn accept_lost_responses_are_outcome_unknown_after_identical_retry() {
    let (_directory, socket) = socket_path();
    let listener = bind(&socket);
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.expect("first request");
        let (first_bytes, mut first_frame) = read_request(&mut first).await;
        assert_eq!(first_frame.kind, WireKind::LocalPairAcceptRequest);
        first_frame.payload.zeroize();
        drop(first);
        let (mut second, _) = listener.accept().await.expect("same-operation retry");
        let (second_bytes, mut second_frame) = read_request(&mut second).await;
        second_frame.payload.zeroize();
        assert_eq!(&*first_bytes, &*second_bytes);
        drop(second);
    });
    let error = LocalPairingClient::new(&socket)
        .accept(mint_ticket(), None)
        .await
        .expect_err("both replies lost after the request was delivered");
    server.await.expect("pair server");
    assert_eq!(
        error.kind(),
        zterm_core::DomainErrorKind::PairOutcomeUnknown
    );
}

#[tokio::test]
async fn accept_missing_daemon_before_any_write_is_not_ambiguous() {
    let (_directory, socket) = socket_path();
    let error = LocalPairingClient::new(&socket)
        .accept(mint_ticket(), None)
        .await
        .expect_err("no request reached the absent daemon");
    assert_eq!(error.kind(), zterm_core::DomainErrorKind::DaemonStopped);
}

#[tokio::test]
async fn accept_failed_retry_connect_does_not_erase_first_delivery() {
    let (_directory, socket) = socket_path();
    let listener = bind(&socket);
    let server_socket = socket.clone();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("delivered request");
        let (_, mut frame) = read_request(&mut stream).await;
        frame.payload.zeroize();
        drop(listener);
        std::fs::remove_file(server_socket).expect("remove stopped daemon socket");
        drop(stream);
    });
    let error = LocalPairingClient::new(&socket)
        .accept(mint_ticket(), None)
        .await
        .expect_err("reply lost and retry daemon unavailable");
    server.await.expect("pair server");
    assert_eq!(
        error.kind(),
        zterm_core::DomainErrorKind::PairOutcomeUnknown
    );
}

#[tokio::test]
async fn accept_authoritative_error_is_preserved_without_retry() {
    let (_directory, socket) = socket_path();
    let listener = bind(&socket);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("pair request");
        let (_, mut frame) = read_request(&mut stream).await;
        frame.payload.zeroize();
        let response = encode_message(
            WireKind::ServiceErrorResponse,
            frame.request_id,
            0,
            &v2::ServiceError {
                code: "pair_ticket_expired".into(),
                message: "expired fixture".into(),
            },
        )
        .expect("typed error response");
        stream.write_all(&response).await.expect("write rejection");
        stream.shutdown().await.expect("finish rejection");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "a definitive rejection must not be retried"
        );
    });
    let error = LocalPairingClient::new(&socket)
        .accept(mint_ticket(), None)
        .await
        .expect_err("authoritative rejection");
    assert_eq!(error.kind(), zterm_core::DomainErrorKind::PairTicketExpired);
    server.await.expect("pair server");
}

#[tokio::test]
async fn create_uses_default_fingerprint_and_pair_deadline() {
    let (_directory, socket) = socket_path();
    let listener = bind(&socket);
    let ticket = mint_ticket();
    let response_ticket = ticket.expose().to_owned();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept create request");
        let (_bytes, frame) = read_request(&mut stream).await;
        assert_eq!(frame.kind, WireKind::LocalPairCreateRequest);
        assert_eq!(frame.deadline_ms, 15_000);
        let request: v2::LocalPairCreateRequest = frame
            .decode_message(WireKind::LocalPairCreateRequest)
            .expect("decode create request");
        assert_eq!(
            request.ephemeral_operation_id.len(),
            EphemeralOperationId::LENGTH
        );
        assert_eq!(request.ttl_seconds, 0);
        assert_eq!(
            request.fingerprint,
            PairFingerprint::for_create(DEFAULT_PAIR_TTL_SECONDS)
                .as_bytes()
                .as_slice()
        );
        let mut response = v2::LocalPairCreateResponse {
            ticket: response_ticket,
        };
        let bytes = Zeroizing::new(
            encode_message(
                WireKind::LocalPairCreateResponse,
                frame.request_id,
                0,
                &response,
            )
            .expect("encode create response"),
        );
        response.ticket.zeroize();
        stream
            .write_all(&bytes)
            .await
            .expect("write create response");
        stream.shutdown().await.expect("finish create response");
    });

    let created = LocalPairingClient::new(&socket)
        .create(0)
        .await
        .expect("pair create response");
    assert_eq!(created.expose(), ticket.expose());
    server.await.expect("create server task");
}

#[tokio::test]
async fn accept_retries_byte_identical_and_decodes_directional_device() {
    let (_directory, socket) = socket_path();
    let listener = bind(&socket);
    let ticket = mint_ticket();
    let expected_ticket = Zeroizing::new(ticket.expose().to_owned());
    let alias = DeviceAlias::new("paired-host").expect("valid alias");
    let expected_alias = alias.clone();
    let expected_device = DeviceSummary::new(
        HOST,
        true,
        Some(alias.clone()),
        "pair-host",
        true,
        AuthorizationStatus::None,
        AuthGeneration::ZERO,
        0,
        0,
        false,
        0,
        0,
    )
    .expect("valid outbound-only device");
    let response_device = expected_device.clone();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.expect("accept first attempt");
        let (first_bytes, mut first_frame) = read_request(&mut first).await;
        assert_eq!(first_frame.kind, WireKind::LocalPairAcceptRequest);
        assert_eq!(first_frame.deadline_ms, 15_000);
        first_frame.payload.zeroize();
        drop(first);

        let (mut second, _) = listener.accept().await.expect("accept retry");
        let (second_bytes, mut second_frame) = read_request(&mut second).await;
        assert_eq!(&*first_bytes, &*second_bytes);
        let mut request: v2::LocalPairAcceptRequest = second_frame
            .decode_message(WireKind::LocalPairAcceptRequest)
            .expect("decode accept request");
        assert_eq!(
            request.ephemeral_operation_id.len(),
            EphemeralOperationId::LENGTH
        );
        assert_eq!(request.alias, expected_alias.as_str());
        assert_eq!(request.ticket, expected_ticket.as_str());
        assert_eq!(
            request.fingerprint,
            PairFingerprint::for_accept(expected_ticket.as_bytes(), Some(&expected_alias))
                .as_bytes()
                .as_slice()
        );
        request.ticket.zeroize();
        let request_id = second_frame.request_id;
        second_frame.payload.zeroize();

        let response = v2::LocalPairAcceptResponse {
            device: Some((&response_device).into()),
        };
        let bytes = Zeroizing::new(
            encode_message(WireKind::LocalPairAcceptResponse, request_id, 0, &response)
                .expect("encode accept response"),
        );
        second
            .write_all(&bytes)
            .await
            .expect("write accept response");
        second.shutdown().await.expect("finish accept response");
    });

    let accepted = LocalPairingClient::new(&socket)
        .accept(ticket, Some(&alias))
        .await
        .expect("pair accept response");
    assert_eq!(accepted, expected_device);
    server.await.expect("accept server task");
}

fn mint_ticket() -> PairTicketText {
    let manager = PairingManager::new(HOST, TransportLimits::default()).expect("pair manager");
    let fingerprint = PairFingerprint::for_create(DEFAULT_PAIR_TTL_SECONDS);
    let request = PairOfferRequest::new(
        EphemeralOperationId::from_array([0x41; 16]),
        fingerprint,
        DeviceDisplayName::new("pair-host").expect("host name"),
        vec![RelayHint::new("https://relay.example.test").expect("relay hint")],
        DEFAULT_PAIR_TTL_SECONDS,
    )
    .expect("offer request");
    manager
        .create_offer(request)
        .expect("offer created")
        .ticket()
        .clone()
}

fn socket_path() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("temporary socket directory");
    let socket = directory.path().join("pair.sock");
    (directory, socket)
}

fn bind(socket: &PathBuf) -> tokio::net::UnixListener {
    let listener = std::os::unix::net::UnixListener::bind(socket).expect("bind Unix socket");
    listener
        .set_nonblocking(true)
        .expect("configure nonblocking listener");
    tokio::net::UnixListener::from_std(listener).expect("adopt Unix listener")
}

async fn read_request(stream: &mut tokio::net::UnixStream) -> (Zeroizing<Vec<u8>>, DecodedFrame) {
    let mut bytes = Zeroizing::new(Vec::new());
    stream
        .read_to_end(&mut bytes)
        .await
        .expect("read complete unary request");
    let mut decoder = FrameDecoder::new();
    let mut frames = decoder.feed(&bytes).expect("decode unary request");
    decoder.finish().expect("request has strict EOF");
    assert_eq!(frames.len(), 1);
    (bytes, frames.pop().expect("one request frame"))
}
