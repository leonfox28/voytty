use std::fmt;

use prost::Message;
use voytty_core::terminal::{
    ActiveScreen, TerminalCell, TerminalClipboardWrite, TerminalColor, TerminalColorProfile,
    TerminalColorSnapshot, TerminalCursor, TerminalHistoryWindowAnchor, TerminalHistoryWindowQuery,
    TerminalKeyboardFlags, TerminalModes, TerminalNotification, TerminalScrollMetrics,
    TerminalSize, TerminalStyle, TerminalSurface, TerminalSurfaceDelta, TerminalSurfaceError,
    TerminalSurfaceHistoryWindowFrame, TerminalSurfaceHistoryWindowResult, TerminalSurfaceRow,
    TerminalSurfaceRowPatch, TerminalSurfaceSnapshot, TerminalUnderline,
    TerminalViewportDisposition,
};
use voytty_core::{AttachmentId, DeviceId, ResumeViewId, SessionId};

use super::*;
use crate::frame::{INITIAL_FRAME_BODY_CAPACITY, encode_varint};
use crate::terminal::required_colors;
use voytty_core::terminal::MAX_TERMINAL_CLIPBOARD_BYTES;

fn assert_message_round_trip<M>(kind: WireKind, message: M)
where
    M: Message + Default + PartialEq + fmt::Debug,
{
    let bytes = encode_message(kind, 41, 9_000, &message).expect("bounded future frame");
    let mut decoder = FrameDecoder::new();
    let frames = decoder.feed(&bytes).expect("future frame decodes");
    decoder.finish().expect("future frame is complete");
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].kind, kind);
    assert_eq!(frames[0].request_id, 41);
    assert_eq!(frames[0].deadline_ms, 9_000);
    let decoded: M = frames[0]
        .decode_message(kind)
        .expect("future message payload decodes");
    assert_eq!(decoded, message);
}

#[test]
fn frame_round_trip_and_unknown_fields_are_compatible() {
    let message = v2::LocalStatusRequest {};
    let mut bytes = encode_message(WireKind::LocalStatusRequest, 17, 5_000, &message)
        .expect("bounded status frame");
    assert_eq!(
        bytes,
        [0x09, 0x08, 0x02, 0x10, 0x03, 0x20, 0x11, 0x28, 0x88, 0x27],
        "language-neutral v2 empty-status golden frame"
    );
    let body_length = bytes[0] as usize;
    assert!(body_length < 0x80, "fixture keeps one-byte prefix");
    bytes[0] = u8::try_from(body_length + 3).expect("small fixture body");
    bytes.extend_from_slice(&[0xf8, 0x07, 0x01]);

    let mut decoder = FrameDecoder::new();
    let frames = decoder
        .feed(&bytes)
        .expect("unknown protobuf field ignored");
    decoder.finish().expect("complete frame boundary");
    assert_eq!(frames.len(), 1);
    let decoded: v2::LocalStatusRequest = frames[0]
        .decode_message(WireKind::LocalStatusRequest)
        .expect("typed payload");
    assert_eq!(decoded, message);
    assert_eq!(frames[0].request_id, 17);
}

#[test]
fn local_session_tunnel_envelope_is_bounded_and_redacted() {
    const SECRET: &[u8] = b"TUNNEL_PAYLOAD_SECRET_22e8";
    let target = DeviceId::from_array([0x7a; DeviceId::LENGTH]);
    let open = v2::LocalSessionTunnelOpenRequest {
        report_progress: false,
        protocol_version: LOCAL_SESSION_TUNNEL_VERSION,
        target_device_id: Some(target.into()),
    };
    assert_message_round_trip(WireKind::LocalSessionTunnelOpenRequest, open.clone());
    let open_debug = format!("{open:?}");
    assert!(!open_debug.contains(&target.to_string()));

    let maximum = v2::LocalSessionTunnelData {
        bytes: vec![0x5a; MAX_LOCAL_SESSION_TUNNEL_DATA_BYTES],
    };
    assert_message_round_trip(WireKind::LocalSessionTunnelData, maximum);
    let oversized = v2::LocalSessionTunnelData {
        bytes: vec![0x5a; MAX_LOCAL_SESSION_TUNNEL_DATA_BYTES + 1],
    };
    assert!(matches!(
        encode_message(WireKind::LocalSessionTunnelData, 0, 0, &oversized),
        Err(ProtocolError::ControlPayloadTooLarge(_))
    ));

    let secret = v2::LocalSessionTunnelData {
        bytes: SECRET.to_vec(),
    };
    let debug = format!("{secret:?}");
    assert!(debug.contains(&format!("bytes_len: {}", SECRET.len())));
    assert!(!debug.contains(std::str::from_utf8(SECRET).expect("ASCII secret")));
}

#[test]
fn decoder_rejects_unknown_major_kind_and_incomplete_or_malformed_lengths() {
    let unknown_major = v2::WireFrame {
        payload_encoding: 0,
        wire_major: 1,
        kind: WireKind::LocalStatusRequest as u32,
        payload: Vec::new(),
        request_id: 1,
        deadline_ms: 0,
    };
    let mut bytes = Vec::new();
    let body = unknown_major.encode_to_vec();
    encode_varint(body.len() as u64, &mut bytes);
    bytes.extend_from_slice(&body);
    assert!(matches!(
        FrameDecoder::new().feed(&bytes),
        Err(ProtocolError::WireMajorMismatch { actual: 1, .. })
    ));

    let unknown_kind = v2::WireFrame {
        payload_encoding: 0,
        wire_major: WIRE_MAJOR,
        kind: 65_535,
        payload: Vec::new(),
        request_id: 1,
        deadline_ms: 0,
    };
    let body = unknown_kind.encode_to_vec();
    let mut bytes = Vec::new();
    encode_varint(body.len() as u64, &mut bytes);
    bytes.extend_from_slice(&body);
    assert!(matches!(
        FrameDecoder::new().feed(&bytes),
        Err(ProtocolError::UnknownKind(65_535))
    ));

    let mut truncated = FrameDecoder::new();
    assert!(truncated.feed(&[5, 1, 2]).is_ok());
    assert!(matches!(
        truncated.finish(),
        Err(ProtocolError::TruncatedFrame)
    ));
    assert!(matches!(
        FrameDecoder::new().feed(&[0x80; MAX_VARINT_BYTES]),
        Err(ProtocolError::MalformedVarint)
    ));
    assert!(matches!(
        FrameDecoder::new().feed(&[0x80, 0x00]),
        Err(ProtocolError::MalformedVarint)
    ));
    assert!(matches!(
        FrameDecoder::new().feed(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02]),
        Err(ProtocolError::MalformedVarint)
    ));
    assert!(matches!(
        FrameDecoder::new().feed(&[0xff, 0xff, 0xff, 0xff, 0x7f]),
        Err(ProtocolError::FrameTooLarge(_))
    ));
}

#[test]
fn decoder_debug_redacts_an_incomplete_body() {
    const SENTINEL: &[u8] = b"PAIR-DECODER-SECRET-SENTINEL";
    let mut bytes = vec![64];
    bytes.extend_from_slice(SENTINEL);
    let mut decoder = FrameDecoder::new();
    decoder
        .feed(&bytes)
        .expect("incomplete bounded body remains buffered");

    let debug = format!("{decoder:?}");
    assert!(!debug.contains("PAIR-DECODER-SECRET-SENTINEL"));
    assert!(!debug.contains(&format!("{SENTINEL:?}")));
    assert!(debug.contains("buffered_body_bytes"));
}

#[test]
fn local_session_forward_envelope_round_trips_and_redacts_inner_bytes() {
    const SENTINEL: &[u8] = b"REMOTE-SESSION-INNER-FRAME-SENTINEL";
    let message = v2::LocalSessionUnaryRequest {
        report_progress: false,
        target_device_id: Some(DeviceId::from_array([7; 32]).into()),
        frame: SENTINEL.to_vec(),
    };
    let debug = format!("{message:?}");
    assert!(!debug.contains("REMOTE-SESSION-INNER-FRAME-SENTINEL"));
    assert!(debug.contains("frame_len"));
    assert_message_round_trip(WireKind::LocalSessionUnaryRequest, message);

    let bounded_tunnel = v2::LocalSessionUnaryRequest {
        report_progress: false,
        target_device_id: Some(DeviceId::from_array([8; 32]).into()),
        frame: vec![0; MAX_CONTROL_PAYLOAD_BYTES],
    };
    assert!(matches!(
        encode_message(WireKind::LocalSessionUnaryRequest, 2, 0, &bounded_tunnel,),
        Err(ProtocolError::ControlPayloadTooLarge(_))
    ));
}

#[test]
fn local_connection_progress_has_a_small_wire_bound() {
    for number in 1..=11 {
        let message = v2::LocalConnectionProgress { stage: number };
        let stage = connection_stage_from_message(message).expect("valid fixed progress mapping");
        assert_message_round_trip(
            WireKind::LocalConnectionProgress,
            connection_stage_to_message(stage).expect("valid fixed progress mapping"),
        );
    }
    let body = v2::WireFrame {
        payload_encoding: 0,
        wire_major: WIRE_MAJOR,
        kind: WireKind::LocalConnectionProgress as u32,
        payload: vec![0; 65],
        request_id: 1,
        deadline_ms: 0,
    }
    .encode_to_vec();
    let mut bytes = Vec::new();
    encode_varint(body.len() as u64, &mut bytes);
    bytes.extend(body);
    assert!(matches!(
        FrameDecoder::new().feed(&bytes),
        Err(ProtocolError::ControlPayloadTooLarge(65))
    ));
    assert!(matches!(
        encode_payload(WireKind::LocalConnectionProgress, 1, 0, vec![0; 65]),
        Err(ProtocolError::ControlPayloadTooLarge(65))
    ));
}

#[test]
fn encoder_and_decoder_enforce_control_and_total_frame_limits() {
    let exact_control = encode_payload(
        WireKind::LocalStatusResponse,
        1,
        0,
        vec![0; MAX_CONTROL_PAYLOAD_BYTES],
    )
    .expect("exact control ceiling is accepted");
    assert_eq!(
        FrameDecoder::new()
            .feed(&exact_control)
            .expect("exact control frame decodes")
            .len(),
        1
    );
    assert!(matches!(
        encode_payload(
            WireKind::LocalStatusResponse,
            1,
            0,
            vec![0; MAX_CONTROL_PAYLOAD_BYTES + 1]
        ),
        Err(ProtocolError::ControlPayloadTooLarge(_))
    ));
    let oversized_control = v2::WireFrame {
        payload_encoding: 0,
        wire_major: WIRE_MAJOR,
        kind: WireKind::LocalStatusResponse as u32,
        payload: vec![0; MAX_CONTROL_PAYLOAD_BYTES + 1],
        request_id: 1,
        deadline_ms: 0,
    }
    .encode_to_vec();
    let mut oversized_control_frame = Vec::new();
    encode_varint(oversized_control.len() as u64, &mut oversized_control_frame);
    oversized_control_frame.extend_from_slice(&oversized_control);
    assert!(matches!(
        FrameDecoder::new().feed(&oversized_control_frame),
        Err(ProtocolError::ControlPayloadTooLarge(_))
    ));
    let terminal = encode_payload(
        WireKind::TerminalSemanticSnapshot,
        1,
        0,
        vec![0; MAX_CONTROL_PAYLOAD_BYTES + 1],
    )
    .expect("terminal data can use the frame ceiling");
    let mut decoder = FrameDecoder::new();
    assert_eq!(
        decoder
            .feed(&terminal)
            .expect("bounded terminal frame")
            .len(),
        1
    );

    let mut prefix = Vec::new();
    encode_varint((MAX_FRAME_BYTES + 1) as u64, &mut prefix);
    assert!(matches!(
        FrameDecoder::new().feed(&prefix),
        Err(ProtocolError::FrameTooLarge(_))
    ));
}

#[test]
fn fixed_width_domain_ids_validate_once_at_proto_boundary() {
    let device = DeviceId::from_array([42; 32]);
    let decoded = DeviceId::try_from(v2::DeviceId::from(device)).expect("valid device id");
    assert_eq!(decoded, device);
    assert!(matches!(
        DeviceId::try_from(v2::DeviceId { value: vec![0; 31] }),
        Err(ProtocolError::InvalidIdentifier(_))
    ));
}

#[test]
fn wire_kind_registry_matches_the_proto_source_of_truth() {
    let kinds = [
        (
            WireKind::LocalReadinessRequest,
            v2::MessageKind::LocalReadinessRequest as u32,
        ),
        (
            WireKind::LocalReadinessResponse,
            v2::MessageKind::LocalReadinessResponse as u32,
        ),
        (
            WireKind::LocalStatusRequest,
            v2::MessageKind::LocalStatusRequest as u32,
        ),
        (
            WireKind::LocalStatusResponse,
            v2::MessageKind::LocalStatusResponse as u32,
        ),
        (
            WireKind::LocalValidateSetupRequest,
            v2::MessageKind::LocalValidateSetupRequest as u32,
        ),
        (
            WireKind::LocalValidateSetupResponse,
            v2::MessageKind::LocalValidateSetupResponse as u32,
        ),
        (
            WireKind::LocalStopRequest,
            v2::MessageKind::LocalStopRequest as u32,
        ),
        (
            WireKind::LocalStopResponse,
            v2::MessageKind::LocalStopResponse as u32,
        ),
        (
            WireKind::LocalUpdatePreflightRequest,
            v2::MessageKind::LocalUpdatePreflightRequest as u32,
        ),
        (
            WireKind::LocalUpdatePreflightResponse,
            v2::MessageKind::LocalUpdatePreflightResponse as u32,
        ),
        (
            WireKind::ServiceErrorResponse,
            v2::MessageKind::ServiceErrorResponse as u32,
        ),
        (
            WireKind::LocalPairCreateRequest,
            v2::MessageKind::LocalPairCreateRequest as u32,
        ),
        (
            WireKind::LocalPairCreateResponse,
            v2::MessageKind::LocalPairCreateResponse as u32,
        ),
        (
            WireKind::LocalPairAcceptRequest,
            v2::MessageKind::LocalPairAcceptRequest as u32,
        ),
        (
            WireKind::LocalPairAcceptResponse,
            v2::MessageKind::LocalPairAcceptResponse as u32,
        ),
        (
            WireKind::LocalDeviceListRequest,
            v2::MessageKind::LocalDeviceListRequest as u32,
        ),
        (
            WireKind::LocalDeviceListResponse,
            v2::MessageKind::LocalDeviceListResponse as u32,
        ),
        (
            WireKind::LocalDeviceRenameRequest,
            v2::MessageKind::LocalDeviceRenameRequest as u32,
        ),
        (
            WireKind::LocalDeviceRenameResponse,
            v2::MessageKind::LocalDeviceRenameResponse as u32,
        ),
        (
            WireKind::LocalDeviceRevokeRequest,
            v2::MessageKind::LocalDeviceRevokeRequest as u32,
        ),
        (
            WireKind::LocalDeviceRevokeResponse,
            v2::MessageKind::LocalDeviceRevokeResponse as u32,
        ),
        (
            WireKind::LocalTargetResolveRequest,
            v2::MessageKind::LocalTargetResolveRequest as u32,
        ),
        (
            WireKind::LocalTargetResolveResponse,
            v2::MessageKind::LocalTargetResolveResponse as u32,
        ),
        (
            WireKind::LocalSessionUnaryRequest,
            v2::MessageKind::LocalSessionUnaryRequest as u32,
        ),
        (
            WireKind::LocalSessionTunnelOpenRequest,
            v2::MessageKind::LocalSessionTunnelOpenRequest as u32,
        ),
        (
            WireKind::LocalSessionTunnelOpened,
            v2::MessageKind::LocalSessionTunnelOpened as u32,
        ),
        (
            WireKind::LocalSessionTunnelData,
            v2::MessageKind::LocalSessionTunnelData as u32,
        ),
        (
            WireKind::LocalSessionTunnelPath,
            v2::MessageKind::LocalSessionTunnelPath as u32,
        ),
        (
            WireKind::LocalConnectionProgress,
            v2::MessageKind::LocalConnectionProgress as u32,
        ),
        (
            WireKind::LocalSessionTunnelHalfClose,
            v2::MessageKind::LocalSessionTunnelHalfClose as u32,
        ),
        (
            WireKind::LocalSessionTunnelClosed,
            v2::MessageKind::LocalSessionTunnelClosed as u32,
        ),
        (WireKind::PairBegin, v2::MessageKind::PairBegin as u32),
        (
            WireKind::PairChallenge,
            v2::MessageKind::PairChallenge as u32,
        ),
        (WireKind::PairProof, v2::MessageKind::PairProof as u32),
        (WireKind::PairAccepted, v2::MessageKind::PairAccepted as u32),
        (
            WireKind::ConnectionHello,
            v2::MessageKind::ConnectionHello as u32,
        ),
        (
            WireKind::ConnectionWelcome,
            v2::MessageKind::ConnectionWelcome as u32,
        ),
        (
            WireKind::SessionListRequest,
            v2::MessageKind::SessionListRequest as u32,
        ),
        (
            WireKind::SessionListResponse,
            v2::MessageKind::SessionListResponse as u32,
        ),
        (
            WireKind::SessionCreateRequest,
            v2::MessageKind::SessionCreateRequest as u32,
        ),
        (
            WireKind::SessionMutateResponse,
            v2::MessageKind::SessionMutateResponse as u32,
        ),
        (
            WireKind::SessionRenameRequest,
            v2::MessageKind::SessionRenameRequest as u32,
        ),
        (
            WireKind::SessionCloseRequest,
            v2::MessageKind::SessionCloseRequest as u32,
        ),
        (
            WireKind::SessionTakeoverRequest,
            v2::MessageKind::SessionTakeoverRequest as u32,
        ),
        (
            WireKind::SessionOperationLeaseRequest,
            v2::MessageKind::SessionOperationLeaseRequest as u32,
        ),
        (
            WireKind::SessionOperationLeaseResponse,
            v2::MessageKind::SessionOperationLeaseResponse as u32,
        ),
        (
            WireKind::TerminalAttachRequest,
            v2::MessageKind::TerminalAttachRequest as u32,
        ),
        (
            WireKind::TerminalInput,
            v2::MessageKind::TerminalInput as u32,
        ),
        (
            WireKind::TerminalResize,
            v2::MessageKind::TerminalResize as u32,
        ),
        (
            WireKind::TerminalDetach,
            v2::MessageKind::TerminalDetach as u32,
        ),
        (
            WireKind::TerminalSnapshotApplied,
            v2::MessageKind::TerminalSnapshotApplied as u32,
        ),
        (
            WireKind::TerminalSyncRequest,
            v2::MessageKind::TerminalSyncRequest as u32,
        ),
        (
            WireKind::TerminalSyncRequired,
            v2::MessageKind::TerminalSyncRequired as u32,
        ),
        (
            WireKind::TerminalLeaseLost,
            v2::MessageKind::TerminalLeaseLost as u32,
        ),
        (
            WireKind::TerminalSessionEnded,
            v2::MessageKind::TerminalSessionEnded as u32,
        ),
        (
            WireKind::TerminalTransportStateEvent,
            v2::MessageKind::TerminalTransportStateEvent as u32,
        ),
        (
            WireKind::TerminalConnectionStatusEvent,
            v2::MessageKind::TerminalConnectionStatusEvent as u32,
        ),
        (
            WireKind::TerminalHistoryWindowRequest,
            v2::MessageKind::TerminalHistoryWindowRequest as u32,
        ),
        (
            WireKind::TerminalSemanticSnapshot,
            v2::MessageKind::TerminalSemanticSnapshot as u32,
        ),
        (
            WireKind::TerminalSemanticDelta,
            v2::MessageKind::TerminalSemanticDelta as u32,
        ),
        (
            WireKind::TerminalSemanticHistoryWindowFrame,
            v2::MessageKind::TerminalSemanticHistoryWindowFrame as u32,
        ),
        (
            WireKind::TerminalNotification,
            v2::MessageKind::TerminalNotification as u32,
        ),
        (
            WireKind::TerminalSurfaceFramesReceived,
            v2::MessageKind::TerminalSurfaceFramesReceived as u32,
        ),
        (
            WireKind::TerminalClipboardWrite,
            v2::MessageKind::TerminalClipboardWrite as u32,
        ),
    ];

    for (kind, proto_number) in kinds {
        assert_eq!(kind as u32, proto_number);
        assert_eq!(
            WireKind::try_from(proto_number).expect("registered kind"),
            kind
        );
    }
}

#[test]
fn session_and_terminal_contract_shapes_round_trip() {
    let operation_id = Some(v2::OperationId {
        lease_ordinal: 7,
        sequence: 11,
        daemon_incarnation: vec![9; 16],
    });
    let target = Some(v2::TargetSelector {
        target: Some(v2::target_selector::Target::Device(v2::DeviceId {
            value: vec![7; DeviceId::LENGTH],
        })),
    });
    let session_id = Some(v2::SessionId { value: vec![3; 16] });
    let attachment_id = Some(v2::AttachmentId { value: vec![5; 16] });

    assert_message_round_trip(
        WireKind::SessionRenameRequest,
        v2::SessionRenameRequest {
            operation_id: operation_id.clone(),
            target: target.clone(),
            session_id: session_id.clone(),
            name: "build".to_owned(),
        },
    );
    assert_message_round_trip(
        WireKind::SessionCloseRequest,
        v2::SessionCloseRequest {
            operation_id: operation_id.clone(),
            target: target.clone(),
            session_id: session_id.clone(),
        },
    );
    assert_message_round_trip(
        WireKind::TerminalAttachRequest,
        v2::TerminalAttachRequest {
            accept_zlib_frames: false,
            acknowledge_surface_frames: false,
            base_colors: Default::default(),

            target: target.clone(),
            session_id: session_id.clone(),
            takeover: true,
            session_name: String::new(),
            create_main: false,
            viewport: Some(v2::TerminalViewport {
                rows: 40,
                columns: 120,
            }),
            resume_view_id: Some(v2::ResumeViewId { value: vec![6; 16] }),
            known_revision: Some(12),
        },
    );
    assert_message_round_trip(
        WireKind::SessionTakeoverRequest,
        v2::SessionTakeoverRequest {
            operation_id,
            target,
            session_id,
            attachment_id: attachment_id.clone(),
        },
    );
    assert_message_round_trip(
        WireKind::TerminalSnapshotApplied,
        v2::TerminalSnapshotApplied {
            attachment_id: attachment_id.clone(),
            revision: 13,
        },
    );
    assert_message_round_trip(
        WireKind::TerminalSurfaceFramesReceived,
        v2::TerminalSurfaceFramesReceived {
            attachment_id: attachment_id.clone(),
            frame_sequence: 21,
        },
    );
    assert!(WireKind::TerminalSurfaceFramesReceived.is_control());
    assert_message_round_trip(
        WireKind::TerminalSyncRequest,
        v2::TerminalSyncRequest {
            attachment_id: attachment_id.clone(),
            known_revision: 13,
        },
    );
    assert_message_round_trip(
        WireKind::TerminalSyncRequired,
        v2::TerminalSyncRequired {
            attachment_id: attachment_id.clone(),
            latest_revision: 17,
        },
    );
    assert_message_round_trip(
        WireKind::TerminalLeaseLost,
        v2::TerminalLeaseLost {
            attachment_id: Some(v2::AttachmentId { value: vec![5; 16] }),
            generation: 3,
        },
    );
    assert_message_round_trip(
        WireKind::TerminalSessionEnded,
        v2::TerminalSessionEnded {
            session_id: Some(v2::SessionId { value: vec![3; 16] }),
            attachment_id: Some(v2::AttachmentId { value: vec![5; 16] }),
            reason: v2::TerminalSessionEndReason::NaturalExit as i32,
            exit_code: 0,
            signal: String::new(),
        },
    );
    assert_message_round_trip(
        WireKind::TerminalTransportStateEvent,
        v2::TerminalTransportStateEvent {
            attachment_id: Some(v2::AttachmentId { value: vec![5; 16] }),
            state: v2::TerminalTransportState::Reconnecting as i32,
        },
    );
    assert_message_round_trip(
        WireKind::TerminalConnectionStatusEvent,
        v2::TerminalConnectionStatusEvent {
            attachment_id: attachment_id.clone(),
            path: v2::TerminalConnectionPath::Direct as i32,
            rtt_ms: Some(42),
        },
    );
    let window_anchor = v2::TerminalHistoryWindowAnchor {
        epoch: 3,
        revision: 17,
        max_offset_from_bottom: 9,
        viewport_rows: 2,
        viewport_columns: 80,
    };
    assert_message_round_trip(
        WireKind::TerminalHistoryWindowRequest,
        v2::TerminalHistoryWindowRequest {
            attachment_id: attachment_id.clone(),
            anchor: Some(window_anchor),
            target_offset_from_bottom: 3,
            older_margin_rows: 2,
            newer_margin_rows: 2,
        },
    );
    assert_eq!(
        ResumeViewId::try_from(v2::ResumeViewId { value: vec![6; 16] })
            .expect("fixed-width resume view ID"),
        ResumeViewId::from_array([6; 16])
    );
    assert!(matches!(
        ResumeViewId::try_from(v2::ResumeViewId { value: vec![6; 15] }),
        Err(ProtocolError::InvalidIdentifier(_))
    ));
    assert_eq!(
        TerminalSize::try_from(v2::TerminalViewport {
            rows: 40,
            columns: 120,
        })
        .expect("bounded viewport"),
        TerminalSize::new(40, 120)
    );
    assert!(matches!(
        TerminalSize::try_from(v2::TerminalViewport {
            rows: 0,
            columns: 120,
        }),
        Err(ProtocolError::InvalidTerminalSize { .. })
    ));
}

fn semantic_row(columns: u16, contents: &str) -> TerminalSurfaceRow {
    let link = std::sync::Arc::new(
        voytty_core::terminal::TerminalHyperlink::new(
            "doc",
            "https://example.invalid/semantic-link",
        )
        .expect("web link"),
    );
    TerminalSurfaceRow {
        cells: (0..columns)
            .map(|_| TerminalCell {
                hyperlink: Some(std::sync::Arc::clone(&link)),
                contents: contents.to_owned(),
                style: TerminalStyle {
                    underline_color: Default::default(),

                    foreground: TerminalColor::Rgb(1, 2, 3),
                    background: TerminalColor::Rgb(4, 5, 6),
                    bold: true,
                    dim: true,
                    italic: true,
                    underline: TerminalUnderline::Single,
                    inverse: true,
                    strike: true,
                    conceal: true,
                },
                ..TerminalCell::default()
            })
            .collect(),
        wrapped: true,
    }
}

#[test]
fn cursor_presentation_round_trips_and_rejects_missing_or_unknown_shape() {
    use voytty_core::terminal::{TerminalCursorPresentation, TerminalCursorShape};
    for shape in [
        TerminalCursorShape::Block,
        TerminalCursorShape::Beam,
        TerminalCursorShape::Underline,
    ] {
        let cursor = TerminalCursor {
            row: 0,
            column: 0,
            visible: true,
            style: Default::default(),
            presentation: TerminalCursorPresentation {
                shape,
                blinking: true,
            },
        };
        let encoded: v2::TerminalCursor = cursor.into();
        assert_eq!(TerminalCursor::try_from(encoded).expect("cursor"), cursor);
        let mut missing = encoded;
        missing.presentation = None;
        assert!(TerminalCursor::try_from(missing).is_err());
        let mut invalid = encoded;
        invalid.presentation.as_mut().expect("presentation").shape = 99;
        assert!(TerminalCursor::try_from(invalid).is_err());
    }
}

#[test]
fn semantic_surface_messages_round_trip_validate_and_redact_content() {
    const SENTINEL: &str = "SEMANTIC_57c1";
    let session_id = SessionId::from_array([3; 16]);
    let attachment_id = AttachmentId::from_array([4; 16]);
    let revision = voytty_core::Revision::new(7);
    let snapshot = TerminalSurfaceSnapshot {
        revision,
        surface: TerminalSurface {
            application_title: format!("title {SENTINEL}"),
            colors: Default::default(),

            size: TerminalSize::new(2, 3),
            active_screen: ActiveScreen::Main,
            rows: vec![semantic_row(3, SENTINEL), semantic_row(3, "x")],
            cursor: TerminalCursor {
                presentation: Default::default(),
                row: 1,
                column: 2,
                visible: true,
                style: TerminalStyle::default(),
            },
            modes: TerminalModes::default(),
            scroll_metrics: Some(TerminalScrollMetrics {
                epoch: voytty_core::Revision::new(1),
                revision,
                offset_from_bottom: 0,
                max_offset_from_bottom: 5,
                viewport_rows: 2,
            }),
        },
    };
    let message = terminal_surface_snapshot_message(session_id, attachment_id, snapshot.clone());
    let debug = format!("{message:?}");
    assert!(!debug.contains(SENTINEL));
    let (decoded_session, decoded_attachment, decoded) =
        terminal_surface_snapshot_from_message(message.clone()).expect("valid snapshot");
    assert_eq!(decoded_session, session_id);
    assert_eq!(decoded_attachment, attachment_id);
    assert_eq!(decoded, snapshot);
    assert_message_round_trip(WireKind::TerminalSemanticSnapshot, message);

    let delta = TerminalSurfaceDelta {
        application_title: format!("title {SENTINEL}"),
        colors: Default::default(),

        from_revision: revision,
        to_revision: voytty_core::Revision::new(8),
        size: snapshot.surface.size,
        active_screen: ActiveScreen::Main,
        row_patches: vec![TerminalSurfaceRowPatch {
            row: 1,
            replacement: semantic_row(3, "y"),
        }],
        cursor: snapshot.surface.cursor,
        modes: snapshot.surface.modes,
        scroll_metrics: Some(TerminalScrollMetrics {
            revision: voytty_core::Revision::new(8),
            ..snapshot.surface.scroll_metrics.expect("metrics")
        }),
    };
    let message = terminal_surface_delta_message(attachment_id, delta.clone());
    let (decoded_attachment, decoded) =
        terminal_surface_delta_from_message(message.clone()).expect("valid delta");
    assert_eq!(decoded_attachment, attachment_id);
    assert_eq!(decoded, delta);
    assert!(!format!("{message:?}").contains(SENTINEL));
    assert!(!format!("{delta:?}").contains(SENTINEL));
    for title in ["bad\x1btitle".to_owned(), "界".repeat(86)] {
        let mut invalid = message.clone();
        invalid.application_title.clone_from(&title);
        assert!(terminal_surface_delta_from_message(invalid).is_err());
        let mut invalid =
            terminal_surface_snapshot_message(session_id, attachment_id, snapshot.clone());
        invalid.surface.as_mut().expect("surface").application_title = title;
        assert!(terminal_surface_snapshot_from_message(invalid).is_err());
    }
    assert_message_round_trip(WireKind::TerminalSemanticDelta, message);

    let mut malformed = terminal_surface_snapshot_message(session_id, attachment_id, snapshot);
    malformed.surface.as_mut().expect("surface").rows[0].cells[0].contents =
        "bad\u{1b}cell".to_owned();
    assert!(matches!(
        terminal_surface_snapshot_from_message(malformed),
        Err(ProtocolError::InvalidTerminalSurface(
            TerminalSurfaceError::InvalidCellText
        ))
    ));
}

#[test]
fn notifications_round_trip_validate_and_redact_nested_content() {
    use v2::terminal_notification::Content;
    let id = AttachmentId::from_array([8; 16]);
    for value in [
        TerminalNotification::osc9("secret 结果".into()),
        TerminalNotification::osc777("secret title".into(), "body;结果".into()),
    ] {
        let value = value.expect("valid notification");
        let message = terminal_notification_message(id, value.clone());
        assert!(!format!("{message:?}").contains("secret"));
        assert!(!format!("{:?}", message.content).contains("secret"));
        assert_eq!(
            terminal_notification_from_message(message.clone()).expect("round trip"),
            (id, value)
        );
        assert_message_round_trip(WireKind::TerminalNotification, message);
    }
    for content in [
        None,
        Some(Content::Osc9("4;1;20".into())),
        Some(Content::Osc9("bad\x1bcontrol".into())),
        Some(Content::Osc9("x".repeat(1023))),
        Some(Content::Osc777(v2::TerminalNotificationOsc777 {
            title: "a;b".into(),
            body: "body".into(),
        })),
    ] {
        assert!(
            terminal_notification_from_message(v2::TerminalNotification {
                attachment_id: Some(id.into()),
                content
            })
            .is_err()
        );
    }
    let mut missing = terminal_notification_message(
        id,
        TerminalNotification::osc9("hello".into()).expect("valid"),
    );
    missing.attachment_id = None;
    assert!(terminal_notification_from_message(missing).is_err());
    assert!(WireKind::TerminalNotification.is_control());
}

#[test]
fn clipboard_write_round_trips_as_decoded_redacted_content() {
    const SENTINEL: &str = "CLIPBOARD_3f7a";
    let attachment_id = AttachmentId::from_array([7; 16]);
    let write = TerminalClipboardWrite::new(SENTINEL.to_owned()).expect("valid write");
    let message = terminal_clipboard_write_message(attachment_id, write.clone());

    let debug = format!("{message:?}");
    assert!(!debug.contains(SENTINEL));
    assert!(debug.contains("[REDACTED]"));
    let (decoded_attachment, decoded) =
        terminal_clipboard_write_from_message(message.clone()).expect("valid message");
    assert_eq!(decoded_attachment, attachment_id);
    assert_eq!(decoded, write);
    assert_message_round_trip(WireKind::TerminalClipboardWrite, message);

    let invalid = v2::TerminalClipboardWrite {
        attachment_id: Some(attachment_id.into()),
        text: "bad\0clipboard".to_owned(),
    };
    assert!(matches!(
        terminal_clipboard_write_from_message(invalid),
        Err(ProtocolError::InvalidTerminalSemanticField(
            "clipboard_text"
        ))
    ));

    for invalid in [
        v2::TerminalClipboardWrite {
            attachment_id: None,
            text: "missing owner".to_owned(),
        },
        v2::TerminalClipboardWrite {
            attachment_id: Some(attachment_id.into()),
            text: String::new(),
        },
        v2::TerminalClipboardWrite {
            attachment_id: Some(attachment_id.into()),
            text: "x".repeat(MAX_TERMINAL_CLIPBOARD_BYTES + 1),
        },
    ] {
        assert!(terminal_clipboard_write_from_message(invalid).is_err());
    }

    let maximum = terminal_clipboard_write_message(
        attachment_id,
        TerminalClipboardWrite::new("x".repeat(MAX_TERMINAL_CLIPBOARD_BYTES))
            .expect("exact maximum clipboard value"),
    );
    let encoded = encode_message(WireKind::TerminalClipboardWrite, 0, 0, &maximum)
        .expect("exact maximum clipboard frame fits the control cap");
    let decoded = FrameDecoder::new()
        .feed(&encoded)
        .expect("exact maximum clipboard frame decodes")
        .pop()
        .expect("one exact maximum clipboard frame");
    assert_eq!(decoded.kind, WireKind::TerminalClipboardWrite);
    assert!(decoded.payload.len() <= MAX_CONTROL_PAYLOAD_BYTES);
    let maximum: v2::TerminalClipboardWrite = decoded
        .decode_message(WireKind::TerminalClipboardWrite)
        .expect("exact maximum clipboard message decodes");
    assert_eq!(maximum.text.len(), MAX_TERMINAL_CLIPBOARD_BYTES);
}

#[test]
fn terminal_keyboard_flags_round_trip_and_reject_unknown_bits() {
    let flags = TerminalKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES
        .union(TerminalKeyboardFlags::REPORT_EVENT_TYPES)
        .union(TerminalKeyboardFlags::REPORT_ASSOCIATED_TEXT);
    let modes = TerminalModes {
        keyboard_flags: flags,
        ..TerminalModes::default()
    };
    assert_eq!(
        TerminalModes::try_from(v2::TerminalModes::from(modes)).expect("valid flags"),
        modes
    );

    let malformed = v2::TerminalModes {
        keyboard_flags: 32,
        ..v2::TerminalModes::default()
    };
    assert!(matches!(
        TerminalModes::try_from(malformed),
        Err(ProtocolError::InvalidTerminalSemanticField(
            "keyboard_flags"
        ))
    ));
}

#[test]
fn maximum_semantic_frames_fit_the_existing_eight_mib_bound() {
    let attachment_id = AttachmentId::from_array([9; 16]);
    let session_id = SessionId::from_array([8; 16]);
    let revision = voytty_core::Revision::new(11);
    let size = TerminalSize::new(80, 240);
    let row = semantic_row(size.columns, "abcdefghijklmnopqrstuv");
    let surface = TerminalSurface {
        application_title: String::new(),
        colors: Default::default(),

        size,
        active_screen: ActiveScreen::Main,
        rows: vec![row.clone(); usize::from(size.rows)],
        cursor: TerminalCursor {
            presentation: Default::default(),
            row: 79,
            column: 239,
            visible: true,
            style: TerminalStyle::default(),
        },
        modes: TerminalModes::default(),
        scroll_metrics: Some(TerminalScrollMetrics {
            epoch: voytty_core::Revision::new(1),
            revision,
            offset_from_bottom: 0,
            max_offset_from_bottom: 240,
            viewport_rows: 80,
        }),
    };
    let snapshot = terminal_surface_snapshot_message(
        session_id,
        attachment_id,
        TerminalSurfaceSnapshot { revision, surface },
    );
    let encoded = encode_message(WireKind::TerminalSemanticSnapshot, 1, 0, &snapshot)
        .expect("maximum legal semantic snapshot fits");
    assert!(encoded.len() <= MAX_FRAME_BYTES + MAX_VARINT_BYTES);

    let query = TerminalHistoryWindowQuery {
        anchor: TerminalHistoryWindowAnchor {
            epoch: voytty_core::Revision::new(1),
            revision,
            max_offset_from_bottom: 240,
            viewport: size,
        },
        target_offset_from_bottom: 160,
        older_margin_rows: 80,
        newer_margin_rows: 80,
    };
    let frame = TerminalSurfaceHistoryWindowFrame {
        colors: Default::default(),

        disposition: TerminalViewportDisposition::Exact,
        anchor: query.anchor,
        target_offset_from_bottom: 160,
        first_row_from_live_top: -240,
        rows: vec![row; voytty_core::terminal::MAX_HISTORY_WINDOW_ROWS],
    };
    frame.validate_for(query).expect("maximum window shape");
    let message = terminal_surface_history_window_frame_message(
        attachment_id,
        TerminalSurfaceHistoryWindowResult::Frame(frame),
    );
    let encoded = encode_message(WireKind::TerminalSemanticHistoryWindowFrame, 2, 0, &message)
        .expect("maximum legal semantic history window fits");
    assert!(encoded.len() <= MAX_FRAME_BYTES + MAX_VARINT_BYTES);
}

#[test]
fn semantic_history_frames_keep_content_and_control_limits_and_reject_malformed_payloads() {
    assert!(WireKind::TerminalHistoryWindowRequest.is_control());
    assert!(!WireKind::TerminalSemanticSnapshot.is_control());
    assert!(!WireKind::TerminalSemanticDelta.is_control());
    assert!(!WireKind::TerminalSemanticHistoryWindowFrame.is_control());
    assert!(WireKind::TerminalConnectionStatusEvent.is_control());
    assert!(WireKind::TerminalClipboardWrite.is_control());

    let content = vec![0_u8; MAX_CONTROL_PAYLOAD_BYTES + 1];
    let encoded = encode_payload(
        WireKind::TerminalSemanticHistoryWindowFrame,
        7,
        0,
        content.clone(),
    )
    .expect("semantic history content may use the ordinary frame ceiling");
    assert_eq!(
        FrameDecoder::new()
            .feed(&encoded)
            .expect("bounded semantic history content frame")
            .len(),
        1
    );
    assert!(matches!(
        encode_payload(WireKind::TerminalHistoryWindowRequest, 7, 0, content),
        Err(ProtocolError::ControlPayloadTooLarge(_))
    ));

    let malformed = encode_payload(
        WireKind::TerminalSemanticHistoryWindowFrame,
        8,
        0,
        vec![0x80],
    )
    .expect("malformed concrete protobuf still has a valid outer frame");
    let frame = FrameDecoder::new()
        .feed(&malformed)
        .expect("outer frame decodes")
        .pop()
        .expect("one malformed concrete payload");
    assert!(matches!(
        frame.decode_message::<v2::TerminalSemanticHistoryWindowFrame>(
            WireKind::TerminalSemanticHistoryWindowFrame
        ),
        Err(ProtocolError::MalformedProtobuf(_))
    ));

    const SENTINEL: &str = "SECRET742f";
    let query = TerminalHistoryWindowQuery {
        anchor: TerminalHistoryWindowAnchor {
            epoch: voytty_core::Revision::new(1),
            revision: voytty_core::Revision::new(2),
            max_offset_from_bottom: 1,
            viewport: TerminalSize::new(1, 8),
        },
        target_offset_from_bottom: 1,
        older_margin_rows: 0,
        newer_margin_rows: 0,
    };
    let message = terminal_surface_history_window_frame_message(
        AttachmentId::from_array([0x47; 16]),
        TerminalSurfaceHistoryWindowResult::Frame(TerminalSurfaceHistoryWindowFrame {
            colors: Default::default(),

            disposition: TerminalViewportDisposition::Exact,
            anchor: query.anchor,
            target_offset_from_bottom: 1,
            first_row_from_live_top: -1,
            rows: vec![semantic_row(8, SENTINEL)],
        }),
    );
    let debug = format!("{message:?}");
    assert!(!debug.contains(SENTINEL));
    let (_, decoded) =
        terminal_surface_history_window_from_message(message, query).expect("valid window");
    assert!(matches!(
        decoded,
        TerminalSurfaceHistoryWindowResult::Frame(_)
    ));

    for (current_epoch, current_revision) in [(3, 2), (0, 0)] {
        let malformed = v2::TerminalSemanticHistoryWindowFrame {
            hyperlinks: Vec::new(),
            colors: None,
            attachment_id: Some(AttachmentId::from_array([0x48; 16]).into()),
            outcome: v2::TerminalHistoryWindowOutcome::Gap as i32,
            disposition: v2::TerminalViewportDisposition::Unspecified as i32,
            anchor: None,
            target_offset_from_bottom: 0,
            first_row_from_live_top: 0,
            rows: Vec::new(),
            current_epoch,
            current_revision,
        };
        assert!(matches!(
            terminal_surface_history_window_from_message(malformed, query),
            Err(ProtocolError::InvalidTerminalSurface(
                TerminalSurfaceError::InvalidHistoryWindow
            ))
        ));
    }
}
#[test]
fn color_profiles_and_snapshot_metadata_are_required_and_bounded() {
    let profile = TerminalColorProfile::default();
    let wire: v2::TerminalColorProfile = profile.clone().into();
    assert_eq!(
        TerminalColorProfile::try_from(wire.clone()).expect("profile round trip"),
        profile
    );
    let mut short = wire.clone();
    short.values.pop();
    assert!(TerminalColorProfile::try_from(short).is_err());
    let mut long = wire.clone();
    long.values.push(wire.values[0]);
    assert!(TerminalColorProfile::try_from(long).is_err());
    let mut invalid = wire.clone();
    invalid.appearance = 3;
    assert!(TerminalColorProfile::try_from(invalid).is_err());
    let mut dynamic = wire.clone();
    dynamic.values[0].value = Some(v2::terminal_color_observation::Value::Dynamic(true));
    assert!(TerminalColorProfile::try_from(dynamic).is_err());
    let mut invalid = wire;
    invalid.values[0].value = None;
    assert!(TerminalColorProfile::try_from(invalid).is_err());
    assert!(required_colors(None).is_err());
    let mut snapshot: v2::TerminalColorSnapshot = TerminalColorSnapshot::default().into();
    snapshot.inherited_sources.pop();
    assert!(required_colors(Some(snapshot)).is_err());
}
#[test]
fn color_capable_create_attach_kinds_retire_the_old_side_effecting_requests() {
    assert_eq!(WireKind::SessionCreateRequest as u32, 209);
    assert_eq!(WireKind::TerminalAttachRequest as u32, 323);
    assert_eq!(WireKind::TerminalBaseColors as u32, 324);
    for kind in [202, 300] {
        assert!(WireKind::try_from(kind).is_err());
        assert!(v2::MessageKind::try_from(kind as i32).is_err());
    }
}

#[test]
fn frame_decoder_caps_initial_body_allocation_before_bytes_arrive() {
    let mut decoder = FrameDecoder::new();
    let mut prefix = Vec::new();
    encode_varint(MAX_FRAME_BYTES as u64, &mut prefix);
    assert!(decoder.feed(&prefix).expect("valid prefix").is_empty());
    assert_eq!(decoder.expected_body, Some(MAX_FRAME_BYTES));
    assert!(decoder.body.capacity() <= INITIAL_FRAME_BODY_CAPACITY);
}
