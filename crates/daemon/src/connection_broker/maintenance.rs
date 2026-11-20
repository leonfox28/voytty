//! Peer lifetime and serialized persistence of verified transport observations.

use super::*;

const OBSERVATION_WINDOW: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(super) struct PendingObservations {
    peers: BTreeMap<DeviceId, VerifiedObservation>,
    writing: bool,
}

#[derive(Default)]
struct VerifiedObservation {
    last_seen: Option<i64>,
    route: Option<RelayRouteCache>,
}

impl PendingObservations {
    /// Returns true to the caller that owns the next write batch.
    fn record(
        &mut self,
        remote: DeviceId,
        handshake: bool,
        relay: Option<RelayHint>,
        now: i64,
    ) -> bool {
        let observation = self.peers.entry(remote).or_default();
        if handshake {
            observation.last_seen = Some(now);
        }
        if let Some(relay) = relay {
            observation.route = Some(RelayRouteCache {
                relay_hints: vec![relay],
                verified_at_unix: now,
            });
        }
        !std::mem::replace(&mut self.writing, true)
    }
}

impl ConnectionBroker {
    // Called with the peer map locked on its empty-to-nonempty transition.
    pub(super) fn start_peer_cleanup(&self) -> Result<(), DaemonError> {
        let task = self.inner.lifecycle.admit_task()?;
        let stopping = self.inner.lifecycle.stopping.clone();
        let broker = Arc::downgrade(&self.inner);
        let window =
            self.inner.limits.first_frame_deadline + self.inner.limits.connect_attempt_budget;
        tokio::spawn(async move {
            let _task = task;
            loop {
                tokio::select! {
                    () = stopping.cancelled() => return,
                    () = tokio::time::sleep(window) => {}
                }
                let Some(broker) = broker.upgrade() else {
                    return;
                };
                if broker.reap_idle_peers(Instant::now()) {
                    return;
                }
            }
        });
        Ok(())
    }

    pub(super) fn queue_observation(
        &self,
        remote: DeviceId,
        handshake: bool,
        relay: Option<RelayHint>,
    ) {
        let Ok(task) = self.inner.lifecycle.admit_task() else {
            return;
        };
        if mutex_lock(&self.inner.observations).record(remote, handshake, relay, now_unix_i64()) {
            let broker = self.clone();
            tokio::spawn(async move {
                let _task = task;
                broker.write_observations().await;
            });
        }
    }

    async fn write_observations(&self) {
        loop {
            // Batch route flaps and repeated notifications for the same peer.
            // Shutdown flushes admitted updates without waiting for this timer.
            tokio::select! {
                () = self.inner.lifecycle.stopping.cancelled() => {}
                () = tokio::time::sleep(OBSERVATION_WINDOW) => {}
            }
            let batch = std::mem::take(&mut mutex_lock(&self.inner.observations).peers);
            for (remote, observation) in batch {
                let seen = observation
                    .last_seen
                    .filter(|_| self.inner.authorization.admit(remote).is_ok());
                let deadline = Instant::now() + Duration::from_secs(5);
                let _ = self
                    .inner
                    .store
                    .run_blocking_until(deadline, move |store, deadline| {
                        if let Some(now) = seen {
                            let _ = store.set_last_seen(remote, now, deadline);
                        }
                        if let Some(route) = observation.route {
                            store.set_known_route(remote, route, deadline)?;
                        }
                        Ok(())
                    })
                    .await;
            }
            let mut pending = mutex_lock(&self.inner.observations);
            if pending.peers.is_empty() {
                pending.writing = false;
                return;
            }
        }
    }
}

impl BrokerInner {
    fn reap_idle_peers(&self, now: Instant) -> bool {
        let mut peers = mutex_lock(&self.peers);
        peers.retain(|_, slot| {
            // Lookup clones under this same map lock. A demand, handshake,
            // actor, or pending retirement therefore pins the canonical slot.
            Arc::strong_count(slot) > 1
                || slot.state.try_lock().map_or(true, |state| {
                    state
                        .incarnation
                        .retired
                        .iter()
                        .any(|(_, until)| *until > now)
                })
        });
        peers.is_empty()
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::super::tests::{device, relay};
    use super::*;

    #[tokio::test]
    async fn idle_peers_are_reclaimed_after_the_last_owner_and_incarnation_fence() {
        let fixture = crate::remote_session::cache_tests::Fixture::with_limits(TransportLimits {
            first_frame_deadline: Duration::from_millis(20),
            connect_attempt_budget: Duration::from_millis(20),
            ..TransportLimits::default()
        })
        .await;
        let broker = &fixture.broker;
        let active = broker.peer_slot(device(1)).expect("active");
        let fenced = broker.peer_slot(device(2)).expect("recent restart");
        let now = Instant::now();
        let window = Duration::from_secs(15);
        {
            let mut state = fenced.state.lock().await;
            for id in [1, 2] {
                state
                    .incarnation
                    .observe(ConnectionIncarnation::from_array([id; 16]), now, window, 8)
                    .expect("owner change");
            }
        }
        let old = Arc::downgrade(&fenced);
        drop(fenced);
        for id in 3..=200 {
            drop(broker.peer_slot(device(id)).expect("short-lived peer"));
        }
        let idle = Arc::downgrade(&broker.peer_slot(device(200)).expect("idle peer"));
        tokio::time::timeout(Duration::from_secs(3), async {
            while idle.strong_count() > 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("scheduled cleanup reclaims idle peers");
        assert_eq!(mutex_lock(&broker.inner.peers).len(), 2);
        let retained = broker.peer_slot(device(2)).expect("same slot");
        assert!(Arc::ptr_eq(
            &retained,
            &old.upgrade().expect("retained fence")
        ));
        assert!(
            retained
                .state
                .lock()
                .await
                .incarnation
                .observe(ConnectionIncarnation::from_array([1; 16]), now, window, 8)
                .is_err()
        );
        drop(retained);
        assert!(!broker.inner.reap_idle_peers(now + window));
        assert!(old.upgrade().is_none());
        assert!(Arc::ptr_eq(
            &active,
            &broker.peer_slot(device(1)).expect("live owner")
        ));
        drop(active);
        assert!(broker.inner.reap_idle_peers(now + window));
        fixture.close().await;
    }

    #[tokio::test]
    async fn quiesce_drains_coalesced_verified_routes_and_refuses_late_work() {
        use futures_util::FutureExt;

        let fixture = crate::remote_session::cache_tests::Fixture::new().await;
        let broker = &fixture.broker;
        let remote = fixture.remote;
        let latest = relay("https://latest.example");
        let deadline = Instant::now() + Duration::from_secs(5);
        let store = broker.inner.store.clone();
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, released) = std::sync::mpsc::channel();
        let blocked =
            tokio::task::spawn_blocking(move || store.block_for_test(deadline, entered, released));
        tokio::task::spawn_blocking(move || waiting.recv_timeout(Duration::from_secs(5)))
            .await
            .expect("wait for store")
            .expect("store is blocked");
        broker.queue_observation(remote, true, None);
        for i in 0..512 {
            let route = if i % 2 == 0 {
                relay("https://old.example")
            } else {
                latest.clone()
            };
            broker.queue_observation(remote, false, Some(route));
        }
        assert_eq!(broker.inner.lifecycle.tasks.len(), 1, "one batch writer");
        assert_eq!(mutex_lock(&broker.inner.observations).peers.len(), 1);
        // The active SQLite writer keeps shutdown pending even after the
        // broker has stopped admitting observations.
        let mut quiesce =
            Box::pin(broker.quiesce_until(Instant::now() + Duration::from_millis(50)));
        assert!(quiesce.as_mut().now_or_never().is_none());
        assert!(broker.inner.lifecycle.admit_task().is_err());
        broker.queue_observation(remote, false, Some(relay("https://late.example")));
        assert_eq!(
            quiesce.await.expect_err("store still blocked").kind(),
            DomainErrorKind::DeadlineExceeded
        );
        release.send(()).expect("release store");
        blocked
            .await
            .expect("blocking task")
            .expect("unblocked store");
        broker
            .quiesce_until(deadline)
            .await
            .expect("writer drained");
        assert_eq!(broker.inner.lifecycle.tasks.len(), 0);
        let known = broker
            .inner
            .store
            .known_device(remote, deadline)
            .expect("read route")
            .expect("known peer");
        assert_eq!(
            known.route_cache.expect("verified route").relay_hints,
            [latest]
        );
        let auth = broker
            .inner
            .store
            .list_authorizations(deadline)
            .expect("read authorizations");
        assert!(
            auth.iter()
                .any(|peer| peer.device_id == remote && peer.last_seen_at_unix.is_some())
        );
        fixture.close().await;
    }
}
