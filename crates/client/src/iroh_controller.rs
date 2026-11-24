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
        ReplacementTransport, RequestAcknowledgement, decode_session_service_error,
        read_acknowledged_response, timeout_until,
    },
    route::{
        RouteRefresh, combined_relay_address, device_from_endpoint_id, endpoint_id_from_device,
        selected_relay,
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
use voytty_core::{
    AuthGeneration, Capabilities, ConnectionAttemptId, ConnectionIncarnation, DeviceId,
    DomainErrorKind, PairNonce, RelayHint, TransportLimits, connection_progress::ConnectionStage,
};
use voytty_proto::{DecodedFrame, FrameDecoder, WireKind, v2};
use zeroize::Zeroizing;

/// Existing normal service protocol; incoming host services are not exposed.
pub const VOYTTY_ALPN: &[u8] = b"voytty/2";
/// Existing one-time pairing protocol.
pub const VOYTTY_PAIR_ALPN: &[u8] = b"voytty-pair/2";

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
    diagnostic: voytty_diagnostics::Operation,
    capabilities: voytty_core::Capabilities,
    connection: Connection,
    generation: AuthGeneration,
    verified_relay: Option<RelayHint>,
}
struct Peer {
    routes: StdMutex<Vec<RelayHint>>,
    normal: Mutex<Option<NormalConnection>>,
    // Set when an unacknowledged request retires the cached connection.
    replacement: StdMutex<Option<ReplacementTransport>>,
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
            replacement: StdMutex::new(None),
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
        route_refresh: RouteRefresh,
        alpn: &[u8],
        deadline: Instant,
    ) -> Result<(Connection, Option<RelayHint>), ClientError> {
        let operation = voytty_diagnostics::Operation::default();
        let _permit = timeout_until(deadline, self.inner.dials.acquire())
            .await?
            .map_err(|_| attachment_cancelled())?;
        let endpoint_id = endpoint_id_from_device(remote)?;
        record_connection_stage(&operation, ConnectionStage::LookingUpAddress);
        let routes = route_refresh
            .candidates(
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
        let until = if alpn == VOYTTY_ALPN {
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
                        voytty_diagnostics::Stage::ReusingTransport, None);
                    return Ok(normal.clone());
                }
                if let Some(normal) = slot.take() {
                    record_transport_observation(&normal.diagnostic, &normal.connection,
                        voytty_diagnostics::Stage::RetiringTransport, None);
                }
                let routes = peer.routes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
                let replacement = peer.replacement.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
                let dial = |route_refresh| async move {
                    Ok(self.dial(remote, routes, Vec::new(), route_refresh, VOYTTY_ALPN, deadline).await?.0)
                };
                let connection = match replacement {
                    Some(replacement) => replacement.race(dial(RouteRefresh::Refresh)).await?,
                    None => dial(RouteRefresh::UseCached).await?,
                };
                let relay = selected_relay(&connection);
                let mut guard = UnconfirmedConnection(Some(connection));
                let connection = guard.0.as_ref().expect("connection guard is populated");
                let operation = voytty_diagnostics::Operation::default();
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
        let path_events = Some(normal.connection.path_events());
        Ok(IrohSessionIo {
            diagnostic: normal.diagnostic,
            connection: normal.connection,
            path_events,
            last_stream_sample: None,
            read_bytes: 0,
            decoded_frames: 0,
            last_path: None,
            send,
            recv,
            read_buffer: Zeroizing::new(vec![0_u8; 16 * 1024]),
            decoder: FrameDecoder::new(),
            queued: VecDeque::new(),
            _permit: permit,
        })
    }

    /// Dials `remote` without Hello for the request that `until` bounds.
    fn dial_replacement(
        &self,
        remote: DeviceId,
        peer: &Peer,
        until: Instant,
    ) -> ReplacementTransport {
        let controller = self.clone();
        let routes = peer
            .routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        ReplacementTransport::dial(until, async move {
            let (connection, _) = controller
                .dial(
                    remote,
                    routes,
                    Vec::new(),
                    RouteRefresh::UseCached,
                    VOYTTY_ALPN,
                    until,
                )
                .await?;
            Ok(connection)
        })
    }

    /// Retires a failed request's cached connection if no other stream uses
    /// it, or if it stayed `silent`: then its other streams cannot reach the
    /// peer either, and they fail now and reconnect instead of at the QUIC
    /// idle timeout. The next dial for this peer races any replacement
    /// against a fresh dial.
    fn retire_failed_connection(
        &self,
        peer: &Peer,
        failed: usize,
        replacement: Option<ReplacementTransport>,
        silent: bool,
    ) {
        let Ok(maximum) = u32::try_from(self.inner.limits.max_bi_streams_per_connection) else {
            return;
        };
        // Own every admission permit while removing an idle cache entry.
        // Merely reading available_permits would race another stream's
        // admission. Cleanup must not wait beyond the failed request's deadline.
        let idle = Arc::clone(&peer.streams).try_acquire_many_owned(maximum);
        if idle.is_err() && !silent {
            return;
        }
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
                voytty_diagnostics::Stage::RetiringTransport,
                idle.is_ok().then_some(0),
            );
            normal
                .connection
                .close(VarInt::from_u32(0), b"request transport failed");
            *peer
                .replacement
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = replacement;
        }
    }

    async fn retire_connection_if_current(
        &self,
        peer: &Peer,
        failed: usize,
        reason: &'static [u8],
    ) {
        let mut slot = peer.normal.lock().await;
        if slot
            .as_ref()
            .is_some_and(|normal| normal.connection.stable_id() == failed)
            && let Some(normal) = slot.take()
        {
            record_transport_observation(
                &normal.diagnostic,
                &normal.connection,
                voytty_diagnostics::Stage::RetiringTransport,
                None,
            );
            normal.connection.close(VarInt::from_u32(0), reason);
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
        let mut backoff = OPEN_BI_RETRY_INITIAL;
        let mut retries: usize = 0;
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
                drop(permit);
                if retries > 0 {
                    wait_retry_backoff(&peer.cancelled, deadline, &mut backoff).await?;
                }
                retries = retries.saturating_add(1);
                continue;
            }
            match timeout_until(deadline, normal.connection.open_bi()).await? {
                Ok((send, recv)) => {
                    if peer.cancelled.is_cancelled() {
                        return Err(attachment_cancelled());
                    }
                    return Ok((normal, permit, send, recv));
                }
                Err(_) => {
                    drop(permit);
                    let still_open = normal.connection.close_reason().is_none();
                    if still_open {
                        timeout_until(
                            deadline,
                            self.retire_connection_if_current(
                                peer,
                                normal.connection.stable_id(),
                                b"open_bi failed",
                            ),
                        )
                        .await?;
                    }
                    if still_open || retries > 0 {
                        wait_retry_backoff(&peer.cancelled, deadline, &mut backoff).await?;
                    }
                    retries = retries.saturating_add(1);
                }
            }
        }
    }
    /// Pairs a validated one-time ticket and resolves ambiguity on the normal ALPN.
    /// The caller must persist the returned host before reporting success to the UI.
    pub async fn pair_ticket(&self, text: &str) -> Result<PairedHost, ClientError> {
        let (fields, secret) = voytty_proto::decode_pair_ticket(text).map_err(|_| {
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
            .dial(
                remote,
                Vec::new(),
                routes,
                RouteRefresh::UseCached,
                VOYTTY_PAIR_ALPN,
                deadline,
            )
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
        attempt.require_confirmation()?;
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
            if stream.connection.paths().iter().any(|path| {
                path.is_selected() && matches!(path.remote_addr(), TransportAddr::Relay(_))
            }) {
                let queued = self
                    .inner
                    .endpoint
                    .request_direct_path(stream.connection.remote_id());
                voytty_diagnostics::record(
                    voytty_diagnostics::Event::new(
                        voytty_diagnostics::Kind::DirectPathUpgradeRequested,
                    )
                    .level(voytty_diagnostics::Level::Debug)
                    .operation(&stream.diagnostic)
                    .connection(stream.diagnostic.id())
                    .stage(if queued {
                        voytty_diagnostics::Stage::Accepted
                    } else {
                        voytty_diagnostics::Stage::Failed
                    }),
                );
            }
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
                voytty_proto::encode_payload(
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
            let mut replacement = None;
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
                read_acknowledged_response(
                    &mut stream.recv,
                    deadline,
                    acknowledgement.as_ref(),
                    || {
                        replacement = Some(self.controller.dial_replacement(
                            self.remote,
                            &self.peer,
                            deadline,
                        ));
                    },
                )
                .await
            }
            .await;
            drop(stream);
            if result
                .as_ref()
                .is_err_and(RemoteAttemptError::is_transport_failure_after_write)
            {
                let silent = acknowledgement
                    .as_ref()
                    .is_some_and(RequestAcknowledgement::silent);
                self.controller.retire_failed_connection(
                    &self.peer,
                    connection,
                    replacement,
                    silent,
                );
            }
            result
        })
    }
}
const OPEN_BI_RETRY_INITIAL: std::time::Duration = std::time::Duration::from_millis(20);
const OPEN_BI_RETRY_MAX: std::time::Duration = std::time::Duration::from_millis(500);
const DETAIL_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
const MIN_RTT_REPORT_DELTA_MS: u32 = 5;

async fn wait_retry_backoff(
    cancelled: &CancellationToken,
    deadline: Instant,
    backoff: &mut std::time::Duration,
) -> Result<(), ClientError> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(crate::protocol::control_timeout)?;
    let hit_deadline = *backoff >= remaining;
    let delay = (*backoff).min(remaining);
    *backoff = (*backoff).saturating_mul(2).min(OPEN_BI_RETRY_MAX);
    tokio::select! {
        biased;
        _ = cancelled.cancelled() => Err(attachment_cancelled()),
        () = tokio::time::sleep(delay) => {
            if hit_deadline {
                Err(crate::protocol::control_timeout())
            } else {
                Ok(())
            }
        }
    }
}

fn sample_selected_path(connection: &Connection) -> (i32, Option<u32>) {
    connection
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
        })
}

fn should_emit_path_sample(last: Option<(i32, Option<u32>)>, current: (i32, Option<u32>)) -> bool {
    let Some((prev_path, prev_rtt)) = last else {
        return true;
    };
    if prev_path != current.0 {
        return true;
    }
    match (prev_rtt, current.1) {
        (None, None) => false,
        (Some(prev), Some(curr)) => {
            let threshold = (prev / 10).max(MIN_RTT_REPORT_DELTA_MS);
            curr.abs_diff(prev) >= threshold
        }
        (None, Some(_)) | (Some(_), None) => true,
    }
}

async fn next_path_event(
    events: &mut Option<iroh::endpoint::PathEventStream>,
) -> Option<iroh::endpoint::PathEvent> {
    use futures_util::StreamExt;
    match events {
        Some(stream) => stream.next().await,
        None => None,
    }
}

struct IrohSessionIo {
    diagnostic: voytty_diagnostics::Operation,
    connection: Connection,
    path_events: Option<iroh::endpoint::PathEventStream>,
    last_stream_sample: Option<Instant>,
    read_bytes: u64,
    decoded_frames: u64,
    last_path: Option<(i32, Option<u32>)>,
    send: SendStream,
    recv: RecvStream,
    read_buffer: Zeroizing<Vec<u8>>,
    decoder: FrameDecoder,
    queued: VecDeque<DecodedFrame>,
    _permit: OwnedSemaphorePermit,
}
impl IrohSessionIo {
    fn stream_progress(&self) -> voytty_diagnostics::StreamProgress {
        voytty_diagnostics::StreamProgress {
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
        if !voytty_diagnostics::detail_enabled() {
            self.last_stream_sample = None;
            return;
        }
        let now = Instant::now();
        if self
            .last_stream_sample
            .is_some_and(|last| now.saturating_duration_since(last) < DETAIL_SAMPLE_INTERVAL)
        {
            return;
        }
        self.last_stream_sample = Some(now);
        // Sample only while the existing reader is polled and detail logging is enabled.
        voytty_diagnostics::record(crate::handshake::with_transport_state(
            voytty_diagnostics::Event::new(voytty_diagnostics::Kind::StreamObserved)
                .level(voytty_diagnostics::Level::Debug)
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
                    Instant::now() + crate::protocol::startup_budget(target),
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
                let sample = sample_selected_path(&self.connection);
                if should_emit_path_sample(self.last_path, sample) {
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
                let detail_enabled = voytty_diagnostics::detail_enabled();
                // Wake on stream traffic or QUIC path transitions instead of polling every second.
                let read = tokio::select! {
                    read = self.recv.read(&mut self.read_buffer) => Some(read),
                    event = next_path_event(&mut self.path_events), if self.path_events.is_some() => {
                        if event.is_none() {
                            self.path_events = None;
                        }
                        None
                    }
                    () = tokio::time::sleep(DETAIL_SAMPLE_INTERVAL), if detail_enabled => None,
                };
                let Some(read) = read else {
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
                    .feed(&self.read_buffer[..count])
                    .map_err(protocol_error);
                self.read_buffer[..count].fill(0);
                let frames = frames?;
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
fn record_connection_stage(operation: &voytty_diagnostics::Operation, stage: ConnectionStage) {
    voytty_diagnostics::record(
        voytty_diagnostics::Event::new(voytty_diagnostics::Kind::ConnectionStartup)
            .operation(operation)
            .level(voytty_diagnostics::Level::Debug)
            .startup(stage),
    );
}

fn unavailable(detail: &'static str) -> ClientError {
    ClientError::new(DomainErrorKind::TransportUnavailable, detail)
}

#[cfg(test)]
mod tests;
