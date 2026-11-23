//! Versioned protobuf DTOs and the one bounded voytty frame codec.

mod compression;
mod connection_progress;
mod frame;
mod pairing;
mod redacted_debug;
mod terminal;
mod terminal_links;
pub mod upload;

#[cfg(test)]
mod tests;

pub use compression::compress_terminal_frames;
pub use connection_progress::{connection_stage_from_message, connection_stage_to_message};
pub(crate) use frame::encode_varint;
pub use frame::{
    DecodedFrame, FrameDecoder, LOCAL_SESSION_TUNNEL_VERSION, MAX_CONTROL_PAYLOAD_BYTES,
    MAX_FRAME_BYTES, MAX_LOCAL_SESSION_TUNNEL_DATA_BYTES, MAX_VARINT_BYTES, ProtocolError,
    STATE_SCHEMA_VERSION, WIRE_MAJOR, WireKind, encode_message, encode_payload,
};
pub use pairing::{
    MAX_PAIR_HANDSHAKE_BYTES, MAX_PAIR_HELLO_FRAME_BYTES, MAX_RELAY_ROUTE_CACHE_BYTES,
    PAIR_TICKET_PREFIX, PairOperationError, RELAY_ROUTE_CACHE_VERSION, RouteCacheError,
    TicketTextError, WireFieldError, decode_pair_ticket, decode_relay_route_cache,
    encode_pair_ticket, encode_relay_route_cache, validate_pair_operation,
};
pub use terminal::{
    terminal_clipboard_write_from_message, terminal_clipboard_write_message,
    terminal_notification_from_message, terminal_notification_message,
    terminal_surface_delta_from_message, terminal_surface_delta_message,
    terminal_surface_history_window_frame_message, terminal_surface_history_window_from_message,
    terminal_surface_snapshot_from_message, terminal_surface_snapshot_message,
};

/// Generated version-two protocol DTOs.
pub mod v2 {
    #![allow(missing_docs)]
    include!(concat!(env!("OUT_DIR"), "/voytty.v2.rs"));
}
