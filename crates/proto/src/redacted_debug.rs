//! Content-redacted `Debug` implementations for generated protobuf types.

use std::fmt;

use crate::v2;

impl fmt::Debug for v2::WireFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WireFrame")
            .field("wire_major", &self.wire_major)
            .field("kind", &self.kind)
            .field("payload", &"[REDACTED]")
            .field("payload_len", &self.payload.len())
            .field("payload_encoding", &self.payload_encoding)
            .field("request_id", &self.request_id)
            .field("deadline_ms", &self.deadline_ms)
            .finish()
    }
}

impl fmt::Debug for v2::TerminalCell {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalCell")
            .field("contents", &"[REDACTED]")
            .field("contents_len", &self.contents.len())
            .field("wide", &self.wide)
            .field("wide_continuation", &self.wide_continuation)
            .field("style", &self.style)
            .finish()
    }
}

impl fmt::Debug for v2::TerminalNotification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalNotification")
            .field("attachment_id_present", &self.attachment_id.is_some())
            .field("content", &self.content)
            .finish()
    }
}
impl fmt::Debug for v2::terminal_notification::Content {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Osc9(text) => formatter
                .debug_struct("Osc9")
                .field("bytes", &text.len())
                .finish(),
            Self::Osc777(value) => value.fmt(formatter),
        }
    }
}
impl fmt::Debug for v2::TerminalNotificationOsc777 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Osc777")
            .field("title_bytes", &self.title.len())
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

impl fmt::Debug for v2::TerminalClipboardWrite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalClipboardWrite")
            .field("attachment_id_present", &self.attachment_id.is_some())
            .field("text", &"[REDACTED]")
            .field("text_len", &self.text.len())
            .finish()
    }
}

impl fmt::Debug for v2::TerminalSurfaceRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalSurfaceRow")
            .field("cell_count", &self.cells.len())
            .field("wrapped", &self.wrapped)
            .finish()
    }
}

impl fmt::Debug for v2::TerminalSurface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalSurface")
            .field("row_count", &self.row_count)
            .field("column_count", &self.column_count)
            .field("active_screen", &self.active_screen)
            .field("encoded_rows", &self.rows.len())
            .field("cursor", &self.cursor)
            .field("modes", &self.modes)
            .field("scroll_metrics", &self.scroll_metrics)
            .finish()
    }
}

impl fmt::Debug for v2::TerminalSemanticSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalSemanticSnapshot")
            .field("session_id_present", &self.session_id.is_some())
            .field("attachment_id_present", &self.attachment_id.is_some())
            .field("revision", &self.revision)
            .field("surface", &self.surface)
            .field("frame_sequence", &self.frame_sequence)
            .finish()
    }
}

impl fmt::Debug for v2::TerminalSemanticRowPatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalSemanticRowPatch")
            .field("row", &self.row)
            .field("replacement", &self.replacement)
            .finish()
    }
}

impl fmt::Debug for v2::TerminalSemanticDelta {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalSemanticDelta")
            .field("attachment_id_present", &self.attachment_id.is_some())
            .field("from_revision", &self.from_revision)
            .field("to_revision", &self.to_revision)
            .field("row_count", &self.row_count)
            .field("column_count", &self.column_count)
            .field("active_screen", &self.active_screen)
            .field("row_patch_count", &self.row_patches.len())
            .field("cursor", &self.cursor)
            .field("modes", &self.modes)
            .field("scroll_metrics", &self.scroll_metrics)
            .field("frame_sequence", &self.frame_sequence)
            .finish()
    }
}

impl fmt::Debug for v2::TerminalSemanticHistoryWindowFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalSemanticHistoryWindowFrame")
            .field("attachment_id_present", &self.attachment_id.is_some())
            .field("outcome", &self.outcome)
            .field("disposition", &self.disposition)
            .field("anchor", &self.anchor)
            .field("target_offset_from_bottom", &self.target_offset_from_bottom)
            .field("first_row_from_live_top", &self.first_row_from_live_top)
            .field("row_count", &self.rows.len())
            .field("current_epoch", &self.current_epoch)
            .field("current_revision", &self.current_revision)
            .finish()
    }
}

impl fmt::Debug for v2::PairTicketV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairTicketV1")
            .field("format_version", &self.format_version)
            .field("host_device_id", &self.host_device_id)
            .field("host_name", &self.host_name)
            .field("relay_url_count", &self.relay_urls.len())
            .field("offer_id", &"[REDACTED]")
            .field("secret", &"[REDACTED]")
            .field("expires_at_unix", &self.expires_at_unix)
            .finish()
    }
}

impl fmt::Debug for v2::PairBegin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairBegin")
            .field("offer_id", &"[REDACTED]")
            .field("controller_name", &self.controller_name)
            .field("controller_nonce", &"[REDACTED]")
            .field("pair_protocol_version", &self.pair_protocol_version)
            .finish()
    }
}

impl fmt::Debug for v2::PairChallenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairChallenge")
            .field("host_nonce", &"[REDACTED]")
            .field("selected_version", &self.selected_version)
            .field("ticket_expiry_unix", &self.ticket_expiry_unix)
            .finish()
    }
}

impl fmt::Debug for v2::PairProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairProof")
            .field("controller_proof", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for v2::PairAccepted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairAccepted")
            .field("authorization_generation", &self.authorization_generation)
            .field("host_confirmation_proof", &"[REDACTED]")
            .field("host_diagnostic_version", &self.host_diagnostic_version)
            .finish()
    }
}

impl fmt::Debug for v2::LocalPairCreateResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalPairCreateResponse")
            .field("ticket", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for v2::LocalPairAcceptRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalPairAcceptRequest")
            .field("ephemeral_operation_id", &"[REDACTED]")
            .field("fingerprint", &"[REDACTED]")
            .field("ticket", &"[REDACTED]")
            .field("alias", &self.alias)
            .finish()
    }
}

impl fmt::Debug for v2::LocalSessionUnaryRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalSessionUnaryRequest")
            .field("target_device_id", &self.target_device_id)
            .field("frame", &"[REDACTED]")
            .field("frame_len", &self.frame.len())
            .finish()
    }
}

impl fmt::Debug for v2::LocalSessionTunnelOpenRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalSessionTunnelOpenRequest")
            .field("protocol_version", &self.protocol_version)
            .field("target_device_id", &"[REDACTED]")
            .field("target_device_id_present", &self.target_device_id.is_some())
            .finish()
    }
}

impl fmt::Debug for v2::LocalSessionTunnelData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalSessionTunnelData")
            .field("bytes", &"[REDACTED]")
            .field("bytes_len", &self.bytes.len())
            .finish()
    }
}

impl fmt::Debug for v2::LocalStatusResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalStatusResponse")
            .field("protocol", &self.protocol)
            .field("version", &self.version)
            .field("phase", &self.phase)
            .field("device_id", &self.device_id)
            .field("endpoint_id", &self.endpoint_id)
            .field("device_name", &self.device_name)
            .field("infrastructure_profile", &self.infrastructure_profile)
            .field("started_at_unix", &self.started_at_unix)
            .field("active_session_count", &self.active_session_count)
            .field("active_session_names", &self.active_session_names)
            .field("network_state", &self.network_state)
            .field("endpoint_bound", &self.endpoint_bound)
            .field("network_bind_attempts", &self.network_bind_attempts)
            .field("home_relay", &"[REDACTED]")
            .field("home_relay_present", &!self.home_relay.is_empty())
            .field("address_publish_state", &self.address_publish_state)
            .field("address_lookup_state", &self.address_lookup_state)
            .field(
                "authenticated_connection_count",
                &self.authenticated_connection_count,
            )
            .field("primary_connection_count", &self.primary_connection_count)
            .field("active_stream_count", &self.active_stream_count)
            .field("direct_path_count", &self.direct_path_count)
            .field("relay_path_count", &self.relay_path_count)
            .field("network_diagnostic", &self.network_diagnostic)
            .finish()
    }
}

impl fmt::Debug for v2::LocalValidateSetupRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalValidateSetupRequest")
            .field("device_name", &self.device_name)
            .field("infrastructure_profile", &self.infrastructure_profile)
            .field("relay_url", &"[REDACTED]")
            .field("relay_url_present", &!self.relay_url.is_empty())
            .finish()
    }
}

impl fmt::Debug for v2::RelayRouteCacheV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayRouteCacheV1")
            .field("format_version", &self.format_version)
            .field("relay_url_count", &self.relay_urls.len())
            .finish()
    }
}

impl fmt::Debug for v2::SessionSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionSummary")
            .field("session_id", &self.session_id)
            .field("name", &self.name)
            .field("revision", &self.revision)
            .field("has_controller", &self.has_controller)
            .field("working_directory", &"[REDACTED]")
            .field("working_directory_len", &self.working_directory.len())
            .field("viewport", &self.viewport)
            .finish()
    }
}

impl fmt::Debug for v2::SessionCreateRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionCreateRequest")
            .field("operation_id", &self.operation_id)
            .field("target", &self.target)
            .field("name", &self.name)
            .field("working_directory", &"[REDACTED]")
            .field("working_directory_len", &self.working_directory.len())
            .field("viewport", &self.viewport)
            .finish()
    }
}

impl fmt::Debug for v2::ResumeViewId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResumeViewId([REDACTED])")
    }
}

impl fmt::Debug for v2::TerminalAttachRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalAttachRequest")
            .field("target", &self.target)
            .field("session_id", &self.session_id)
            .field("takeover", &self.takeover)
            .field("session_name", &self.session_name)
            .field("create_main", &self.create_main)
            .field("viewport", &self.viewport)
            .field(
                "resume_view_id",
                &self.resume_view_id.as_ref().map(|_| "[REDACTED]"),
            )
            .field("known_revision", &self.known_revision)
            .field("accept_zlib_frames", &self.accept_zlib_frames)
            .field(
                "acknowledge_surface_frames",
                &self.acknowledge_surface_frames,
            )
            .finish()
    }
}

impl fmt::Debug for v2::TerminalInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalInput")
            .field("operation_id", &self.operation_id)
            .field("attachment_id", &self.attachment_id)
            .field("bytes", &"[REDACTED]")
            .field("input_len", &self.bytes.len())
            .finish()
    }
}
