//! Shared normal-ALPN greeting, Welcome validation and bounded handshake I/O.
use crate::{error::ClientError as DaemonError, protocol::protocol_error};
use iroh::{
    Endpoint, EndpointAddr, TransportAddr,
    endpoint::{Connection, ConnectionError},
};
use ring::rand::{SecureRandom, SystemRandom};
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zterm_core::{
    Capabilities, ConnectionAttemptId, ConnectionHello, ConnectionIncarnation, ConnectionWelcome,
    DeviceDisplayName, DeviceId, DomainErrorKind,
};
use zterm_diagnostics::{Event, Kind, Level, Operation, Outcome, Reason, Stage};
use zterm_proto::{DecodedFrame, FrameDecoder, WireKind, encode_message, v2};

/// Connects under the caller's unchanged deadline, distinguishing Iroh route
/// preparation from the cryptographic handshake in content-free diagnostics.
pub async fn connect_transport(
    endpoint: &Endpoint,
    address: EndpointAddr,
    alpn: &[u8],
    deadline: Instant,
) -> Result<Connection, DaemonError> {
    let operation = Operation::default();
    let mut stage = Stage::ResolvingTransport;
    let result: Result<Connection, DaemonError> = async {
        record_transport_stage(&operation, stage);
        let mut connecting = timeout_until(
            deadline,
            endpoint.connect_with_opts(address, alpn, Default::default()),
        )
        .await?
        .map_err(|_| transport_unavailable("Iroh route preparation failed"))?;
        stage = Stage::HandshakingTransport;
        record_transport_stage(&operation, stage);
        // ALPN availability marks receipt of TLS handshake data. It is not
        // authentication: still await the fully authenticated Connection below.
        timeout_until(deadline, connecting.alpn())
            .await?
            .map_err(|_| transport_unavailable("Iroh handshake data unavailable"))?;
        stage = Stage::AuthenticatingTransport;
        record_transport_stage(&operation, stage);
        let connection = timeout_until(deadline, connecting)
            .await?
            .map_err(|_| transport_unavailable("Iroh secure handshake failed"))?;
        record_transport_ready(&operation, &connection);
        Ok(connection)
    }
    .await;
    if let Err(error) = &result {
        zterm_diagnostics::record(
            Event::new(Kind::ConnectionFailed)
                .operation(&operation)
                .stage(stage)
                .level(Level::Warn)
                .outcome(Outcome::Failed)
                .error(error.kind()),
        );
    }
    result
}

/// Records selected path and counters only after complete transport authentication.
pub fn record_transport_ready(operation: &Operation, connection: &Connection) {
    if !zterm_diagnostics::detail_enabled() {
        return;
    }
    zterm_diagnostics::record(with_transport_state(
        Event::new(Kind::ConnectionStartup)
            .operation(operation)
            .stage(Stage::TransportReady)
            .level(Level::Debug),
        connection,
    ));
}

/// Observes an owner's cached connection at reuse or retirement. Retirement
/// describes local ownership, not acknowledgment of closure by the remote.
pub fn record_transport_observation(
    operation: &Operation,
    connection: &Connection,
    stage: Stage,
    active_streams: Option<u64>,
) {
    if !zterm_diagnostics::detail_enabled() {
        return;
    }
    let mut event = Event::new(Kind::TransportObserved)
        .operation(operation)
        .connection(operation.id())
        .stage(stage)
        .level(Level::Debug);
    if let Some(count) = active_streams {
        event = event.transport_streams(count);
    }
    zterm_diagnostics::record(with_transport_state(event, connection));
}

pub(crate) fn with_transport_state(event: Event, connection: &Connection) -> Event {
    let stats = connection.stats();
    let paths = connection.paths();
    let max_rtt = paths
        .iter()
        .map(|path| u64::try_from(path.rtt().as_millis()).unwrap_or(u64::MAX))
        .max();
    let mut event = event
        .transport_counters(stats.udp_tx.bytes, stats.udp_rx.bytes, stats.lost_packets)
        .transport_paths(paths.iter().count() as u64, max_rtt);
    if let Some(path) = paths.iter().find(|path| path.is_selected()) {
        let reason = match path.remote_addr() {
            TransportAddr::Ip(_) => Reason::Direct,
            TransportAddr::Relay(_) => Reason::Relay,
            _ => Reason::Unknown,
        };
        event = event
            .reason(reason)
            .transport_rtt(u64::try_from(path.rtt().as_millis()).unwrap_or(u64::MAX));
    }
    event
}

fn record_transport_stage(operation: &Operation, stage: Stage) {
    zterm_diagnostics::record(
        Event::new(Kind::ConnectionStartup)
            .operation(operation)
            .stage(stage)
            .level(Level::Debug),
    );
}

/// Existing normal-ALPN application close code for rejected/revoked authorization.
pub const CLOSE_UNAUTHORIZED: u32 = 0x100;

/// Stable local diagnostics placed in every normal connection handshake.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionIdentity {
    device_id: DeviceId,
    display_name: DeviceDisplayName,
    build: String,
    platform: String,
    capabilities: Capabilities,
    incarnation: ConnectionIncarnation,
}

impl ConnectionIdentity {
    /// Validates local display/build/platform fields once at composition time.
    pub fn new(
        device_id: DeviceId,
        display_name: impl Into<String>,
        build: impl Into<String>,
        platform: impl Into<String>,
        capabilities: Capabilities,
    ) -> Result<Self, DaemonError> {
        let display_name = DeviceDisplayName::new(display_name).map_err(|error| {
            DaemonError::new(
                DomainErrorKind::IdentityInvalid,
                format!("invalid local device display name: {error}"),
            )
        })?;
        let build = build.into();
        let platform = platform.into();
        // Reuse the domain handshake constructor as the single text boundary.
        ConnectionHello::new(
            zterm_proto::WIRE_MAJOR,
            zterm_proto::WIRE_MAJOR,
            capabilities,
            ConnectionAttemptId::from_array([1; 16]),
            display_name.as_str(),
            build.clone(),
            platform.clone(),
        )
        .map_err(|error| {
            DaemonError::new(
                DomainErrorKind::IdentityInvalid,
                format!("invalid local connection diagnostics: {error}"),
            )
        })?;
        let mut incarnation = [0_u8; 16];
        SystemRandom::new().fill(&mut incarnation).map_err(|_| {
            DaemonError::new(
                DomainErrorKind::IdentityInvalid,
                "connection incarnation entropy unavailable",
            )
        })?;
        Ok(Self {
            device_id,
            display_name,
            build,
            platform,
            capabilities,
            incarnation: ConnectionIncarnation::from_array(incarnation),
        })
    }

    /// Product-default local diagnostics.
    pub fn product(
        device_id: DeviceId,
        display_name: impl Into<String>,
    ) -> Result<Self, DaemonError> {
        Self::new(
            device_id,
            display_name,
            env!("CARGO_PKG_VERSION"),
            format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            Capabilities::from_bits_retain(
                Capabilities::LOCAL_LIFECYCLE
                    | Capabilities::SESSION_SERVICE
                    | Capabilities::TERMINAL_SERVICE,
            ),
        )
    }

    /// Adds services at the host composition boundary, without changing controllers.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Stable public device ID.
    #[must_use]
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Transient identity shared by candidates in this peer connection generation.
    #[must_use]
    pub const fn incarnation(&self) -> ConnectionIncarnation {
        self.incarnation
    }

    /// Binds the greeting to the owner's current live generation for this peer.
    /// A new generation is required when no previous connection can be reused.
    #[must_use]
    pub fn with_incarnation(mut self, incarnation: ConnectionIncarnation) -> Self {
        self.incarnation = incarnation;
        self
    }

    /// Validated local display name used by pairing and normal handshakes.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.display_name.as_str()
    }

    /// Stable product build string used only for peer diagnostics.
    #[must_use]
    pub fn build(&self) -> &str {
        &self.build
    }
    /// Declared semantic service capabilities.
    pub const fn capabilities(&self) -> Capabilities {
        self.capabilities
    }
    /// Redaction-safe platform diagnostic.
    pub fn platform(&self) -> &str {
        &self.platform
    }
    /// Builds the exact existing normal-ALPN greeting for this connection attempt.
    pub fn hello(&self, attempt: ConnectionAttemptId) -> Result<ConnectionHello, DaemonError> {
        ConnectionHello::new(
            zterm_proto::WIRE_MAJOR,
            zterm_proto::WIRE_MAJOR,
            self.capabilities,
            attempt,
            self.display_name.as_str(),
            self.build.clone(),
            self.platform.clone(),
        )
        .map_err(|error| {
            DaemonError::new(
                DomainErrorKind::IdentityInvalid,
                format!("local Hello became invalid: {error}"),
            )
        })
        .map(|hello| hello.with_incarnation(Some(self.incarnation)))
    }
}

/// Writes one zero-metadata connection handshake frame.
pub async fn write_handshake_message<M: prost::Message, Writer: AsyncWrite + Unpin>(
    send: &mut Writer,
    kind: WireKind,
    message: &M,
    deadline: Instant,
) -> Result<(), DaemonError> {
    let bytes = encode_message(kind, 0, 0, message).map_err(protocol_error)?;
    timeout_until(deadline, send.write_all(&bytes))
        .await?
        .map_err(|_| transport_unavailable("handshake stream write failed"))
}

/// Reads one bounded dispatch frame with the existing connection-handshake contract.
pub async fn read_one_frame<Reader>(
    recv: &mut Reader,
    maximum_body_bytes: usize,
    deadline: Instant,
) -> Result<DecodedFrame, DaemonError>
where
    Reader: AsyncRead + Unpin,
{
    timeout_until(deadline, async {
        let mut decoder = FrameDecoder::with_maximum_body_bytes(maximum_body_bytes);
        let mut buffer = [0_u8; 4096];
        loop {
            let read = AsyncReadExt::read(recv, &mut buffer)
                .await
                .map_err(|_| transport_unavailable("stream read failed"))?;
            if read == 0 {
                decoder.finish().map_err(protocol_error)?;
                return Err(DaemonError::new(
                    DomainErrorKind::MalformedFrame,
                    "stream ended before its first frame",
                ));
            }
            let frames = decoder.feed(&buffer[..read]).map_err(protocol_error)?;
            match frames.len() {
                0 => {}
                1 => {
                    return frames.into_iter().next().ok_or_else(|| {
                        DaemonError::new(
                            DomainErrorKind::MalformedFrame,
                            "decoder returned an inconsistent frame count",
                        )
                    });
                }
                _ => {
                    return Err(DaemonError::new(
                        DomainErrorKind::MalformedFrame,
                        "stream sent multiple frames before dispatch",
                    ));
                }
            }
        }
    })
    .await?
}

/// Validates the normal receiver's authorization proof and version selection.
pub async fn read_controller_welcome<Reader: AsyncRead + Unpin>(
    recv: &mut Reader,
    maximum_body_bytes: usize,
    deadline: Instant,
) -> Result<ConnectionWelcome, DaemonError> {
    let frame = read_one_frame(recv, maximum_body_bytes, deadline).await?;
    let wire: v2::ConnectionWelcome = frame
        .decode_message(WireKind::ConnectionWelcome)
        .map_err(protocol_error)?;
    let welcome = ConnectionWelcome::try_from(wire).map_err(|error| {
        DaemonError::new(
            DomainErrorKind::WireMajorMismatch,
            format!("invalid ConnectionWelcome: {error}"),
        )
    })?;
    if welcome.wire_major() != zterm_proto::WIRE_MAJOR {
        return Err(DaemonError::new(
            DomainErrorKind::WireMajorMismatch,
            "remote selected an incompatible wire major",
        ));
    }
    Ok(welcome)
}

/// Completes the common outbound Hello/Welcome exchange on an authenticated peer.
/// Admission, connection arbitration and successful-route persistence stay with
/// the adapter. Inspect peer closure before the caller retires its connection.
pub async fn controller_handshake(
    connection: &Connection,
    identity: &ConnectionIdentity,
    attempt: ConnectionAttemptId,
    maximum_body_bytes: usize,
    deadline: Instant,
) -> Result<ConnectionWelcome, DaemonError> {
    let operation = Operation::default();
    let mut stage = Stage::OpeningGreeting;
    let result = async {
        record_transport_stage(&operation, stage);
        let (mut send, mut recv) = timeout_until(deadline, connection.open_bi())
            .await?
            .map_err(|_| transport_unavailable("unable to open Hello stream"))?;
        let hello = identity.hello(attempt)?;
        stage = Stage::WritingGreeting;
        record_transport_stage(&operation, stage);
        write_handshake_message(
            &mut send,
            WireKind::ConnectionHello,
            &v2::ConnectionHello::from(&hello),
            deadline,
        )
        .await?;
        send.finish()
            .map_err(|_| transport_unavailable("unable to finish Hello stream"))?;
        stage = Stage::ReadingGreeting;
        record_transport_stage(&operation, stage);
        read_controller_welcome(&mut recv, maximum_body_bytes, deadline).await
    }
    .await;
    result.map_err(|error| {
        let error = classify_handshake_failure(error, connection.close_reason());
        let stats = connection.stats();
        zterm_diagnostics::record(
            Event::new(Kind::ConnectionFailed)
                .operation(&operation)
                .stage(stage)
                .level(Level::Warn)
                .outcome(Outcome::Failed)
                .error(error.kind())
                .transport_counters(stats.udp_tx.bytes, stats.udp_rx.bytes, stats.lost_packets),
        );
        error
    })
}

fn classify_handshake_failure(error: DaemonError, closed: Option<ConnectionError>) -> DaemonError {
    if matches!(closed, Some(ConnectionError::ApplicationClosed(ref close))
        if close.error_code == CLOSE_UNAUTHORIZED.into())
    {
        DaemonError::new(
            DomainErrorKind::Unauthorized,
            "remote rejected normal connection authorization",
        )
    } else {
        error
    }
}

async fn timeout_until<F>(deadline: Instant, future: F) -> Result<F::Output, DaemonError>
where
    F: std::future::Future,
{
    if Instant::now() >= deadline {
        return Err(deadline_exceeded());
    }
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), future)
        .await
        .map_err(|_| deadline_exceeded())
}
fn deadline_exceeded() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::DeadlineExceeded,
        "transport operation deadline elapsed",
    )
}
fn transport_unavailable(detail: &'static str) -> DaemonError {
    DaemonError::new(DomainErrorKind::TransportUnavailable, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_peer_authorization_close_is_unauthorized() {
        let close = |code: u32| {
            ConnectionError::ApplicationClosed(iroh::endpoint::ApplicationClose {
                error_code: code.into(),
                // Text is deliberately identical; classification must use the code.
                reason: b"not authorized".to_vec().into(),
            })
        };
        let failure = || transport_unavailable("handshake read failed");
        assert_eq!(
            classify_handshake_failure(failure(), Some(close(CLOSE_UNAUTHORIZED))).kind(),
            DomainErrorKind::Unauthorized
        );
        for closed in [
            None,
            Some(ConnectionError::Reset),
            Some(ConnectionError::TimedOut),
            Some(close(0x102)),
            Some(close(0x103)),
        ] {
            assert_eq!(
                classify_handshake_failure(failure(), closed).kind(),
                DomainErrorKind::TransportUnavailable
            );
        }
    }

    #[tokio::test]
    async fn incomplete_welcome_remains_a_framing_error() {
        let mut partial = encode_message(
            WireKind::ConnectionWelcome,
            0,
            0,
            &v2::ConnectionWelcome::default(),
        )
        .expect("frame encoding");
        partial.pop();
        for bytes in [&[][..], partial.as_slice()] {
            let mut reader = bytes;
            let error = read_controller_welcome(
                &mut reader,
                1024,
                Instant::now() + std::time::Duration::from_secs(1),
            )
            .await
            .expect_err("no complete Welcome");
            assert_eq!(error.kind(), DomainErrorKind::MalformedFrame);
        }
    }

    #[tokio::test]
    async fn complete_welcome_does_not_require_transport_eof() {
        let welcome = ConnectionWelcome::new(
            zterm_proto::WIRE_MAJOR,
            Capabilities::from_bits_retain(0),
            "peer",
            "test",
            "test",
            zterm_core::AuthGeneration::new(1).expect("generation"),
        )
        .expect("welcome");
        let bytes = encode_message(
            WireKind::ConnectionWelcome,
            0,
            0,
            &v2::ConnectionWelcome::from(&welcome),
        )
        .expect("frame");
        let (mut reader, mut writer) = tokio::io::duplex(4096);
        writer
            .write_all(&bytes)
            .await
            .expect("write Welcome without FIN");
        assert_eq!(
            read_controller_welcome(
                &mut reader,
                4096,
                Instant::now() + std::time::Duration::from_millis(100)
            )
            .await
            .expect("complete framed reply"),
            welcome
        );
        drop(writer);
    }

    #[tokio::test]
    async fn welcome_deadline_does_not_claim_authorization_rejection() {
        let (mut reader, _peer) = tokio::io::duplex(64);
        let error = read_controller_welcome(&mut reader, 1024, Instant::now())
            .await
            .expect_err("deadline expires before Welcome");
        assert_eq!(error.kind(), DomainErrorKind::DeadlineExceeded);
    }

    #[tokio::test]
    async fn welcome_transport_failure_does_not_claim_authorization_rejection() {
        struct BrokenReader;
        impl AsyncRead for BrokenReader {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Err(std::io::ErrorKind::ConnectionReset.into()))
            }
        }
        let error = read_controller_welcome(
            &mut BrokenReader,
            1024,
            Instant::now() + std::time::Duration::from_secs(1),
        )
        .await
        .expect_err("transport failed before Welcome");
        assert_eq!(error.kind(), DomainErrorKind::TransportUnavailable);
    }
}
