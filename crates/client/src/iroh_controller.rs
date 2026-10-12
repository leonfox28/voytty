//! Outbound-only Iroh controller used by the mobile application runtime.
use crate::{
    error::ClientError,
    handshake::{
        ConnectionIdentity, connect_transport, controller_handshake, record_transport_observation,
    },
    model::ResolvedSessionTarget,
    pairing::{
        ConfirmedAuthorization, ControllerPairAttempt, ControllerPairIdentity, PairProtocolIo,
        resolve_normal_confirmation, run_controller_pair,
    },
    protocol::{
        attachment_cancelled, attachment_command_stream_closed, malformed, protocol_error,
        resource_error,
    },
    remote_unary::{
        RemoteAttemptError, RemoteUnaryClient, RemoteUnaryDemand, RemoteUnaryTransport,
        RequestAcknowledgement, decode_session_service_error, read_acknowledged_response,
        timeout_until,
    },
    route::{
        combined_relay_address, device_from_endpoint_id, endpoint_id_from_device,
        resolve_relay_candidates, selected_relay,
    },
    transport::{
        AttachmentConnector, AttachmentIo, AttachmentTransport, AttachmentTransportItem,
        TransportFuture,
    },
    unary::{SessionUnaryRequest, SessionUnaryTransport},
};
use iroh::{
    Endpoint, RelayMode, SecretKey, TransportAddr,
    endpoint::{Connection, RecvStream, SendStream, VarInt, presets},
};
use ring::rand::{SecureRandom, SystemRandom};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    pin::Pin,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{Mutex, OwnedSemaphorePermit, Semaphore},
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;
use zterm_core::{
    AuthGeneration, Capabilities, ConnectionAttemptId, ConnectionIncarnation, DeviceId,
    DomainErrorKind, PairNonce, RelayHint, TransportLimits, connection_progress::ConnectionStage,
};
use zterm_proto::{DecodedFrame, FrameDecoder, WireKind, v2};

/// Existing normal service protocol; incoming host services are not exposed.
pub const ZTERM_ALPN: &[u8] = b"zterm/2";
/// Existing one-time pairing protocol.
pub const ZTERM_PAIR_ALPN: &[u8] = b"zterm-pair/2";

#[cfg(test)]
mod cache_tests;
#[cfg(test)]
mod progress_tests;
#[cfg(test)]
mod restart_tests;

/// Builds the official n0 profile with an explicit platform DNS adapter.
/// Publication remains relay-only; authenticated connections can upgrade to direct paths.
/// The application owns exactly one live endpoint per identity, including shutdown.
pub fn endpoint_builder(secret: SecretKey, dns: iroh::dns::DnsResolver) -> iroh::endpoint::Builder {
    use iroh::address_lookup::{
        AddrFilter, DnsAddressLookup, N0_DNS_ENDPOINT_ORIGIN_PROD, N0_DNS_PKARR_RELAY_PROD,
        PkarrPublisher, PkarrResolver,
    };
    let relay: iroh::RelayUrl = N0_DNS_PKARR_RELAY_PROD
        .parse()
        .expect("pinned n0 production URL");
    Endpoint::builder(presets::Minimal)
        .secret_key(secret)
        .persistent_stateless_reset(true)
        .dns_resolver(dns)
        .relay_mode(RelayMode::Default)
        .address_lookup(PkarrPublisher::builder(relay.clone().into()))
        .address_lookup(PkarrResolver::builder(relay.into()))
        .address_lookup(DnsAddressLookup::builder(
            N0_DNS_ENDPOINT_ORIGIN_PROD.to_owned(),
        ))
        .addr_filter(AddrFilter::relay_only())
        .alpns(Vec::new())
}

#[derive(Clone)]
struct NormalConnection {
    diagnostic: zterm_diagnostics::Operation,
    capabilities: zterm_core::Capabilities,
    connection: Connection,
    generation: AuthGeneration,
    verified_relay: Option<RelayHint>,
}
struct Peer {
    routes: StdMutex<Vec<RelayHint>>,
    normal: Mutex<Option<NormalConnection>>,
    streams: Arc<Semaphore>,
    cancelled: CancellationToken,
}
struct Inner {
    endpoint: Endpoint,
    identity: ConnectionIdentity,
    limits: TransportLimits,
    peers: StdMutex<BTreeMap<DeviceId, Arc<Peer>>>,
    pair_targets: StdMutex<BTreeSet<DeviceId>>,
    dials: Semaphore,
    pairs: Semaphore,
    request_id: AtomicU64,
}

/// Address-free counters for connection health and reproducible acceptance evidence.
#[derive(Clone, Debug)]
pub struct ControllerConnectionInfo {
    /// Selected path class, unknown/direct/relay.
    pub path: &'static str,
    /// Rounded selected path RTT.
    pub rtt_ms: u64,
    /// Transmitted QUIC bytes.
    pub sent_bytes: u64,
    /// Received QUIC bytes.
    pub received_bytes: u64,
    /// Lost packet count.
    pub lost_packets: u64,
    /// Whether the transport has reported closure.
    pub closed: bool,
}

/// One application-owned endpoint and bounded, reused authenticated connections.
#[derive(Clone)]
pub struct IrohController {
    inner: Arc<Inner>,
}

/// Provisional paired identity; commit only after its host record is durably stored.
pub struct PairedHost {
    device_id: DeviceId,
    name: String,
    generation: AuthGeneration,
    verified_relay: Option<RelayHint>,
    peer: Arc<Peer>,
}
impl PairedHost {
    /// TLS-authenticated host public identity.
    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }
    /// Validated host diagnostic name from the ticket.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Normal Welcome's receiver-owned authorization generation.
    pub fn generation(&self) -> AuthGeneration {
        self.generation
    }
    /// Authenticated route eligible for durable caching.
    pub fn verified_relay(&self) -> Option<&RelayHint> {
        self.verified_relay.as_ref()
    }
}
impl std::fmt::Debug for PairedHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairedHost")
            .field("device_id", &self.device_id)
            .field("name_len", &self.name.len())
            .field("generation", &self.generation)
            .field("route_verified", &self.verified_relay.is_some())
            .finish()
    }
}
impl Peer {
    fn new(routes: Vec<RelayHint>, limits: TransportLimits) -> Self {
        Self {
            routes: StdMutex::new(routes),
            normal: Mutex::new(None),
            streams: Arc::new(Semaphore::new(limits.max_bi_streams_per_connection)),
            cancelled: CancellationToken::new(),
        }
    }
    fn retire(&self) {
        self.cancelled.cancel();
        if let Ok(mut normal) = self.normal.try_lock()
            && let Some(normal) = normal.take()
        {
            normal
                .connection
                .close(VarInt::from_u32(0), b"local host retired");
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.cancelled.cancel();
        if let Some(normal) = self.normal.get_mut().take() {
            normal
                .connection
                .close(VarInt::from_u32(0), b"controller released");
        }
    }
}
struct PairTargetLease {
    inner: Arc<Inner>,
    remote: DeviceId,
}
impl Drop for PairTargetLease {
    fn drop(&mut self) {
        self.inner
            .pair_targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.remote);
    }
}
impl IrohController {
    /// Composes an already bound platform endpoint without creating host services.
    pub fn new(endpoint: Endpoint, display_name: &str) -> Result<Self, ClientError> {
        let limits = TransportLimits::default();
        let identity = ConnectionIdentity::new(
            device_from_endpoint_id(endpoint.id()),
            display_name,
            env!("CARGO_PKG_VERSION"),
            format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            Capabilities::from_bits_retain(
                Capabilities::SESSION_SERVICE | Capabilities::TERMINAL_SERVICE,
            ),
        )?;
        Ok(Self {
            inner: Arc::new(Inner {
                endpoint,
                identity,
                limits,
                peers: StdMutex::new(BTreeMap::new()),
                pair_targets: StdMutex::new(BTreeSet::new()),
                dials: Semaphore::new(limits.max_pending_dials),
                pairs: Semaphore::new(limits.max_pairing_handshakes),
                request_id: AtomicU64::new(1),
            }),
        })
    }
    /// Reconciles an OS-reported network change without replacing healthy Sessions.
    pub async fn network_change(&self) {
        self.inner.endpoint.network_change().await;
    }
    /// Public identity of the durable local key used by this endpoint.
    pub fn device_id(&self) -> DeviceId {
        self.inner.identity.device_id()
    }
    /// Closes this controller's endpoint while its network executor is still alive.
    /// Explicit runtime teardown only; foreground/background changes retain it.
    pub async fn shutdown(&self) {
        self.inner.endpoint.close().await;
    }
    /// Observes a retained connection without creating one or probing with input.
    pub async fn connection_info(
        &self,
        remote: DeviceId,
    ) -> Result<Option<ControllerConnectionInfo>, ClientError> {
        let peer = self.peer(remote, None)?;
        let guard = peer.normal.lock().await;
        Ok(guard.as_ref().map(|normal| {
            let stats = normal.connection.stats();
            let (path, rtt_ms) = normal
                .connection
                .paths()
                .iter()
                .find(|path| path.is_selected())
                .map_or(("unknown", 0), |path| {
                    (
                        match path.remote_addr() {
                            TransportAddr::Ip(_) => "direct",
                            TransportAddr::Relay(_) => "relay",
                            _ => "unknown",
                        },
                        u64::try_from(path.rtt().as_millis()).unwrap_or(u64::MAX),
                    )
                });
            ControllerConnectionInfo {
                path,
                rtt_ms,
                sent_bytes: stats.udp_tx.bytes,
                received_bytes: stats.udp_rx.bytes,
                lost_packets: stats.lost_packets,
                closed: normal.connection.close_reason().is_some(),
            }
        }))
    }
    /// Registers a durably known host and its previously authenticated routes.
    pub fn remember_host(
        &self,
        remote: DeviceId,
        routes: Vec<RelayHint>,
    ) -> Result<(), ClientError> {
        self.peer(remote, Some(routes)).map(|_| ())
    }
    /// Publishes a durably saved pair result and reuses its confirmed normal connection.
    pub fn commit_pair(&self, paired: PairedHost) -> Result<(), ClientError> {
        let mut peers = self
            .inner
            .peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !peers.contains_key(&paired.device_id)
            && peers.len() >= self.inner.limits.max_remote_connections
        {
            return Err(resource_error("known connection capacity exhausted"));
        }
        if let Some(old) = peers.insert(paired.device_id, paired.peer) {
            old.retire();
        }
        Ok(())
    }
    /// Retires only this host's local connection state after durable local removal.
    pub async fn forget_host(&self, remote: DeviceId) {
        let peer = self
            .inner
            .peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&remote);
        if let Some(peer) = peer {
            peer.cancelled.cancel();
            if let Some(normal) = peer.normal.lock().await.take() {
                normal
                    .connection
                    .close(VarInt::from_u32(0), b"local host removed");
            }
        }
    }
    fn peer(
        &self,
        remote: DeviceId,
        routes: Option<Vec<RelayHint>>,
    ) -> Result<Arc<Peer>, ClientError> {
        endpoint_id_from_device(remote)?;
        if remote == self.device_id() {
            return Err(ClientError::new(
                DomainErrorKind::PairTicketInvalid,
                "a device cannot pair with itself",
            ));
        }
        if routes
            .as_ref()
            .is_some_and(|routes| routes.len() > self.inner.limits.max_relay_hints)
        {
            return Err(resource_error("host route bound exceeded"));
        }
        let mut peers = self
            .inner
            .peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(peer) = peers.get(&remote) {
            if let Some(routes) = routes {
                *peer
                    .routes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = routes;
            }
            return Ok(Arc::clone(peer));
        }
        let routes = routes.ok_or_else(|| {
            ClientError::new(
                DomainErrorKind::Unauthorized,
                "host is not in the local address book",
            )
        })?;
        if peers.len() >= self.inner.limits.max_remote_connections {
            return Err(resource_error("known connection capacity exhausted"));
        }
        let peer = Arc::new(Peer::new(routes, self.inner.limits));
        peers.insert(remote, Arc::clone(&peer));
        Ok(peer)
    }
    async fn dial(
        &self,
        remote: DeviceId,
        cache: Vec<RelayHint>,
        ticket: Vec<RelayHint>,
        alpn: &[u8],
        deadline: Instant,
    ) -> Result<(Connection, Option<RelayHint>), ClientError> {
        let operation = zterm_diagnostics::Operation::default();
        let _permit = timeout_until(deadline, self.inner.dials.acquire())
            .await?
            .map_err(|_| attachment_cancelled())?;
        let endpoint_id = endpoint_id_from_device(remote)?;
        record_connection_stage(&operation, ConnectionStage::LookingUpAddress);
        let routes = resolve_relay_candidates(
            self.inner.limits,
            &self.inner.endpoint,
            endpoint_id,
            cache,
            ticket,
            deadline,
        )
        .await?;
        let address = combined_relay_address(endpoint_id, &routes)?;
        // Normal startup already has one caller-owned deadline spanning route
        // lookup, transport and Hello/Welcome. A shorter pairing-attempt cap
        // must not discard a slow normal connection before that deadline.
        let until = if alpn == ZTERM_ALPN {
            deadline
        } else {
            deadline.min(Instant::now() + self.inner.limits.connect_attempt_budget)
        };
        record_connection_stage(&operation, ConnectionStage::ConnectingSecurely);
        let connection = connect_transport(&self.inner.endpoint, address, alpn, until).await?;
        if device_from_endpoint_id(connection.remote_id()) != remote {
            connection.close(VarInt::from_u32(0x101), b"identity mismatch");
            return Err(ClientError::new(
                DomainErrorKind::Unauthorized,
                "Iroh identity does not match the exact target",
            ));
        }
        // The mobile controller accepts no unsolicited host service streams.
        connection.set_max_concurrent_bi_streams(VarInt::from_u32(0));
        connection.set_max_concurrent_uni_streams(VarInt::from_u32(0));
        record_connection_stage(&operation, ConnectionStage::SecureConnectionReady);
        let relay = selected_relay(&connection);
        Ok((connection, relay))
    }
    async fn normal(
        &self,
        remote: DeviceId,
        peer: &Arc<Peer>,
        deadline: Instant,
    ) -> Result<NormalConnection, ClientError> {
        tokio::select! {
            _ = peer.cancelled.cancelled() => Err(attachment_cancelled()),
            result = timeout_until(deadline, async {
                let mut slot = peer.normal.lock().await;
                if let Some(normal) = slot.as_ref().filter(|normal| normal.connection.close_reason().is_none()) {
                    record_transport_observation(&normal.diagnostic, &normal.connection,
                        zterm_diagnostics::Stage::ReusingTransport, None);
                    return Ok(normal.clone());
                }
                if let Some(normal) = slot.take() {
                    record_transport_observation(&normal.diagnostic, &normal.connection,
                        zterm_diagnostics::Stage::RetiringTransport, None);
                }
                let routes = peer.routes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
                let (connection, relay) = self.dial(remote, routes, Vec::new(), ZTERM_ALPN, deadline).await?;
                let mut guard = UnconfirmedConnection(Some(connection));
                let connection = guard.0.as_ref().expect("connection guard is populated");
                let operation = zterm_diagnostics::Operation::default();
                record_connection_stage(&operation, ConnectionStage::CheckingProtocolAndAccess);
                // This outbound-only owner retains no usable connection here.
                // Fence any still-live receiver-side primary from the previous
                // generation, including a one-sided network timeout.
                let identity = self.inner.identity.clone().with_incarnation(
                    ConnectionIncarnation::from_array(random_bytes()?));
                let welcome = controller_handshake(connection, &identity,
                    ConnectionAttemptId::from_array(random_bytes()?),
                    self.inner.limits.max_pair_hello_frame_bytes, deadline).await?;
                record_connection_stage(&operation, ConnectionStage::ProtocolAndAccessReady);
                let normal = NormalConnection { diagnostic: operation, capabilities: welcome.capabilities(), connection: guard.0.take().expect("validated connection remains present"), generation: welcome.accepted_authorization_generation(), verified_relay: relay };
                *slot = Some(normal.clone());
                Ok(normal)
            }) => result?,
        }
    }
    async fn service_stream(
        &self,
        remote: DeviceId,
        peer: &Arc<Peer>,
        deadline: Instant,
    ) -> Result<IrohSessionIo, ClientError> {
        let (normal, permit, send, recv) = self.admitted_stream(remote, peer, deadline).await?;
        Ok(IrohSessionIo {
            diagnostic: normal.diagnostic,
            connection: normal.connection,
            last_stream_sample: None,
            read_bytes: 0,
            decoded_frames: 0,
            last_path: None,
            send,
            recv,
            decoder: FrameDecoder::new(),
            queued: VecDeque::new(),
            _permit: permit,
        })
    }

    fn retire_idle_connection(&self, peer: &Peer, failed: usize) {
        let Ok(maximum) = u32::try_from(self.inner.limits.max_bi_streams_per_connection) else {
            return;
        };
        // Own every admission permit while removing the cache entry. Merely
        // reading available_permits would race another stream's admission.
        // Cleanup must not wait beyond the failed request's deadline.
        let Ok(_idle) = Arc::clone(&peer.streams).try_acquire_many_owned(maximum) else {
            return;
        };
        let Ok(mut slot) = peer.normal.try_lock() else {
            return;
        };
        if slot
            .as_ref()
            .is_some_and(|normal| normal.connection.stable_id() == failed)
            && let Some(normal) = slot.take()
        {
            record_transport_observation(
                &normal.diagnostic,
                &normal.connection,
                zterm_diagnostics::Stage::RetiringTransport,
                Some(0),
            );
            normal
                .connection
                .close(VarInt::from_u32(0), b"idle request transport failed");
        }
    }

    async fn admitted_stream(
        &self,
        remote: DeviceId,
        peer: &Arc<Peer>,
        deadline: Instant,
    ) -> Result<
        (
            NormalConnection,
            OwnedSemaphorePermit,
            SendStream,
            RecvStream,
        ),
        ClientError,
    > {
        loop {
            let normal = self.normal(remote, peer, deadline).await?;
            let permit = timeout_until(deadline, Arc::clone(&peer.streams).acquire_owned())
                .await?
                .map_err(|_| attachment_cancelled())?;
            if peer.cancelled.is_cancelled() {
                return Err(attachment_cancelled());
            }
            // An idle failed connection may have been retired while this
            // opener waited for admission. No application bytes were sent.
            if normal.connection.close_reason().is_some() {
                continue;
            }
            let Ok((send, recv)) = timeout_until(deadline, normal.connection.open_bi()).await?
            else {
                continue;
            };
            if peer.cancelled.is_cancelled() {
                return Err(attachment_cancelled());
            }
            return Ok((normal, permit, send, recv));
        }
    }
    /// Pairs a validated one-time ticket and resolves ambiguity on the normal ALPN.
    /// The caller must persist the returned host before reporting success to the UI.
    pub async fn pair_ticket(&self, text: &str) -> Result<PairedHost, ClientError> {
        let (fields, secret) = zterm_proto::decode_pair_ticket(text).map_err(|_| {
            ClientError::new(DomainErrorKind::PairTicketInvalid, "invalid pairing ticket")
        })?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| unavailable("pairing clock unavailable"))?
            .as_secs();
        if fields.is_expired(now) {
            return Err(ClientError::new(
                DomainErrorKind::PairTicketExpired,
                "pairing ticket expired",
            ));
        }
        let deadline = Instant::now() + self.inner.limits.pairing_total_deadline;
        let _permit = timeout_until(deadline, self.inner.pairs.acquire())
            .await?
            .map_err(|_| attachment_cancelled())?;
        let remote = fields.host_device_id();
        let routes = fields.relay_hints().to_vec();
        endpoint_id_from_device(remote)?;
        if remote == self.device_id() {
            return Err(ClientError::new(
                DomainErrorKind::PairTicketInvalid,
                "a device cannot pair with itself",
            ));
        }
        if !self
            .inner
            .pair_targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(remote)
        {
            return Err(resource_error(
                "pairing with this host is already in progress",
            ));
        }
        let _target_lease = PairTargetLease {
            inner: Arc::clone(&self.inner),
            remote,
        };
        let peer = Arc::new(Peer::new(routes.clone(), self.inner.limits));
        let attempt = match self
            .dial(remote, Vec::new(), routes, ZTERM_PAIR_ALPN, deadline)
            .await
        {
            Err(error) => ControllerPairAttempt::failed(error, false),
            Ok((connection, _)) => {
                let result = timeout_until(deadline, connection.open_bi()).await;
                match result {
                    Ok(Ok((send, recv))) => {
                        run_controller_pair(
                            Box::new(IrohPairIo {
                                local: self.device_id(),
                                remote,
                                send,
                                recv,
                                connection,
                            }),
                            &fields,
                            &secret,
                            ControllerPairIdentity {
                                device_id: self.device_id(),
                                display_name: self.inner.identity.display_name(),
                            },
                            &self.inner.limits,
                            deadline,
                            || random_bytes().map(PairNonce::from_array),
                        )
                        .await
                    }
                    Ok(Err(_)) => ControllerPairAttempt::failed(
                        unavailable("unable to open pairing stream"),
                        false,
                    ),
                    Err(error) => ControllerPairAttempt::failed(error, false),
                }
            }
        };
        let confirmation =
            self.normal(remote, &peer, deadline)
                .await
                .map(|normal| ConfirmedAuthorization {
                    remote,
                    generation: normal.generation,
                    verified_relay: normal.verified_relay,
                });
        let confirmation = resolve_normal_confirmation(remote, attempt, confirmation)?;
        Ok(PairedHost {
            device_id: remote,
            name: fields.host_name().to_owned(),
            generation: confirmation.generation,
            verified_relay: confirmation.verified_relay,
            peer,
        })
    }
}
struct UnconfirmedConnection(Option<Connection>);
impl Drop for UnconfirmedConnection {
    fn drop(&mut self) {
        if let Some(connection) = &self.0 {
            connection.close(VarInt::from_u32(0x101), b"handshake incomplete");
        }
    }
}

impl AttachmentConnector for IrohController {
    fn open(
        &self,
        target: ResolvedSessionTarget,
    ) -> TransportFuture<'_, Result<AttachmentTransport, ClientError>> {
        Box::pin(async move {
            let remote = require_remote(target)?;
            let peer = self.peer(remote, None)?;
            let stream = self
                .service_stream(
                    remote,
                    &peer,
                    Instant::now() + crate::protocol::REMOTE_STARTUP_DEADLINE,
                )
                .await?;
            Ok(AttachmentTransport::new(stream))
        })
    }
}
impl SessionUnaryTransport for IrohController {
    fn request(
        &self,
        request: SessionUnaryRequest,
    ) -> TransportFuture<'_, Result<DecodedFrame, ClientError>> {
        Box::pin(async move {
            let remote = require_remote(request.target)?;
            let request_id = self
                .inner
                .request_id
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_add(1)
                })
                .map_err(|_| resource_error("request ID exhausted"))?;
            let bytes = Zeroizing::new(
                zterm_proto::encode_payload(
                    request.request_kind,
                    request_id,
                    u32::try_from(request.deadline.as_millis()).unwrap_or(u32::MAX),
                    request.payload,
                )
                .map_err(protocol_error)?,
            );
            RemoteUnaryClient::new(Arc::new(self.clone()))
                .execute_preencoded(
                    remote,
                    request_id,
                    &bytes,
                    Instant::now() + request.startup_deadline,
                )
                .await
                .and_then(session_unary_result)
        })
    }
}
struct IrohDemand {
    controller: IrohController,
    remote: DeviceId,
    peer: Arc<Peer>,
}
// RemoteUnaryClient retains a validated ServiceError frame for forwarding
// adapters. A frontend unary consumer instead needs the typed error, including
// OutcomeUnknown so its old-daemon operation lease can be retired.
fn session_unary_result(frame: DecodedFrame) -> Result<DecodedFrame, ClientError> {
    if frame.kind == WireKind::ServiceErrorResponse {
        Err(decode_session_service_error(&frame)?)
    } else {
        Ok(frame)
    }
}
impl RemoteUnaryTransport for IrohController {
    fn demand<'a>(
        &'a self,
        remote: DeviceId,
        _deadline: Instant,
    ) -> TransportFuture<'a, Result<Box<dyn RemoteUnaryDemand>, ClientError>> {
        Box::pin(async move {
            Ok(Box::new(IrohDemand {
                controller: self.clone(),
                remote,
                peer: self.peer(remote, None)?,
            }) as Box<dyn RemoteUnaryDemand>)
        })
    }
}
impl RemoteUnaryDemand for IrohDemand {
    fn prepare(&mut self, deadline: Instant) -> TransportFuture<'_, Result<(), ClientError>> {
        Box::pin(async move {
            self.controller
                .normal(self.remote, &self.peer, deadline)
                .await?;
            Ok(())
        })
    }

    fn exchange<'a>(
        &'a mut self,
        request: &'a [u8],
        deadline: Instant,
        retry_follows: bool,
    ) -> TransportFuture<'a, Result<DecodedFrame, RemoteAttemptError>> {
        Box::pin(async move {
            let mut stream = self
                .controller
                .service_stream(self.remote, &self.peer, deadline)
                .await
                .map_err(RemoteAttemptError::PreWrite)?;
            let connection = stream.connection.stable_id();
            let acknowledgement = retry_follows
                .then(|| RequestAcknowledgement::before_write(&stream.connection, deadline));
            let result = async {
                timeout_until(deadline, stream.send.write_all(request))
                    .await
                    .map_err(RemoteAttemptError::PostWrite)?
                    .map_err(|_| {
                        RemoteAttemptError::PostWrite(unavailable("Session request write failed"))
                    })?;
                stream.send.finish().map_err(|_| {
                    RemoteAttemptError::PostWrite(unavailable("Session request finish failed"))
                })?;
                read_acknowledged_response(&mut stream.recv, deadline, acknowledgement).await
            }
            .await;
            drop(stream);
            if result
                .as_ref()
                .is_err_and(RemoteAttemptError::is_transport_failure_after_write)
            {
                self.controller
                    .retire_idle_connection(&self.peer, connection);
            }
            result
        })
    }
}
struct IrohSessionIo {
    diagnostic: zterm_diagnostics::Operation,
    connection: Connection,
    last_stream_sample: Option<Instant>,
    read_bytes: u64,
    decoded_frames: u64,
    last_path: Option<(i32, Option<u32>)>,
    send: SendStream,
    recv: RecvStream,
    decoder: FrameDecoder,
    queued: VecDeque<DecodedFrame>,
    _permit: OwnedSemaphorePermit,
}
impl IrohSessionIo {
    fn stream_progress(&self) -> zterm_diagnostics::StreamProgress {
        zterm_diagnostics::StreamProgress {
            stream_id: self.recv.id().into(),
            // QUIC may reclaim receive state at EOF/reset. Preserve the bytes
            // actually consumed by this adapter instead of reporting zero.
            read_bytes: self.read_bytes,
            decoded_frames: self.decoded_frames,
            queued_frames: self.queued.len() as u64,
            buffered_bytes: self.decoder.buffered_len() as u64,
            connection_stream_frames: self.connection.stats().frame_rx.stream,
        }
    }

    fn record_stream_progress(&mut self) {
        if !zterm_diagnostics::detail_enabled() {
            self.last_stream_sample = None;
            return;
        }
        let now = Instant::now();
        if self.last_stream_sample.is_some_and(|last| {
            now.saturating_duration_since(last) < std::time::Duration::from_secs(1)
        }) {
            return;
        }
        self.last_stream_sample = Some(now);
        // Sample only while the existing reader is polled. Its one-second quiet
        // read wakeup also samples a stall; no task, probe or deadline is added.
        zterm_diagnostics::record(crate::handshake::with_transport_state(
            zterm_diagnostics::Event::new(zterm_diagnostics::Kind::StreamObserved)
                .level(zterm_diagnostics::Level::Debug)
                .connection(self.diagnostic.id())
                .operation(&self.diagnostic)
                .stream_progress(self.stream_progress()),
            &self.connection,
        ));
    }
}
impl crate::upload::UploadConnector for IrohController {
    fn open_upload(
        &self,
        target: ResolvedSessionTarget,
    ) -> TransportFuture<'_, Result<crate::upload::UploadConnection, ClientError>> {
        Box::pin(async move {
            let remote = require_remote(target)?;
            let peer = self.peer(remote, None)?;
            let (normal, permit, send, recv) = self
                .admitted_stream(
                    remote,
                    &peer,
                    Instant::now() + crate::protocol::DEFAULT_DEADLINE,
                )
                .await?;
            Ok(crate::upload::UploadConnection {
                capabilities: normal.capabilities,
                reader: Box::new(crate::upload::FramedUploadReader::new(
                    recv,
                    (normal.connection, permit),
                )),
                writer: Box::new(crate::upload::AsyncUploadWriter(send)),
            })
        })
    }
}
impl AttachmentIo for IrohSessionIo {
    fn queued_session_count(&self) -> usize {
        self.queued.len()
    }
    fn write<'a>(&'a mut self, bytes: &'a [u8]) -> TransportFuture<'a, Result<(), ClientError>> {
        Box::pin(async move {
            tokio::io::AsyncWriteExt::write_all(&mut self.send, bytes)
                .await
                .map_err(|error| {
                    if matches!(
                        error.kind(),
                        io::ErrorKind::BrokenPipe
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::NotConnected
                    ) {
                        attachment_command_stream_closed()
                    } else {
                        unavailable("Session input transport failed")
                    }
                })
        })
    }
    fn read(&mut self) -> TransportFuture<'_, Result<AttachmentTransportItem, ClientError>> {
        Box::pin(async move {
            loop {
                self.record_stream_progress();
                let sample = self
                    .connection
                    .paths()
                    .iter()
                    .find(|path| path.is_selected())
                    .map_or((v2::TerminalConnectionPath::Unknown as i32, None), |path| {
                        let kind = match path.remote_addr() {
                            TransportAddr::Ip(_) => v2::TerminalConnectionPath::Direct,
                            TransportAddr::Relay(_) => v2::TerminalConnectionPath::Relay,
                            _ => v2::TerminalConnectionPath::Unknown,
                        };
                        (
                            kind as i32,
                            Some(
                                u32::try_from((path.rtt().as_nanos() + 500_000) / 1_000_000)
                                    .unwrap_or(u32::MAX),
                            ),
                        )
                    });
                if self.last_path != Some(sample) {
                    self.last_path = Some(sample);
                    return Ok(AttachmentTransportItem::Path(v2::LocalSessionTunnelPath {
                        path: sample.0,
                        rtt_ms: sample.1,
                    }));
                }
                if let Some(frame) = self.queued.pop_front() {
                    if !matches!(
                        frame.kind,
                        WireKind::TerminalSemanticSnapshot
                            | WireKind::TerminalSemanticDelta
                            | WireKind::TerminalSemanticHistoryWindowFrame
                            | WireKind::TerminalSyncRequired
                            | WireKind::TerminalLeaseLost
                            | WireKind::TerminalSessionEnded
                            | WireKind::TerminalClipboardWrite
                            | WireKind::TerminalNotification
                            | WireKind::SessionOperationLeaseResponse
                            | WireKind::SessionMutateResponse
                            | WireKind::ServiceErrorResponse
                    ) {
                        return Err(malformed(
                            "remote stream carried a non-Session or local-only frame",
                        ));
                    }
                    return Ok(AttachmentTransportItem::Session(frame));
                }
                let mut buffer = Zeroizing::new([0_u8; 16 * 1024]);
                // A quiet shell must still expose path upgrades and new RTT
                // estimates. This only observes QUIC; it sends no terminal input.
                let read = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    self.recv.read(&mut *buffer),
                )
                .await;
                let Ok(read) = read else {
                    continue;
                };
                let count = read
                    .map_err(|_| unavailable("Session stream read failed"))?
                    .unwrap_or(0);
                if count == 0 {
                    std::mem::take(&mut self.decoder)
                        .finish()
                        .map_err(protocol_error)?;
                    return Err(attachment_cancelled());
                }
                self.read_bytes = self.read_bytes.saturating_add(count as u64);
                let frames = self
                    .decoder
                    .feed(&buffer[..count])
                    .map_err(protocol_error)?;
                self.decoded_frames = self.decoded_frames.saturating_add(frames.len() as u64);
                self.queued.extend(frames);
            }
        })
    }
    fn shutdown(&mut self) -> TransportFuture<'_, Result<(), ClientError>> {
        Box::pin(async move {
            self.send
                .finish()
                .map_err(|_| unavailable("Session detach finish failed"))
        })
    }
}
impl Drop for IrohSessionIo {
    fn drop(&mut self) {
        let _ = self.recv.stop(VarInt::from_u32(0));
    }
}
struct IrohPairIo {
    local: DeviceId,
    remote: DeviceId,
    send: SendStream,
    recv: RecvStream,
    connection: Connection,
}
impl PairProtocolIo for IrohPairIo {
    fn local(&self) -> DeviceId {
        self.local
    }
    fn remote(&self) -> DeviceId {
        self.remote
    }
}
impl AsyncRead for IrohPairIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.get_mut().recv), cx, buf)
    }
}
impl AsyncWrite for IrohPairIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.get_mut().send), cx, bytes)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.get_mut().send), cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.get_mut().send), cx)
    }
}
impl Drop for IrohPairIo {
    fn drop(&mut self) {
        self.connection
            .close(VarInt::from_u32(0x105), b"pair exchange complete");
    }
}
fn require_remote(target: ResolvedSessionTarget) -> Result<DeviceId, ClientError> {
    target.device_id().ok_or_else(|| {
        ClientError::new(
            DomainErrorKind::Unauthorized,
            "mobile controller requires an exact remote host",
        )
    })
}
fn random_bytes<const N: usize>() -> Result<[u8; N], ClientError> {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| unavailable("operating-system randomness unavailable"))?;
    Ok(bytes)
}
fn record_connection_stage(operation: &zterm_diagnostics::Operation, stage: ConnectionStage) {
    zterm_diagnostics::record(
        zterm_diagnostics::Event::new(zterm_diagnostics::Kind::ConnectionStartup)
            .operation(operation)
            .level(zterm_diagnostics::Level::Debug)
            .startup(stage),
    );
}

fn unavailable(detail: &'static str) -> ClientError {
    ClientError::new(DomainErrorKind::TransportUnavailable, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unary::SessionUnaryClient;
    use zterm_core::{DaemonIncarnation, OperationLease, SessionId};

    #[derive(Debug)]
    struct FixtureLookup {
        relay: iroh::RelayUrl,
        stalled: bool,
        delay: std::time::Duration,
    }

    impl iroh::address_lookup::AddressLookup for FixtureLookup {
        fn resolve(
            &self,
            remote: iroh::EndpointId,
        ) -> Option<
            futures_util::stream::BoxStream<
                'static,
                Result<iroh::address_lookup::Item, iroh::address_lookup::Error>,
            >,
        > {
            if self.stalled {
                return Some(Box::pin(futures_util::stream::pending()));
            }
            let item = iroh::address_lookup::Item::new(
                iroh::EndpointAddr::new(remote)
                    .with_relay_url(self.relay.clone())
                    .into(),
                "stale-route-fixture",
                None,
            );
            let delay = self.delay;
            Some(Box::pin(futures_util::stream::once(async move {
                tokio::time::sleep(delay).await;
                Ok(item)
            })))
        }
    }

    #[derive(Clone, Copy)]
    enum FreshRoute {
        Stale,
        Stalled,
        Working,
    }

    #[tokio::test]
    async fn unusable_fresh_route_cannot_starve_a_working_verified_route() {
        route_fallback_fixture(true, FreshRoute::Stale).await;
    }

    #[tokio::test]
    async fn unusable_fresh_route_cannot_starve_a_working_pair_ticket_route() {
        route_fallback_fixture(false, FreshRoute::Stale).await;
    }

    #[tokio::test]
    async fn cold_verified_route_does_not_wait_for_a_stalled_lookup() {
        let elapsed = route_fallback_fixture(true, FreshRoute::Stalled).await;
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "a working cold cache was held behind discovery for {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn cold_pair_ticket_does_not_wait_for_a_stalled_lookup() {
        let elapsed = route_fallback_fixture(false, FreshRoute::Stalled).await;
        assert!(elapsed < std::time::Duration::from_secs(1));
    }

    #[tokio::test]
    async fn cold_stale_cache_still_uses_concurrent_fresh_lookup() {
        route_fallback_fixture(true, FreshRoute::Working).await;
    }

    async fn route_fallback_fixture(
        verified_cache: bool,
        fresh: FreshRoute,
    ) -> std::time::Duration {
        use iroh::tls::CaTlsConfig;
        use std::time::Duration;
        let alpn = if verified_cache {
            ZTERM_ALPN
        } else {
            ZTERM_PAIR_ALPN
        };
        let mut relay_config = iroh_relay::server::ServerConfig::default();
        relay_config.relay = Some(iroh_relay::server::testing::relay_config());
        let server = iroh_relay::server::Server::spawn(relay_config)
            .await
            .expect("isolated loopback relay");
        let good: iroh::RelayUrl = format!(
            "https://{}",
            server.https_addr().expect("relay HTTPS address")
        )
        .parse()
        .expect("loopback relay URL");
        // An unrelated live relay has no route to this host. This models a
        // correctly signed but stale advertised home, not an identity mismatch.
        let mut stale_config = iroh_relay::server::ServerConfig::default();
        stale_config.relay = Some(iroh_relay::server::testing::relay_config());
        let stale_server = iroh_relay::server::Server::spawn(stale_config)
            .await
            .expect("unrelated relay");
        let stale: iroh::RelayUrl = format!(
            "https://{}",
            stale_server.https_addr().expect("stale HTTPS address")
        )
        .parse()
        .expect("stale relay URL");
        let host = Endpoint::builder(presets::Minimal)
            .ca_tls_config(CaTlsConfig::insecure_skip_verify())
            .relay_mode(RelayMode::Custom(
                iroh::RelayConfig::new(good.clone(), None).into(),
            ))
            .clear_ip_transports()
            .alpns(vec![alpn.to_vec()])
            .bind()
            .await
            .expect("relay-only host");
        tokio::time::timeout(Duration::from_secs(5), host.online())
            .await
            .expect("host online");
        let endpoint = Endpoint::builder(presets::Minimal)
            .ca_tls_config(CaTlsConfig::insecure_skip_verify())
            .relay_mode(RelayMode::Custom(
                iroh::RelayConfig::new(good.clone(), None).into(),
            ))
            .clear_ip_transports()
            .address_lookup(FixtureLookup {
                relay: if matches!(fresh, FreshRoute::Working) {
                    good.clone()
                } else {
                    stale.clone()
                },
                stalled: matches!(fresh, FreshRoute::Stalled),
                delay: Duration::from_millis(100),
            })
            .bind()
            .await
            .expect("relay-only client");
        let controller = IrohController::new(endpoint, "route fallback test").expect("controller");
        let accepting = host.clone();
        let accepted = tokio::spawn(async move {
            accepting
                .accept()
                .await
                .expect("incoming test connection")
                .await
                .expect("authenticated transport")
        });
        let started = Instant::now();
        let good_hint = RelayHint::new(good.to_string()).expect("validated fallback route");
        let known_hint = if matches!(fresh, FreshRoute::Working) {
            RelayHint::new(stale.to_string()).expect("stale cached route")
        } else {
            good_hint.clone()
        };
        let result = controller
            .dial(
                device_from_endpoint_id(host.id()),
                if verified_cache {
                    vec![known_hint.clone()]
                } else {
                    vec![]
                },
                if verified_cache {
                    vec![]
                } else {
                    vec![known_hint]
                },
                alpn,
                started + crate::protocol::DEFAULT_DEADLINE,
            )
            .await;
        let elapsed = started.elapsed();
        let connected = result.is_ok();
        let verified_route = result.as_ref().ok().and_then(|(_, relay)| relay.clone());
        eprintln!(
            "verified fallback result: elapsed={elapsed:?} connected={connected} error={:?}",
            result.as_ref().err().map(|error| error.kind())
        );
        if let Ok((connection, _)) = result.as_ref() {
            connection.close(VarInt::from_u32(0), b"fixture complete");
        }
        accepted.abort();
        let _ = accepted.await;
        let _ = tokio::time::timeout(Duration::from_secs(5), controller.shutdown()).await;
        let _ = tokio::time::timeout(Duration::from_secs(5), host.close()).await;
        server.shutdown().await.expect("relay shutdown");
        stale_server.shutdown().await.expect("stale relay shutdown");
        assert!(
            connected,
            "working verified fallback was starved for {elapsed:?}: {}",
            result.expect_err("failed dial").kind().code()
        );
        assert_eq!(
            verified_route,
            Some(good_hint),
            "cache only the selected working relay"
        );
        elapsed
    }

    /// Signed lookup whose advertised Relay follows the host, like Pkarr.
    #[derive(Debug)]
    struct MovableLookup {
        relay: Arc<StdMutex<iroh::RelayUrl>>,
        delay: std::time::Duration,
    }

    impl iroh::address_lookup::AddressLookup for MovableLookup {
        fn resolve(
            &self,
            remote: iroh::EndpointId,
        ) -> Option<
            futures_util::stream::BoxStream<
                'static,
                Result<iroh::address_lookup::Item, iroh::address_lookup::Error>,
            >,
        > {
            let relay = self
                .relay
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let item = iroh::address_lookup::Item::new(
                iroh::EndpointAddr::new(remote).with_relay_url(relay).into(),
                "movable-route-fixture",
                None,
            );
            let delay = self.delay;
            Some(Box::pin(futures_util::stream::once(async move {
                tokio::time::sleep(delay).await;
                Ok(item)
            })))
        }
    }

    async fn relay_host(secret: SecretKey, relay: iroh::RelayUrl) -> Endpoint {
        let host = Endpoint::builder(presets::Minimal)
            .secret_key(secret)
            .ca_tls_config(iroh::tls::CaTlsConfig::insecure_skip_verify())
            .relay_mode(RelayMode::Custom(
                iroh::RelayConfig::new(relay, None).into(),
            ))
            .clear_ip_transports()
            .alpns(vec![ZTERM_ALPN.to_vec()])
            .bind()
            .await
            .expect("relay-only host");
        tokio::time::timeout(std::time::Duration::from_secs(5), host.online())
            .await
            .expect("host relay online");
        host
    }

    async fn hold_incoming(host: Endpoint) {
        let mut held = tokio::task::JoinSet::new();
        while let Some(incoming) = host.accept().await {
            held.spawn(async move {
                if let Ok(connection) = incoming.await {
                    connection.closed().await;
                }
            });
        }
    }

    /// A host incarnation that disappears without a QUIC close, like a killed
    /// process or VM. Its runtime is abandoned, so no close frame is written.
    async fn spawn_killable_host(
        secret: SecretKey,
        relay: iroh::RelayUrl,
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        std::thread::JoinHandle<()>,
    ) {
        let (ready, online) = tokio::sync::oneshot::channel();
        let (kill, killed) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("killable host runtime");
            runtime.block_on(async move {
                let host = relay_host(secret, relay).await;
                tokio::spawn(hold_incoming(host.clone()));
                ready.send(()).expect("report host online");
                let _ = killed.await;
                std::mem::forget(host);
            });
            runtime.shutdown_background();
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), online)
            .await
            .expect("killable host online")
            .expect("killable host reports readiness");
        (kill, thread)
    }

    /// The host restarts abruptly and returns on another Relay. A faster stale
    /// lookup can still advertise the old Relay, as DNS did in the field.
    async fn moved_host_fixture(old_connection_open: bool, stale_lookup_first: bool) {
        use std::time::Duration;
        let spawn_relay = || async {
            let mut config = iroh_relay::server::ServerConfig::default();
            config.relay = Some(iroh_relay::server::testing::relay_config());
            let server = iroh_relay::server::Server::spawn(config)
                .await
                .expect("isolated loopback relay");
            let url: iroh::RelayUrl =
                format!("https://{}", server.https_addr().expect("relay HTTPS"))
                    .parse()
                    .expect("relay URL");
            (server, url)
        };
        let (old_server, old_relay) = spawn_relay().await;
        let (new_server, new_relay) = spawn_relay().await;
        let advertised = Arc::new(StdMutex::new(old_relay.clone()));
        let mut client = Endpoint::builder(presets::Minimal)
            .ca_tls_config(iroh::tls::CaTlsConfig::insecure_skip_verify())
            .relay_mode(RelayMode::Custom(
                [
                    iroh::RelayConfig::new(old_relay.clone(), None),
                    iroh::RelayConfig::new(new_relay.clone(), None),
                ]
                .into_iter()
                .collect(),
            ))
            .clear_ip_transports()
            .address_lookup(MovableLookup {
                relay: Arc::clone(&advertised),
                delay: Duration::from_millis(300),
            });
        if stale_lookup_first {
            client = client.address_lookup(FixtureLookup {
                relay: old_relay.clone(),
                stalled: false,
                delay: Duration::from_millis(20),
            });
        }
        let controller = IrohController::new(client.bind().await.expect("client"), "moved host")
            .expect("controller");
        let secret = SecretKey::generate();
        let remote = device_from_endpoint_id(secret.public());
        let verified = RelayHint::new(old_relay.to_string()).expect("verified route");

        let (kill, old_host) = spawn_killable_host(secret.clone(), old_relay.clone()).await;
        let (old_connection, _) = controller
            .dial(
                remote,
                vec![verified.clone()],
                Vec::new(),
                ZTERM_ALPN,
                Instant::now() + crate::protocol::REMOTE_STARTUP_DEADLINE,
            )
            .await
            .expect("original connection");
        // A real restart takes seconds; the original connection's own lookup
        // (300 ms here) has finished long before the host returns elsewhere.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = kill.send(());
        tokio::task::spawn_blocking(move || old_host.join())
            .await
            .expect("join old host")
            .expect("old host thread");
        if !old_connection_open {
            old_connection.close(VarInt::from_u32(0), b"idle timeout model");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        *advertised
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = new_relay.clone();
        let host = relay_host(secret, new_relay).await;
        let holding = tokio::spawn(hold_incoming(host.clone()));

        let started = Instant::now();
        let result = controller
            .dial(
                remote,
                vec![verified],
                Vec::new(),
                ZTERM_ALPN,
                started + crate::protocol::REMOTE_STARTUP_DEADLINE,
            )
            .await;
        let elapsed = started.elapsed();
        let error = result.as_ref().err().map(ClientError::kind);
        eprintln!(
            "moved host: old_connection_open={old_connection_open} \
             stale_lookup_first={stale_lookup_first} elapsed={elapsed:?} error={error:?}"
        );
        drop(result);
        drop(old_connection);
        holding.abort();
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(controller.shutdown(), host.close());
        })
        .await;
        old_server.shutdown().await.expect("old relay shutdown");
        new_server.shutdown().await.expect("new relay shutdown");
        assert_eq!(error, None, "moved host was not found after {elapsed:?}");
        // The old connection's idle timeout (30 s on Relay) must not gate it.
        assert!(
            elapsed < crate::protocol::DEFAULT_DEADLINE,
            "moved host took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn moved_host_is_found_after_the_old_connection_closed_despite_a_stale_first_lookup() {
        moved_host_fixture(false, true).await;
    }

    #[tokio::test]
    async fn moved_host_is_found_while_the_old_connection_is_open_without_a_stale_lookup() {
        moved_host_fixture(true, false).await;
    }

    #[tokio::test]
    async fn moved_host_is_found_while_the_old_connection_is_open_despite_a_stale_first_lookup() {
        moved_host_fixture(true, true).await;
    }

    async fn exchange_test_greeting(
        client: &Endpoint,
        host: &Endpoint,
        address: iroh::EndpointAddr,
    ) -> Result<(Connection, Connection), ClientError> {
        let deadline = Instant::now() + crate::protocol::DEFAULT_DEADLINE;
        let incoming = async {
            let connection = host
                .accept()
                .await
                .expect("incoming connection")
                .await
                .expect("authenticated incoming transport");
            let (mut send, mut receive) = connection.accept_bi().await.expect("greeting stream");
            let mut byte = [0];
            receive.read_exact(&mut byte).await.expect("read greeting");
            send.write_all(&byte).await.expect("write greeting reply");
            send.finish().expect("finish greeting reply");
            connection
        };
        let outgoing = async {
            let connection = connect_transport(client, address, ZTERM_ALPN, deadline).await?;
            let (mut send, mut receive) = connection.open_bi().await.expect("outgoing greeting");
            send.write_all(b"x").await.expect("send greeting");
            send.finish().expect("finish greeting");
            let mut reply = [0];
            receive
                .read_exact(&mut reply)
                .await
                .expect("receive greeting reply");
            assert_eq!(&reply, b"x");
            Ok::<_, ClientError>(connection)
        };
        timeout_until(deadline, async {
            let (remote, local) = tokio::join!(incoming, outgoing);
            local.map(|local| (local, remote))
        })
        .await?
    }

    #[tokio::test]
    async fn new_endpoint_port_with_same_identity_can_exchange_while_old_connection_survives() {
        use std::{net::Ipv4Addr, time::Duration};
        async fn endpoint(secret: SecretKey) -> Endpoint {
            Endpoint::builder(presets::Minimal)
                .secret_key(secret)
                .relay_mode(RelayMode::Disabled)
                .clear_ip_transports()
                .bind_addr((Ipv4Addr::LOCALHOST, 0))
                .expect("loopback socket")
                .alpns(vec![ZTERM_ALPN.to_vec()])
                .bind()
                .await
                .expect("isolated endpoint")
        }
        let host = endpoint(SecretKey::generate()).await;
        let identity = SecretKey::generate();
        let old = endpoint(identity.clone()).await;
        let address = iroh::EndpointAddr::new(host.id()).with_ip_addr(host.bound_sockets()[0]);
        let old_connections = exchange_test_greeting(&old, &host, address.clone())
            .await
            .expect("original live connection");
        let new = endpoint(identity).await;
        assert_ne!(old.bound_sockets(), new.bound_sockets());
        let started = Instant::now();
        let result = exchange_test_greeting(&new, &host, address).await;
        eprintln!(
            "same identity/new port result: elapsed={:?} error={:?}",
            started.elapsed(),
            result.as_ref().err().map(|error| error.kind())
        );
        drop(old_connections);
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(old.close(), new.close(), host.close());
        })
        .await;
        result.expect("old transport must not starve the new endpoint's greeting");
    }

    #[tokio::test]
    async fn fresh_relay_endpoint_can_greet_while_same_identity_keeps_an_old_direct_path() {
        use iroh::tls::CaTlsConfig;
        use std::{net::Ipv4Addr, time::Duration};
        let mut config = iroh_relay::server::ServerConfig::default();
        config.relay = Some(iroh_relay::server::testing::relay_config());
        let server = iroh_relay::server::Server::spawn(config)
            .await
            .expect("isolated relay");
        let relay: iroh::RelayUrl =
            format!("https://{}", server.https_addr().expect("relay HTTPS"))
                .parse()
                .expect("relay URL");
        let builder = |secret| {
            Endpoint::builder(presets::Minimal)
                .secret_key(secret)
                .ca_tls_config(CaTlsConfig::insecure_skip_verify())
                .relay_mode(RelayMode::Custom(
                    iroh::RelayConfig::new(relay.clone(), None).into(),
                ))
                .clear_ip_transports()
                .alpns(vec![ZTERM_ALPN.to_vec()])
        };
        let host = builder(SecretKey::generate())
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("host loopback")
            .bind()
            .await
            .expect("host endpoint");
        tokio::time::timeout(Duration::from_secs(5), host.online())
            .await
            .expect("host relay online");
        let identity = SecretKey::generate();
        let old = builder(identity.clone())
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("old loopback")
            .bind()
            .await
            .expect("old direct endpoint");
        tokio::time::timeout(Duration::from_secs(5), old.online())
            .await
            .expect("old relay registration is live before restart");
        let direct = iroh::EndpointAddr::new(host.id()).with_ip_addr(host.bound_sockets()[0]);
        let old_connections = exchange_test_greeting(&old, &host, direct)
            .await
            .expect("original direct connection");
        assert!(
            old_connections.1.paths().iter().any(
                |path| path.is_selected() && matches!(path.remote_addr(), TransportAddr::Ip(_))
            )
        );
        // A restarted process can only use Relay while the host still retains
        // its previous incarnation's selected Direct path. No prewarming of the
        // new endpoint, and the old connection deliberately remains alive.
        let new = builder(identity)
            .bind()
            .await
            .expect("fresh relay-only endpoint");
        let started = Instant::now();
        let result = exchange_test_greeting(
            &new,
            &host,
            iroh::EndpointAddr::new(host.id()).with_relay_url(relay),
        )
        .await;
        eprintln!(
            "old direct/new relay greeting: elapsed={:?} error={:?}",
            started.elapsed(),
            result.as_ref().err().map(|error| error.kind())
        );
        let selected_relay = result.as_ref().is_ok_and(|(connection, _)| {
            connection.paths().iter().any(|path| {
                path.is_selected() && matches!(path.remote_addr(), TransportAddr::Relay(_))
            })
        });
        drop(old_connections);
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(old.close(), new.close(), host.close());
        })
        .await;
        server.shutdown().await.expect("relay shutdown");
        result.expect("retained Direct path must not starve a fresh Relay handshake");
        assert!(selected_relay, "the new endpoint must actually use Relay");
    }

    struct Replies {
        frames: StdMutex<VecDeque<DecodedFrame>>,
        requests: StdMutex<Vec<WireKind>>,
    }
    impl SessionUnaryTransport for Replies {
        fn request(
            &self,
            request: SessionUnaryRequest,
        ) -> TransportFuture<'_, Result<DecodedFrame, ClientError>> {
            self.requests
                .lock()
                .expect("request capture lock")
                .push(request.request_kind);
            let frame = self
                .frames
                .lock()
                .expect("reply fixture lock")
                .pop_front()
                .expect("expected unary request");
            Box::pin(async move { session_unary_result(frame) })
        }
    }
    fn frame(kind: WireKind, message: &impl prost::Message) -> DecodedFrame {
        let bytes =
            zterm_proto::encode_message(kind, 1, 0, message).expect("encode fixture response");
        FrameDecoder::new()
            .feed(&bytes)
            .expect("decode fixture response")
            .pop()
            .expect("one fixture frame")
    }
    fn error(kind: DomainErrorKind) -> DecodedFrame {
        frame(
            WireKind::ServiceErrorResponse,
            &v2::ServiceError {
                code: kind.code().to_owned(),
                message: "PRIVATE_REMOTE_DIAGNOSTIC".to_owned(),
            },
        )
    }
    #[tokio::test]
    async fn restarted_daemon_error_retires_unary_lease_without_replaying_mutation() {
        let lease = |incarnation| {
            frame(
                WireKind::SessionOperationLeaseResponse,
                &v2::SessionOperationLeaseResponse {
                    lease: Some(
                        OperationLease {
                            daemon_incarnation: DaemonIncarnation::from_array([incarnation; 16]),
                            ordinal: 1,
                        }
                        .into(),
                    ),
                },
            )
        };
        let transport = Replies {
            frames: StdMutex::new(VecDeque::from([
                lease(1),
                error(DomainErrorKind::OperationOutcomeUnknown),
                lease(2),
                error(DomainErrorKind::SessionNotFound),
            ])),
            requests: StdMutex::new(Vec::new()),
        };
        let client = SessionUnaryClient::default();
        let target = ResolvedSessionTarget::device(DeviceId::from_array([7; 32]));
        let session = SessionId::from_array([1; 16]);
        let first = client
            .close_session_at(&transport, target, session)
            .await
            .expect_err("definitive fixture service failure");
        assert_eq!(first.kind(), DomainErrorKind::OperationOutcomeUnknown);
        assert!(!first.detail().contains("PRIVATE_REMOTE_DIAGNOSTIC"));
        assert_eq!(
            *transport.requests.lock().expect("request capture lock"),
            vec![
                WireKind::SessionOperationLeaseRequest,
                WireKind::SessionCloseRequest
            ]
        );
        // Only a new explicit call acquires the new daemon's lease. Its definitive
        // service failure remains typed rather than being decoded as Session data.
        let second = client
            .close_session_at(&transport, target, session)
            .await
            .expect_err("definitive fixture service failure");
        assert_eq!(second.kind(), DomainErrorKind::SessionNotFound);
        assert_eq!(
            *transport.requests.lock().expect("request capture lock"),
            vec![
                WireKind::SessionOperationLeaseRequest,
                WireKind::SessionCloseRequest,
                WireKind::SessionOperationLeaseRequest,
                WireKind::SessionCloseRequest
            ]
        );
        for kind in [
            DomainErrorKind::Unauthorized,
            DomainErrorKind::SessionOccupied,
        ] {
            assert_eq!(
                session_unary_result(error(kind))
                    .expect_err("typed service error")
                    .kind(),
                kind
            );
        }
    }
}
