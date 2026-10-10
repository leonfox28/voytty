//! Opt-in, per-frame compression of terminal snapshots, deltas and history.
//! Commands, handshakes and file data never use this encoding.

use std::io::Write;

use flate2::{Compression, Decompress, FlushDecompress, Status, write::ZlibEncoder};
use prost::Message;
use zeroize::{Zeroize, Zeroizing};

use crate::{FrameDecoder, MAX_FRAME_BYTES, ProtocolError, WireKind, encode_varint, v2};

const ZLIB: u32 = 1;
const MIN_COMPRESS_BYTES: usize = 1024;

fn is_terminal_surface(kind: WireKind) -> bool {
    matches!(
        kind,
        WireKind::TerminalSemanticSnapshot
            | WireKind::TerminalSemanticDelta
            | WireKind::TerminalSemanticHistoryWindowFrame
    )
}

/// Compresses complete terminal surface frames for an attachment that explicitly
/// requested this encoding. Preserves frame order, correlation and all other kinds.
/// Legacy attachments must continue to use their original uncompressed bytes.
pub fn compress_terminal_frames(input: Vec<u8>) -> Result<Vec<u8>, ProtocolError> {
    let input = Zeroizing::new(input);
    let mut decoder = FrameDecoder::new();
    let frames = decoder.feed(&input)?;
    decoder.finish()?;
    let mut output = Zeroizing::new(Vec::with_capacity(input.len()));
    for frame in frames {
        let mut payload = Zeroizing::new(frame.payload);
        let mut encoding = 0;
        if is_terminal_surface(frame.kind) && payload.len() >= MIN_COMPRESS_BYTES {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
            encoder
                .write_all(&payload)
                .map_err(|_| ProtocolError::InvalidPayloadEncoding)?;
            let mut compressed = Zeroizing::new(
                encoder
                    .finish()
                    .map_err(|_| ProtocolError::InvalidPayloadEncoding)?,
            );
            if compressed.len().saturating_add(64) < payload.len() {
                payload.zeroize();
                *payload = std::mem::take(&mut *compressed);
                encoding = ZLIB;
            }
        }
        let mut wire = v2::WireFrame {
            wire_major: crate::WIRE_MAJOR,
            kind: frame.kind as u32,
            request_id: frame.request_id,
            deadline_ms: frame.deadline_ms,
            payload: std::mem::take(&mut *payload),
            payload_encoding: encoding,
        };
        let body = Zeroizing::new(wire.encode_to_vec());
        wire.payload.zeroize();
        if body.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge(body.len()));
        }
        encode_varint(body.len() as u64, &mut output);
        output.extend_from_slice(&body);
    }
    Ok(std::mem::take(&mut *output))
}

pub(super) fn decode_payload(
    wire: &mut v2::WireFrame,
    kind: WireKind,
    maximum_body_bytes: usize,
) -> Result<(), ProtocolError> {
    if wire.payload_encoding == 0 {
        return Ok(());
    }
    if wire.payload_encoding != ZLIB || !is_terminal_surface(kind) {
        return Err(ProtocolError::InvalidPayloadEncoding);
    }
    let mut decoder = Decompress::new(true);
    let mut output = Zeroizing::new(Vec::new());
    let mut chunk = Zeroizing::new([0u8; 4096]);
    let mut consumed = 0;
    loop {
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let status = decoder
            .decompress(
                &wire.payload[consumed..],
                &mut *chunk,
                FlushDecompress::None,
            )
            .map_err(|_| ProtocolError::InvalidPayloadEncoding)?;
        let read = (decoder.total_in() - before_in) as usize;
        let written = (decoder.total_out() - before_out) as usize;
        consumed += read;
        if output.len().saturating_add(written) > maximum_body_bytes {
            return Err(ProtocolError::FrameTooLarge(
                output.len().saturating_add(written),
            ));
        }
        output.extend_from_slice(&chunk[..written]);
        if status == Status::StreamEnd {
            if consumed != wire.payload.len() {
                return Err(ProtocolError::InvalidPayloadEncoding);
            }
            break;
        }
        if read == 0 && written == 0 {
            return Err(ProtocolError::InvalidPayloadEncoding);
        }
    }
    wire.payload.zeroize();
    wire.payload = std::mem::take(&mut *output);
    wire.payload_encoding = 0;
    // The expanded frame, including its header, obeys the same original bound.
    let length = wire.encoded_len();
    if length > maximum_body_bytes {
        wire.payload.zeroize();
        return Err(ProtocolError::FrameTooLarge(length));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compressed_wire(payload: &[u8], kind: WireKind) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(payload).expect("fixture compression");
        v2::WireFrame {
            wire_major: crate::WIRE_MAJOR,
            kind: kind as u32,
            payload: encoder.finish().expect("fixture stream end"),
            payload_encoding: ZLIB,
            request_id: 17,
            deadline_ms: 5000,
        }
        .encode_length_delimited_to_vec()
    }

    #[test]
    fn compressed_surface_frames_preserve_payload_metadata_order_and_plain_controls() {
        let mut plain = Vec::new();
        for kind in [
            WireKind::TerminalSyncRequired,
            WireKind::TerminalSemanticSnapshot,
            WireKind::TerminalSemanticDelta,
            WireKind::TerminalSemanticHistoryWindowFrame,
        ] {
            plain
                .extend(crate::encode_payload(kind, 17, 5000, vec![7; 8192]).expect("plain frame"));
        }
        let compressed = compress_terminal_frames(plain.clone()).expect("compress surface batch");
        assert!(compressed.len() < plain.len() / 2);
        let decoded = FrameDecoder::new()
            .feed(&compressed)
            .expect("compressed batch");
        assert_eq!(
            decoded,
            FrameDecoder::new().feed(&plain).expect("plain batch")
        );
        let control =
            crate::encode_payload(WireKind::TerminalSyncRequired, 17, 5000, vec![7; 8192])
                .expect("control");
        assert_eq!(
            compress_terminal_frames(control.clone()).expect("control bypass"),
            control
        );
        let small = crate::encode_payload(WireKind::TerminalSemanticDelta, 18, 0, vec![1; 10])
            .expect("small");
        assert_eq!(
            compress_terminal_frames(small.clone()).expect("small bypass"),
            small
        );
    }

    #[test]
    fn compressed_frames_reject_truncation_trailing_data_and_unsupported_encoding() {
        let valid = compressed_wire(&vec![0; 4096], WireKind::TerminalSemanticSnapshot);
        let wire = v2::WireFrame::decode_length_delimited(valid.as_slice()).expect("fixture wire");
        for variant in 0..4 {
            let mut broken = wire.clone();
            match variant {
                0 => {
                    broken.payload.pop();
                }
                1 => broken.payload.push(0),
                2 => broken.payload_encoding = 99,
                _ => broken.kind = WireKind::ConnectionHello as u32,
            }
            assert!(matches!(
                FrameDecoder::new().feed(&broken.encode_length_delimited_to_vec()),
                Err(ProtocolError::InvalidPayloadEncoding)
            ));
        }
    }

    #[test]
    fn decompression_preserves_custom_and_global_frame_limits() {
        let bytes = compressed_wire(&vec![0; 2048], WireKind::TerminalSemanticSnapshot);
        assert!(matches!(
            FrameDecoder::with_maximum_body_bytes(1024).feed(&bytes),
            Err(ProtocolError::FrameTooLarge(_))
        ));
        let bytes = compressed_wire(
            &vec![0; MAX_FRAME_BYTES],
            WireKind::TerminalSemanticSnapshot,
        );
        assert!(
            matches!(
                FrameDecoder::new().feed(&bytes),
                Err(ProtocolError::FrameTooLarge(_))
            ),
            "header plus expanded payload exceeds the original frame limit"
        );
        let bytes = compressed_wire(
            &vec![0; MAX_FRAME_BYTES + 4096],
            WireKind::TerminalSemanticSnapshot,
        );
        assert!(matches!(
            FrameDecoder::new().feed(&bytes),
            Err(ProtocolError::FrameTooLarge(_))
        ));
    }
}
