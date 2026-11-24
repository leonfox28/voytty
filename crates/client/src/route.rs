//! Relay-only signed discovery and authenticated cache/ticket route ordering.
use crate::error::ClientError as DaemonError;
use futures_util::StreamExt;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl, TransportAddr, endpoint::Connection};
use std::{collections::BTreeSet, fmt, time::Instant};
use voytty_core::{DeviceId, DomainErrorKind, RelayHint, TransportLimits};

/// Origin of one ordered dial candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteSource {
    /// Signed address-lookup result obtained for this dial.
    FreshLookup,
    /// Previously handshake-verified SQLite route cache.
    VerifiedCache,
    /// Short-lived route carried by a pairing ticket.
    TransientTicket,
}

/// One independently dialable route containing no direct IP address.
#[derive(Clone, Eq, PartialEq)]
pub struct RouteCandidate {
    source: RouteSource,
    relay_hint: RelayHint,
    endpoint_addr: EndpointAddr,
}

impl fmt::Debug for RouteCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RouteCandidate")
            .field("source", &self.source)
            .field("relay_hint", &self.relay_hint)
            .field("endpoint_id", &self.endpoint_addr.id)
            .field(
                "relay_address_count",
                &self.endpoint_addr.relay_urls().count(),
            )
            .field(
                "direct_address_count",
                &self.endpoint_addr.ip_addrs().count(),
            )
            .finish()
    }
}

impl RouteCandidate {
    fn relay(
        remote: EndpointId,
        source: RouteSource,
        relay_hint: RelayHint,
    ) -> Result<Self, DaemonError> {
        let relay_url: RelayUrl = relay_hint.as_str().parse().map_err(|_| {
            DaemonError::new(
                DomainErrorKind::AddressUnavailable,
                "relay route could not be adapted to Iroh's URL type",
            )
        })?;
        Ok(Self {
            source,
            endpoint_addr: EndpointAddr::new(remote).with_relay_url(relay_url),
            relay_hint,
        })
    }

    /// Candidate origin, in fallback order.
    #[must_use]
    pub const fn source(&self) -> RouteSource {
        self.source
    }

    /// Exact validated relay URL used by this candidate.
    #[must_use]
    pub fn relay_hint(&self) -> &RelayHint {
        &self.relay_hint
    }

    /// Relay-only Iroh endpoint address.
    #[must_use]
    pub fn endpoint_addr(&self) -> &EndpointAddr {
        &self.endpoint_addr
    }
}

/// Uses cached or ticket routes immediately. Iroh runs signed lookup alongside
/// the dial, including for a previously used remote. With no hints, waits for
/// a relay within the lookup budget so the dial has an initial candidate.
pub async fn resolve_relay_candidates(
    limits: TransportLimits,
    endpoint: &Endpoint,
    remote: EndpointId,
    cache: Vec<RelayHint>,
    transient: Vec<RelayHint>,
    deadline: Instant,
) -> Result<Vec<RouteCandidate>, DaemonError> {
    RouteRefresh::UseCached
        .candidates(limits, endpoint, remote, cache, transient, deadline)
        .await
}

/// Starts an independent bounded signed lookup and places its relays ahead of
/// cached and ticket routes. Those routes remain usable if discovery fails.
pub async fn refresh_relay_candidates(
    limits: TransportLimits,
    endpoint: &Endpoint,
    remote: EndpointId,
    cache: Vec<RelayHint>,
    transient: Vec<RelayHint>,
    deadline: Instant,
) -> Result<Vec<RouteCandidate>, DaemonError> {
    RouteRefresh::Refresh
        .candidates(limits, endpoint, remote, cache, transient, deadline)
        .await
}

pub(crate) enum RouteRefresh {
    UseCached,
    Refresh,
}

impl RouteRefresh {
    pub(crate) async fn candidates(
        self,
        limits: TransportLimits,
        endpoint: &Endpoint,
        remote: EndpointId,
        cache: Vec<RelayHint>,
        transient: Vec<RelayHint>,
        deadline: Instant,
    ) -> Result<Vec<RouteCandidate>, DaemonError> {
        if Instant::now() >= deadline {
            return Err(deadline_exceeded("route resolution deadline elapsed"));
        }
        let fresh = match self {
            Self::UseCached if !cache.is_empty() || !transient.is_empty() => Vec::new(),
            _ => fresh_relay_hints(limits, endpoint, remote, deadline)
                .await
                .unwrap_or_default(),
        };
        let candidates =
            build_relay_candidates(remote, fresh, cache, transient, limits.max_relay_hints)?;
        if candidates.is_empty() {
            Err(DaemonError::new(
                DomainErrorKind::AddressUnavailable,
                "no relay route is available for the target device",
            ))
        } else {
            Ok(candidates)
        }
    }
}

/// Reads the first usable bounded fresh signed hints, before cache/ticket fallback.
pub async fn fresh_relay_hints(
    limits: TransportLimits,
    endpoint: &Endpoint,
    remote: EndpointId,
    deadline: Instant,
) -> Result<Vec<RelayHint>, DaemonError> {
    let services = endpoint.address_lookup().map_err(|_| {
        DaemonError::new(
            DomainErrorKind::TransportUnavailable,
            "endpoint address lookup is unavailable",
        )
    })?;
    let services = services.clone();
    let remaining = deadline.saturating_duration_since(Instant::now());
    let budget = limits.address_lookup_budget.min(remaining);
    if budget.is_zero() {
        return Err(deadline_exceeded("route resolution deadline elapsed"));
    }

    // Providers run concurrently. Use the first exact-peer result containing
    // validated relays without waiting for a slow provider to finish. The
    // original deadline still bounds discovery when no usable result arrives.
    let mut hints = Vec::new();
    let mut seen = BTreeSet::new();
    let resolved = tokio::time::timeout(budget, async {
        let mut stream = Box::pin(services.resolve(remote));
        while let Some(result) = stream.next().await {
            let Ok(Ok(item)) = result else {
                continue;
            };
            if item.endpoint_id() != remote {
                continue;
            }
            for relay in item.to_endpoint_addr().relay_urls() {
                let text = relay.to_string();
                if seen.insert(text.clone())
                    && let Ok(hint) = RelayHint::new(text)
                {
                    hints.push(hint);
                    if hints.len() == limits.max_relay_hints {
                        return;
                    }
                }
            }
            if !hints.is_empty() {
                return;
            }
        }
    })
    .await;

    if resolved.is_ok() || !hints.is_empty() {
        Ok(hints)
    } else {
        Err(DaemonError::new(
            DomainErrorKind::AddressUnavailable,
            "fresh address lookup timed out",
        ))
    }
}
/// Adapts merged validated hints into independently dialable Iroh candidates.
pub fn build_relay_candidates(
    remote: EndpointId,
    fresh: Vec<RelayHint>,
    cache: Vec<RelayHint>,
    transient: Vec<RelayHint>,
    maximum: usize,
) -> Result<Vec<RouteCandidate>, DaemonError> {
    merge_relay_sources(fresh, cache, transient, maximum)
        .into_iter()
        .map(|(source, hint)| RouteCandidate::relay(remote, source, hint))
        .collect()
}

/// Supplies all bounded, exact-peer relay candidates to one authenticated dial.
/// Iroh probes these paths within that dial; a stale first hint must not consume
/// the caller's entire deadline before a verified cache or ticket can be used.
pub fn combined_relay_address(
    remote: EndpointId,
    candidates: &[RouteCandidate],
) -> Result<EndpointAddr, DaemonError> {
    if candidates.is_empty() {
        return Err(DaemonError::new(
            DomainErrorKind::AddressUnavailable,
            "no relay route is available for the target device",
        ));
    }
    let mut address = EndpointAddr::new(remote);
    for candidate in candidates {
        if candidate.endpoint_addr.id != remote {
            return Err(DaemonError::new(
                DomainErrorKind::IdentityInvalid,
                "relay candidate does not belong to the exact target",
            ));
        }
        for relay in candidate.endpoint_addr.relay_urls() {
            address = address.with_relay_url(relay.clone());
        }
    }
    Ok(address)
}

/// Only an actually selected relay path is eligible for the verified cache.
/// A supplied hint alone is not evidence that the peer was reached through it.
pub fn selected_relay(connection: &Connection) -> Option<RelayHint> {
    connection
        .paths()
        .iter()
        .find(|path| path.is_selected())
        .and_then(|path| match path.remote_addr() {
            TransportAddr::Relay(url) => RelayHint::new(url.to_string()).ok(),
            _ => None,
        })
}

/// Converts the product's fixed bytes into Iroh's authenticated endpoint ID.
pub fn endpoint_id_from_device(remote: DeviceId) -> Result<EndpointId, DaemonError> {
    EndpointId::from_bytes(remote.as_bytes()).map_err(|_| {
        DaemonError::new(
            DomainErrorKind::IdentityInvalid,
            "device ID is not a valid Iroh endpoint key",
        )
    })
}

/// Converts an Iroh-authenticated endpoint ID into the transport-neutral ID.
#[must_use]
pub fn device_from_endpoint_id(remote: EndpointId) -> DeviceId {
    DeviceId::from_array(*remote.as_bytes())
}

/// Orders and deduplicates fresh, verified-cache and ticket sources independently.
pub fn merge_relay_sources(
    fresh: Vec<RelayHint>,
    cache: Vec<RelayHint>,
    transient: Vec<RelayHint>,
    maximum_per_source: usize,
) -> Vec<(RouteSource, RelayHint)> {
    if maximum_per_source == 0 {
        return Vec::new();
    }
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for (source, hints) in [
        (RouteSource::FreshLookup, fresh),
        (RouteSource::VerifiedCache, cache),
        (RouteSource::TransientTicket, transient),
    ] {
        // Each persisted or transient route set is independently bounded by
        // `max_relay_hints`. Do not apply that bound to the merged sequence:
        // a full fresh result must not starve cache/ticket fallback after its
        // candidates fail to connect.
        for hint in hints.into_iter().take(maximum_per_source) {
            if seen.insert(hint.as_str().to_owned()) {
                ordered.push((source, hint));
            }
        }
    }
    ordered
}

fn deadline_exceeded(detail: &'static str) -> DaemonError {
    DaemonError::new(DomainErrorKind::DeadlineExceeded, detail)
}

#[cfg(test)]
pub(crate) use tests::StalledLookup;

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;
    use futures_util::stream::{self, BoxStream};
    use iroh::address_lookup::{AddressLookup, Error, Item};
    use std::{
        net::Ipv4Addr,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    #[derive(Clone, Debug, Default)]
    pub(crate) struct StalledLookup(Arc<AtomicUsize>);

    impl AddressLookup for StalledLookup {
        fn resolve(&self, _: EndpointId) -> Option<BoxStream<'static, Result<Item, Error>>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Some(stream::pending().boxed())
        }
    }

    #[derive(Debug)]
    struct PartialLookup {
        reported: Option<EndpointId>,
    }

    impl AddressLookup for PartialLookup {
        fn resolve(&self, remote: EndpointId) -> Option<BoxStream<'static, Result<Item, Error>>> {
            let reported = self.reported.unwrap_or(remote);
            let item = Item::new(
                EndpointAddr::new(reported)
                    .with_relay_url("https://fresh.example".parse().expect("fixture relay"))
                    .into(),
                "partial-fixture",
                None,
            );
            Some(
                stream::once(async move { Ok(item) })
                    .chain(stream::pending())
                    .boxed(),
            )
        }
    }

    async fn lookup_fixture(lookup: impl AddressLookup + 'static) -> Endpoint {
        Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .clear_ip_transports()
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("loopback fixture")
            .address_lookup(lookup)
            .bind()
            .await
            .expect("isolated lookup endpoint")
    }

    #[tokio::test]
    async fn fresh_relay_lookup_preserves_results_when_another_source_never_finishes() {
        let endpoint = lookup_fixture(PartialLookup { reported: None }).await;
        let remote = iroh::SecretKey::from_bytes(&[0x71; 32]).public();
        let limits = TransportLimits {
            address_lookup_budget: Duration::from_millis(30),
            ..TransportLimits::default()
        };
        let result = fresh_relay_hints(
            limits,
            &endpoint,
            remote,
            Instant::now() + Duration::from_secs(1),
        )
        .await;
        endpoint.close().await;
        assert_eq!(
            result.expect("retain the valid result within the existing budget"),
            vec![relay("https://fresh.example/")]
        );
    }

    #[tokio::test]
    async fn usable_route_does_not_wait_for_a_stalled_lookup_source() {
        let endpoint = lookup_fixture(PartialLookup { reported: None }).await;
        let remote = iroh::SecretKey::from_bytes(&[0x71; 32]).public();
        // Discovery has already produced an authenticated route. A second
        // provider that never finishes must not consume the rest of startup.
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            fresh_relay_hints(
                TransportLimits::default(),
                &endpoint,
                remote,
                Instant::now() + Duration::from_secs(5),
            ),
        )
        .await;
        endpoint.close().await;
        assert_eq!(
            result
                .expect("usable route must be returned before the unrelated lookup deadline")
                .expect("valid exact-peer route"),
            vec![relay("https://fresh.example/")],
        );
    }

    #[tokio::test]
    async fn fresh_relay_lookup_never_retains_a_different_peers_result_at_timeout() {
        let endpoint = lookup_fixture(PartialLookup {
            reported: Some(iroh::SecretKey::from_bytes(&[0x72; 32]).public()),
        })
        .await;
        let remote = iroh::SecretKey::from_bytes(&[0x71; 32]).public();
        let limits = TransportLimits {
            address_lookup_budget: Duration::from_millis(30),
            ..TransportLimits::default()
        };
        let result = fresh_relay_hints(
            limits,
            &endpoint,
            remote,
            Instant::now() + Duration::from_secs(1),
        )
        .await;
        endpoint.close().await;
        assert_eq!(
            result.expect_err("no valid route for this peer").kind(),
            DomainErrorKind::AddressUnavailable
        );
    }

    #[tokio::test]
    async fn warm_cached_and_ticket_routes_are_ready_without_another_lookup() {
        let lookup = StalledLookup::default();
        let endpoint = lookup_fixture(lookup.clone()).await;
        let remote = iroh::SecretKey::generate().public();
        // Starting a loopback dial registers the remote and its pending Iroh lookup.
        let socket = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("loopback");
        let address =
            EndpointAddr::new(remote).with_ip_addr(socket.local_addr().expect("bound socket"));
        let _connecting = endpoint
            .connect_with_opts(address, b"route-test", Default::default())
            .await
            .expect("start dial");
        assert!(endpoint.remote_info(remote).await.is_some());
        assert_eq!(lookup.0.load(Ordering::SeqCst), 1);

        let hint = relay("https://known.example");
        for (cache, ticket, source) in [
            (vec![hint.clone()], vec![], RouteSource::VerifiedCache),
            (vec![], vec![hint.clone()], RouteSource::TransientTicket),
        ] {
            let candidates = resolve_relay_candidates(
                TransportLimits::default(),
                &endpoint,
                remote,
                cache,
                ticket,
                Instant::now() + Duration::from_secs(1),
            )
            .now_or_never()
            .expect("a known route is ready on the first poll")
            .expect("known route");
            assert_eq!(route_sources(&candidates), [source]);
            assert_eq!(candidates[0].relay_hint(), &hint);
        }
        assert_eq!(lookup.0.load(Ordering::SeqCst), 1);
        endpoint.close().await;
    }

    #[tokio::test]
    async fn zero_hints_require_discovery_and_refresh_prefers_its_result() {
        let endpoint = lookup_fixture(PartialLookup { reported: None }).await;
        let remote = iroh::SecretKey::generate().public();
        let limits = TransportLimits::default();
        let deadline = Instant::now() + Duration::from_secs(1);
        let discovered =
            resolve_relay_candidates(limits, &endpoint, remote, vec![], vec![], deadline)
                .await
                .expect("zero-hint lookup");
        assert_eq!(route_sources(&discovered), [RouteSource::FreshLookup]);
        assert_eq!(discovered[0].relay_hint(), &relay("https://fresh.example/"));
        let refreshed = refresh_relay_candidates(
            limits,
            &endpoint,
            remote,
            vec![relay("https://cached.example")],
            vec![relay("https://ticket.example")],
            deadline,
        )
        .await
        .expect("fresh and fallback routes");
        assert_eq!(
            route_sources(&refreshed),
            [
                RouteSource::FreshLookup,
                RouteSource::VerifiedCache,
                RouteSource::TransientTicket
            ]
        );
        endpoint.close().await;
    }

    #[tokio::test]
    async fn stalled_lookup_uses_its_budget_then_returns_only_available_fallbacks() {
        let lookup = StalledLookup::default();
        let endpoint = lookup_fixture(lookup.clone()).await;
        let remote = iroh::SecretKey::generate().public();
        let budget = Duration::from_millis(30);
        let limits = TransportLimits {
            address_lookup_budget: budget,
            ..TransportLimits::default()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        tokio::time::pause();
        let started = tokio::time::Instant::now();
        let missing = tokio::time::timeout(
            budget * 2,
            resolve_relay_candidates(limits, &endpoint, remote, vec![], vec![], deadline),
        )
        .await
        .expect("zero-hint discovery respects the lookup budget");
        assert!(started.elapsed() >= budget);
        assert_eq!(
            missing.expect_err("no routes available").kind(),
            DomainErrorKind::AddressUnavailable
        );
        assert_eq!(lookup.0.load(Ordering::SeqCst), 1);

        let started = tokio::time::Instant::now();
        let fallback = tokio::time::timeout(
            budget * 2,
            refresh_relay_candidates(
                limits,
                &endpoint,
                remote,
                vec![relay("https://cached.example")],
                vec![relay("https://ticket.example")],
                deadline,
            ),
        )
        .await
        .expect("refresh respects the lookup budget")
        .expect("fallback routes");
        assert!(started.elapsed() >= budget);
        assert_eq!(
            route_sources(&fallback),
            [RouteSource::VerifiedCache, RouteSource::TransientTicket]
        );
        assert_eq!(lookup.0.load(Ordering::SeqCst), 2);
        endpoint.close().await;
    }

    fn route_sources(candidates: &[RouteCandidate]) -> Vec<RouteSource> {
        candidates.iter().map(RouteCandidate::source).collect()
    }

    fn relay(url: &str) -> RelayHint {
        RelayHint::new(url).expect("valid route fixture")
    }
    #[test]
    fn route_candidate_contains_only_the_relay_transport() {
        let remote = iroh::SecretKey::from_bytes(&[7; 32]).public();
        let candidate = RouteCandidate::relay(
            remote,
            RouteSource::VerifiedCache,
            relay("https://relay.example"),
        )
        .expect("candidate adapts");
        assert_eq!(candidate.endpoint_addr().id, remote);
        assert_eq!(candidate.endpoint_addr().relay_urls().count(), 1);
        assert_eq!(candidate.endpoint_addr().ip_addrs().count(), 0);
    }

    #[test]
    fn route_debug_redacts_relay_and_direct_addresses_but_keeps_shape() {
        let relay_sentinel = "https://ROUTE_CANDIDATE_SENTINEL_2a9c.example.test/private";
        let direct_sentinel = "203.0.113.197:49152";
        let remote = iroh::SecretKey::from_bytes(&[0x6e; 32]).public();
        let relay_hint = relay(relay_sentinel);
        let candidate = RouteCandidate {
            source: RouteSource::TransientTicket,
            relay_hint: relay_hint.clone(),
            endpoint_addr: EndpointAddr::new(remote)
                .with_relay_url(relay_sentinel.parse().expect("valid Relay URL"))
                .with_ip_addr(
                    direct_sentinel
                        .parse()
                        .expect("valid direct socket address"),
                ),
        };
        let rendered = format!("{candidate:?}");
        assert!(!rendered.contains(relay_sentinel));
        assert!(!rendered.contains(direct_sentinel));
        assert!(rendered.contains("TransientTicket"));
        assert!(rendered.contains("relay_address_count: 1"));
        assert!(rendered.contains("direct_address_count: 1"));
        assert_eq!(candidate.relay_hint().as_str(), relay_sentinel);
        assert_eq!(candidate.endpoint_addr().id, remote);
        assert_eq!(candidate.endpoint_addr().ip_addrs().count(), 1);
    }
}
