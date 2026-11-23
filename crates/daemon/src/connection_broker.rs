//! Per-peer Iroh connection singleflight, duplicate arbitration, and stream admission.

mod demand;
mod dial;
mod maintenance;

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
#[cfg(any(unix, windows))]
use std::future::Future;
use std::future::pending;
#[cfg(any(unix, windows))]
use std::pin::Pin;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use voytty_client::progress::{ProgressEvent, ProgressHistory, ProgressObserver};
use voytty_client::remote_unary::ReplacementTransport;
use voytty_client::route::{combined_relay_address, selected_relay};
use voytty_core::connection_progress::ConnectionStage;

use futures_util::StreamExt;
use iroh::endpoint::{Connection, PathEvent, RecvStream, SendStream, VarInt};
use iroh::{Endpoint, EndpointAddr, TransportAddr};
use ring::rand::{SecureRandom, SystemRandom};

use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use tokio_util::task::{TaskTracker, task_tracker::TaskTrackerToken};

use maintenance::PendingObservations;
use voytty_core::{
    AuthGeneration, AuthorizationSnapshot, AuthorizationStatus, ConnectionAttemptId,
    ConnectionCandidateKey, ConnectionHello, ConnectionIncarnation, ConnectionWelcome, DeviceId,
    DomainErrorKind, RelayHint, TransportLimits, designated_primary,
};
use voytty_proto::{DecodedFrame, WireKind, encode_message, v2};

use crate::authorization::AuthorizationRegistry;
use crate::error::DaemonError;
use crate::network::{
    NetworkObservation, NetworkObserver, NetworkReporter, PairHandshakeAdmission,
    PairHandshakePermit, PathKind,
};
use crate::route::{RouteResolver, device_from_endpoint_id, endpoint_id_from_device};
use crate::store::{RelayRouteCache, StoreHandle};
use crate::transport::{VOYTTY_ALPN, VOYTTY_PAIR_ALPN};

const CLOSE_INCOMPATIBLE: u32 = 0x101;
const CLOSE_DUPLICATE: u32 = 0x102;
const CLOSE_SHUTTING_DOWN: u32 = 0x103;
const CLOSE_OVERLOADED: u32 = 0x104;
const CLOSE_PAIR_COMPLETE: u32 = 0x105;
const STREAM_REJECTED: u32 = 0x200;
const RETRY_BASE: Duration = Duration::from_millis(250);
const RETRY_CAP: Duration = Duration::from_secs(10);

pub use voytty_client::handshake::ConnectionIdentity;
use voytty_client::handshake::{
    CLOSE_UNAUTHORIZED, connect_transport, controller_handshake, read_one_frame,
    write_handshake_message,
};

/// Why the broker is intentionally closing a peer connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionCloseReason {
    /// Current local authorization no longer admits the peer.
    Unauthorized,
    /// A lower deterministic candidate replaced this connection.
    Duplicate,
    /// The daemon is quiescing its network owner.
    ShuttingDown,
    /// The sole Endpoint was lost and will be rebound.
    EndpointReset,
}

impl ConnectionCloseReason {
    const fn code(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Duplicate => "duplicate",
            Self::ShuttingDown => "shutting_down",
            Self::EndpointReset => "endpoint_reset",
        }
    }

    const fn wire(self) -> (VarInt, &'static [u8]) {
        match self {
            Self::Unauthorized => (VarInt::from_u32(CLOSE_UNAUTHORIZED), b"not authorized"),
            Self::Duplicate => (VarInt::from_u32(CLOSE_DUPLICATE), b"duplicate connection"),
            Self::ShuttingDown => (VarInt::from_u32(CLOSE_SHUTTING_DOWN), b"transport stopping"),
            Self::EndpointReset => (
                VarInt::from_u32(CLOSE_SHUTTING_DOWN),
                b"transport restarting",
            ),
        }
    }
}

/// Semantic owner requesting a new bidirectional stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamPurpose {
    /// A future M7 Session/terminal service stream.
    Service,
    /// Normal-ALPN confirmation after a pairing outcome.
    AuthorizationConfirmation,
}

/// Redacted live state for one peer slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerConnectionObservation {
    /// Promoted deterministic candidate, when connected.
    pub primary: Option<ConnectionCandidateKey>,
    /// Number of provisional plus primary candidates.
    pub candidate_count: usize,
    /// Number of current RAII consumers.
    pub demand_count: usize,
    /// Number of currently open outbound or accepted service streams.
    pub active_stream_count: u32,
    /// Latest generation with which the remote receiver accepted this host.
    pub remote_acceptance_generation: Option<AuthGeneration>,
    /// Redacted selected path kind.
    pub path: PathKind,
}

/// Pure duplicate-arbitration evidence exposed to named integration gates.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuplicateConnectionTestEvidence {
    /// Candidate selected by both registration schedules.
    pub primary: Option<ConnectionCandidateKey>,
    /// Candidates retained after duplicate cleanup.
    pub remaining_candidate_count: usize,
    /// Duplicate candidates returned to the caller for connection-local close.
    pub loser_count: usize,
    /// Whether a provisional candidate prevented a redundant outbound dial.
    pub redial_suppressed_while_provisional: bool,
    /// Whether receiver authorization prevented redial after promotion.
    pub redial_suppressed_after_confirmation: bool,
    /// Whether peer-close cleanup removed the final candidate and primary.
    pub empty_after_peer_close: bool,
}

/// Socket-free stream-admission and RAII evidence for named integration gates.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamLimitTestEvidence {
    /// Global handler admission rejected a second concurrent holder.
    pub global_overflow_rejected: bool,
    /// Dropping the global permit restored its capacity.
    pub global_capacity_released: bool,
    /// One peer's open-stream queue rejected its own overflow.
    pub peer_overflow_rejected: bool,
    /// A full peer queue did not consume another peer's permit.
    pub peer_isolated: bool,
    /// Dropping peer queue permits restored both peers.
    pub peer_capacity_released: bool,
    /// Per-connection stream and handler limits rejected overflow.
    pub connection_overflow_rejected: bool,
    /// A full connection did not consume another connection's permits.
    pub connection_isolated: bool,
    /// Dropping connection permits restored stream and handler capacity.
    pub connection_capacity_released: bool,
    /// Per-peer metric guards remained independent while both were live.
    pub metric_peer_isolated: bool,
    /// Dropping every metric guard restored the aggregate observation.
    pub metric_capacity_released: bool,
}

/// Redacted path projection evidence exposed to named integration gates.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathObservationTestEvidence {
    /// Path kinds observed in order, followed by `Unknown` after cleanup.
    pub timeline: Vec<PathKind>,
    /// Persistable relay hints for each input; direct IP paths always yield `None`.
    pub persistable_relays: Vec<Option<RelayHint>>,
    /// Aggregate metrics after the final selected input path.
    pub selected_observation: NetworkObservation,
    /// Aggregate metrics after peer-path cleanup.
    pub cleared_observation: NetworkObservation,
}

/// Sole registry of primary Iroh connections for all remote devices.
#[derive(Clone)]
pub struct ConnectionBroker {
    inner: Arc<BrokerInner>,
}

/// Boxed future returned by the narrow inbound normal-service callback.
#[cfg(any(unix, windows))]
pub(crate) type RemoteServiceHandlerFuture =
    Pin<Box<dyn Future<Output = Result<(), DaemonError>> + Send + 'static>>;

/// Object-safe owner of one authenticated inbound normal service stream.
///
/// The callback cannot reach Endpoint, candidate, route, profile, or peer-slot
/// state. It receives only the owned stream, verified peer identity, accepted
/// receiver generation, and the first-frame deadline.
#[cfg(any(unix, windows))]
pub(crate) trait RemoteServiceHandler: Send + Sync + 'static {
    fn handle_service_stream(
        &self,
        stream: InboundAuthenticatedStream,
        first_frame_deadline: Instant,
    ) -> RemoteServiceHandlerFuture;
}

/// Owned Iroh halves and receiver-owned authorization identity for one
/// inbound normal service stream.
#[cfg(any(unix, windows))]
pub(crate) struct InboundAuthenticatedStream {
    send: SendStream,
    recv: RecvStream,
    remote_device_id: DeviceId,
    accepted_generation: AuthGeneration,
}

#[cfg(any(unix, windows))]
impl fmt::Debug for InboundAuthenticatedStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InboundAuthenticatedStream")
            .field("remote_device_id", &self.remote_device_id)
            .field("accepted_generation", &self.accepted_generation)
            .finish_non_exhaustive()
    }
}

#[cfg(any(unix, windows))]
impl InboundAuthenticatedStream {
    #[must_use]
    pub(crate) const fn remote_device_id(&self) -> DeviceId {
        self.remote_device_id
    }

    #[must_use]
    pub(crate) const fn accepted_generation(&self) -> AuthGeneration {
        self.accepted_generation
    }

    pub(crate) fn into_parts(self) -> (SendStream, RecvStream) {
        (self.send, self.recv)
    }
}

#[derive(Default)]
#[cfg(any(unix, windows))]
struct RemoteServiceHandlerSlot {
    handler: OnceLock<Arc<dyn RemoteServiceHandler>>,
}

#[cfg(any(unix, windows))]
impl RemoteServiceHandlerSlot {
    fn install(&self, handler: Arc<dyn RemoteServiceHandler>) -> Result<(), DaemonError> {
        self.handler.set(handler).map_err(|_| {
            DaemonError::new(
                DomainErrorKind::IdentityStateMismatch,
                "remote service handler is already installed",
            )
        })
    }

    fn get(&self) -> Option<Arc<dyn RemoteServiceHandler>> {
        self.handler.get().cloned()
    }

    fn is_installed(&self) -> bool {
        self.handler.get().is_some()
    }
}

impl fmt::Debug for ConnectionBroker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("ConnectionBroker");
        debug.field("local_device_id", &self.inner.identity.device_id());
        #[cfg(any(unix, windows))]
        debug.field(
            "service_handler_installed",
            &self.inner.service_handler.is_installed(),
        );
        debug.finish_non_exhaustive()
    }
}

struct BrokerInner {
    identity: ConnectionIdentity,
    store: StoreHandle,
    authorization: AuthorizationRegistry,
    resolver: RouteResolver,
    limits: TransportLimits,
    endpoint: watch::Sender<Option<Endpoint>>,
    peers: Mutex<BTreeMap<DeviceId, Arc<PeerSlot>>>,
    admission: BrokerAdmission,
    metrics: Arc<BrokerMetrics>,
    observer: NetworkObserver,
    lifecycle: BrokerLifecycle,
    observations: Mutex<PendingObservations>,
    pairing: PairHandshakeAdmission,
    #[cfg(any(unix, windows))]
    service_handler: RemoteServiceHandlerSlot,
    test_routes: Mutex<BTreeMap<DeviceId, EndpointAddr>>,
}

struct PeerSlot {
    remote: DeviceId,
    state: AsyncMutex<PeerState>,
    changed: Notify,
    demand: DemandState,
    admission: PeerAdmission,
    active_streams: Arc<AtomicUsize>,
}

impl PeerSlot {
    async fn report_progress(&self, stage: ConnectionStage) {
        self.state.lock().await.progress.record(ProgressEvent {
            stage,
            failure: None,
        });
        self.changed.notify_waiters();
    }
}

#[derive(Default)]
struct PeerState {
    progress: ProgressHistory,
    candidates: CandidateRegistry<Arc<Candidate>>,
    remote_acceptance: Option<AuthGeneration>,
    dial_worker_running: bool,
    terminal_error: Option<DaemonError>,
    incarnation: PeerIncarnation,
    local_incarnation: Option<ConnectionIncarnation>,
    // Set when an unacknowledged request retires the primary; the next
    // outbound dial races it against a fresh one.
    replacement: Option<ReplacementTransport>,
}

#[derive(Default)]
struct PeerIncarnation {
    current: Option<ConnectionIncarnation>,
    // A Hello/Welcome cannot remain pending beyond its handshake deadline.
    // Retaining retired owners for that whole window fences delayed handshakes
    // without accumulating generation IDs over the daemon's lifetime.
    retired: VecDeque<(ConnectionIncarnation, Instant)>,
}

impl PeerIncarnation {
    fn observe(
        &mut self,
        incarnation: ConnectionIncarnation,
        now: Instant,
        handshake_window: Duration,
        maximum: usize,
    ) -> Result<bool, DaemonError> {
        self.retired.retain(|(_, deadline)| *deadline > now);
        if self.current == Some(incarnation) {
            return Ok(false);
        }
        if self
            .retired
            .iter()
            .any(|(retired, _)| *retired == incarnation)
        {
            return Err(transport_unavailable(
                "handshake belongs to a retired connection owner",
            ));
        }
        if let Some(previous) = self.current {
            if self.retired.len() >= maximum {
                return Err(resource_exhausted("too many connection owner restarts"));
            }
            self.retired.push_back((previous, now + handshake_window));
        }
        self.current = Some(incarnation);
        Ok(true)
    }
}

struct CandidateRegistry<T> {
    entries: BTreeMap<ConnectionCandidateKey, CandidateEntry<T>>,
    primary: Option<ConnectionCandidateKey>,
}

struct CandidateEntry<T> {
    value: T,
}

enum CandidateDecision<T> {
    Promoted(Vec<T>),
    Lost(T),
    Wait,
    Missing,
}

struct DemandState {
    count: AtomicUsize,
    transient_routes: Mutex<TransientRouteLeases>,
}

#[derive(Default)]
struct TransientRouteLeases {
    ordered: Vec<(RelayHint, usize)>,
}

struct BrokerAdmission {
    pending_dials: Arc<Semaphore>,
    authenticated_connections: Arc<Semaphore>,
    global_stream_handlers: Arc<Semaphore>,
}

struct PeerAdmission {
    open_queue: Arc<Semaphore>,
}

struct CandidateAdmission {
    streams: Arc<Semaphore>,
    handlers: Arc<Semaphore>,
}

#[derive(Debug)]
struct ServiceHandlerPermits {
    _connection: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
}

#[derive(Default)]
struct BrokerLifecycle {
    tasks: TaskTracker,
    stopping: CancellationToken,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandidateSide {
    Outbound,
    Inbound,
}

struct Candidate {
    diagnostic: voytty_diagnostics::Operation,
    remote_capabilities: OnceLock<voytty_core::Capabilities>,
    remote_incarnation: OnceLock<Option<ConnectionIncarnation>>,
    key: ConnectionCandidateKey,
    remote: DeviceId,
    connection: Connection,
    side: CandidateSide,
    inbound_authorization: Option<AuthorizationSnapshot>,
    verified_relay: Option<RelayHint>,
    cancel: watch::Sender<bool>,
    actor_started: AtomicBool,
    primary: AtomicBool,
    admission: CandidateAdmission,
    metrics: Arc<BrokerMetrics>,
    _connection_permit: OwnedSemaphorePermit,
    _metric: ConnectionMetricGuard,
}

impl fmt::Debug for Candidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Candidate")
            .field("key", &self.key)
            .field("remote", &self.remote)
            .field("side", &self.side)
            .field("inbound_authorization", &self.inbound_authorization)
            .field("verified_relay", &self.verified_relay)
            .finish_non_exhaustive()
    }
}

struct BrokerMetrics {
    authenticated: AtomicUsize,
    primary: AtomicUsize,
    streams: AtomicUsize,
    paths: Mutex<BTreeMap<DeviceId, PathKind>>,
    reporter: NetworkReporter,
}

struct ConnectionMetricGuard {
    metrics: Arc<BrokerMetrics>,
}

struct StreamMetricGuard {
    metrics: Arc<BrokerMetrics>,
    peer_streams: Arc<AtomicUsize>,
}

/// RAII proof that at least one consumer still wants connectivity to a peer.
pub struct ConnectionDemand {
    broker: ConnectionBroker,
    slot: Arc<PeerSlot>,
    transient_routes: Vec<RelayHint>,
    released: bool,
}

/// Address-free selected-path status for one currently promoted connection.
#[cfg(any(unix, windows))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SelectedPathObservation {
    pub(crate) path: PathKind,
    pub(crate) rtt_ms: Option<u32>,
}

/// Address-free observations bound to the exact candidate that opened a
/// service stream. Keeping this handle with the stream prevents a later
/// primary replacement from changing the path reported for an already-open
/// epoch.
#[cfg(any(unix, windows))]
#[derive(Clone)]
pub(crate) struct SelectedCandidateObserver {
    candidate: Arc<Candidate>,
}

#[cfg(any(unix, windows))]
impl SelectedCandidateObserver {
    /// Observes only the selected path class and its current RTT estimate.
    /// Remote addresses and relay URLs never cross this boundary.
    pub(crate) fn selected_path_observation(&self) -> SelectedPathObservation {
        self.candidate
            .connection
            .paths()
            .iter()
            .find(|path| path.is_selected())
            .map_or_else(SelectedPathObservation::default, |path| {
                let (kind, _) = classify_transport_addr(path.remote_addr());
                SelectedPathObservation {
                    path: kind,
                    rtt_ms: Some(round_rtt_millis(path.rtt())),
                }
            })
    }
}

/// Normal-ALPN proof that the remote receiver currently authorizes this host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationConfirmation {
    remote: DeviceId,
    generation: AuthGeneration,
    verified_relay: Option<RelayHint>,
}

impl AuthorizationConfirmation {
    /// Receiver whose normal handshake proved authorization.
    #[must_use]
    pub const fn remote(&self) -> DeviceId {
        self.remote
    }

    /// Current receiver-owned authorization generation.
    #[must_use]
    pub const fn generation(&self) -> AuthGeneration {
        self.generation
    }

    /// Relay selected by the promoted, TLS/application-authenticated candidate.
    /// Direct IP addresses are never represented here.
    #[must_use]
    pub fn verified_relay(&self) -> Option<&RelayHint> {
        self.verified_relay.as_ref()
    }
}

/// Short-lived, fully TLS-authenticated `voytty-pair/2` connection.
///
/// This owner intentionally exposes stream operations rather than the raw
/// Endpoint or normal peer registry. Its admission permit remains held until
/// drop, and network quiesce closes it even if a handshake stream is stalled.
pub struct PairConnection {
    connection: Connection,
    local: DeviceId,
    remote: DeviceId,
    verified_relay: Option<RelayHint>,
    shutdown: watch::Receiver<bool>,
    cancellation_task: Option<JoinHandle<()>>,
    _admission: PairHandshakePermit,
    closed: bool,
}

impl fmt::Debug for PairConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairConnection")
            .field("local", &self.local)
            .field("remote", &self.remote)
            .field("verified_relay", &self.verified_relay)
            .finish_non_exhaustive()
    }
}

impl PairConnection {
    fn from_authenticated(
        connection: Connection,
        local: DeviceId,
        remote: DeviceId,
        verified_relay: Option<RelayHint>,
        admission: PairHandshakePermit,
        shutdown: watch::Receiver<bool>,
        lifecycle: &BrokerLifecycle,
    ) -> Result<Self, DaemonError> {
        if *shutdown.borrow() {
            close_connection(&connection, CLOSE_SHUTTING_DOWN, b"transport stopping");
            return Err(cancelled("pairing transport is stopping"));
        }
        connection.set_max_concurrent_bi_streams(VarInt::from_u32(1));
        connection.set_max_concurrent_uni_streams(VarInt::from_u32(0));
        let mut cancellation = shutdown.clone();
        let closing = connection.clone();
        let task = lifecycle.admit_task()?;
        let cancellation_task = tokio::spawn(async move {
            let _task = task;
            if !*cancellation.borrow() {
                let _ = cancellation.changed().await;
            }
            closing.close(VarInt::from_u32(CLOSE_SHUTTING_DOWN), b"transport stopping");
        });
        Ok(Self {
            connection,
            local,
            remote,
            verified_relay,
            shutdown,
            cancellation_task: Some(cancellation_task),
            _admission: admission,
            closed: false,
        })
    }

    /// Local long-term endpoint identity.
    #[must_use]
    pub const fn local(&self) -> DeviceId {
        self.local
    }

    /// Remote identity authenticated by the completed Iroh TLS handshake.
    #[must_use]
    pub const fn remote(&self) -> DeviceId {
        self.remote
    }

    /// Relay candidate that survived TLS, if the dial used one.
    #[must_use]
    pub fn verified_relay(&self) -> Option<&RelayHint> {
        self.verified_relay.as_ref()
    }

    /// Opens the controller side's sole pairing stream before the same
    /// absolute deadline.
    pub async fn open_bi(
        &self,
        deadline: Instant,
    ) -> Result<(SendStream, RecvStream), DaemonError> {
        timeout_or_pair_shutdown(deadline, self.shutdown.clone(), self.connection.open_bi())
            .await?
            .map_err(|_| transport_unavailable("unable to open pairing stream"))
    }

    /// Accepts the host side's sole pairing stream before the same absolute
    /// deadline.
    pub async fn accept_bi(
        &self,
        deadline: Instant,
    ) -> Result<(SendStream, RecvStream), DaemonError> {
        timeout_or_pair_shutdown(deadline, self.shutdown.clone(), self.connection.accept_bi())
            .await?
            .map_err(|_| transport_unavailable("unable to accept pairing stream"))
    }

    /// Explicitly closes only this short-lived pair connection.
    pub fn close(mut self) {
        self.close_inner();
    }

    fn close_inner(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(task) = self.cancellation_task.take() {
            task.abort();
        }
        self.connection.close(
            VarInt::from_u32(CLOSE_PAIR_COMPLETE),
            b"pairing connection closed",
        );
    }
}

impl Drop for PairConnection {
    fn drop(&mut self) {
        self.close_inner();
    }
}

impl fmt::Debug for ConnectionDemand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionDemand")
            .field("remote", &self.slot.remote)
            .finish_non_exhaustive()
    }
}

/// A promoted stream plus the remote receiver generation proved by Welcome.
pub struct AuthenticatedBiStream {
    /// Send half of the Iroh bidirectional stream.
    pub send: SendStream,
    /// Receive half of the Iroh bidirectional stream.
    pub recv: RecvStream,
    remote: DeviceId,
    remote_generation: AuthGeneration,
    candidate: ConnectionCandidateKey,
    #[cfg(any(unix, windows))]
    candidate_observer: SelectedCandidateObserver,
    purpose: StreamPurpose,
    _stream_permit: OwnedSemaphorePermit,
    _metric: StreamMetricGuard,
}

impl fmt::Debug for AuthenticatedBiStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedBiStream")
            .field("remote", &self.remote)
            .field("remote_generation", &self.remote_generation)
            .field("candidate", &self.candidate)
            .field("purpose", &self.purpose)
            .finish_non_exhaustive()
    }
}

impl AuthenticatedBiStream {
    /// Handshake capabilities of the exact candidate selected for this stream.
    #[cfg(any(unix, windows))]
    pub fn remote_capabilities(&self) -> voytty_core::Capabilities {
        self.candidate_observer
            .candidate
            .remote_capabilities
            .get()
            .copied()
            .unwrap_or_default()
    }
    /// Remote device whose receiver admitted this host.
    #[must_use]
    pub const fn remote(&self) -> DeviceId {
        self.remote
    }

    /// Receiver-side authorization generation returned in ConnectionWelcome.
    #[must_use]
    pub const fn remote_generation(&self) -> AuthGeneration {
        self.remote_generation
    }

    /// Deterministic primary candidate used to open the stream.
    #[must_use]
    pub const fn candidate(&self) -> ConnectionCandidateKey {
        self.candidate
    }

    /// Cloneable observations tied to the candidate that opened this stream.
    #[cfg(any(unix, windows))]
    pub(crate) fn candidate_observer(&self) -> SelectedCandidateObserver {
        self.candidate_observer.clone()
    }

    /// Transport connection of the candidate that opened this stream.
    #[cfg(any(unix, windows))]
    pub(crate) fn connection(&self) -> &Connection {
        &self.candidate_observer.candidate.connection
    }
}

impl<T> Default for CandidateRegistry<T> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            primary: None,
        }
    }
}

impl<T> CandidateRegistry<T> {
    fn register(&mut self, key: ConnectionCandidateKey, value: T) -> Result<(), T> {
        if self.entries.contains_key(&key) {
            return Err(value);
        }
        let previous = self.entries.insert(key, CandidateEntry { value });
        debug_assert!(previous.is_none());
        Ok(())
    }

    fn mark_ready_and_decide(&mut self, key: ConnectionCandidateKey) -> CandidateDecision<T> {
        if !self.entries.contains_key(&key) {
            return CandidateDecision::Missing;
        }

        if self.primary.is_some_and(|primary| primary < key) {
            return self
                .remove(key)
                .map_or(CandidateDecision::Missing, CandidateDecision::Lost);
        }

        if designated_primary(self.entries.keys().copied()) != Some(key) {
            return CandidateDecision::Wait;
        }

        self.primary = Some(key);
        let loser_keys = self
            .entries
            .keys()
            .copied()
            .filter(|candidate| *candidate != key)
            .collect::<Vec<_>>();
        let losers = loser_keys
            .into_iter()
            .filter_map(|loser| self.remove_entry(loser))
            .collect();
        CandidateDecision::Promoted(losers)
    }

    fn primary(&self) -> Option<ConnectionCandidateKey> {
        self.primary
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn get(&self, key: &ConnectionCandidateKey) -> Option<&T> {
        self.entries.get(key).map(|entry| &entry.value)
    }

    fn values(&self) -> impl Iterator<Item = &T> {
        self.entries.values().map(|entry| &entry.value)
    }

    fn remove(&mut self, key: ConnectionCandidateKey) -> Option<T> {
        if self.primary == Some(key) {
            self.primary = None;
        }
        self.remove_entry(key)
    }

    fn remove_entry(&mut self, key: ConnectionCandidateKey) -> Option<T> {
        self.entries.remove(&key).map(|entry| entry.value)
    }

    fn take_all(&mut self) -> Vec<T> {
        self.primary = None;
        std::mem::take(&mut self.entries)
            .into_values()
            .map(|entry| entry.value)
            .collect()
    }
}

impl DemandState {
    fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
            transient_routes: Mutex::new(TransientRouteLeases::default()),
        }
    }

    fn acquire(
        &self,
        routes: Vec<RelayHint>,
        maximum_routes: usize,
    ) -> Result<(usize, Vec<RelayHint>), DaemonError> {
        let previous = increment_atomic_previous(&self.count)
            .map_err(|()| resource_exhausted("peer demand reference count exhausted"))?;
        let routes = match mutex_lock(&self.transient_routes).acquire(routes, maximum_routes) {
            Ok(routes) => routes,
            Err(error) => {
                decrement_atomic(&self.count);
                return Err(error);
            }
        };
        Ok((previous, routes))
    }

    fn release(&self, routes: &[RelayHint]) {
        mutex_lock(&self.transient_routes).release(routes);
        decrement_atomic(&self.count);
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    fn routes(&self) -> Vec<RelayHint> {
        mutex_lock(&self.transient_routes).snapshot()
    }
}

impl TransientRouteLeases {
    fn acquire(
        &mut self,
        routes: Vec<RelayHint>,
        maximum: usize,
    ) -> Result<Vec<RelayHint>, DaemonError> {
        let mut unique = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for route in routes {
            if seen.insert(route.clone()) {
                unique.push(route);
            }
        }

        let additional = unique
            .iter()
            .filter(|route| !self.ordered.iter().any(|(existing, _)| existing == *route))
            .count();
        let combined = self.ordered.len().checked_add(additional).ok_or_else(|| {
            resource_exhausted("concurrent transient route count exhausted its address space")
        })?;
        if combined > maximum {
            return Err(resource_exhausted(
                "concurrent transient route leases exceed the peer route bound",
            ));
        }
        if unique.iter().any(|route| {
            self.ordered
                .iter()
                .find(|(existing, _)| existing == route)
                .is_some_and(|(_, count)| *count == usize::MAX)
        }) {
            return Err(resource_exhausted(
                "transient route reference count exhausted",
            ));
        }

        for route in &unique {
            if let Some((_, count)) = self
                .ordered
                .iter_mut()
                .find(|(existing, _)| existing == route)
            {
                *count = count.checked_add(1).ok_or_else(|| {
                    resource_exhausted("transient route reference count exhausted")
                })?;
            } else {
                self.ordered.push((route.clone(), 1));
            }
        }
        Ok(unique)
    }

    fn release(&mut self, routes: &[RelayHint]) {
        for route in routes {
            if let Some((_, count)) = self
                .ordered
                .iter_mut()
                .find(|(existing, _)| existing == route)
            {
                if let Some(next) = count.checked_sub(1) {
                    *count = next;
                } else {
                    debug_assert!(false, "transient route lease released more than once");
                }
            }
        }
        self.ordered.retain(|(_, count)| *count > 0);
    }

    fn snapshot(&self) -> Vec<RelayHint> {
        self.ordered
            .iter()
            .map(|(route, _)| route.clone())
            .collect()
    }
}

impl BrokerAdmission {
    fn new(limits: TransportLimits) -> Self {
        Self {
            pending_dials: Arc::new(Semaphore::new(limits.max_pending_dials)),
            authenticated_connections: Arc::new(Semaphore::new(limits.max_remote_connections)),
            global_stream_handlers: Arc::new(Semaphore::new(limits.max_stream_handlers_global)),
        }
    }
}

impl PeerAdmission {
    fn new(limits: TransportLimits) -> Self {
        Self {
            open_queue: Arc::new(Semaphore::new(limits.max_open_stream_queue_per_connection)),
        }
    }
}

impl CandidateAdmission {
    fn new(limits: TransportLimits) -> Self {
        Self {
            streams: Arc::new(Semaphore::new(limits.max_bi_streams_per_connection)),
            handlers: Arc::new(Semaphore::new(limits.max_stream_handlers_per_connection)),
        }
    }
}

fn try_admit_service_handler(
    lifecycle: &BrokerLifecycle,
    connection: &Arc<Semaphore>,
    global: &Arc<Semaphore>,
) -> Result<ServiceHandlerPermits, DaemonError> {
    if lifecycle.is_quiescing() {
        return Err(transport_unavailable("network transport is stopping"));
    }
    let connection = Arc::clone(connection)
        .try_acquire_owned()
        .map_err(|_| resource_exhausted("connection handler limit reached"))?;
    let global = Arc::clone(global)
        .try_acquire_owned()
        .map_err(|_| resource_exhausted("global handler limit reached"))?;
    if lifecycle.is_quiescing() {
        return Err(transport_unavailable("network transport is stopping"));
    }
    Ok(ServiceHandlerPermits {
        _connection: connection,
        _global: global,
    })
}

async fn wait_for_service_handlers_until(
    handlers: Arc<Semaphore>,
    maximum: usize,
    deadline: Instant,
) -> Result<(), DaemonError> {
    if handlers.available_permits() == maximum {
        return Ok(());
    }
    let maximum = u32::try_from(maximum)
        .map_err(|_| resource_exhausted("global handler limit exceeds the semaphore boundary"))?;
    let permits = timeout_until(deadline, handlers.acquire_many_owned(maximum))
        .await?
        .map_err(|_| transport_unavailable("global handler admission is unavailable"))?;
    drop(permits);
    Ok(())
}

impl BrokerLifecycle {
    fn is_quiescing(&self) -> bool {
        self.tasks.is_closed()
    }

    fn begin_quiesce(&self) {
        self.tasks.close();
        self.stopping.cancel();
    }

    fn admit_task(&self) -> Result<TaskTrackerToken, DaemonError> {
        // Count the owner before checking closure. Shutdown either waits for
        // this token, or the new owner observes closure and never starts work.
        let token = self.tasks.token();
        if self.is_quiescing() {
            return Err(transport_unavailable("network transport is stopping"));
        }
        Ok(token)
    }
}

impl PeerState {
    fn claim_dial_worker(&mut self) -> bool {
        if self.dial_worker_running {
            false
        } else {
            self.dial_worker_running = true;
            true
        }
    }

    fn begin_demand_cycle(&mut self, previous_demands: usize) {
        if previous_demands == 0 {
            self.terminal_error = None;
            self.progress.clear();
        }
    }

    fn close_candidates(&mut self) -> Vec<Arc<Candidate>> {
        self.remote_acceptance = None;
        self.replacement = None;
        self.candidates.take_all()
    }
}

impl ConnectionBroker {
    pub(crate) fn with_reporter(
        identity: ConnectionIdentity,
        store: StoreHandle,
        authorization: AuthorizationRegistry,
        limits: TransportLimits,
        reporter: NetworkReporter,
        observer: NetworkObserver,
    ) -> Result<Self, DaemonError> {
        validate_limits(limits)?;
        let resolver = RouteResolver::new(store.clone(), limits)?;
        let pairing = PairHandshakeAdmission::new(limits)?;
        let (endpoint, _) = watch::channel(None);
        let metrics = Arc::new(BrokerMetrics {
            authenticated: AtomicUsize::new(0),
            primary: AtomicUsize::new(0),
            streams: AtomicUsize::new(0),
            paths: Mutex::new(BTreeMap::new()),
            reporter: reporter.clone(),
        });
        Ok(Self {
            inner: Arc::new(BrokerInner {
                identity,
                store,
                authorization,
                resolver,
                limits,
                endpoint,
                peers: Mutex::new(BTreeMap::new()),
                admission: BrokerAdmission::new(limits),
                metrics,
                observer,
                lifecycle: BrokerLifecycle::default(),
                observations: Mutex::default(),
                pairing,
                #[cfg(any(unix, windows))]
                service_handler: RemoteServiceHandlerSlot::default(),
                test_routes: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    /// Shared pair admission used by the inbound ALPN router and outbound
    /// transient dial path.
    pub(crate) fn pair_handshake_admission(&self) -> PairHandshakeAdmission {
        self.inner.pairing.clone()
    }

    /// Installs the single inbound normal-service owner before Endpoint spawn.
    #[cfg(any(unix, windows))]
    pub(crate) fn install_remote_service_handler(
        &self,
        handler: Arc<dyn RemoteServiceHandler>,
    ) -> Result<(), DaemonError> {
        if self.inner.lifecycle.is_quiescing() {
            return Err(transport_unavailable("network transport is stopping"));
        }
        self.inner.service_handler.install(handler)
    }

    /// Creates a broker around a task-private already-bound Endpoint.
    ///
    /// Production composition uses the endpoint supervisor. This constructor is
    /// intentionally reserved for deterministic real-Iroh integration tests.
    #[doc(hidden)]
    pub fn for_test(
        endpoint: Endpoint,
        identity: ConnectionIdentity,
        store: StoreHandle,
        authorization: AuthorizationRegistry,
        limits: TransportLimits,
    ) -> Result<Self, DaemonError> {
        if endpoint.id().as_bytes() != identity.device_id().as_bytes() {
            return Err(DaemonError::new(
                DomainErrorKind::IdentityStateMismatch,
                "test Endpoint identity does not match broker identity",
            ));
        }
        let (reporter, observer) = NetworkReporter::initializing(identity.device_id());
        reporter.update(|observation| {
            observation.state = crate::network::NetworkState::Bound;
            observation.endpoint_bound = true;
            observation.bind_attempts = 1;
            observation.publish = crate::network::AddressServiceState::Configured;
            observation.lookup = crate::network::AddressServiceState::Configured;
        });
        let broker =
            Self::with_reporter(identity, store, authorization, limits, reporter, observer)?;
        broker.inner.endpoint.send_replace(Some(endpoint));
        Ok(broker)
    }

    /// Adds one explicit address for a task-private real-Iroh dial fixture.
    ///
    /// The override is never persisted, merged into address lookup, or exposed
    /// by status. Production code has no call site for this hook.
    #[doc(hidden)]
    pub fn set_test_route(
        &self,
        remote: DeviceId,
        address: EndpointAddr,
    ) -> Result<(), DaemonError> {
        if address.id != endpoint_id_from_device(remote)? {
            return Err(DaemonError::new(
                DomainErrorKind::AddressUnavailable,
                "test route endpoint ID does not match its device key",
            ));
        }
        mutex_lock(&self.inner.test_routes).insert(remote, address);
        Ok(())
    }

    /// Returns the shared passive network observation.
    #[must_use]
    pub fn observe(&self) -> NetworkObserver {
        self.inner.observer.clone()
    }

    /// Returns redacted live state for one peer.
    pub async fn peer_observation(&self, remote: DeviceId) -> PeerConnectionObservation {
        let Some(slot) = mutex_lock(&self.inner.peers).get(&remote).cloned() else {
            return PeerConnectionObservation {
                primary: None,
                candidate_count: 0,
                demand_count: 0,
                active_stream_count: 0,
                remote_acceptance_generation: None,
                path: PathKind::Unknown,
            };
        };
        let state = slot.state.lock().await;
        PeerConnectionObservation {
            primary: state.candidates.primary(),
            candidate_count: state.candidates.len(),
            demand_count: slot.demand.count(),
            active_stream_count: bounded_u32(slot.active_streams.load(Ordering::Acquire)),
            remote_acceptance_generation: state.remote_acceptance,
            path: self.inner.metrics.path(remote),
        }
    }

    /// Acquires one demand for a durable outbound known device.
    pub async fn demand(
        &self,
        remote: DeviceId,
        deadline: Instant,
    ) -> Result<ConnectionDemand, DaemonError> {
        if self.inner.lifecycle.is_quiescing() {
            return Err(transport_unavailable("network transport is stopping"));
        }
        let known = self
            .inner
            .store
            .run_blocking_until(deadline, move |store, deadline| {
                store.known_device(remote, deadline)
            })
            .await?;
        if known.is_none() {
            return Err(DaemonError::new(
                DomainErrorKind::DeviceNotFound,
                "target is not an outbound known device",
            ));
        }
        self.acquire_demand(remote, Vec::new()).await
    }

    /// Acquires one short-lived demand using validated pairing-ticket routes.
    pub async fn demand_transient(
        &self,
        remote: DeviceId,
        routes: Vec<RelayHint>,
    ) -> Result<ConnectionDemand, DaemonError> {
        if self.inner.lifecycle.is_quiescing() {
            return Err(transport_unavailable("network transport is stopping"));
        }
        if routes.is_empty() || routes.len() > self.inner.limits.max_relay_hints {
            return Err(DaemonError::new(
                DomainErrorKind::AddressUnavailable,
                "transient target requires a bounded relay route",
            ));
        }
        self.acquire_demand(remote, routes).await
    }

    /// Establishes one short-lived pair-ALPN connection without touching the
    /// normal peer registry, primary metrics, route cache, or profile.
    pub(crate) async fn connect_pair_transient(
        &self,
        remote: DeviceId,
        routes: Vec<RelayHint>,
        deadline: Instant,
    ) -> Result<PairConnection, DaemonError> {
        if self.inner.lifecycle.is_quiescing() {
            return Err(cancelled("pairing transport is stopping"));
        }
        if remote == self.inner.identity.device_id() {
            return Err(DaemonError::new(
                DomainErrorKind::Unauthorized,
                "a device cannot pair with its own endpoint identity",
            ));
        }
        if routes.is_empty() || routes.len() > self.inner.limits.max_relay_hints {
            return Err(DaemonError::new(
                DomainErrorKind::AddressUnavailable,
                "pairing target requires a bounded relay route",
            ));
        }

        let admission = self.inner.pairing.acquire(remote, deadline).await?;
        let _dial_permit = timeout_or_pair_shutdown(
            deadline,
            self.inner.pairing.subscribe(),
            self.inner.admission.pending_dials.clone().acquire_owned(),
        )
        .await?
        .map_err(|_| transport_unavailable("global outbound dial admission is unavailable"))?;
        let endpoint = self
            .inner
            .endpoint
            .borrow()
            .clone()
            .ok_or_else(|| transport_unavailable("Iroh Endpoint is not bound"))?;
        if endpoint.id().as_bytes() != self.inner.identity.device_id().as_bytes() {
            return Err(DaemonError::new(
                DomainErrorKind::IdentityStateMismatch,
                "bound Endpoint does not own the broker identity",
            ));
        }

        let dial_routes = timeout_or_pair_shutdown(
            deadline,
            self.inner.pairing.subscribe(),
            self.dial_routes(&endpoint, remote, &routes, deadline),
        )
        .await??;
        let mut last_error = None;
        for route in dial_routes {
            if Instant::now() >= deadline {
                return Err(deadline_exceeded("pairing dial deadline elapsed"));
            }
            let attempt_deadline = deadline.min(
                Instant::now()
                    .checked_add(self.inner.limits.connect_attempt_budget)
                    .unwrap_or(deadline),
            );
            let result = timeout_or_pair_shutdown(
                attempt_deadline,
                self.inner.pairing.subscribe(),
                connect_transport(&endpoint, route.address, VOYTTY_PAIR_ALPN, attempt_deadline),
            )
            .await;
            let connection = match result {
                Ok(Ok(connection)) => connection,
                Ok(Err(error)) => {
                    last_error = Some(error);
                    continue;
                }
                Err(error) if should_try_next_pair_route(&error, deadline) => {
                    last_error = Some(error);
                    continue;
                }
                Err(error) => return Err(error),
            };
            if let Err(error) = validate_pair_tls_connection(&connection, remote) {
                close_connection(&connection, CLOSE_INCOMPATIBLE, b"identity mismatch");
                return Err(error);
            }
            let verified_relay = selected_relay(&connection);
            return PairConnection::from_authenticated(
                connection,
                self.inner.identity.device_id(),
                remote,
                verified_relay,
                admission,
                self.inner.pairing.subscribe(),
                &self.inner.lifecycle,
            );
        }

        Err(last_error.unwrap_or_else(|| {
            DaemonError::new(
                DomainErrorKind::AddressUnavailable,
                "no route candidate established a pairing connection",
            )
        }))
    }

    /// Wraps one fully awaited inbound pair connection without registering a
    /// normal candidate or changing transport metrics.
    pub(crate) fn pair_from_incoming(
        &self,
        connection: Connection,
        admission: PairHandshakePermit,
    ) -> Result<PairConnection, DaemonError> {
        let remote = device_from_endpoint_id(connection.remote_id());
        if let Err(error) = validate_pair_tls_connection(&connection, remote) {
            close_connection(&connection, CLOSE_INCOMPATIBLE, b"identity mismatch");
            return Err(error);
        }
        if remote == self.inner.identity.device_id() {
            close_connection(&connection, CLOSE_UNAUTHORIZED, b"not authorized");
            return Err(DaemonError::new(
                DomainErrorKind::Unauthorized,
                "a device cannot pair with its own endpoint identity",
            ));
        }
        let verified_relay = selected_relay(&connection);
        PairConnection::from_authenticated(
            connection,
            self.inner.identity.device_id(),
            remote,
            verified_relay,
            admission,
            self.inner.pairing.subscribe(),
            &self.inner.lifecycle,
        )
    }

    async fn acquire_demand(
        &self,
        remote: DeviceId,
        transient_routes: Vec<RelayHint>,
    ) -> Result<ConnectionDemand, DaemonError> {
        if remote == self.inner.identity.device_id() {
            return Err(DaemonError::new(
                DomainErrorKind::TransportUnavailable,
                "the local device cannot be reached through its own broker",
            ));
        }
        let slot = self.peer_slot(remote)?;
        let (previous_demands, transient_routes) = slot
            .demand
            .acquire(transient_routes, self.inner.limits.max_relay_hints)?;
        // Construct the RAII owner before the first await. Cancellation of this
        // future therefore cannot leak the demand count or ticket routes.
        let demand = ConnectionDemand {
            broker: self.clone(),
            slot: Arc::clone(&slot),
            transient_routes,
            released: false,
        };
        slot.state.lock().await.begin_demand_cycle(previous_demands);
        self.ensure_dial_worker(Arc::clone(&slot)).await?;
        slot.changed.notify_waiters();
        Ok(demand)
    }

    fn peer_slot(&self, remote: DeviceId) -> Result<Arc<PeerSlot>, DaemonError> {
        let mut peers = mutex_lock(&self.inner.peers);
        if peers.is_empty() {
            self.start_peer_cleanup()?;
        } else if self.inner.lifecycle.is_quiescing() {
            return Err(transport_unavailable("network transport is stopping"));
        }
        Ok(peers
            .entry(remote)
            .or_insert_with(|| {
                Arc::new(PeerSlot {
                    remote,
                    state: AsyncMutex::new(PeerState::default()),
                    changed: Notify::new(),
                    demand: DemandState::new(),
                    admission: PeerAdmission::new(self.inner.limits),
                    active_streams: Arc::new(AtomicUsize::new(0)),
                })
            })
            .clone())
    }

    async fn ensure_dial_worker(&self, slot: Arc<PeerSlot>) -> Result<(), DaemonError> {
        if self.inner.lifecycle.is_quiescing() {
            return Err(transport_unavailable("network transport is stopping"));
        }
        let mut state = slot.state.lock().await;
        let task = self.inner.lifecycle.admit_task()?;
        if state.claim_dial_worker() {
            let broker = self.clone();
            drop(state);
            tokio::spawn(async move {
                let _task = task;
                broker.run_dial_worker(slot).await;
            });
        }
        Ok(())
    }

    /// Registers a fully TLS-authenticated inbound normal connection.
    ///
    /// Authorization is checked before this function accepts or reads the
    /// unique Hello stream.
    pub async fn accept_normal(&self, connection: Connection) -> Result<(), DaemonError> {
        if connection.alpn() != VOYTTY_ALPN {
            close_connection(&connection, CLOSE_INCOMPATIBLE, b"incompatible protocol");
            return Err(DaemonError::new(
                DomainErrorKind::WireMajorMismatch,
                "incoming connection negotiated an unexpected ALPN",
            ));
        }
        configure_normal_connection_limits(&connection, self.inner.limits);
        let remote = device_from_endpoint_id(connection.remote_id());
        let admission = match admit_inbound_before_payload(&self.inner.authorization, remote) {
            Ok(admission) => admission,
            Err(error) => {
                close_connection(&connection, CLOSE_UNAUTHORIZED, b"not authorized");
                return Err(error);
            }
        };
        let connection_permit = self
            .inner
            .admission
            .authenticated_connections
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                close_connection(&connection, CLOSE_OVERLOADED, b"transport overloaded");
                resource_exhausted("authenticated connection limit reached")
            })?;
        let _task = self.inner.lifecycle.admit_task()?;
        tokio::select! {
            biased;
            () = self.inner.lifecycle.stopping.cancelled() => Err(cancelled("network transport is stopping")),
            result = self.run_inbound_handshake(connection, remote, admission, connection_permit) => result,
        }
    }

    /// Replaces the current Endpoint after a supervisor bind succeeds.
    pub(crate) async fn attach_endpoint(&self, endpoint: Endpoint) -> Result<(), DaemonError> {
        if self.inner.lifecycle.is_quiescing() {
            endpoint.close().await;
            return Err(transport_unavailable("network transport is stopping"));
        }
        if endpoint.id().as_bytes() != self.inner.identity.device_id().as_bytes() {
            endpoint.close().await;
            return Err(DaemonError::new(
                DomainErrorKind::IdentityStateMismatch,
                "bound Endpoint identity changed across retry",
            ));
        }
        self.inner.endpoint.send_replace(Some(endpoint));
        self.clear_retryable_errors().await;
        self.wake_all_peers();
        Ok(())
    }

    /// Removes a lost Endpoint and closes only transport state.
    pub(crate) async fn detach_endpoint(&self) {
        self.inner.endpoint.send_replace(None);
        self.close_all(ConnectionCloseReason::EndpointReset).await;
        self.wake_all_peers();
    }

    /// Permanently refuses new work and closes every current connection.
    pub async fn quiesce(&self) {
        let deadline = Instant::now() + self.inner.limits.first_frame_deadline;
        let _ = self.quiesce_until(deadline).await;
    }

    /// Permanently refuses new work and reclaims every admitted service
    /// handler before the caller closes the owning Endpoint.
    pub(crate) async fn quiesce_until(&self, deadline: Instant) -> Result<(), DaemonError> {
        self.inner.lifecycle.begin_quiesce();
        self.inner.pairing.begin_quiesce();
        self.inner.endpoint.send_replace(None);
        timeout_until(
            deadline,
            self.close_all(ConnectionCloseReason::ShuttingDown),
        )
        .await?;
        self.wake_all_peers();
        wait_for_service_handlers_until(
            Arc::clone(&self.inner.admission.global_stream_handlers),
            self.inner.limits.max_stream_handlers_global,
            deadline,
        )
        .await?;
        timeout_until(deadline, self.inner.lifecycle.tasks.wait()).await?;
        mutex_lock(&self.inner.peers).clear();
        Ok(())
    }

    /// Closes all current candidates for one endpoint without touching Session state.
    pub async fn close_remote(&self, remote: DeviceId, reason: ConnectionCloseReason) {
        let Some(slot) = mutex_lock(&self.inner.peers).get(&remote).cloned() else {
            return;
        };
        let candidates = {
            let mut state = slot.state.lock().await;
            state.close_candidates()
        };
        for candidate in candidates {
            candidate.set_primary(false, reason.code());
            candidate.cancel(reason);
        }
        slot.changed.notify_waiters();
    }

    async fn close_all(&self, reason: ConnectionCloseReason) {
        let slots = mutex_lock(&self.inner.peers)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for slot in slots {
            self.close_remote(slot.remote, reason).await;
        }
    }

    fn wake_all_peers(&self) {
        for slot in mutex_lock(&self.inner.peers).values() {
            slot.changed.notify_waiters();
        }
    }

    async fn clear_retryable_errors(&self) {
        let slots = mutex_lock(&self.inner.peers)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for slot in slots {
            let mut state = slot.state.lock().await;
            if state
                .terminal_error
                .as_ref()
                .is_some_and(|error| error.kind() == DomainErrorKind::TransportUnavailable)
            {
                state.terminal_error = None;
            }
        }
    }
}

struct DialRoute {
    address: EndpointAddr,
}

impl ConnectionBroker {
    async fn connection_identity(&self, slot: &PeerSlot) -> ConnectionIdentity {
        self.inner.identity.clone().with_incarnation(
            slot.state
                .lock()
                .await
                .local_incarnation
                .expect("registered candidate has a local connection generation"),
        )
    }

    async fn observe_incarnation(
        &self,
        slot: &Arc<PeerSlot>,
        candidate: &Arc<Candidate>,
        incarnation: Option<ConnectionIncarnation>,
    ) -> Result<(), DaemonError> {
        let _ = candidate.remote_incarnation.set(incarnation);
        let Some(incarnation) = incarnation else {
            return Ok(());
        };
        let retired = {
            let mut state = slot.state.lock().await;
            if !state.incarnation.observe(
                incarnation,
                Instant::now(),
                self.inner.limits.first_frame_deadline + self.inner.limits.connect_attempt_budget,
                self.inner.limits.max_remote_connections,
            )? {
                return Ok(());
            }
            state.remote_acceptance = None;
            let keys: Vec<_> = state
                .candidates
                .entries
                .iter()
                .filter_map(|(key, entry)| {
                    entry
                        .value
                        .remote_incarnation
                        .get()
                        .is_some_and(|remote| *remote != Some(incarnation))
                        .then_some(*key)
                })
                .collect();
            keys.into_iter()
                .filter_map(|key| state.candidates.remove(key))
                .collect::<Vec<_>>()
        };
        for old in retired {
            old.set_primary(false, "peer_restarted");
            old.cancel(ConnectionCloseReason::EndpointReset);
        }
        slot.changed.notify_waiters();
        Ok(())
    }

    async fn register_candidate(
        &self,
        slot: &Arc<PeerSlot>,
        candidate: Arc<Candidate>,
    ) -> Result<(), DaemonError> {
        let mut state = slot.state.lock().await;
        if self.inner.lifecycle.is_quiescing() {
            candidate.cancel(ConnectionCloseReason::ShuttingDown);
            return Err(transport_unavailable("network transport is stopping"));
        }
        if state.candidates.is_empty() {
            // Reconnect can also follow a one-sided QUIC timeout without a
            // process restart. The remote must not retain a primary that this
            // owner can no longer reuse. Concurrent/reverse candidates share
            // this generation and still use deterministic duplicate arbitration.
            state.local_incarnation = Some(ConnectionIncarnation::from_array(
                random_attempt_id()?.to_bytes(),
            ));
        }
        if let Err(duplicate) = state.candidates.register(candidate.key, candidate) {
            duplicate.cancel(ConnectionCloseReason::Duplicate);
            return Err(transport_unavailable(
                "duplicate candidate key was registered",
            ));
        }
        drop(state);
        slot.changed.notify_waiters();
        Ok(())
    }

    async fn promote_candidate(
        &self,
        slot: Arc<PeerSlot>,
        candidate: Arc<Candidate>,
        deadline: Instant,
    ) -> Result<(), DaemonError> {
        loop {
            let changed = slot.changed.notified();
            let outcome = {
                let mut state = slot.state.lock().await;
                state.candidates.mark_ready_and_decide(candidate.key)
            };

            match outcome {
                CandidateDecision::Promoted(losers) => {
                    candidate.set_primary(true, "promoted");
                    for loser in losers {
                        loser.set_primary(false, "duplicate");
                        loser.cancel(ConnectionCloseReason::Duplicate);
                    }
                    slot.changed.notify_waiters();
                    self.start_connection_actor(Arc::clone(&slot), candidate)?;
                    return Ok(());
                }
                CandidateDecision::Lost(loser) => {
                    loser.cancel(ConnectionCloseReason::Duplicate);
                    slot.changed.notify_waiters();
                    return Err(transport_unavailable(
                        "connection candidate lost duplicate arbitration",
                    ));
                }
                CandidateDecision::Wait => {
                    if let Err(error) = timeout_until(deadline, changed).await {
                        // A lower provisional candidate must never leave this
                        // ready connection retained forever if arbitration
                        // cannot finish before its bounded deadline.
                        self.remove_candidate(
                            &slot,
                            candidate.key,
                            ConnectionCloseReason::EndpointReset,
                        )
                        .await;
                        return Err(error);
                    }
                }
                CandidateDecision::Missing => {
                    return Err(transport_unavailable(
                        "connection candidate lost duplicate arbitration",
                    ));
                }
            }
        }
    }

    fn start_connection_actor(
        &self,
        slot: Arc<PeerSlot>,
        candidate: Arc<Candidate>,
    ) -> Result<(), DaemonError> {
        let task = self.inner.lifecycle.admit_task()?;
        if candidate.actor_started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let broker = self.clone();
        tokio::spawn(async move {
            let _task = task;
            broker.run_connection_actor(slot, candidate).await;
        });
        Ok(())
    }

    async fn run_connection_actor(&self, slot: Arc<PeerSlot>, candidate: Arc<Candidate>) {
        let mut cancel = candidate.cancel.subscribe();
        let mut auth_changes = self
            .inner
            .authorization
            .admit(candidate.remote)
            .ok()
            .and_then(|admission| {
                candidate
                    .inbound_authorization
                    .is_none_or(|accepted| accepted == admission.snapshot)
                    .then_some((admission.snapshot, admission.changes))
            });
        if *cancel.borrow() || (candidate.side == CandidateSide::Inbound && auth_changes.is_none())
        {
            candidate.cancel(ConnectionCloseReason::Unauthorized);
            self.candidate_closed(&slot, candidate.key).await;
            return;
        }
        let mut path_events = candidate.connection.path_events();
        let mut handlers = JoinSet::new();
        self.observe_current_path(&candidate);

        loop {
            tokio::select! {
                biased;
                changed = cancel.changed() => {
                    if changed.is_err() || *cancel.borrow() {
                        break;
                    }
                }
                stale = wait_for_authorization_change(&mut auth_changes) => {
                    if stale {
                        candidate.cancel(ConnectionCloseReason::Unauthorized);
                        break;
                    }
                }
                _ = candidate.connection.closed() => break,
                joined = handlers.join_next(), if !handlers.is_empty() => {
                    let _ = joined;
                }
                event = path_events.next() => {
                    match event {
                        Some(event) => self.observe_path_event(&candidate, event),
                        None => break,
                    }
                }
                accepted = candidate.connection.accept_bi() => {
                    let Ok((mut send, mut recv)) = accepted else {
                        break;
                    };
                    match try_admit_service_handler(
                        &self.inner.lifecycle,
                        &candidate.admission.handlers,
                        &self.inner.admission.global_stream_handlers,
                    ) {
                        Ok(handler_permits) => {
                            let metric = StreamMetricGuard::new(
                                Arc::clone(&self.inner.metrics),
                                Arc::clone(&slot.active_streams),
                            );
                            let Ok(metric) = metric else {
                                reject_stream(&mut send, &mut recv, b"stream overloaded");
                                continue;
                            };
                            let broker = self.clone();
                            let remote = candidate.remote;
                            let connection_generation = candidate
                                .inbound_authorization
                                .map(|snapshot| snapshot.generation);
                            handlers.spawn(async move {
                                let _handler_permits = handler_permits;
                                let _metric = metric;
                                broker
                                    .handle_service_stream(
                                        remote,
                                        connection_generation,
                                        send,
                                        recv,
                                    )
                                    .await;
                            });
                        }
                        Err(_) => reject_stream(&mut send, &mut recv, b"stream overloaded"),
                    }
                }
            }
        }
        handlers.abort_all();
        while handlers.join_next().await.is_some() {}
        self.candidate_closed(&slot, candidate.key).await;
    }

    async fn handle_service_stream(
        &self,
        remote: DeviceId,
        connection_generation: Option<AuthGeneration>,
        mut send: SendStream,
        mut recv: RecvStream,
    ) {
        if self.inner.lifecycle.is_quiescing() {
            reject_stream(&mut send, &mut recv, b"transport stopping");
            return;
        }
        // Receiver-side directionality: an outbound-only known device is not
        // implicitly authorized merely because QUIC permits reverse streams.
        let accepted_generation = match receiver_generation_for_stream(
            &self.inner.authorization,
            remote,
            connection_generation,
        ) {
            Ok(generation) => generation,
            Err(_) => {
                reject_stream(&mut send, &mut recv, b"not authorized");
                return;
            }
        };
        let deadline = Instant::now() + self.inner.limits.first_frame_deadline;
        #[cfg(any(unix, windows))]
        if let Some(handler) = self.inner.service_handler.get() {
            if self.inner.lifecycle.is_quiescing() {
                reject_stream(&mut send, &mut recv, b"transport stopping");
                return;
            }
            let stream = InboundAuthenticatedStream {
                send,
                recv,
                remote_device_id: remote,
                accepted_generation,
            };
            let operation = voytty_diagnostics::Operation::default();
            if let Err(error) = handler.handle_service_stream(stream, deadline).await {
                voytty_diagnostics::record(
                    voytty_diagnostics::Event::new(voytty_diagnostics::Kind::RequestFailed)
                        .operation(&operation)
                        .level(if error.kind() == DomainErrorKind::Cancelled {
                            voytty_diagnostics::Level::Info
                        } else {
                            voytty_diagnostics::Level::Warn
                        })
                        .error(error.kind()),
                );
            }
            return;
        }
        #[cfg(not(any(unix, windows)))]
        let _ = accepted_generation;

        // Isolated M5-M6 composition intentionally retains the typed fallback.
        // Only this no-handler branch owns its compatibility pre-read.
        let frame = match read_service_frame(&mut recv, deadline).await {
            Ok(frame) => frame,
            Err(_) => {
                reject_stream(&mut send, &mut recv, b"invalid service frame");
                return;
            }
        };
        if !is_remote_service_kind(frame.kind) {
            reject_stream(&mut send, &mut recv, b"invalid service kind");
            return;
        }
        let response = v2::ServiceError {
            code: DomainErrorKind::ServiceNotImplemented.code().to_owned(),
            message: "remote Session service is not implemented in M5-M6".to_owned(),
        };
        let bytes = match encode_message(
            WireKind::ServiceErrorResponse,
            frame.request_id,
            0,
            &response,
        ) {
            Ok(bytes) => bytes,
            Err(_) => {
                reject_stream(&mut send, &mut recv, b"service response unavailable");
                return;
            }
        };
        if timeout_until(deadline, send.write_all(&bytes))
            .await
            .is_ok_and(|result| result.is_ok())
        {
            let _ = send.finish();
        } else {
            reject_stream(&mut send, &mut recv, b"service response failed");
        }
    }

    async fn candidate_closed(&self, slot: &Arc<PeerSlot>, key: ConnectionCandidateKey) {
        let removed = {
            let mut state = slot.state.lock().await;
            let removed = state.candidates.remove(key);
            if state.candidates.is_empty() {
                state.remote_acceptance = None;
            }
            removed
        };
        if let Some(candidate) = removed {
            candidate.set_primary(false, "transport_closed");
            candidate.cancel(ConnectionCloseReason::EndpointReset);
        }
        slot.changed.notify_waiters();
        if slot.demand.count() > 0 && !self.inner.lifecycle.is_quiescing() {
            let _ = self.ensure_dial_worker(Arc::clone(slot)).await;
        }
    }

    async fn remove_candidate(
        &self,
        slot: &Arc<PeerSlot>,
        key: ConnectionCandidateKey,
        reason: ConnectionCloseReason,
    ) {
        let removed = {
            let mut state = slot.state.lock().await;
            let removed = state.candidates.remove(key);
            if state.candidates.is_empty() {
                state.remote_acceptance = None;
            }
            removed
        };
        if let Some(candidate) = removed {
            candidate.set_primary(false, reason.code());
            candidate.cancel(reason);
        }
        slot.changed.notify_waiters();
    }

    fn observe_current_path(&self, candidate: &Candidate) {
        if !candidate.primary.load(Ordering::Acquire) {
            return;
        }
        let (kind, relay) = candidate
            .connection
            .paths()
            .iter()
            .find(|path| path.is_selected())
            .map_or((PathKind::Unknown, None), |path| {
                classify_transport_addr(path.remote_addr())
            });
        self.record_path(candidate, kind, relay);
    }

    fn record_path(&self, candidate: &Candidate, kind: PathKind, relay: Option<RelayHint>) {
        self.inner.metrics.set_path(candidate.remote, kind);
        if relay.is_some() {
            self.queue_observation(candidate.remote, false, relay);
        }
    }

    fn observe_path_event(&self, candidate: &Candidate, event: PathEvent) {
        if !candidate.primary.load(Ordering::Acquire) {
            return;
        }
        match event {
            PathEvent::Selected { remote_addr, .. } => {
                let (kind, relay) = classify_transport_addr(&remote_addr);
                self.record_path(candidate, kind, relay);
            }
            PathEvent::Closed { .. } | PathEvent::Opened { .. } | PathEvent::Lagged { .. } => {
                self.observe_current_path(candidate);
            }
            _ => self.observe_current_path(candidate),
        }
    }
}

impl Candidate {
    #[allow(clippy::too_many_arguments)]
    fn new(
        key: ConnectionCandidateKey,
        remote: DeviceId,
        connection: Connection,
        side: CandidateSide,
        inbound_authorization: Option<AuthorizationSnapshot>,
        verified_relay: Option<RelayHint>,
        connection_permit: OwnedSemaphorePermit,
        metrics: Arc<BrokerMetrics>,
        limits: TransportLimits,
    ) -> Result<Arc<Self>, DaemonError> {
        let metric = ConnectionMetricGuard::new(Arc::clone(&metrics))?;
        let (cancel, _) = watch::channel(false);
        Ok(Arc::new(Self {
            diagnostic: voytty_diagnostics::Operation::default(),
            remote_capabilities: OnceLock::new(),
            remote_incarnation: OnceLock::new(),
            key,
            remote,
            connection,
            side,
            inbound_authorization,
            verified_relay,
            cancel,
            actor_started: AtomicBool::new(false),
            primary: AtomicBool::new(false),
            admission: CandidateAdmission::new(limits),
            metrics,
            _connection_permit: connection_permit,
            _metric: metric,
        }))
    }

    fn set_primary(&self, primary: bool, reason: &'static str) {
        let previous = self.primary.swap(primary, Ordering::AcqRel);
        if previous == primary {
            return;
        }
        if primary {
            if increment_atomic(&self.metrics.primary).is_err() {
                self.primary.store(false, Ordering::Release);
                return;
            }
            self.metrics.set_path(self.remote, PathKind::Unknown);
        } else {
            decrement_atomic(&self.metrics.primary);
            self.metrics.remove_path(self.remote);
            voytty_client::handshake::record_transport_observation(
                &self.diagnostic,
                &self.connection,
                voytty_diagnostics::Stage::RetiringTransport,
                None,
            );
        }
        self.metrics.publish();
        tracing::info!(
            component = "connection",
            connection = self.diagnostic.id(),
            operation = if primary {
                "primary_established"
            } else {
                "primary_closed"
            },
            reason,
            "Primary connection changed"
        );
    }

    fn cancel(&self, reason: ConnectionCloseReason) {
        self.cancel.send_replace(true);
        let (code, message) = reason.wire();
        self.connection.close(code, message);
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        if self.primary.swap(false, Ordering::AcqRel) {
            decrement_atomic(&self.metrics.primary);
            self.metrics.remove_path(self.remote);
            self.metrics.publish();
        }
    }
}

impl ConnectionMetricGuard {
    fn new(metrics: Arc<BrokerMetrics>) -> Result<Self, DaemonError> {
        increment_atomic(&metrics.authenticated).map_err(|()| {
            resource_exhausted("network observation counter exhausted its address space")
        })?;
        metrics.publish();
        Ok(Self { metrics })
    }
}

impl Drop for ConnectionMetricGuard {
    fn drop(&mut self) {
        decrement_atomic(&self.metrics.authenticated);
        self.metrics.publish();
    }
}

impl StreamMetricGuard {
    fn new(
        metrics: Arc<BrokerMetrics>,
        peer_streams: Arc<AtomicUsize>,
    ) -> Result<Self, DaemonError> {
        increment_atomic(&peer_streams).map_err(|()| {
            resource_exhausted("peer stream observation counter exhausted its address space")
        })?;
        if increment_atomic(&metrics.streams).is_err() {
            decrement_atomic(&peer_streams);
            return Err(resource_exhausted(
                "network observation counter exhausted its address space",
            ));
        }
        metrics.publish();
        Ok(Self {
            metrics,
            peer_streams,
        })
    }
}

impl Drop for StreamMetricGuard {
    fn drop(&mut self) {
        decrement_atomic(&self.peer_streams);
        decrement_atomic(&self.metrics.streams);
        self.metrics.publish();
    }
}

impl BrokerMetrics {
    fn publish(&self) {
        let (direct, relay) = {
            let paths = mutex_lock(&self.paths);
            (
                paths
                    .values()
                    .filter(|kind| **kind == PathKind::Direct)
                    .count(),
                paths
                    .values()
                    .filter(|kind| **kind == PathKind::Relay)
                    .count(),
            )
        };
        self.reporter.transport_metrics(
            self.authenticated.load(Ordering::Acquire),
            self.primary.load(Ordering::Acquire),
            self.streams.load(Ordering::Acquire),
            direct,
            relay,
        );
    }

    fn set_path(&self, remote: DeviceId, kind: PathKind) {
        mutex_lock(&self.paths).insert(remote, kind);
        self.publish();
    }

    fn remove_path(&self, remote: DeviceId) {
        mutex_lock(&self.paths).remove(&remote);
        self.publish();
    }

    fn path(&self, remote: DeviceId) -> PathKind {
        mutex_lock(&self.paths)
            .get(&remote)
            .copied()
            .unwrap_or_default()
    }
}

async fn read_handshake_frame(
    recv: &mut RecvStream,
    maximum_body_bytes: usize,
    deadline: Instant,
) -> Result<DecodedFrame, DaemonError> {
    read_one_frame(recv, maximum_body_bytes, deadline).await
}

async fn read_service_frame(
    recv: &mut RecvStream,
    deadline: Instant,
) -> Result<DecodedFrame, DaemonError> {
    read_one_frame(recv, voytty_proto::MAX_FRAME_BYTES, deadline).await
}

fn reject_stream(send: &mut SendStream, recv: &mut RecvStream, _reason: &'static [u8]) {
    let code = VarInt::from_u32(STREAM_REJECTED);
    let _ = recv.stop(code);
    let _ = send.reset(code);
}

fn close_connection(connection: &Connection, code: u32, reason: &'static [u8]) {
    connection.close(VarInt::from_u32(code), reason);
}

fn admit_inbound_before_payload(
    authorization: &AuthorizationRegistry,
    remote: DeviceId,
) -> Result<crate::authorization::Admission, DaemonError> {
    authorization.admit(remote)
}

fn recheck_inbound_admission(
    authorization: &AuthorizationRegistry,
    remote: DeviceId,
    accepted: voytty_core::AuthorizationSnapshot,
) -> Result<crate::authorization::Admission, DaemonError> {
    let current = authorization.admit(remote)?;
    if current.snapshot != accepted {
        return Err(DaemonError::new(
            DomainErrorKind::Unauthorized,
            "device is not authorized to control this host",
        ));
    }
    Ok(current)
}

fn receiver_generation_for_stream(
    authorization: &AuthorizationRegistry,
    remote: DeviceId,
    connection_generation: Option<AuthGeneration>,
) -> Result<AuthGeneration, DaemonError> {
    let current = authorization.admit(remote)?;
    if connection_generation.is_some_and(|accepted| accepted != current.snapshot.generation) {
        return Err(DaemonError::new(
            DomainErrorKind::Unauthorized,
            "device is not authorized to control this host",
        ));
    }
    Ok(current.snapshot.generation)
}

async fn wait_for_authorization_change(
    admission: &mut Option<(
        voytty_core::AuthorizationSnapshot,
        watch::Receiver<voytty_core::AuthorizationSnapshot>,
    )>,
) -> bool {
    let Some((accepted, changes)) = admission else {
        return pending::<bool>().await;
    };
    match changes.changed().await {
        Ok(()) => {
            let current = *changes.borrow_and_update();
            current != *accepted || current.status != AuthorizationStatus::Authorized
        }
        Err(_) => true,
    }
}

fn is_remote_service_kind(kind: WireKind) -> bool {
    matches!(
        kind,
        WireKind::SessionListRequest
            | WireKind::SessionCreateRequest
            | WireKind::SessionRenameRequest
            | WireKind::SessionCloseRequest
            | WireKind::SessionTakeoverRequest
            | WireKind::SessionOperationLeaseRequest
            | WireKind::TerminalAttachRequest
            | WireKind::TerminalInput
            | WireKind::TerminalResize
            | WireKind::TerminalBaseColors
            | WireKind::TerminalDetach
            | WireKind::TerminalSnapshotApplied
            | WireKind::TerminalSyncRequest
            | WireKind::TerminalHistoryWindowRequest
    )
}

fn validate_pair_tls_connection(
    connection: &Connection,
    expected_remote: DeviceId,
) -> Result<(), DaemonError> {
    validate_pair_tls_metadata(
        expected_remote,
        device_from_endpoint_id(connection.remote_id()),
        connection.alpn(),
    )
}

fn validate_pair_tls_metadata(
    expected_remote: DeviceId,
    authenticated_remote: DeviceId,
    alpn: &[u8],
) -> Result<(), DaemonError> {
    if alpn != VOYTTY_PAIR_ALPN {
        return Err(DaemonError::new(
            DomainErrorKind::WireMajorMismatch,
            "pairing connection negotiated an unexpected ALPN",
        ));
    }
    if authenticated_remote != expected_remote {
        return Err(DaemonError::new(
            DomainErrorKind::Unauthorized,
            "connected Iroh identity did not match the pairing ticket host",
        ));
    }
    Ok(())
}

fn should_try_next_pair_route(error: &DaemonError, overall_deadline: Instant) -> bool {
    Instant::now() < overall_deadline
        && matches!(
            error.kind(),
            DomainErrorKind::TransportUnavailable | DomainErrorKind::DeadlineExceeded
        )
}

/// Exercises the broker's real candidate registry under one of the two
/// opposite duplicate-arrival schedules without opening a socket.
#[doc(hidden)]
pub fn duplicate_connection_test_evidence(
    lower: ConnectionCandidateKey,
    higher: ConnectionCandidateKey,
    higher_arrives_first: bool,
) -> Result<DuplicateConnectionTestEvidence, DaemonError> {
    if lower >= higher {
        return Err(DaemonError::new(
            DomainErrorKind::MalformedFrame,
            "duplicate test keys must be supplied in strict winner order",
        ));
    }

    let mut provisional = CandidateRegistry::default();
    provisional
        .register(lower, lower)
        .map_err(|_| duplicate_test_error())?;
    provisional
        .register(higher, higher)
        .map_err(|_| duplicate_test_error())?;
    let redial_suppressed_while_provisional =
        !peer_needs_outbound_attempt(&provisional, None, |_| false);

    let mut candidates = CandidateRegistry::default();
    let loser_count = if higher_arrives_first {
        candidates
            .register(higher, higher)
            .map_err(|_| duplicate_test_error())?;
        match candidates.mark_ready_and_decide(higher) {
            CandidateDecision::Promoted(losers) if losers.is_empty() => {}
            _ => return Err(duplicate_test_error()),
        }
        candidates
            .register(lower, lower)
            .map_err(|_| duplicate_test_error())?;
        match candidates.mark_ready_and_decide(lower) {
            CandidateDecision::Promoted(losers) => losers.len(),
            _ => return Err(duplicate_test_error()),
        }
    } else {
        candidates
            .register(lower, lower)
            .map_err(|_| duplicate_test_error())?;
        candidates
            .register(higher, higher)
            .map_err(|_| duplicate_test_error())?;
        if !matches!(
            candidates.mark_ready_and_decide(higher),
            CandidateDecision::Wait
        ) {
            return Err(duplicate_test_error());
        }
        match candidates.mark_ready_and_decide(lower) {
            CandidateDecision::Promoted(losers) => losers.len(),
            _ => return Err(duplicate_test_error()),
        }
    };

    let primary = candidates.primary();
    let remaining_candidate_count = candidates.len();
    let accepted_generation = AuthGeneration::new(1).ok_or_else(duplicate_test_error)?;
    let redial_suppressed_after_confirmation =
        !peer_needs_outbound_attempt(&candidates, Some(accepted_generation), |_| false);
    let _closed = candidates.take_all();
    let empty_after_peer_close = candidates.is_empty() && candidates.primary().is_none();

    Ok(DuplicateConnectionTestEvidence {
        primary,
        remaining_candidate_count,
        loser_count,
        redial_suppressed_while_provisional,
        redial_suppressed_after_confirmation,
        empty_after_peer_close,
    })
}

/// Exercises the broker's real global, peer, connection, and metric permits
/// with capacity one and no transport resources.
#[doc(hidden)]
#[must_use]
pub fn stream_limit_test_evidence() -> StreamLimitTestEvidence {
    let limits = TransportLimits {
        max_stream_handlers_global: 1,
        max_open_stream_queue_per_connection: 1,
        max_bi_streams_per_connection: 1,
        max_stream_handlers_per_connection: 1,
        ..TransportLimits::default()
    };
    let broker = BrokerAdmission::new(limits);
    let global = Arc::clone(&broker.global_stream_handlers)
        .try_acquire_owned()
        .ok();
    let global_overflow_rejected = global.is_some()
        && Arc::clone(&broker.global_stream_handlers)
            .try_acquire_owned()
            .is_err();
    drop(global);
    let global_capacity_released = broker.global_stream_handlers.available_permits() == 1;

    let first_peer = PeerAdmission::new(limits);
    let second_peer = PeerAdmission::new(limits);
    let first_peer_permit = Arc::clone(&first_peer.open_queue).try_acquire_owned().ok();
    let peer_overflow_rejected = first_peer_permit.is_some()
        && Arc::clone(&first_peer.open_queue)
            .try_acquire_owned()
            .is_err();
    let second_peer_permit = Arc::clone(&second_peer.open_queue).try_acquire_owned().ok();
    let peer_isolated = first_peer_permit.is_some() && second_peer_permit.is_some();
    drop((first_peer_permit, second_peer_permit));
    let peer_capacity_released = first_peer.open_queue.available_permits() == 1
        && second_peer.open_queue.available_permits() == 1;

    let first_connection = CandidateAdmission::new(limits);
    let second_connection = CandidateAdmission::new(limits);
    let first_stream = Arc::clone(&first_connection.streams)
        .try_acquire_owned()
        .ok();
    let first_handler = Arc::clone(&first_connection.handlers)
        .try_acquire_owned()
        .ok();
    let connection_overflow_rejected = first_stream.is_some()
        && first_handler.is_some()
        && Arc::clone(&first_connection.streams)
            .try_acquire_owned()
            .is_err()
        && Arc::clone(&first_connection.handlers)
            .try_acquire_owned()
            .is_err();
    let second_stream = Arc::clone(&second_connection.streams)
        .try_acquire_owned()
        .ok();
    let second_handler = Arc::clone(&second_connection.handlers)
        .try_acquire_owned()
        .ok();
    let connection_isolated = first_stream.is_some()
        && first_handler.is_some()
        && second_stream.is_some()
        && second_handler.is_some();
    drop((first_stream, first_handler, second_stream, second_handler));
    let connection_capacity_released = first_connection.streams.available_permits() == 1
        && first_connection.handlers.available_permits() == 1
        && second_connection.streams.available_permits() == 1
        && second_connection.handlers.available_permits() == 1;

    let (reporter, observer) = NetworkReporter::initializing(DeviceId::from_array([0xa0; 32]));
    let metrics = Arc::new(BrokerMetrics {
        authenticated: AtomicUsize::new(0),
        primary: AtomicUsize::new(0),
        streams: AtomicUsize::new(0),
        paths: Mutex::new(BTreeMap::new()),
        reporter,
    });
    let first_peer_streams = Arc::new(AtomicUsize::new(0));
    let second_peer_streams = Arc::new(AtomicUsize::new(0));
    let first_metric =
        StreamMetricGuard::new(Arc::clone(&metrics), Arc::clone(&first_peer_streams)).ok();
    let second_metric =
        StreamMetricGuard::new(Arc::clone(&metrics), Arc::clone(&second_peer_streams)).ok();
    let metric_both_acquired = first_metric.is_some()
        && second_metric.is_some()
        && observer.snapshot().active_stream_count == 2;
    drop(first_metric);
    let metric_peer_isolated = metric_both_acquired
        && first_peer_streams.load(Ordering::Acquire) == 0
        && second_peer_streams.load(Ordering::Acquire) == 1
        && observer.snapshot().active_stream_count == 1;
    drop(second_metric);
    let metric_capacity_released = first_peer_streams.load(Ordering::Acquire) == 0
        && second_peer_streams.load(Ordering::Acquire) == 0
        && observer.snapshot().active_stream_count == 0;

    StreamLimitTestEvidence {
        global_overflow_rejected,
        global_capacity_released,
        peer_overflow_rejected,
        peer_isolated,
        peer_capacity_released,
        connection_overflow_rejected,
        connection_isolated,
        connection_capacity_released,
        metric_peer_isolated,
        metric_capacity_released,
    }
}

/// Projects path transitions through the broker's real redacted metric owner.
/// Direct socket addresses are accepted only as input and never retained or
/// returned by the evidence value.
#[doc(hidden)]
#[must_use]
pub fn path_observation_test_evidence(
    local: DeviceId,
    remote: DeviceId,
    addresses: &[TransportAddr],
) -> PathObservationTestEvidence {
    let (reporter, observer) = NetworkReporter::initializing(local);
    let metrics = BrokerMetrics {
        authenticated: AtomicUsize::new(0),
        primary: AtomicUsize::new(0),
        streams: AtomicUsize::new(0),
        paths: Mutex::new(BTreeMap::new()),
        reporter,
    };
    let mut timeline = Vec::with_capacity(addresses.len().saturating_add(1));
    let mut persistable_relays = Vec::with_capacity(addresses.len());
    for address in addresses {
        let (kind, relay) = classify_transport_addr(address);
        metrics.set_path(remote, kind);
        timeline.push(metrics.path(remote));
        persistable_relays.push(relay);
    }
    let selected_observation = observer.snapshot();
    metrics.remove_path(remote);
    timeline.push(metrics.path(remote));
    let cleared_observation = observer.snapshot();
    PathObservationTestEvidence {
        timeline,
        persistable_relays,
        selected_observation,
        cleared_observation,
    }
}

fn duplicate_test_error() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::TransportUnavailable,
        "duplicate candidate test schedule violated broker invariants",
    )
}

fn classify_transport_addr(address: &TransportAddr) -> (PathKind, Option<RelayHint>) {
    match address {
        TransportAddr::Relay(url) => (PathKind::Relay, RelayHint::new(url.to_string()).ok()),
        TransportAddr::Ip(_) => (PathKind::Direct, None),
        _ => (PathKind::Unknown, None),
    }
}

fn now_unix_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(i64::MAX)
}

fn random_attempt_id() -> Result<ConnectionAttemptId, DaemonError> {
    let mut bytes = [0_u8; 16];
    SystemRandom::new().fill(&mut bytes).map_err(|_| {
        DaemonError::new(
            DomainErrorKind::TransportUnavailable,
            "operating-system randomness is unavailable for a connection attempt",
        )
    })?;
    Ok(ConnectionAttemptId::from_array(bytes))
}

fn retry_delay(attempt: u32, remote: DeviceId) -> Duration {
    let exponent = attempt.min(6);
    let multiplier = 1_u32 << exponent;
    let base = RETRY_BASE.saturating_mul(multiplier).min(RETRY_CAP);
    let bytes = remote.as_bytes();
    let mixed =
        u32::from(bytes[(attempt as usize) % bytes.len()]) ^ attempt.wrapping_mul(0x9e37_79b9);
    let jitter_ceiling = base / 5;
    let jitter = if jitter_ceiling.is_zero() {
        Duration::ZERO
    } else {
        Duration::from_nanos(
            u64::from(mixed) % u64::try_from(jitter_ceiling.as_nanos()).unwrap_or(u64::MAX),
        )
    };
    base.saturating_add(jitter).min(RETRY_CAP)
}

fn validate_limits(limits: TransportLimits) -> Result<(), DaemonError> {
    limits
        .validate()
        .map_err(|error| DaemonError::new(DomainErrorKind::ResourceExhausted, error.to_string()))?;
    limits
        .max_unauthenticated_connections
        .checked_add(limits.max_pairing_handshakes)
        .ok_or_else(|| {
            DaemonError::new(
                DomainErrorKind::ResourceExhausted,
                "pre-authentication connection limit overflow",
            )
        })?;
    for (field, value) in [
        (
            "max_bi_streams_per_connection",
            limits.max_bi_streams_per_connection,
        ),
        (
            "max_stream_handlers_per_connection",
            limits.max_stream_handlers_per_connection,
        ),
    ] {
        if u32::try_from(value).is_err() {
            return Err(DaemonError::new(
                DomainErrorKind::ResourceExhausted,
                format!("transport limit {field} exceeds QUIC's u32 boundary"),
            ));
        }
    }
    Ok(())
}

fn configure_normal_connection_limits(connection: &Connection, limits: TransportLimits) {
    // `ConnectionBroker::new` rejects values outside QUIC's u32 boundary.
    // Keeping the conversion defensive avoids a panic if this helper is ever
    // reused at another composition boundary.
    let maximum_bi_streams =
        u32::try_from(limits.max_bi_streams_per_connection).unwrap_or(u32::MAX);
    connection.set_max_concurrent_bi_streams(VarInt::from_u32(maximum_bi_streams));
    // The product protocol owns only bidirectional streams. Advertising zero
    // prevents an authenticated peer from accumulating unconsumed uni streams.
    connection.set_max_concurrent_uni_streams(VarInt::from_u32(0));
}

fn protocol_error(error: voytty_proto::ProtocolError) -> DaemonError {
    use voytty_proto::ProtocolError;
    let kind = match error {
        ProtocolError::WireMajorMismatch { .. } => DomainErrorKind::WireMajorMismatch,
        ProtocolError::UnknownKind(_) => DomainErrorKind::UnknownKind,
        ProtocolError::FrameTooLarge(_) => DomainErrorKind::FrameTooLarge,
        ProtocolError::ControlPayloadTooLarge(_) => DomainErrorKind::ControlPayloadTooLarge,
        ProtocolError::MalformedVarint
        | ProtocolError::TruncatedFrame
        | ProtocolError::MalformedProtobuf(_)
        | ProtocolError::UnexpectedKind { .. }
        | ProtocolError::InvalidIdentifier(_)
        | ProtocolError::InvalidTerminalSize { .. }
        | ProtocolError::InvalidTerminalSurface(_)
        | ProtocolError::InvalidPayloadEncoding
        | ProtocolError::InvalidLocalConnectionStage
        | ProtocolError::InvalidTerminalSemanticField(_)
        | ProtocolError::InvalidUpload(_) => DomainErrorKind::MalformedFrame,
    };
    DaemonError::new(kind, error.to_string())
}

fn increment_atomic(counter: &AtomicUsize) -> Result<(), ()> {
    increment_atomic_previous(counter).map(|_| ())
}

fn increment_atomic_previous(counter: &AtomicUsize) -> Result<usize, ()> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .map_err(|_| ())
}

fn decrement_atomic(counter: &AtomicUsize) {
    let _ = counter.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        current.checked_sub(1)
    });
}

async fn acquire_until(
    semaphore: Arc<Semaphore>,
    deadline: Instant,
    detail: &'static str,
) -> Result<OwnedSemaphorePermit, DaemonError> {
    timeout_until(deadline, semaphore.acquire_owned())
        .await?
        .map_err(|_| transport_unavailable(detail))
}

async fn timeout_until<F>(deadline: Instant, future: F) -> Result<F::Output, DaemonError>
where
    F: std::future::Future,
{
    voytty_client::protocol::timeout_until(deadline, future, "transport operation deadline elapsed")
        .await
}

async fn timeout_or_pair_shutdown<F>(
    deadline: Instant,
    mut shutdown: watch::Receiver<bool>,
    future: F,
) -> Result<F::Output, DaemonError>
where
    F: std::future::Future,
{
    if *shutdown.borrow() {
        return Err(cancelled("pairing transport is stopping"));
    }
    if Instant::now() >= deadline {
        return Err(deadline_exceeded("pairing operation deadline elapsed"));
    }
    tokio::select! {
        biased;
        changed = shutdown.changed() => {
            let _ = changed;
            Err(cancelled("pairing transport is stopping"))
        }
        result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), future) => {
            result.map_err(|_| deadline_exceeded("pairing operation deadline elapsed"))
        }
    }
}

fn is_retryable(kind: DomainErrorKind) -> bool {
    matches!(
        kind,
        DomainErrorKind::AddressUnavailable
            | DomainErrorKind::TransportUnavailable
            | DomainErrorKind::DeadlineExceeded
            | DomainErrorKind::ResourceExhausted
            | DomainErrorKind::StoreUnavailable
    )
}

fn peer_needs_outbound_attempt<T>(
    candidates: &CandidateRegistry<T>,
    remote_acceptance: Option<AuthGeneration>,
    is_outbound: impl Fn(&T) -> bool,
) -> bool {
    match candidates.primary() {
        None => candidates.is_empty(),
        Some(_) => remote_acceptance.is_none() && !candidates.values().any(is_outbound),
    }
}

fn mutex_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn resource_exhausted(detail: &'static str) -> DaemonError {
    DaemonError::new(DomainErrorKind::ResourceExhausted, detail)
}

fn transport_unavailable(detail: &'static str) -> DaemonError {
    DaemonError::new(DomainErrorKind::TransportUnavailable, detail)
}

fn no_route_connected() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::AddressUnavailable,
        "no route candidate established a connection",
    )
}

fn deadline_exceeded(detail: &'static str) -> DaemonError {
    DaemonError::new(DomainErrorKind::DeadlineExceeded, detail)
}

fn cancelled(detail: &'static str) -> DaemonError {
    DaemonError::new(DomainErrorKind::Cancelled, detail)
}

fn bounded_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(any(unix, windows))]
fn round_rtt_millis(rtt: Duration) -> u32 {
    let rounded = rtt.as_nanos().saturating_add(500_000) / 1_000_000;
    u32::try_from(rounded).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests;
