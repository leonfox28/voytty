//! Connection lifetime and stream admission for an active peer demand.

use super::*;
use futures_util::FutureExt;

impl ConnectionDemand {
    /// Dials this demand's peer without Hello for the request that `until`
    /// bounds. Progress is left to the dial that adopts it.
    #[cfg(any(unix, windows))]
    pub(crate) fn dial_replacement(&self, until: Instant) -> Option<ReplacementTransport> {
        let task = self.broker.inner.lifecycle.admit_task().ok()?;
        let broker = self.broker.clone();
        let slot = Arc::clone(&self.slot);
        let stopping = broker.inner.lifecycle.stopping.clone();
        Some(ReplacementTransport::dial(until, async move {
            let _task = task;
            stopping
                .run_until_cancelled(async {
                    let _dial_permit = broker.dial_permit()?;
                    let endpoint = broker.bound_endpoint()?;
                    let routes = broker
                        .dial_routes(&endpoint, slot.remote, &slot.demand.routes(), false, until)
                        .await?;
                    let mut last_error = None;
                    for route in routes {
                        match connect_transport(&endpoint, route.address, VOYTTY_ALPN, until).await
                        {
                            Ok(connection) => return Ok(connection),
                            Err(error) => last_error = Some(error),
                        }
                    }
                    Err(last_error.unwrap_or_else(no_route_connected))
                })
                .await
                .unwrap_or_else(|| Err(cancelled("network transport is stopping")))
        }))
    }

    /// Retires only this failed, still-selected transport when neither outbound
    /// streams nor inbound handlers use it, or when it stayed `silent`: then
    /// those streams cannot reach the peer either, and they fail now instead
    /// of at the QUIC idle timeout. Never wait past a request deadline.
    /// The next outbound dial races any `replacement` against a fresh one.
    pub(crate) fn retire_failed_candidate(
        &self,
        failed: ConnectionCandidateKey,
        replacement: Option<ReplacementTransport>,
        silent: bool,
    ) {
        let Ok(task) = self.broker.inner.lifecycle.admit_task() else {
            return;
        };
        let slot = Arc::clone(&self.slot);
        let limits = self.broker.inner.limits;
        let mut retire = Box::pin(async move {
            let _task = task;
            slot.retire_selected(limits, failed, replacement, silent)
                .await;
        });
        // Poll once before returning: a contended retirement is already in
        // the mutex FIFO ahead of the next request. Keep that same waiter.
        if retire.as_mut().now_or_never().is_none() {
            tokio::spawn(retire);
        }
    }

    fn ensure_active(&self) -> Result<(), DaemonError> {
        if self.released {
            Err(DaemonError::new(
                DomainErrorKind::Cancelled,
                "connection demand has already been released",
            ))
        } else {
            Ok(())
        }
    }

    async fn wait_for_confirmed_primary(
        &self,
        deadline: Instant,
        progress: &ProgressObserver,
        seen: &mut u64,
    ) -> Result<(Arc<Candidate>, AuthGeneration), DaemonError> {
        self.ensure_active()?;
        let mut first = true;
        loop {
            // Register before reading the journal/selection: observations may
            // arrive while this reader forwards a previous burst to the CLI.
            let changed = self.slot.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if Instant::now() >= deadline {
                return Err(deadline_exceeded(
                    "timed out waiting for a primary connection",
                ));
            }
            if self.broker.inner.lifecycle.is_quiescing() {
                return Err(transport_unavailable("network transport is stopping"));
            }
            let (selection, records, terminal_error) = {
                let state = self.slot.state.lock().await;
                let selection = match (state.candidates.primary(), state.remote_acceptance) {
                    (Some(key), Some(generation)) => state
                        .candidates
                        .get(&key)
                        .map(|candidate| (Arc::clone(candidate), generation)),
                    _ => None,
                };
                (
                    selection,
                    state.progress.after(*seen).collect::<Vec<_>>(),
                    state
                        .terminal_error
                        .as_ref()
                        .filter(|error| !is_retryable(error.kind()))
                        .cloned(),
                )
            };
            let reusing = first && selection.is_some();
            for (sequence, event) in records {
                *seen = sequence;
                if !reusing {
                    progress.emit(event);
                }
            }
            if let Some(error) = terminal_error {
                return Err(error);
            }
            if let Some(selection) = selection {
                if reusing {
                    progress.report(ConnectionStage::ReusingConnection);
                    voytty_client::handshake::record_transport_observation(
                        &selection.0.diagnostic,
                        &selection.0.connection,
                        voytty_diagnostics::Stage::ReusingTransport,
                        Some(self.slot.active_streams.load(Ordering::Acquire) as u64),
                    );
                }
                return Ok(selection);
            }
            first = false;
            self.broker
                .ensure_dial_worker(Arc::clone(&self.slot))
                .await?;
            timeout_until(deadline, changed).await?;
        }
    }

    /// Waits for normal Hello/Welcome authorization proof without opening an
    /// application stream.
    pub async fn confirm_authorization(
        &self,
        deadline: Instant,
    ) -> Result<AuthorizationConfirmation, DaemonError> {
        self.confirm_authorization_with_progress(deadline, &ProgressObserver::default())
            .await
    }

    /// Observes initial authentication without consuming a business-stream permit.
    pub(crate) async fn confirm_authorization_with_progress(
        &self,
        deadline: Instant,
        progress: &ProgressObserver,
    ) -> Result<AuthorizationConfirmation, DaemonError> {
        let (candidate, generation) = self
            .wait_for_confirmed_primary(deadline, progress, &mut 0)
            .await?;
        Ok(AuthorizationConfirmation {
            remote: self.slot.remote,
            generation,
            verified_relay: candidate.verified_relay.clone(),
        })
    }

    /// Opens a business stream only after deterministic candidate promotion.
    pub async fn open_bi(
        &self,
        purpose: StreamPurpose,
        deadline: Instant,
    ) -> Result<AuthenticatedBiStream, DaemonError> {
        self.open_bi_with_progress(purpose, deadline, &ProgressObserver::default())
            .await
    }

    /// Observes admission and dialing without introducing another dial owner.
    pub async fn open_bi_with_progress(
        &self,
        purpose: StreamPurpose,
        deadline: Instant,
        progress: &ProgressObserver,
    ) -> Result<AuthenticatedBiStream, DaemonError> {
        progress.report(ConnectionStage::CheckingConnection);
        let mut seen = 0;
        self.ensure_active()?;
        let _queue = self
            .slot
            .admission
            .open_queue
            .clone()
            .try_acquire_owned()
            .map_err(|_| resource_exhausted("peer open-stream queue is full"))?;

        loop {
            let (candidate, generation) = self
                .wait_for_confirmed_primary(deadline, progress, &mut seen)
                .await?;
            let stream_permit = acquire_until(
                Arc::clone(&candidate.admission.streams),
                deadline,
                "connection stream limit reached",
            )
            .await?;

            let still_primary = {
                let state = self.slot.state.lock().await;
                state.candidates.primary() == Some(candidate.key)
                    && state.remote_acceptance == Some(generation)
            };
            if !still_primary {
                drop(stream_permit);
                continue;
            }

            progress.report(ConnectionStage::OpeningSessionChannel);
            match timeout_until(deadline, candidate.connection.open_bi()).await {
                Ok(Ok((send, recv))) => {
                    let metric = StreamMetricGuard::new(
                        Arc::clone(&self.broker.inner.metrics),
                        Arc::clone(&self.slot.active_streams),
                    )?;
                    progress.report(ConnectionStage::SessionChannelReady);
                    return Ok(AuthenticatedBiStream {
                        send,
                        recv,
                        remote: self.slot.remote,
                        remote_generation: generation,
                        candidate: candidate.key,
                        #[cfg(any(unix, windows))]
                        candidate_observer: SelectedCandidateObserver {
                            candidate: Arc::clone(&candidate),
                        },
                        purpose,
                        _stream_permit: stream_permit,
                        _metric: metric,
                    });
                }
                Ok(Err(_)) => {
                    self.broker
                        .candidate_closed(&self.slot, candidate.key)
                        .await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for ConnectionDemand {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.slot.demand.release(&self.transient_routes);
        self.slot.changed.notify_waiters();
    }
}

impl PeerSlot {
    async fn retire_selected(
        &self,
        limits: TransportLimits,
        failed: ConnectionCandidateKey,
        replacement: Option<ReplacementTransport>,
        silent: bool,
    ) {
        let mut state = self.state.lock().await;
        if state.candidates.primary() != Some(failed) {
            return;
        }
        let Some(candidate) = state.candidates.get(&failed).cloned() else {
            return;
        };
        let (Ok(streams), Ok(handlers)) = (
            u32::try_from(limits.max_bi_streams_per_connection),
            u32::try_from(limits.max_stream_handlers_per_connection),
        ) else {
            return;
        };
        // Acquiring all permits makes the idle check atomic with admission in
        // both directions. A busy terminal/upload keeps a connection that the
        // peer still acknowledges.
        let streams = Arc::clone(&candidate.admission.streams).try_acquire_many_owned(streams);
        let handlers = Arc::clone(&candidate.admission.handlers).try_acquire_many_owned(handlers);
        if (streams.is_err() || handlers.is_err()) && !silent {
            return;
        }
        state.candidates.remove(failed);
        if state.candidates.is_empty() {
            state.remote_acceptance = None;
        }
        state.replacement = replacement;
        drop(state);
        candidate.set_primary(false, "retry");
        candidate.cancel(ConnectionCloseReason::EndpointReset);
        self.changed.notify_waiters();
    }
}
