//! Wire conversions and bounded codecs for pairing tickets, routes and handshakes.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use prost::Message;
use voytty_core::{
    AuthGeneration, AuthorizationStatus, Capabilities, ConnectionAttemptId, ConnectionError,
    ConnectionHello, ConnectionWelcome, DeviceAlias, DeviceAliasError, DeviceId, DeviceSummary,
    DeviceSummaryError, EphemeralOperationId, IdLengthError, MAX_TICKET_TEXT_BYTES, PairAccepted,
    PairBegin, PairChallenge, PairFingerprint, PairFingerprintError, PairNonce, PairOfferId,
    PairProof, PairSecret, PairSecretError, PairTicketError, PairTicketFields, RelayHint,
    RelayHintError,
};
use zeroize::{Zeroize, Zeroizing};

use crate::v2;

/// Fixed text prefix before the base64url-no-pad ticket payload.
pub const PAIR_TICKET_PREFIX: &str = "voytty-pair-v1:";
/// Version of the persisted relay route cache.
pub const RELAY_ROUTE_CACHE_VERSION: u32 = 1;
/// Maximum bytes in a persisted relay route cache blob before decoding.
///
/// The product ceiling is four 2048-byte relay URLs plus framing, so 16 KiB is
/// a generous bound that still rejects a malformed or hostile oversized blob.
pub const MAX_RELAY_ROUTE_CACHE_BYTES: usize = 16 * 1024;
/// Maximum hello or pair frame body bytes for kinds 100-105.
pub const MAX_PAIR_HELLO_FRAME_BYTES: usize = voytty_core::MAX_PAIR_HELLO_FRAME_BYTES;
/// Maximum total bytes exchanged in one pairing handshake.
pub const MAX_PAIR_HANDSHAKE_BYTES: usize = voytty_core::MAX_PAIR_HANDSHAKE_BYTES;

/// Failure while encoding or decoding a pairing ticket text.
#[derive(Debug)]
pub enum TicketTextError {
    /// Text did not begin with the fixed pairing prefix.
    MissingPrefix,
    /// Ticket text exceeded the 16 KiB bound before any allocation.
    TooLong {
        /// Observed byte count.
        actual: usize,
    },
    /// Base64url payload was malformed.
    InvalidBase64(base64::DecodeError),
    /// Decoded protobuf was malformed.
    InvalidProtobuf(prost::DecodeError),
    /// A fixed-width domain field had the wrong byte length.
    InvalidIdentifier(IdLengthError),
    /// The pairing secret had the wrong byte length.
    InvalidSecret(PairSecretError),
    /// A relay hint URL was invalid.
    InvalidRelayHint(RelayHintError),
    /// Ticket fields failed their product contract.
    InvalidTicket(PairTicketError),
}

impl fmt::Display for TicketTextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPrefix => {
                write!(
                    formatter,
                    "ticket text must start with {PAIR_TICKET_PREFIX:?}"
                )
            }
            Self::TooLong { actual } => write!(
                formatter,
                "ticket text length {actual} exceeds {MAX_TICKET_TEXT_BYTES} bytes"
            ),
            Self::InvalidBase64(error) => write!(formatter, "invalid ticket base64url: {error}"),
            Self::InvalidProtobuf(error) => write!(formatter, "invalid ticket protobuf: {error}"),
            Self::InvalidIdentifier(error) => error.fmt(formatter),
            Self::InvalidSecret(error) => error.fmt(formatter),
            Self::InvalidRelayHint(error) => error.fmt(formatter),
            Self::InvalidTicket(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TicketTextError {}

/// Failure while encoding or decoding a persisted relay route cache.
#[derive(Debug)]
pub enum RouteCacheError {
    /// Cache protobuf bytes were malformed.
    Malformed(prost::DecodeError),
    /// Cache version is not recognized; the caller ignores it with a diagnostic.
    UnsupportedVersion {
        /// Observed cache version.
        actual: u32,
    },
    /// A cached relay URL was invalid.
    InvalidRelayHint(RelayHintError),
    /// A persisted verified route cache contained no relay URL.
    MissingUrl,
    /// The cache advertised more relay URLs than the product bound.
    TooManyUrls {
        /// Observed count.
        actual: usize,
    },
    /// The cache byte blob exceeded its pre-decode ceiling.
    TooLarge {
        /// Observed byte count.
        actual: usize,
    },
}

impl fmt::Display for RouteCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(error) => write!(formatter, "malformed relay route cache: {error}"),
            Self::UnsupportedVersion { actual } => {
                write!(formatter, "unsupported relay route cache version {actual}")
            }
            Self::InvalidRelayHint(error) => error.fmt(formatter),
            Self::MissingUrl => write!(formatter, "relay route cache must contain a relay URL"),
            Self::TooManyUrls { actual } => {
                write!(
                    formatter,
                    "relay route cache advertised too many URLs ({actual})"
                )
            }
            Self::TooLarge { actual } => write!(
                formatter,
                "relay route cache length {actual} exceeds {MAX_RELAY_ROUTE_CACHE_BYTES} bytes"
            ),
        }
    }
}

impl std::error::Error for RouteCacheError {}

impl From<(&PairTicketFields, &PairSecret)> for v2::PairTicketV1 {
    fn from((fields, secret): (&PairTicketFields, &PairSecret)) -> Self {
        Self {
            format_version: fields.format_version(),
            host_device_id: Some(fields.host_device_id().into()),
            host_name: fields.host_name().to_owned(),
            relay_urls: fields
                .relay_hints()
                .iter()
                .map(|hint| hint.as_str().to_owned())
                .collect(),
            offer_id: fields.offer_id().to_bytes().to_vec(),
            secret: secret.as_bytes().to_vec(),
            expires_at_unix: fields.expires_at_unix(),
        }
    }
}

impl TryFrom<v2::PairTicketV1> for (PairTicketFields, PairSecret) {
    type Error = TicketTextError;

    fn try_from(value: v2::PairTicketV1) -> Result<Self, Self::Error> {
        let v2::PairTicketV1 {
            format_version,
            host_device_id,
            host_name,
            relay_urls,
            offer_id,
            secret,
            expires_at_unix,
        } = value;
        // Move the raw secret into zeroizing ownership before any fallible
        // validation, so every early return scrubs the bytes.
        let secret: Zeroizing<Vec<u8>> = Zeroizing::new(secret);
        let host_device_bytes = host_device_id.unwrap_or_default().value;
        let host_device_id =
            DeviceId::from_bytes(&host_device_bytes).map_err(TicketTextError::InvalidIdentifier)?;
        let offer_id = voytty_core::PairOfferId::from_bytes(&offer_id)
            .map_err(TicketTextError::InvalidIdentifier)?;
        let secret =
            PairSecret::from_slice(secret.as_slice()).map_err(TicketTextError::InvalidSecret)?;
        let relay_hints = relay_urls
            .iter()
            .map(|url| RelayHint::new(url.clone()).map_err(TicketTextError::InvalidRelayHint))
            .collect::<Result<Vec<_>, _>>()?;
        let fields = PairTicketFields::new(
            format_version,
            host_device_id,
            host_name,
            relay_hints,
            offer_id,
            expires_at_unix,
        )
        .map_err(TicketTextError::InvalidTicket)?;
        Ok((fields, secret))
    }
}

/// Encodes a pairing ticket as `voytty-pair-v1:` + base64url-no-pad text.
///
/// The secret-bearing fields are already validated, so a well-formed ticket
/// always fits the 16 KiB text bound. Every temporary prost/encoded secret
/// buffer is zeroized before the function returns; only the intended ticket
/// text (the caller's bearer value) is produced.
#[must_use]
pub fn encode_pair_ticket(fields: &PairTicketFields, secret: &PairSecret) -> String {
    let mut message = v2::PairTicketV1::from((fields, secret));
    let encoded: Zeroizing<Vec<u8>> = Zeroizing::new(message.encode_to_vec());
    message.secret.zeroize();
    // The base64 string is a second bearer copy; zeroize it once copied out.
    let encoded_text: Zeroizing<String> = Zeroizing::new(URL_SAFE_NO_PAD.encode(&*encoded));
    let mut text = String::with_capacity(PAIR_TICKET_PREFIX.len() + encoded_text.len());
    text.push_str(PAIR_TICKET_PREFIX);
    text.push_str(encoded_text.as_str());
    debug_assert!(text.len() <= MAX_TICKET_TEXT_BYTES);
    text
}

/// Decodes and validates a `voytty-pair-v1:` ticket text.
///
/// The 16 KiB bound is enforced before base64 decoding, and every secret or
/// decoded buffer is zeroized on drop or immediately after extraction.
pub fn decode_pair_ticket(text: &str) -> Result<(PairTicketFields, PairSecret), TicketTextError> {
    if text.len() > MAX_TICKET_TEXT_BYTES {
        return Err(TicketTextError::TooLong { actual: text.len() });
    }
    let encoded = text
        .strip_prefix(PAIR_TICKET_PREFIX)
        .ok_or(TicketTextError::MissingPrefix)?;
    let decoded: Zeroizing<Vec<u8>> = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(TicketTextError::InvalidBase64)?,
    );
    let message =
        v2::PairTicketV1::decode(decoded.as_slice()).map_err(TicketTextError::InvalidProtobuf)?;
    (message).try_into()
}

/// Encodes a versioned relay-only route cache, rejecting an oversized slice.
pub fn encode_relay_route_cache(urls: &[RelayHint]) -> Result<Vec<u8>, RouteCacheError> {
    if urls.is_empty() {
        return Err(RouteCacheError::MissingUrl);
    }
    if urls.len() > voytty_core::MAX_RELAY_HINTS {
        return Err(RouteCacheError::TooManyUrls { actual: urls.len() });
    }
    Ok(v2::RelayRouteCacheV1 {
        format_version: RELAY_ROUTE_CACHE_VERSION,
        relay_urls: urls.iter().map(|hint| hint.as_str().to_owned()).collect(),
    }
    .encode_to_vec())
}

/// Decodes a versioned relay-only route cache; unknown versions, oversized
/// blobs, and excess URLs fail with a typed diagnostic and are never migrated
/// or used.
pub fn decode_relay_route_cache(bytes: &[u8]) -> Result<Vec<RelayHint>, RouteCacheError> {
    if bytes.len() > MAX_RELAY_ROUTE_CACHE_BYTES {
        return Err(RouteCacheError::TooLarge {
            actual: bytes.len(),
        });
    }
    let cache = v2::RelayRouteCacheV1::decode(bytes).map_err(RouteCacheError::Malformed)?;
    if cache.format_version != RELAY_ROUTE_CACHE_VERSION {
        return Err(RouteCacheError::UnsupportedVersion {
            actual: cache.format_version,
        });
    }
    if cache.relay_urls.is_empty() {
        return Err(RouteCacheError::MissingUrl);
    }
    if cache.relay_urls.len() > voytty_core::MAX_RELAY_HINTS {
        return Err(RouteCacheError::TooManyUrls {
            actual: cache.relay_urls.len(),
        });
    }
    cache
        .relay_urls
        .iter()
        .map(|url| RelayHint::new(url.clone()).map_err(RouteCacheError::InvalidRelayHint))
        .collect()
}

/// Failure while converting a wire message into a validated domain value.
#[derive(Debug)]
pub enum WireFieldError {
    /// A fixed-width field had the wrong byte length.
    InvalidIdentifier(IdLengthError),
    /// A pair handshake field failed its product contract.
    InvalidPair(PairTicketError),
    /// A connection handshake field failed its product contract.
    InvalidConnection(ConnectionError),
    /// A device alias failed its product contract.
    InvalidAlias(DeviceAliasError),
    /// A device summary projection was internally inconsistent.
    InvalidSummary(DeviceSummaryError),
    /// A device projection used an unspecified or unknown authorization status.
    InvalidAuthStatus {
        /// Observed protobuf enum value.
        actual: i32,
    },
    /// An authorization generation exceeded the SQLite signed 64-bit ceiling.
    InvalidGeneration {
        /// Observed generation value.
        actual: u64,
    },
}

impl fmt::Display for WireFieldError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentifier(error) => error.fmt(formatter),
            Self::InvalidPair(error) => error.fmt(formatter),
            Self::InvalidConnection(error) => error.fmt(formatter),
            Self::InvalidAlias(error) => error.fmt(formatter),
            Self::InvalidSummary(error) => error.fmt(formatter),
            Self::InvalidAuthStatus { actual } => {
                write!(formatter, "invalid device authorization status {actual}")
            }
            Self::InvalidGeneration { actual } => write!(
                formatter,
                "authorization generation {actual} exceeds the signed 64-bit ceiling"
            ),
        }
    }
}

impl std::error::Error for WireFieldError {}

impl TryFrom<v2::PairBegin> for PairBegin {
    type Error = WireFieldError;

    fn try_from(value: v2::PairBegin) -> Result<Self, Self::Error> {
        let offer_id =
            PairOfferId::from_bytes(&value.offer_id).map_err(WireFieldError::InvalidIdentifier)?;
        let controller_nonce = PairNonce::from_bytes(&value.controller_nonce)
            .map_err(WireFieldError::InvalidIdentifier)?;
        PairBegin::new(
            offer_id,
            value.controller_name,
            controller_nonce,
            value.pair_protocol_version,
        )
        .map_err(WireFieldError::InvalidPair)
    }
}

impl From<&PairBegin> for v2::PairBegin {
    fn from(value: &PairBegin) -> Self {
        Self {
            offer_id: value.offer_id().to_bytes().to_vec(),
            controller_name: value.controller_name().to_owned(),
            controller_nonce: value.controller_nonce().to_bytes().to_vec(),
            pair_protocol_version: value.pair_protocol_version(),
        }
    }
}

impl TryFrom<v2::PairChallenge> for PairChallenge {
    type Error = WireFieldError;

    fn try_from(value: v2::PairChallenge) -> Result<Self, Self::Error> {
        let host_nonce =
            PairNonce::from_bytes(&value.host_nonce).map_err(WireFieldError::InvalidIdentifier)?;
        PairChallenge::new(host_nonce, value.selected_version, value.ticket_expiry_unix)
            .map_err(WireFieldError::InvalidPair)
    }
}

impl From<&PairChallenge> for v2::PairChallenge {
    fn from(value: &PairChallenge) -> Self {
        Self {
            host_nonce: value.host_nonce().to_bytes().to_vec(),
            selected_version: value.selected_version(),
            ticket_expiry_unix: value.ticket_expiry_unix(),
        }
    }
}

impl TryFrom<v2::PairProof> for PairProof {
    type Error = WireFieldError;

    fn try_from(value: v2::PairProof) -> Result<Self, Self::Error> {
        let proof = Zeroizing::new(value.controller_proof);
        PairProof::from_slice(&proof).map_err(WireFieldError::InvalidIdentifier)
    }
}

impl From<&PairProof> for v2::PairProof {
    fn from(value: &PairProof) -> Self {
        Self {
            controller_proof: value.as_bytes().to_vec(),
        }
    }
}

impl TryFrom<v2::PairAccepted> for PairAccepted {
    type Error = WireFieldError;

    fn try_from(value: v2::PairAccepted) -> Result<Self, Self::Error> {
        let v2::PairAccepted {
            authorization_generation,
            host_confirmation_proof,
            host_diagnostic_version,
        } = value;
        let proof = Zeroizing::new(host_confirmation_proof);
        let generation = AuthGeneration::new(authorization_generation).ok_or(
            WireFieldError::InvalidGeneration {
                actual: authorization_generation,
            },
        )?;
        let proof = PairProof::from_slice(&proof).map_err(WireFieldError::InvalidIdentifier)?;
        PairAccepted::from_proof(generation, proof, host_diagnostic_version)
            .map_err(WireFieldError::InvalidPair)
    }
}

impl From<&PairAccepted> for v2::PairAccepted {
    fn from(value: &PairAccepted) -> Self {
        Self {
            authorization_generation: value.authorization_generation().get(),
            host_confirmation_proof: value.host_confirmation_proof().to_vec(),
            host_diagnostic_version: value.host_diagnostic_version().to_owned(),
        }
    }
}

impl TryFrom<v2::ConnectionHello> for ConnectionHello {
    type Error = WireFieldError;

    fn try_from(value: v2::ConnectionHello) -> Result<Self, Self::Error> {
        let incarnation = decode_connection_incarnation(&value.incarnation)?;
        let attempt_id = ConnectionAttemptId::from_bytes(&value.attempt_id)
            .map_err(WireFieldError::InvalidIdentifier)?;
        ConnectionHello::new(
            value.min_wire_major,
            value.max_wire_major,
            Capabilities::from_bits_retain(value.capabilities),
            attempt_id,
            value.initiator_display,
            value.initiator_build,
            value.initiator_platform,
        )
        .map_err(WireFieldError::InvalidConnection)
        .map(|hello| hello.with_incarnation(incarnation))
    }
}

impl From<&ConnectionHello> for v2::ConnectionHello {
    fn from(value: &ConnectionHello) -> Self {
        Self {
            min_wire_major: value.min_wire_major(),
            max_wire_major: value.max_wire_major(),
            capabilities: value.capabilities().bits(),
            attempt_id: value.attempt_id().to_bytes().to_vec(),
            initiator_display: value.initiator_display().to_owned(),
            initiator_build: value.initiator_build().to_owned(),
            initiator_platform: value.initiator_platform().to_owned(),
            incarnation: value
                .incarnation()
                .map_or_else(Vec::new, |id| id.to_bytes().to_vec()),
        }
    }
}

impl TryFrom<v2::ConnectionWelcome> for ConnectionWelcome {
    type Error = WireFieldError;

    fn try_from(value: v2::ConnectionWelcome) -> Result<Self, Self::Error> {
        let incarnation = decode_connection_incarnation(&value.incarnation)?;
        let generation = AuthGeneration::new(value.accepted_authorization_generation).ok_or(
            WireFieldError::InvalidGeneration {
                actual: value.accepted_authorization_generation,
            },
        )?;
        ConnectionWelcome::new(
            value.wire_major,
            Capabilities::from_bits_retain(value.capabilities),
            value.responder_display,
            value.responder_build,
            value.responder_platform,
            generation,
        )
        .map_err(WireFieldError::InvalidConnection)
        .map(|welcome| welcome.with_incarnation(incarnation))
    }
}

impl From<&ConnectionWelcome> for v2::ConnectionWelcome {
    fn from(value: &ConnectionWelcome) -> Self {
        Self {
            wire_major: value.wire_major(),
            capabilities: value.capabilities().bits(),
            responder_display: value.responder_display().to_owned(),
            responder_build: value.responder_build().to_owned(),
            responder_platform: value.responder_platform().to_owned(),
            accepted_authorization_generation: value.accepted_authorization_generation().get(),
            incarnation: value
                .incarnation()
                .map_or_else(Vec::new, |id| id.to_bytes().to_vec()),
        }
    }
}

fn decode_connection_incarnation(
    bytes: &[u8],
) -> Result<Option<voytty_core::ConnectionIncarnation>, WireFieldError> {
    if bytes.is_empty() {
        Ok(None)
    } else {
        voytty_core::ConnectionIncarnation::from_bytes(bytes)
            .map(Some)
            .map_err(WireFieldError::InvalidIdentifier)
    }
}

impl TryFrom<v2::LocalDeviceRenameRequest> for (DeviceId, DeviceAlias) {
    type Error = WireFieldError;

    fn try_from(value: v2::LocalDeviceRenameRequest) -> Result<Self, Self::Error> {
        let device_bytes = value.device_id.unwrap_or_default().value;
        let device_id =
            DeviceId::from_bytes(&device_bytes).map_err(WireFieldError::InvalidIdentifier)?;
        let alias = DeviceAlias::new(value.alias).map_err(WireFieldError::InvalidAlias)?;
        Ok((device_id, alias))
    }
}

impl TryFrom<v2::LocalDeviceRevokeRequest> for DeviceId {
    type Error = WireFieldError;

    fn try_from(value: v2::LocalDeviceRevokeRequest) -> Result<Self, Self::Error> {
        let device_bytes = value.device_id.unwrap_or_default().value;
        DeviceId::from_bytes(&device_bytes).map_err(WireFieldError::InvalidIdentifier)
    }
}

impl TryFrom<v2::DeviceSummary> for DeviceSummary {
    type Error = WireFieldError;

    fn try_from(value: v2::DeviceSummary) -> Result<Self, Self::Error> {
        let device_bytes = value.device_id.unwrap_or_default().value;
        let device_id =
            DeviceId::from_bytes(&device_bytes).map_err(WireFieldError::InvalidIdentifier)?;
        let generation =
            AuthGeneration::new(value.generation).ok_or(WireFieldError::InvalidGeneration {
                actual: value.generation,
            })?;
        let alias = if value.alias.is_empty() {
            None
        } else {
            Some(DeviceAlias::new(value.alias).map_err(WireFieldError::InvalidAlias)?)
        };
        let auth_status = match v2::DeviceAuthStatus::try_from(value.auth_status) {
            Ok(v2::DeviceAuthStatus::None) => AuthorizationStatus::None,
            Ok(v2::DeviceAuthStatus::Authorized) => AuthorizationStatus::Authorized,
            Ok(v2::DeviceAuthStatus::Revoked) => AuthorizationStatus::Revoked,
            Ok(v2::DeviceAuthStatus::Unspecified) | Err(_) => {
                return Err(WireFieldError::InvalidAuthStatus {
                    actual: value.auth_status,
                });
            }
        };
        DeviceSummary::new(
            device_id,
            value.outbound_known,
            alias,
            value.remote_name,
            value.route_verified,
            auth_status,
            generation,
            value.paired_at_unix,
            value.last_seen_at_unix,
            value.online,
            value.active_stream_count,
            value.remote_attachment_count,
        )
        .map_err(WireFieldError::InvalidSummary)
    }
}

impl From<&DeviceSummary> for v2::DeviceSummary {
    fn from(value: &DeviceSummary) -> Self {
        Self {
            device_id: Some(value.device_id().into()),
            outbound_known: value.outbound_known(),
            alias: value
                .alias()
                .map_or_else(String::new, |alias| alias.as_str().to_owned()),
            remote_name: value.remote_name().to_owned(),
            route_verified: value.route_verified(),
            auth_status: match value.auth_status() {
                AuthorizationStatus::None => v2::DeviceAuthStatus::None as i32,
                AuthorizationStatus::Authorized => v2::DeviceAuthStatus::Authorized as i32,
                AuthorizationStatus::Revoked => v2::DeviceAuthStatus::Revoked as i32,
            },
            generation: value.generation().get(),
            paired_at_unix: value.paired_at_unix(),
            last_seen_at_unix: value.last_seen_at_unix(),
            online: value.online(),
            active_stream_count: value.active_stream_count(),
            remote_attachment_count: value.remote_attachment_count(),
        }
    }
}

/// Failure while validating the shared local pairing operation identity.
#[derive(Debug)]
pub enum PairOperationError {
    /// The ephemeral operation ID had the wrong fixed byte width.
    InvalidOperationId(IdLengthError),
    /// The semantic fingerprint had the wrong fixed byte width.
    InvalidFingerprint(PairFingerprintError),
}

impl fmt::Display for PairOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOperationId(error) => error.fmt(formatter),
            Self::InvalidFingerprint(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for PairOperationError {}

/// Validates the shared local pairing operation identity before allocation.
///
/// Both local pair create and accept carry a client-generated 16-byte
/// ephemeral operation ID and a bounded semantic fingerprint. This is the
/// single boundary that converts the raw wire bytes into validated domain
/// values, so the service never allocates replay state for a malformed request.
pub fn validate_pair_operation(
    ephemeral_operation_id: &[u8],
    fingerprint: &[u8],
) -> Result<(EphemeralOperationId, PairFingerprint), PairOperationError> {
    let operation_id = EphemeralOperationId::from_bytes(ephemeral_operation_id)
        .map_err(PairOperationError::InvalidOperationId)?;
    let fingerprint =
        PairFingerprint::from_slice(fingerprint).map_err(PairOperationError::InvalidFingerprint)?;
    Ok((operation_id, fingerprint))
}
