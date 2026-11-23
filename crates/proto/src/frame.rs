//! Bounded length-delimited wire framing and message kind dispatch.

use std::fmt;

use prost::Message;
use voytty_core::IdLengthError;
use voytty_core::terminal::TerminalSurfaceError;
use zeroize::{Zeroize, Zeroizing};

use crate::{MAX_PAIR_HELLO_FRAME_BYTES, compression, v2};

/// Product wire major shared by local IPC, `voytty/2`, and `voytty-pair/2`.
pub const WIRE_MAJOR: u32 = voytty_core::WIRE_MAJOR;
/// Current persistent-state schema exposed in readiness/status.
pub const STATE_SCHEMA_VERSION: u32 = voytty_core::STATE_SCHEMA_VERSION;
/// Maximum encoded `WireFrame` body size.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// Maximum concrete control-message payload size.
pub const MAX_CONTROL_PAYLOAD_BYTES: usize = 1024 * 1024;
/// Version of the same-UID-only opaque Session tunnel envelope.
pub const LOCAL_SESSION_TUNNEL_VERSION: u32 = 1;
/// Maximum opaque Session bytes carried by one local tunnel Data frame.
pub const MAX_LOCAL_SESSION_TUNNEL_DATA_BYTES: usize = 64 * 1024;
/// Maximum bytes in an unsigned 64-bit varint prefix.
pub const MAX_VARINT_BYTES: usize = 10;

/// Stable validated wire message kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum WireKind {
    /// Local daemon readiness request.
    LocalReadinessRequest = 1,
    /// Local daemon readiness response.
    LocalReadinessResponse = 2,
    /// Local daemon status request.
    LocalStatusRequest = 3,
    /// Local daemon status response.
    LocalStatusResponse = 4,
    /// Validate existing setup request.
    LocalValidateSetupRequest = 5,
    /// Validate existing setup response.
    LocalValidateSetupResponse = 6,
    /// Graceful daemon stop request.
    LocalStopRequest = 7,
    /// Graceful daemon stop response.
    LocalStopResponse = 8,
    /// Manual update preflight request.
    LocalUpdatePreflightRequest = 9,
    /// Manual update preflight response.
    LocalUpdatePreflightResponse = 10,
    /// Typed service error response.
    ServiceErrorResponse = 11,
    /// Local one-time pairing ticket creation request.
    LocalPairCreateRequest = 12,
    /// Local one-time pairing ticket creation response.
    LocalPairCreateResponse = 13,
    /// Local one-time pairing acceptance request.
    LocalPairAcceptRequest = 14,
    /// Local one-time pairing acceptance response.
    LocalPairAcceptResponse = 15,
    /// Local device list request.
    LocalDeviceListRequest = 16,
    /// Local device list response.
    LocalDeviceListResponse = 17,
    /// Local device alias rename request.
    LocalDeviceRenameRequest = 18,
    /// Local device alias rename response.
    LocalDeviceRenameResponse = 19,
    /// Local inbound authorization revoke request.
    LocalDeviceRevokeRequest = 20,
    /// Local inbound authorization revoke response.
    LocalDeviceRevokeResponse = 21,
    /// Resolve one exact local daemon target selector.
    LocalTargetResolveRequest = 22,
    /// Frozen local/device target selected by the daemon.
    LocalTargetResolveResponse = 23,
    /// Same-UID envelope containing one preencoded remote Session unary.
    LocalSessionUnaryRequest = 24,
    /// Same-UID request for one opaque authenticated Session service stream.
    LocalSessionTunnelOpenRequest = 25,
    /// Confirmation that the remote service stream is ready.
    LocalSessionTunnelOpened = 26,
    /// One bounded opaque chunk of target Session bytes.
    LocalSessionTunnelData = 27,
    /// Address-free path and RTT sideband for this tunnel epoch.
    LocalSessionTunnelPath = 28,
    /// Directional end-of-data marker for one tunnel half.
    LocalSessionTunnelHalfClose = 29,
    /// Content-free terminal outcome for one tunnel epoch.
    LocalSessionTunnelClosed = 30,
    /// Opt-in, same-UID connection-stage observation before a local result.
    LocalConnectionProgress = 31,
    /// Controller opens a pairing handshake.
    PairBegin = 100,
    /// Host responds to a pairing handshake.
    PairChallenge = 101,
    /// Controller submits its secret-possession proof.
    PairProof = 102,
    /// Host confirms a committed authorization.
    PairAccepted = 103,
    /// First authenticated frame on a normal connection.
    ConnectionHello = 104,
    /// Responder half of a normal connection handshake.
    ConnectionWelcome = 105,
    /// Session list request.
    SessionListRequest = 200,
    /// Session list response.
    SessionListResponse = 201,
    /// Session creation request.
    SessionCreateRequest = 209,
    /// Session mutation response.
    SessionMutateResponse = 203,
    /// Session rename request.
    SessionRenameRequest = 204,
    /// Session close request.
    SessionCloseRequest = 205,
    /// Controller takeover request.
    SessionTakeoverRequest = 206,
    /// Allocate a daemon-issued mutation replay lease.
    SessionOperationLeaseRequest = 207,
    /// Daemon-issued mutation replay lease.
    SessionOperationLeaseResponse = 208,
    /// Terminal attach request.
    TerminalAttachRequest = 323,
    /// Complete exact semantic terminal surface.
    TerminalSemanticSnapshot = 301,
    /// Merged exact semantic terminal row replacements.
    TerminalSemanticDelta = 302,
    /// Terminal input.
    TerminalInput = 303,
    /// Terminal resize.
    TerminalResize = 304,
    /// Terminal detach.
    TerminalDetach = 305,
    /// Acknowledgement that a snapshot was applied atomically.
    TerminalSnapshotApplied = 306,
    /// Request for a snapshot from the client's known revision.
    TerminalSyncRequest = 307,
    /// Instruction to resynchronize from the latest revision.
    TerminalSyncRequired = 308,
    /// A controller attachment lost its lease to a takeover.
    TerminalLeaseLost = 309,
    /// The root shell and session have ended.
    TerminalSessionEnded = 310,
    /// Same-UID-only frontend attachment transport-state projection.
    TerminalTransportStateEvent = 311,
    /// Same-UID-only selected connection path and RTT projection.
    TerminalConnectionStatusEvent = 314,
    /// Requests one stateless bounded contiguous history window.
    TerminalHistoryWindowRequest = 317,
    /// Correlated semantic-cell history-window outcome.
    TerminalSemanticHistoryWindowFrame = 318,
    /// Decoded latest-only child clipboard write for the current controller.
    TerminalClipboardWrite = 322,
    /// Current controller base-color observations.
    TerminalBaseColors = 324,
    /// Ordinary transient controller notification.
    TerminalNotification = 325,
    /// Controller receipt of numbered live surface frames on one stream.
    TerminalSurfaceFramesReceived = 326,
    /// Starts an attachment-bound upload on a separate service stream.
    UploadBegin = 400,
    /// Host admission and unique transfer identity.
    UploadReady = 401,
    /// Bounded in-order file bytes.
    UploadChunk = 402,
    /// Acknowledges bytes written by the host.
    UploadProgress = 403,
    /// Requests publication after exact byte completion.
    UploadFinish = 404,
    /// Published safe absolute path.
    UploadCompleted = 405,
    /// Explicit cancellation before publication.
    UploadCancel = 406,
    /// Confirms incomplete staging was removed.
    UploadCancelled = 407,
}

impl WireKind {
    /// Returns whether the kind uses the stricter control-payload limit.
    #[must_use]
    pub const fn is_control(self) -> bool {
        !matches!(
            self,
            Self::TerminalSemanticSnapshot
                | Self::TerminalSemanticDelta
                | Self::TerminalSemanticHistoryWindowFrame
        )
    }

    /// Returns whether the kind is a short pair/hello transport frame.
    #[must_use]
    pub const fn is_pair_hello(self) -> bool {
        matches!(
            self,
            Self::PairBegin
                | Self::PairChallenge
                | Self::PairProof
                | Self::PairAccepted
                | Self::ConnectionHello
                | Self::ConnectionWelcome
        )
    }

    /// Returns the maximum admitted control payload bytes for this kind.
    #[must_use]
    pub const fn max_control_payload_bytes(self) -> usize {
        if matches!(self, Self::LocalConnectionProgress) {
            64
        } else if self.is_pair_hello() {
            MAX_PAIR_HELLO_FRAME_BYTES
        } else if matches!(self, Self::LocalSessionTunnelData) {
            // One field tag plus the three-byte encoded length at this fixed
            // ceiling. This makes a one-byte-oversized canonical Data message
            // fail before it can enter the tunnel adapter.
            MAX_LOCAL_SESSION_TUNNEL_DATA_BYTES + 4
        } else if matches!(self, Self::UploadChunk) {
            voytty_core::upload::UPLOAD_CHUNK_BYTES + 64
        } else {
            MAX_CONTROL_PAYLOAD_BYTES
        }
    }
}

impl TryFrom<u32> for WireKind {
    type Error = ProtocolError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        let kind = match value {
            1 => Self::LocalReadinessRequest,
            2 => Self::LocalReadinessResponse,
            3 => Self::LocalStatusRequest,
            4 => Self::LocalStatusResponse,
            5 => Self::LocalValidateSetupRequest,
            6 => Self::LocalValidateSetupResponse,
            7 => Self::LocalStopRequest,
            8 => Self::LocalStopResponse,
            9 => Self::LocalUpdatePreflightRequest,
            10 => Self::LocalUpdatePreflightResponse,
            11 => Self::ServiceErrorResponse,
            12 => Self::LocalPairCreateRequest,
            13 => Self::LocalPairCreateResponse,
            14 => Self::LocalPairAcceptRequest,
            15 => Self::LocalPairAcceptResponse,
            16 => Self::LocalDeviceListRequest,
            17 => Self::LocalDeviceListResponse,
            18 => Self::LocalDeviceRenameRequest,
            19 => Self::LocalDeviceRenameResponse,
            20 => Self::LocalDeviceRevokeRequest,
            21 => Self::LocalDeviceRevokeResponse,
            22 => Self::LocalTargetResolveRequest,
            23 => Self::LocalTargetResolveResponse,
            24 => Self::LocalSessionUnaryRequest,
            25 => Self::LocalSessionTunnelOpenRequest,
            26 => Self::LocalSessionTunnelOpened,
            27 => Self::LocalSessionTunnelData,
            28 => Self::LocalSessionTunnelPath,
            29 => Self::LocalSessionTunnelHalfClose,
            30 => Self::LocalSessionTunnelClosed,
            31 => Self::LocalConnectionProgress,
            100 => Self::PairBegin,
            101 => Self::PairChallenge,
            102 => Self::PairProof,
            103 => Self::PairAccepted,
            104 => Self::ConnectionHello,
            105 => Self::ConnectionWelcome,
            200 => Self::SessionListRequest,
            201 => Self::SessionListResponse,
            209 => Self::SessionCreateRequest,
            203 => Self::SessionMutateResponse,
            204 => Self::SessionRenameRequest,
            205 => Self::SessionCloseRequest,
            206 => Self::SessionTakeoverRequest,
            207 => Self::SessionOperationLeaseRequest,
            208 => Self::SessionOperationLeaseResponse,
            323 => Self::TerminalAttachRequest,
            301 => Self::TerminalSemanticSnapshot,
            302 => Self::TerminalSemanticDelta,
            303 => Self::TerminalInput,
            304 => Self::TerminalResize,
            305 => Self::TerminalDetach,
            306 => Self::TerminalSnapshotApplied,
            307 => Self::TerminalSyncRequest,
            308 => Self::TerminalSyncRequired,
            309 => Self::TerminalLeaseLost,
            310 => Self::TerminalSessionEnded,
            311 => Self::TerminalTransportStateEvent,
            314 => Self::TerminalConnectionStatusEvent,
            317 => Self::TerminalHistoryWindowRequest,
            318 => Self::TerminalSemanticHistoryWindowFrame,
            322 => Self::TerminalClipboardWrite,
            324 => Self::TerminalBaseColors,
            325 => Self::TerminalNotification,
            326 => Self::TerminalSurfaceFramesReceived,
            400 => Self::UploadBegin,
            401 => Self::UploadReady,
            402 => Self::UploadChunk,
            403 => Self::UploadProgress,
            404 => Self::UploadFinish,
            405 => Self::UploadCompleted,
            406 => Self::UploadCancel,
            407 => Self::UploadCancelled,
            unknown => return Err(ProtocolError::UnknownKind(unknown)),
        };
        Ok(kind)
    }
}

/// A decoded frame after major, kind, and size validation.
#[derive(Clone, Eq, PartialEq)]
pub struct DecodedFrame {
    /// Stable message kind.
    pub kind: WireKind,
    /// Unary request correlation ID.
    pub request_id: u64,
    /// Relative request deadline in milliseconds; zero selects service default.
    pub deadline_ms: u32,
    /// Still-encoded concrete protobuf message.
    pub payload: Vec<u8>,
}

impl fmt::Debug for DecodedFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedFrame")
            .field("kind", &self.kind)
            .field("request_id", &self.request_id)
            .field("deadline_ms", &self.deadline_ms)
            .field("payload", &"[REDACTED]")
            .field("payload_len", &self.payload.len())
            .finish()
    }
}

impl DecodedFrame {
    /// Decodes the concrete message after checking its expected kind.
    pub fn decode_message<M: Message + Default>(
        &self,
        expected: WireKind,
    ) -> Result<M, ProtocolError> {
        if self.kind != expected {
            return Err(ProtocolError::UnexpectedKind {
                expected,
                actual: self.kind,
            });
        }
        M::decode(self.payload.as_slice()).map_err(ProtocolError::MalformedProtobuf)
    }
}

/// Errors returned by the voytty frame/protobuf validation boundary.
#[derive(Debug)]
pub enum ProtocolError {
    /// Length prefix exceeded unsigned 64-bit varint syntax.
    MalformedVarint,
    /// Stream ended in a prefix or frame body.
    TruncatedFrame,
    /// Frame length exceeded the 8 MiB bound before allocation.
    FrameTooLarge(usize),
    /// Control payload exceeded its per-kind bound before concrete decoding.
    ControlPayloadTooLarge(usize),
    /// `WireFrame` or concrete payload protobuf was malformed.
    MalformedProtobuf(prost::DecodeError),
    /// Peer uses an incompatible product wire major.
    WireMajorMismatch {
        /// Local supported major.
        expected: u32,
        /// Peer-provided major.
        actual: u32,
    },
    /// Numeric message kind is not registered.
    UnknownKind(u32),
    /// A compressed payload was malformed, unsupported, or used on a forbidden kind.
    InvalidPayloadEncoding,
    /// Caller attempted to decode a frame as another known kind.
    UnexpectedKind {
        /// Kind required by the typed decoder.
        expected: WireKind,
        /// Kind present in the frame.
        actual: WireKind,
    },
    /// Domain ID field had the wrong fixed width.
    InvalidIdentifier(IdLengthError),
    /// A wire viewport did not fit the voytty-owned non-zero `u16` boundary.
    InvalidTerminalSize {
        /// Wire row count.
        rows: u32,
        /// Wire column count.
        columns: u32,
    },
    /// A semantic surface violated its content-free structural contract.
    InvalidTerminalSurface(TerminalSurfaceError),
    /// A terminal enum or required semantic field used an unsupported value.
    InvalidTerminalSemanticField(&'static str),
    /// A same-UID connection observation was unspecified or unsupported.
    InvalidLocalConnectionStage,
    /// An upload field violated its shared domain contract.
    InvalidUpload(voytty_core::DomainErrorKind),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedVarint => write!(formatter, "malformed frame length varint"),
            Self::TruncatedFrame => write!(formatter, "stream ended inside a frame"),
            Self::FrameTooLarge(length) => {
                write!(
                    formatter,
                    "frame length {length} exceeds {MAX_FRAME_BYTES} bytes"
                )
            }
            Self::ControlPayloadTooLarge(length) => {
                write!(
                    formatter,
                    "control payload length {length} exceeds its per-kind bound"
                )
            }
            Self::MalformedProtobuf(error) => write!(formatter, "malformed protobuf: {error}"),
            Self::WireMajorMismatch { expected, actual } => write!(
                formatter,
                "wire major mismatch: local {expected}, peer {actual}"
            ),
            Self::UnknownKind(kind) => write!(formatter, "unknown wire message kind {kind}"),
            Self::InvalidPayloadEncoding => write!(formatter, "invalid frame payload encoding"),
            Self::UnexpectedKind { expected, actual } => {
                write!(formatter, "expected {expected:?}, got {actual:?}")
            }
            Self::InvalidIdentifier(error) => error.fmt(formatter),
            Self::InvalidTerminalSize { rows, columns } => {
                write!(formatter, "invalid terminal viewport {columns}x{rows}")
            }
            Self::InvalidTerminalSurface(error) => error.fmt(formatter),
            Self::InvalidLocalConnectionStage => {
                write!(formatter, "invalid local connection progress stage")
            }
            Self::InvalidUpload(kind) => write!(formatter, "invalid upload: {}", kind.code()),
            Self::InvalidTerminalSemanticField(field) => {
                write!(formatter, "invalid semantic terminal field {field}")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

/// Incremental decoder for `varint length + WireFrame` streams.
pub struct FrameDecoder {
    allow_compression: bool,
    prefix: [u8; MAX_VARINT_BYTES],
    prefix_len: usize,
    pub(crate) body: Vec<u8>,
    pub(crate) expected_body: Option<usize>,
    maximum_body_bytes: usize,
}

impl fmt::Debug for FrameDecoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrameDecoder")
            .field("prefix_bytes", &self.prefix_len)
            .field("buffered_body_bytes", &self.body.len())
            .field("expected_body_bytes", &self.expected_body)
            .field("maximum_body_bytes", &self.maximum_body_bytes)
            .finish()
    }
}

impl Drop for FrameDecoder {
    fn drop(&mut self) {
        self.prefix.zeroize();
        self.prefix_len = 0;
        self.body.zeroize();
        self.expected_body = None;
        self.maximum_body_bytes = 0;
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    /// Bytes retained for the current incomplete frame, including its prefix.
    /// Reveals no contents and is unaffected by cancellation of the stream read.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.prefix_len + self.body.len()
    }

    /// Creates a decoder for controller-to-host requests, which never carry compressed surfaces.
    #[must_use]
    pub fn for_requests() -> Self {
        let mut decoder = Self::new();
        decoder.allow_compression = false;
        decoder
    }

    /// Creates an empty incremental decoder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            allow_compression: true,
            prefix: [0; MAX_VARINT_BYTES],
            prefix_len: 0,
            body: Vec::new(),
            expected_body: None,
            maximum_body_bytes: MAX_FRAME_BYTES,
        }
    }

    /// Creates an incremental decoder with a stricter frame-body ceiling.
    ///
    /// Transport handshakes use this constructor before authentication so the
    /// length prefix cannot cause an allocation up to the broader 8 MiB service
    /// limit. The requested limit is always capped by [`MAX_FRAME_BYTES`].
    #[must_use]
    pub const fn with_maximum_body_bytes(maximum_body_bytes: usize) -> Self {
        Self {
            allow_compression: true,
            prefix: [0; MAX_VARINT_BYTES],
            prefix_len: 0,
            body: Vec::new(),
            expected_body: None,
            maximum_body_bytes: if maximum_body_bytes < MAX_FRAME_BYTES {
                maximum_body_bytes
            } else {
                MAX_FRAME_BYTES
            },
        }
    }

    /// Feeds arbitrary stream bytes and returns every complete validated frame.
    pub fn feed(&mut self, mut input: &[u8]) -> Result<Vec<DecodedFrame>, ProtocolError> {
        let mut frames = Vec::new();
        while !input.is_empty() {
            if self.expected_body.is_none() {
                let byte = input[0];
                input = &input[1..];
                if self.prefix_len == MAX_VARINT_BYTES {
                    return Err(ProtocolError::MalformedVarint);
                }
                self.prefix[self.prefix_len] = byte;
                self.prefix_len += 1;
                if byte & 0x80 == 0 {
                    let length = decode_varint(&self.prefix[..self.prefix_len])?;
                    let length = usize::try_from(length)
                        .map_err(|_| ProtocolError::FrameTooLarge(usize::MAX))?;
                    if length > self.maximum_body_bytes {
                        return Err(ProtocolError::FrameTooLarge(length));
                    }
                    self.body = Vec::with_capacity(length.min(INITIAL_FRAME_BODY_CAPACITY));
                    self.expected_body = Some(length);
                } else if self.prefix_len == MAX_VARINT_BYTES {
                    return Err(ProtocolError::MalformedVarint);
                }
            }

            if let Some(expected) = self.expected_body {
                let needed = expected.saturating_sub(self.body.len());
                let taken = needed.min(input.len());
                self.body.extend_from_slice(&input[..taken]);
                input = &input[taken..];
                if self.body.len() == expected {
                    let body = Zeroizing::new(std::mem::take(&mut self.body));
                    self.expected_body = None;
                    self.prefix_len = 0;
                    frames.push(decode_wire_frame(
                        body.as_slice(),
                        self.maximum_body_bytes,
                        self.allow_compression,
                    )?);
                }
            }
        }
        Ok(frames)
    }

    /// Verifies that a completed byte stream ended on a frame boundary.
    pub fn finish(self) -> Result<(), ProtocolError> {
        if self.prefix_len == 0 && self.expected_body.is_none() {
            Ok(())
        } else {
            Err(ProtocolError::TruncatedFrame)
        }
    }
}

pub(crate) const INITIAL_FRAME_BODY_CAPACITY: usize = 64 * 1024;

/// Encodes one concrete protobuf message into the bounded stream format.
pub fn encode_message<M: Message>(
    kind: WireKind,
    request_id: u64,
    deadline_ms: u32,
    message: &M,
) -> Result<Vec<u8>, ProtocolError> {
    encode_payload(kind, request_id, deadline_ms, message.encode_to_vec())
}

/// Encodes an already serialized concrete payload into the bounded stream format.
pub fn encode_payload(
    kind: WireKind,
    request_id: u64,
    deadline_ms: u32,
    mut payload: Vec<u8>,
) -> Result<Vec<u8>, ProtocolError> {
    if let Err(error) = validate_payload_limit(kind, payload.len()) {
        payload.zeroize();
        return Err(error);
    }
    let mut wire = v2::WireFrame {
        payload_encoding: 0,
        wire_major: WIRE_MAJOR,
        kind: kind as u32,
        payload,
        request_id,
        deadline_ms,
    };
    let body = Zeroizing::new(wire.encode_to_vec());
    wire.payload.zeroize();
    if body.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge(body.len()));
    }
    let mut output = Vec::with_capacity(MAX_VARINT_BYTES + body.len());
    encode_varint(body.len() as u64, &mut output);
    output.extend_from_slice(&body);
    Ok(output)
}

fn decode_wire_frame(
    body: &[u8],
    maximum_body_bytes: usize,
    allow_compression: bool,
) -> Result<DecodedFrame, ProtocolError> {
    let mut wire = v2::WireFrame::decode(body).map_err(ProtocolError::MalformedProtobuf)?;
    if wire.wire_major != WIRE_MAJOR {
        wire.payload.zeroize();
        return Err(ProtocolError::WireMajorMismatch {
            expected: WIRE_MAJOR,
            actual: wire.wire_major,
        });
    }
    let kind = match WireKind::try_from(wire.kind) {
        Ok(kind) => kind,
        Err(error) => {
            wire.payload.zeroize();
            return Err(error);
        }
    };
    if !allow_compression && wire.payload_encoding != 0 {
        wire.payload.zeroize();
        return Err(ProtocolError::InvalidPayloadEncoding);
    }
    if let Err(error) = compression::decode_payload(&mut wire, kind, maximum_body_bytes) {
        wire.payload.zeroize();
        return Err(error);
    }
    if let Err(error) = validate_payload_limit(kind, wire.payload.len()) {
        wire.payload.zeroize();
        return Err(error);
    }
    Ok(DecodedFrame {
        kind,
        request_id: wire.request_id,
        deadline_ms: wire.deadline_ms,
        payload: wire.payload,
    })
}

fn validate_payload_limit(kind: WireKind, length: usize) -> Result<(), ProtocolError> {
    let maximum = kind.max_control_payload_bytes();
    if kind.is_control() && length > maximum {
        Err(ProtocolError::ControlPayloadTooLarge(length))
    } else {
        Ok(())
    }
}

pub(crate) fn encode_varint(mut value: u64, output: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            output.push(byte);
            return;
        }
        output.push(byte | 0x80);
    }
}

pub(crate) fn decode_varint(bytes: &[u8]) -> Result<u64, ProtocolError> {
    let mut value = 0_u64;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if index == 9 && byte > 1 {
            return Err(ProtocolError::MalformedVarint);
        }
        value |= u64::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            if index > 0 && byte == 0 {
                return Err(ProtocolError::MalformedVarint);
            }
            return Ok(value);
        }
    }
    Err(ProtocolError::MalformedVarint)
}
