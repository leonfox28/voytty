//! Per-peer dial worker, route selection and authenticated handshakes.

use super::*;

impl ConnectionBroker {
    pub(super) async fn run_dial_worker(&self, slot: Arc<PeerSlot>) {
        'restart: loop {
            let mut retry = 0_u32;
            loop {
                let changed = slot.changed.notified();
                if self.inner.lifecycle.is_quiescing() || slot.demand.count() == 0 {
                    break;
                }
                let action = {
                    let state = slot.state.lock().await;
                    if let Some(error) = &state.terminal_error
                        && !is_retryable(error.kind())
                    {
                        break;
                    }
                    peer_needs_outbound_attempt(
                        &state.candidates,
                        state.remote_acceptance,
                        |candidate| candidate.side == CandidateSide::Outbound,
                    )
                };

                if !action {
                    if tokio::time::timeout(Duration::from_secs(30), changed)
                        .await
                        .is_err()
                    {
                        continue;
                    }
                    retry = 0;
                    continue;
                }

                let result = self.dial_once(Arc::clone(&slot)).await;
                match result {
                    Ok(()) => retry = 0,
                    Err(error) if is_retryable(error.kind()) => {
                        {
                            let mut state = slot.state.lock().await;
                            state.terminal_error = Some(error);
                        }
                        slot.report_progress(ConnectionStage::RetryingConnection)
                            .await;
                        let delay = retry_delay(retry, slot.remote);
                        retry = retry.saturating_add(1);
                        tokio::select! {
                            () = tokio::time::sleep(delay) => {}
                            () = slot.changed.notified() => {}
                        }
                        let mut state = slot.state.lock().await;
                        if state
                            .terminal_error
                            .as_ref()
                            .is_some_and(|stored| is_retryable(stored.kind()))
                        {
                            state.terminal_error = None;
                        }
                    }
                    Err(error) => {
                        let mut state = slot.state.lock().await;
                        state.terminal_error = Some(error);
                        slot.changed.notify_waiters();
                        break;
                    }
                }
            }

            let mut state = slot.state.lock().await;
            state.dial_worker_running = false;
            let should_restart = !self.inner.lifecycle.is_quiescing()
                && slot.demand.count() > 0
                && state.terminal_error.is_none()
                && (state.candidates.primary().is_none() || state.remote_acceptance.is_none());
            if should_restart {
                state.dial_worker_running = true;
                drop(state);
                continue 'restart;
            }
            break;
        }
    }

    pub(super) fn dial_permit(&self) -> Result<OwnedSemaphorePermit, DaemonError> {
        self.inner
            .admission
            .pending_dials
            .clone()
            .try_acquire_owned()
            .map_err(|_| resource_exhausted("global outbound dial limit reached"))
    }

    pub(super) fn bound_endpoint(&self) -> Result<Endpoint, DaemonError> {
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
        Ok(endpoint)
    }

    async fn dial_once(&self, slot: Arc<PeerSlot>) -> Result<(), DaemonError> {
        let _dial_permit = self.dial_permit()?;
        let endpoint = self.bound_endpoint()?;

        let transient_routes = slot.demand.routes();
        // A normal attempt shares one setup window across address lookup,
        // transport authentication and Hello/Welcome. Each demand still has
        // its own absolute request deadline and can stop waiting earlier.
        let deadline = Instant::now() + voytty_client::protocol::REMOTE_STARTUP_DEADLINE;
        let replacement = slot.state.lock().await.replacement.take();
        let fresh = async {
            slot.report_progress(ConnectionStage::LookingUpAddress)
                .await;
            let routes = self
                .dial_routes(&endpoint, slot.remote, &transient_routes, deadline)
                .await?;
            let mut last_error = None;
            for route in routes {
                if self.inner.lifecycle.is_quiescing() || slot.demand.count() == 0 {
                    return Err(DaemonError::new(
                        DomainErrorKind::Cancelled,
                        "outbound demand ended before dial",
                    ));
                }
                slot.report_progress(ConnectionStage::ConnectingSecurely)
                    .await;
                match connect_transport(&endpoint, route.address, VOYTTY_ALPN, deadline).await {
                    Ok(connection) => return Ok(connection),
                    Err(error) => last_error = Some(error),
                }
            }
            Err(last_error.unwrap_or_else(no_route_connected))
        };
        let connection = match replacement {
            Some(replacement) => replacement.race(fresh).await?,
            None => fresh.await?,
        };
        let attempt = random_attempt_id()?;
        let key = ConnectionCandidateKey::new(self.inner.identity.device_id(), attempt);
        configure_normal_connection_limits(&connection, self.inner.limits);
        if device_from_endpoint_id(connection.remote_id()) != slot.remote {
            close_connection(&connection, CLOSE_INCOMPATIBLE, b"identity mismatch");
            return Err(DaemonError::new(
                DomainErrorKind::Unauthorized,
                "connected Iroh identity did not match the target device",
            ));
        }
        slot.report_progress(ConnectionStage::SecureConnectionReady)
            .await;
        let connection_permit = match self
            .inner
            .admission
            .authenticated_connections
            .clone()
            .try_acquire_owned()
        {
            Ok(permit) => permit,
            Err(_) => {
                close_connection(&connection, CLOSE_OVERLOADED, b"transport overloaded");
                return Err(resource_exhausted("authenticated connection limit reached"));
            }
        };

        // Cache the path that reached the authenticated peer, never the
        // first supplied hint. A direct path has no relay to certify here.
        let verified_relay = selected_relay(&connection);
        let result = self
            .run_outbound_handshake(
                Arc::clone(&slot),
                connection,
                key,
                verified_relay,
                connection_permit,
                deadline,
            )
            .await;
        match result {
            Err(error) if error.kind() == DomainErrorKind::TransportUnavailable => {
                let state = slot.state.lock().await;
                if state.remote_acceptance.is_some() && !state.candidates.is_empty() {
                    // This outbound handshake already proved remote
                    // acceptance and only lost duplicate arbitration. A
                    // remaining peer candidate owns progress; dialing again
                    // here would create a self-exciting race.
                    return Ok(());
                }
                Err(error)
            }
            result => result,
        }
    }

    pub(super) async fn dial_routes(
        &self,
        endpoint: &Endpoint,
        remote: DeviceId,
        transient: &[RelayHint],
        deadline: Instant,
    ) -> Result<Vec<DialRoute>, DaemonError> {
        if let Some(address) = mutex_lock(&self.inner.test_routes).get(&remote).cloned() {
            return Ok(vec![DialRoute { address }]);
        }
        self.inner
            .resolver
            .candidates(endpoint, remote, transient, deadline)
            .await
            .and_then(|candidates| {
                Ok(vec![DialRoute {
                    address: combined_relay_address(endpoint_id_from_device(remote)?, &candidates)?,
                }])
            })
    }

    async fn run_outbound_handshake(
        &self,
        slot: Arc<PeerSlot>,
        connection: Connection,
        key: ConnectionCandidateKey,
        verified_relay: Option<RelayHint>,
        connection_permit: OwnedSemaphorePermit,
        deadline: Instant,
    ) -> Result<(), DaemonError> {
        let candidate = Candidate::new(
            key,
            slot.remote,
            connection,
            CandidateSide::Outbound,
            None,
            verified_relay,
            connection_permit,
            Arc::clone(&self.inner.metrics),
            self.inner.limits,
        )?;
        self.register_candidate(&slot, Arc::clone(&candidate))
            .await?;
        let result = async {
            slot.report_progress(ConnectionStage::CheckingProtocolAndAccess)
                .await;
            let identity = self.connection_identity(&slot).await;
            let welcome = controller_handshake(
                &candidate.connection,
                &identity,
                key.attempt,
                self.inner.limits.max_pair_hello_frame_bytes,
                deadline,
            )
            .await?;
            let _ = candidate.remote_capabilities.set(welcome.capabilities());
            self.observe_incarnation(&slot, &candidate, welcome.incarnation())
                .await?;
            {
                let mut state = slot.state.lock().await;
                state.remote_acceptance = Some(welcome.accepted_authorization_generation());
                state.terminal_error = None;
            }
            slot.changed.notify_waiters();
            Ok(())
        }
        .await;

        if let Err(error) = result {
            self.remove_candidate(&slot, key, ConnectionCloseReason::EndpointReset)
                .await;
            return Err(error);
        }

        slot.report_progress(ConnectionStage::ProtocolAndAccessReady)
            .await;
        self.persist_verified_handshake(&candidate);
        slot.report_progress(ConnectionStage::SelectingConnection)
            .await;
        self.promote_candidate(slot, candidate, deadline).await
    }

    pub(super) async fn run_inbound_handshake(
        &self,
        connection: Connection,
        remote: DeviceId,
        admission: crate::authorization::Admission,
        connection_permit: OwnedSemaphorePermit,
    ) -> Result<(), DaemonError> {
        let deadline = Instant::now() + self.inner.limits.first_frame_deadline;
        let (mut send, mut recv) = timeout_until(deadline, connection.accept_bi())
            .await?
            .map_err(|_| {
                close_connection(&connection, CLOSE_INCOMPATIBLE, b"missing handshake");
                transport_unavailable("incoming connection did not open Hello stream")
            })?;
        let frame = read_handshake_frame(
            &mut recv,
            self.inner.limits.max_pair_hello_frame_bytes,
            deadline,
        )
        .await
        .inspect_err(|_| {
            close_connection(&connection, CLOSE_INCOMPATIBLE, b"invalid handshake");
        })?;
        let wire: v2::ConnectionHello = frame
            .decode_message(WireKind::ConnectionHello)
            .map_err(protocol_error)?;
        let hello = ConnectionHello::try_from(wire).map_err(|error| {
            DaemonError::new(
                DomainErrorKind::WireMajorMismatch,
                format!("invalid ConnectionHello: {error}"),
            )
        })?;
        if hello.min_wire_major() > voytty_proto::WIRE_MAJOR
            || hello.max_wire_major() < voytty_proto::WIRE_MAJOR
        {
            close_connection(&connection, CLOSE_INCOMPATIBLE, b"incompatible protocol");
            return Err(DaemonError::new(
                DomainErrorKind::WireMajorMismatch,
                "normal connection has no common wire major",
            ));
        }
        let key = ConnectionCandidateKey::new(remote, hello.attempt_id());
        let verified_relay = selected_relay(&connection);
        let candidate = Candidate::new(
            key,
            remote,
            connection,
            CandidateSide::Inbound,
            Some(admission.snapshot),
            verified_relay,
            connection_permit,
            Arc::clone(&self.inner.metrics),
            self.inner.limits,
        )?;
        let slot = self.peer_slot(remote);
        let _ = candidate.remote_capabilities.set(hello.capabilities());
        self.register_candidate(&slot, Arc::clone(&candidate))
            .await?;

        // Registration precedes this exact-generation recheck so revoke has
        // no gap: if it published before registration this check rejects, and
        // if it publishes after registration close_remote observes the
        // provisional candidate.
        let admission = match recheck_inbound_admission(
            &self.inner.authorization,
            remote,
            admission.snapshot,
        ) {
            Ok(admission) => admission,
            Err(error) => {
                self.remove_candidate(&slot, key, ConnectionCloseReason::Unauthorized)
                    .await;
                return Err(error);
            }
        };

        if let Err(error) = self
            .observe_incarnation(&slot, &candidate, hello.incarnation())
            .await
        {
            self.remove_candidate(&slot, key, ConnectionCloseReason::EndpointReset)
                .await;
            return Err(error);
        }

        let identity = self.connection_identity(&slot).await;
        let welcome = ConnectionWelcome::new(
            voytty_proto::WIRE_MAJOR,
            self.inner.identity.capabilities(),
            self.inner.identity.display_name(),
            self.inner.identity.build().to_owned(),
            self.inner.identity.platform().to_owned(),
            admission.snapshot.generation,
        )
        .map_err(|error| {
            DaemonError::new(
                DomainErrorKind::IdentityInvalid,
                format!("local Welcome became invalid: {error}"),
            )
        })?
        .with_incarnation(Some(identity.incarnation()));
        if let Err(error) = write_handshake_message(
            &mut send,
            WireKind::ConnectionWelcome,
            &v2::ConnectionWelcome::from(&welcome),
            deadline,
        )
        .await
        {
            self.remove_candidate(&slot, key, ConnectionCloseReason::EndpointReset)
                .await;
            return Err(error);
        }
        if send.finish().is_err() {
            self.remove_candidate(&slot, key, ConnectionCloseReason::EndpointReset)
                .await;
            return Err(transport_unavailable("unable to finish Welcome stream"));
        }
        // finish only queues the FIN. Duplicate arbitration can immediately
        // close this connection, discarding a Welcome that is still in flight.
        // The reverse direction then has no authorization proof even though
        // the original primary remains healthy. Drain the handshake within its
        // existing deadline before allowing arbitration to retire its carrier.
        let delivered =
            timeout_until(deadline, send.stopped())
                .await
                .and_then(|result| match result {
                    // Readers consume one complete framed Welcome and then
                    // drop the stream, which may report STOP_SENDING(0)
                    // before the FIN acknowledgement. Both mean this carrier
                    // can be retired; a nonzero stop is an application error.
                    Ok(None) => Ok(()),
                    Ok(Some(code)) if code == VarInt::from_u32(0) => Ok(()),
                    Ok(Some(_)) | Err(_) => {
                        Err(transport_unavailable("Welcome stream was not delivered"))
                    }
                });
        if let Err(error) = delivered {
            self.remove_candidate(&slot, key, ConnectionCloseReason::EndpointReset)
                .await;
            return Err(error);
        }
        self.persist_verified_handshake(&candidate);
        self.promote_candidate(
            slot,
            candidate,
            Instant::now() + self.inner.limits.connect_attempt_budget,
        )
        .await
    }
}
