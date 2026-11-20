//! Session creation, admission and attachment operations exposed by the registry.

use super::*;

impl SessionService {
    /// Creates an account-login-shell service with the default identity.
    #[must_use]
    pub fn new(own_device_id: DeviceId) -> Self {
        Self::with_login_identity(own_device_id, || Ok(TerminalIdentity::default()))
    }

    /// Creates the host service, reading the committed identity for each new session.
    #[must_use]
    pub fn from_config(own_device_id: DeviceId, paths: UserPaths) -> Self {
        Self::with_login_identity(own_device_id, move || {
            crate::config::load_config(&paths).map(|config| config.terminal_identity)
        })
    }

    fn with_login_identity(
        own_device_id: DeviceId,
        identity: impl Fn() -> Result<TerminalIdentity, DaemonError> + Send + Sync + 'static,
    ) -> Self {
        Self::with_spawner(
            own_device_id,
            ResourceLimits::default(),
            move |size, requested_cwd| {
                let identity = identity()?;
                let account = EffectiveAccount::current().map_err(|error| {
                    DaemonError::new(DomainErrorKind::UnsupportedPlatform, error.to_string())
                })?;
                let working_directory = requested_cwd
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| account.home().to_path_buf());
                let session = PtyHost::new()
                    .spawn_login_shell_with_identity(
                        PtySize::new(size.rows, size.columns),
                        Some(&working_directory),
                        identity,
                    )
                    .map_err(map_pty_error)?;
                Ok((session, working_directory))
            },
        )
    }

    /// Creates a service with a deterministic task-private PTY spawner.
    #[doc(hidden)]
    #[must_use]
    pub fn with_spawner<F>(own_device_id: DeviceId, limits: ResourceLimits, spawner: F) -> Self
    where
        F: Fn(TerminalSize, Option<&Path>) -> Result<(PtySession, PathBuf), DaemonError>
            + Send
            + Sync
            + 'static,
    {
        Self {
            inner: Arc::new(RegistryInner::default()),
            own_device_id,
            spawner: Arc::new(spawner),
            limits,
            replay: Arc::new(Mutex::new(ReplayRegistry::new())),
            #[cfg(test)]
            panic_creation_after_spawn: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            panic_next_creation_cleanup: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Allocates one bounded daemon-incarnation mutation lease for this principal.
    pub fn issue_operation_lease(
        &self,
        principal: AttachmentPrincipal,
    ) -> Result<OperationLease, DaemonError> {
        lock(&self.replay, "operation replay")?.issue(principal)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn with_child_cleanup_blocked_for_test(
        &self,
        session_id: SessionId,
        blocked: impl FnOnce(),
    ) -> Result<(), DaemonError> {
        self.inner
            .entry(session_id)?
            .actor
            .interrupt
            .with_child_cleanup_blocked_for_test(blocked);
        Ok(())
    }

    #[cfg(all(test, unix))]
    pub(super) fn panic_next_creation_after_spawn_for_test(&self) {
        self.panic_creation_after_spawn
            .store(true, Ordering::Release);
    }

    #[cfg(all(test, unix))]
    pub(super) fn panic_next_creation_cleanup_for_test(&self) {
        self.panic_next_creation_cleanup
            .store(true, Ordering::Release);
    }

    /// Returns the local same-UID principal for one accepted socket view.
    #[must_use]
    pub fn local_principal(&self, local_view_id: AttachmentId) -> AttachmentPrincipal {
        AttachmentPrincipal::LocalSameUid {
            own_device_id: self.own_device_id,
            local_view_id,
        }
    }

    /// Lists live sessions from short cached actor summaries.
    pub fn list(&self) -> Result<Vec<SessionSummary>, DaemonError> {
        let entries = self.inner.live_entries()?;
        let mut summaries = Vec::with_capacity(entries.len());
        for entry in entries {
            match entry.summary() {
                Ok(summary) => summaries.push(summary),
                Err(error) if error.kind() == DomainErrorKind::SessionNotFound => {}
                Err(error) => return Err(error),
            }
        }
        summaries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(summaries)
    }

    /// Creates a named session exactly once for one retained operation ID.
    pub fn create(
        &self,
        principal: AttachmentPrincipal,
        operation_id: OperationId,
        name: SessionName,
        working_directory: Option<PathBuf>,
        viewport: Option<TerminalSize>,
    ) -> Result<SessionSummary, DaemonError> {
        self.create_until(
            principal,
            operation_id,
            name,
            working_directory,
            viewport,
            default_deadline(),
        )
    }

    pub(crate) fn create_until(
        &self,
        principal: AttachmentPrincipal,
        operation_id: OperationId,
        name: SessionName,
        working_directory: Option<PathBuf>,
        viewport: Option<TerminalSize>,
        deadline: Instant,
    ) -> Result<SessionSummary, DaemonError> {
        self.create_with_colors_until(
            principal,
            operation_id,
            name,
            working_directory,
            crate::session::InitialTerminal {
                viewport,
                colors: TerminalColorProfile::default(),
            },
            deadline,
        )
    }

    pub(crate) fn create_with_colors_until(
        &self,
        principal: AttachmentPrincipal,
        operation_id: OperationId,
        name: SessionName,
        working_directory: Option<PathBuf>,
        initial: InitialTerminal,
        deadline: Instant,
    ) -> Result<SessionSummary, DaemonError> {
        let InitialTerminal {
            viewport,
            colors: base_colors,
        } = initial;
        let fingerprint = OperationFingerprint::Create {
            base_colors: base_colors.clone(),
            name: name.clone(),
            working_directory: working_directory.clone(),
            viewport,
        };
        self.execute_replayed(principal, operation_id, fingerprint, || {
            ensure_before_deadline(deadline)?;
            if name.is_main() {
                return Err(reserved_main());
            }
            self.create_inner(
                name,
                working_directory,
                viewport,
                false,
                deadline,
                base_colors,
            )
        })
    }

    /// Renames one live session without changing its stable identity.
    pub fn rename(
        &self,
        principal: AttachmentPrincipal,
        operation_id: OperationId,
        session_id: SessionId,
        new_name: SessionName,
    ) -> Result<SessionSummary, DaemonError> {
        self.rename_until(
            principal,
            operation_id,
            session_id,
            new_name,
            default_deadline(),
        )
    }

    pub(crate) fn rename_until(
        &self,
        principal: AttachmentPrincipal,
        operation_id: OperationId,
        session_id: SessionId,
        new_name: SessionName,
        deadline: Instant,
    ) -> Result<SessionSummary, DaemonError> {
        let fingerprint = OperationFingerprint::Rename {
            session_id,
            new_name: new_name.clone(),
        };
        self.execute_replayed(principal, operation_id, fingerprint, || {
            ensure_before_deadline(deadline)?;
            self.rename_inner(session_id, new_name)
        })
    }

    /// Explicitly closes only the selected session.
    pub fn close(
        &self,
        principal: AttachmentPrincipal,
        operation_id: OperationId,
        session_id: SessionId,
    ) -> Result<SessionSummary, DaemonError> {
        self.close_until(principal, operation_id, session_id, default_deadline())
    }

    pub(crate) fn close_until(
        &self,
        principal: AttachmentPrincipal,
        operation_id: OperationId,
        session_id: SessionId,
        deadline: Instant,
    ) -> Result<SessionSummary, DaemonError> {
        self.execute_replayed(
            principal,
            operation_id,
            OperationFingerprint::Close { session_id },
            || {
                ensure_before_deadline(deadline)?;
                self.close_inner(session_id, SessionEndReason::ExplicitClose)
            },
        )
    }

    /// Resolves a session and prepares its first full snapshot.
    pub fn prepare_attach(
        &self,
        principal: AttachmentPrincipal,
        selector: Option<SessionSelector>,
        create_main: bool,
        takeover: bool,
        initial_viewport: Option<TerminalSize>,
    ) -> Result<PreparedAttachment, DaemonError> {
        self.prepare_attach_until(
            principal,
            selector,
            create_main,
            takeover,
            initial_viewport,
            default_deadline(),
        )
    }

    pub(crate) fn prepare_attach_until(
        &self,
        principal: AttachmentPrincipal,
        selector: Option<SessionSelector>,
        create_main: bool,
        takeover: bool,
        initial_viewport: Option<TerminalSize>,
        deadline: Instant,
    ) -> Result<PreparedAttachment, DaemonError> {
        self.prepare_attach_with_colors_until(
            principal,
            selector,
            create_main,
            takeover,
            crate::session::InitialTerminal {
                viewport: initial_viewport,
                colors: TerminalColorProfile::default(),
            },
            deadline,
        )
    }

    pub(crate) fn prepare_attach_with_colors_until(
        &self,
        principal: AttachmentPrincipal,
        selector: Option<SessionSelector>,
        create_main: bool,
        takeover: bool,
        initial: InitialTerminal,
        deadline: Instant,
    ) -> Result<PreparedAttachment, DaemonError> {
        let InitialTerminal {
            viewport: initial_viewport,
            colors: base_colors,
        } = initial;
        ensure_before_deadline(deadline)?;
        if let Some(size) = initial_viewport {
            validate_viewport(self.limits, size)?;
        }
        let actor = if create_main {
            if selector.is_some() {
                return Err(invalid_session(
                    "default main attach must not include a selector",
                ));
            }
            self.default_main(initial_viewport, deadline, base_colors.clone())?
        } else {
            let selector =
                selector.ok_or_else(|| invalid_session("session selector is required"))?;
            self.resolve(&selector)?
        };
        actor.request(deadline, |meta, reply| SessionCommand::PrepareAttach {
            meta,
            principal,
            takeover,
            base_colors,
            resume: None,
            reply,
        })
    }

    /// Prepares one authenticated remote attachment with an optional exact
    /// latest-state resume baseline. Local adapters never call this path.
    #[cfg(any(unix, windows))]
    pub(crate) fn prepare_remote_attach_until(
        &self,
        principal: AttachmentPrincipal,
        request: RemoteAttachmentRequest,
        deadline: Instant,
    ) -> Result<PreparedAttachment, DaemonError> {
        ensure_before_deadline(deadline)?;
        if !matches!(principal, AttachmentPrincipal::RemoteEndpoint { .. }) {
            return Err(principal_mismatch());
        }
        if let Some(size) = request.initial_viewport {
            validate_viewport(self.limits, size)?;
        }
        let actor = if request.create_main {
            if request.selector.is_some() {
                return Err(invalid_session(
                    "default main attach must not include a selector",
                ));
            }
            self.default_main(
                request.initial_viewport,
                deadline,
                request.base_colors.clone(),
            )?
        } else {
            let selector = request
                .selector
                .ok_or_else(|| invalid_session("session selector is required"))?;
            self.resolve(&selector)?
        };
        actor.request(deadline, |meta, reply| SessionCommand::PrepareAttach {
            meta,
            principal,
            takeover: request.takeover,
            base_colors: request.base_colors,
            resume: Some(request.resume),
            reply,
        })
    }

    /// Checks existing controller authority without creating an attachment or lease.
    #[cfg(any(unix, windows))]
    pub(crate) fn admit_upload_until(
        &self,
        principal: AttachmentPrincipal,
        binding: voytty_core::upload::UploadBinding,
        deadline: Instant,
    ) -> Result<UploadAdmission, DaemonError> {
        if !matches!(principal, AttachmentPrincipal::RemoteEndpoint { .. }) {
            return Err(principal_mismatch());
        }
        let actor = self.resolve(&SessionSelector::Id(binding.session_id))?;
        actor.request(deadline, |meta, reply| SessionCommand::AdmitUpload {
            meta,
            principal,
            attachment_id: binding.attachment_id,
            reply,
        })
    }

    /// Atomically transfers controller ownership to a synchronized pending attachment.
    pub fn takeover(
        &self,
        principal: AttachmentPrincipal,
        operation_id: OperationId,
        attachment: &SessionAttachment,
    ) -> Result<SessionSummary, DaemonError> {
        self.takeover_until(principal, operation_id, attachment, default_deadline())
    }

    pub(crate) fn takeover_until(
        &self,
        principal: AttachmentPrincipal,
        operation_id: OperationId,
        attachment: &SessionAttachment,
        deadline: Instant,
    ) -> Result<SessionSummary, DaemonError> {
        self.execute_takeover_replayed(
            principal,
            operation_id,
            attachment.session_id(),
            attachment.attachment_id,
            deadline,
        )
    }

    /// Commits takeover for adapters which validated an attachment ID on-stream.
    pub fn takeover_by_id(
        &self,
        principal: AttachmentPrincipal,
        operation_id: OperationId,
        session_id: SessionId,
        attachment_id: AttachmentId,
    ) -> Result<SessionSummary, DaemonError> {
        self.takeover_by_id_until(
            principal,
            operation_id,
            session_id,
            attachment_id,
            default_deadline(),
        )
    }

    pub(crate) fn takeover_by_id_until(
        &self,
        principal: AttachmentPrincipal,
        operation_id: OperationId,
        session_id: SessionId,
        attachment_id: AttachmentId,
        deadline: Instant,
    ) -> Result<SessionSummary, DaemonError> {
        self.execute_takeover_replayed(principal, operation_id, session_id, attachment_id, deadline)
    }

    /// Detaches every attachment owned by one remote endpoint across all live
    /// and provisional sessions without closing any session or PTY.
    pub fn detach_remote_principal(
        &self,
        device_id: DeviceId,
    ) -> Result<PrincipalDetachImpact, DaemonError> {
        self.detach_remote_principal_until(device_id, default_deadline())
    }

    pub(crate) fn detach_remote_principal_until(
        &self,
        device_id: DeviceId,
        deadline: Instant,
    ) -> Result<PrincipalDetachImpact, DaemonError> {
        #[cfg(test)]
        {
            self.detach_remote_principal_inner(device_id, deadline, None)
        }
        #[cfg(not(test))]
        {
            self.detach_remote_principal_inner(device_id, deadline)
        }
    }

    /// Counts current attachments owned by one remote endpoint without
    /// detaching them, releasing controller leases, or changing Session state.
    #[cfg(any(unix, windows))]
    pub(crate) fn remote_attachment_count_until(
        &self,
        device_id: DeviceId,
        deadline: Instant,
    ) -> Result<usize, DaemonError> {
        ensure_before_deadline(deadline)?;
        let entries = self.inner.owned_entries()?;
        let mut waiters = Vec::with_capacity(entries.len());
        for entry in entries {
            match entry.actor.enqueue_command(deadline, move |meta, reply| {
                SessionCommand::CountRemoteAttachments {
                    meta,
                    device_id,
                    reply,
                }
            }) {
                Ok(waiter) => waiters.push(waiter),
                Err(error) if error.kind() == DomainErrorKind::SessionNotFound => {}
                Err(error) => return Err(error),
            }
        }

        let mut total = 0_usize;
        for waiter in waiters {
            let count = match waiter.wait(deadline) {
                Ok(count) => count,
                Err(error) if error.kind() == DomainErrorKind::SessionNotFound => continue,
                Err(error) => return Err(error),
            };
            total = total
                .checked_add(count)
                .ok_or_else(|| resource_error("remote attachment count overflowed"))?;
        }
        Ok(total)
    }

    #[cfg(all(test, unix))]
    pub(super) fn detach_remote_principal_until_observed(
        &self,
        device_id: DeviceId,
        deadline: Instant,
        observe: impl Fn(SessionId) -> Option<SyncSender<()>>,
    ) -> Result<PrincipalDetachImpact, DaemonError> {
        self.detach_remote_principal_inner(device_id, deadline, Some(&observe))
    }

    fn detach_remote_principal_inner(
        &self,
        device_id: DeviceId,
        deadline: Instant,
        #[cfg(test)] observe: Option<&dyn Fn(SessionId) -> Option<SyncSender<()>>>,
    ) -> Result<PrincipalDetachImpact, DaemonError> {
        ensure_before_deadline(deadline)?;
        let entries = self.inner.owned_entries()?;
        // Phase 1: admit a detach command to every owned actor. A session
        // whose actor already ended is skipped; its in-flight result would be
        // unreachable anyway. No wait happens here, so a blocked actor cannot
        // prevent another session's detach from being admitted.
        let mut waiters = Vec::with_capacity(entries.len());
        for entry in entries {
            #[cfg(test)]
            let session_id = entry.actor.id;
            match entry.actor.enqueue_command(deadline, move |meta, reply| {
                SessionCommand::DetachRemotePrincipal {
                    meta,
                    device_id,
                    #[cfg(test)]
                    processed: observe.and_then(|observer| observer(session_id)),
                    reply,
                }
            }) {
                Ok(waiter) => waiters.push(waiter),
                Err(error) if error.kind() == DomainErrorKind::SessionNotFound => {}
                Err(error) => return Err(error),
            }
        }
        // Phase 2: collect exact outcomes under the same absolute deadline.
        let mut impact = PrincipalDetachImpact::default();
        for waiter in waiters {
            let outcome = match waiter.wait(deadline) {
                Ok(outcome) => outcome,
                Err(error) if error.kind() == DomainErrorKind::SessionNotFound => continue,
                Err(error) => return Err(error),
            };
            if outcome.attachments_removed > 0 {
                impact.sessions_affected += 1;
                impact.attachments_removed += outcome.attachments_removed;
            }
            if outcome.controller_released {
                impact.controllers_released += 1;
            }
        }
        Ok(impact)
    }

    /// Explicitly closes every current session and returns their pre-stop summaries.
    pub fn shutdown(&self) -> Result<Vec<SessionSummary>, DaemonError> {
        self.shutdown_until(Instant::now() + DEFAULT_SHUTDOWN_TIMEOUT)
    }

    pub(crate) fn shutdown_until(
        &self,
        deadline: Instant,
    ) -> Result<Vec<SessionSummary>, DaemonError> {
        match self.shutdown_with_approval_until(true, deadline)? {
            ShutdownOutcome::Stopped(sessions) => Ok(sessions),
            ShutdownOutcome::NeedsConfirmation(_) => unreachable!("approved shutdown"),
        }
    }

    pub(crate) fn shutdown_with_approval_until(
        &self,
        approved: bool,
        deadline: Instant,
    ) -> Result<ShutdownOutcome, DaemonError> {
        ensure_before_deadline(deadline)?;
        let cancelled_creations = match self.inner.begin_shutdown(approved) {
            Ok(creations) => creations,
            Err(names) => return Ok(ShutdownOutcome::NeedsConfirmation(names)),
        };
        for creation in cancelled_creations {
            creation.cancel();
        }

        let mut summaries = BTreeMap::<SessionId, SessionSummary>::new();
        // Cleanup-only fallback owners can deliberately coexist with a
        // conflicting SessionId after a poisoned/corrupt registration path.
        // Lifecycle work is therefore de-duplicated by actor ownership, never
        // by the externally visible ID which may be the corrupt value.
        let mut summary_checked = BTreeSet::new();
        let mut joined = BTreeSet::new();
        let mut observed = BTreeMap::new();
        let mut cleanup_errors = Vec::new();

        loop {
            let entries = self.inner.owned_entries()?;
            // First issue close to every owner, including actors which became
            // provisional after shutdown began. Summary errors are retained
            // but never short-circuit another owner's interruption.
            for entry in &entries {
                let actor_identity = Arc::as_ptr(&entry.actor);
                observed
                    .entry(actor_identity)
                    .or_insert_with(|| entry.clone());
                if summary_checked.insert(actor_identity) {
                    match entry.summary() {
                        Ok(summary) => {
                            summaries.insert(entry.actor.id, summary);
                        }
                        Err(error) if error.kind() == DomainErrorKind::SessionNotFound => {}
                        Err(error) => cleanup_errors.push(error),
                    }
                }
                entry.actor.begin_end(SessionEndReason::DaemonStop);
            }
            // Retain every actor observed during this shutdown. Its registry
            // finalizer may compare-remove the entry immediately before its OS
            // thread returns; dropping that summary must not let a
            // successful stop skip the corresponding JoinHandle.
            for entry in observed.values() {
                let actor_identity = Arc::as_ptr(&entry.actor);
                if entry.actor.worker_done() && !joined.contains(&actor_identity) {
                    if let Err(error) = entry.actor.wait_finished_until(deadline)
                        && error.kind() != DomainErrorKind::SessionNotFound
                    {
                        cleanup_errors.push(error);
                    }
                    if let Err(error) = entry.actor.join_finished_cleanup() {
                        cleanup_errors.push(error);
                    }
                    joined.insert(actor_identity);
                }
            }
            let all_observed_joined = observed.keys().all(|identity| joined.contains(identity));
            if self.inner.owned_entries()?.is_empty()
                && self.inner.reservation_count()? == 0
                && all_observed_joined
            {
                let mut summaries = summaries.into_values().collect::<Vec<_>>();
                summaries.sort_by(|left, right| left.name.cmp(&right.name));
                if let Some(error) = cleanup_errors.into_iter().next() {
                    self.inner.resume_after_failed_shutdown()?;
                    return Err(error);
                }
                return Ok(ShutdownOutcome::Stopped(summaries));
            }
            if Instant::now() >= deadline {
                self.inner.resume_after_failed_shutdown()?;
                let remaining = self
                    .inner
                    .owned_entries()?
                    .into_iter()
                    .map(|entry| entry.name.to_string())
                    .collect::<Vec<_>>();
                let mut remaining = remaining;
                remaining.extend(
                    observed
                        .iter()
                        .filter(|(identity, _)| !joined.contains(identity))
                        .map(|(_, entry)| entry.name.to_string()),
                );
                remaining.sort();
                remaining.dedup();
                return Err(DaemonError::new(
                    DomainErrorKind::DeadlineExceeded,
                    format!(
                        "session shutdown deadline elapsed; remaining sessions: {}",
                        remaining.join(", ")
                    ),
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn default_main(
        &self,
        initial_viewport: Option<TerminalSize>,
        deadline: Instant,
        base_colors: TerminalColorProfile,
    ) -> Result<Arc<SessionActor>, DaemonError> {
        let name = SessionName::main();
        match self.inner.reserve_name(&name, true)? {
            NameReservation::Existing(entry) => Ok(entry.actor),
            NameReservation::Waiting(creation) => {
                creation.wait_until(deadline)?.map(|entry| entry.actor)
            }
            NameReservation::Owner(creation) => self
                .create_reserved(
                    name,
                    None,
                    initial_viewport,
                    creation,
                    deadline,
                    base_colors,
                )
                .map(|entry| entry.actor),
        }
    }

    fn create_inner(
        &self,
        name: SessionName,
        working_directory: Option<PathBuf>,
        viewport: Option<TerminalSize>,
        allow_main: bool,
        deadline: Instant,
        base_colors: TerminalColorProfile,
    ) -> Result<SessionSummary, DaemonError> {
        if name.is_main() && !allow_main {
            return Err(reserved_main());
        }
        match self.inner.reserve_name(&name, false)? {
            NameReservation::Owner(creation) => self
                .create_reserved(
                    name,
                    working_directory,
                    viewport,
                    creation,
                    deadline,
                    base_colors,
                )?
                .summary(),
            NameReservation::Existing(_) | NameReservation::Waiting(_) => {
                Err(session_already_exists(&name))
            }
        }
    }

    fn create_reserved(
        &self,
        name: SessionName,
        working_directory: Option<PathBuf>,
        viewport: Option<TerminalSize>,
        creation: Arc<CreationCell>,
        deadline: Instant,
        base_colors: TerminalColorProfile,
    ) -> Result<SessionEntry, DaemonError> {
        let mut owner = CreationOwner::new(
            Arc::clone(&self.inner),
            name.clone(),
            Arc::clone(&creation),
            deadline,
        );
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.create_reserved_inner(
                name,
                working_directory,
                crate::session::InitialTerminal {
                    viewport,
                    colors: base_colors,
                },
                &creation,
                deadline,
                &mut owner,
            )
        }))
        .unwrap_or_else(|_| Err(outcome_unknown()));
        owner.finish(result)
    }

    fn create_reserved_inner(
        &self,
        name: SessionName,
        working_directory: Option<PathBuf>,
        initial: InitialTerminal,
        creation: &Arc<CreationCell>,
        deadline: Instant,
        owner: &mut CreationOwner,
    ) -> Result<SessionEntry, DaemonError> {
        let InitialTerminal {
            viewport,
            colors: base_colors,
        } = initial;
        ensure_before_deadline(deadline)?;
        if creation.is_cancelled() {
            return Err(DaemonError::new(
                DomainErrorKind::Cancelled,
                "session creation was cancelled before ownership acquisition",
            ));
        }
        let size = viewport.unwrap_or_else(|| {
            TerminalSize::new(
                self.limits.no_controller_rows,
                self.limits.no_controller_columns,
            )
        });
        validate_viewport(self.limits, size)?;
        let session_id = self
            .inner
            .reserve_creation_session(&name, creation, self.limits)?;
        owner.set_session_id(session_id);

        let mut model = TerminalModel::new(size, self.limits.recent_history_rows)
            .map_err(map_terminal_error)?;
        ensure_before_deadline(deadline)?;
        model
            .update_base_colors(base_colors)
            .map_err(map_terminal_error)?;
        let (session, actual_cwd) = (self.spawner)(size, working_directory.as_deref())?;
        let mut spawned = SpawnedPtyOwner::new(session);
        #[cfg(test)]
        if self
            .panic_creation_after_spawn
            .swap(false, Ordering::AcqRel)
        {
            panic!("injected creation panic after PTY spawn");
        }
        let driver = TerminalDriver::start(spawned.take(), model, TerminalDriverConfig::default())
            .map_err(map_driver_error)?;
        let actor = SessionActor::start(
            session_id,
            size,
            driver,
            Arc::downgrade(&self.inner),
            self.limits,
        )?;
        // From this point onward the creation guard owns the started actor.
        // Registration is fallible, so the guard must be able to publish a
        // cleanup-only provisional owner before any error can unwind this
        // stack frame.
        owner.set_actor(Arc::clone(&actor), actual_cwd.clone());
        #[cfg(test)]
        if self
            .panic_next_creation_cleanup
            .swap(false, Ordering::AcqRel)
        {
            actor.panic_next_close_for_test();
        }
        let entry = SessionEntry {
            actor: Arc::clone(&actor),
            name: name.clone(),
            working_directory: actual_cwd,
            ownership: creation.ownership.clone(),
        };
        if let Err(error) = self.inner.register_provisional(session_id, entry.clone()) {
            owner.retain_provisional_for_cleanup();
            return Err(error);
        }
        actor.mark_registry_owned();
        if !self
            .inner
            .publish_name(&name, creation, session_id, &actor)?
        {
            return Err(DaemonError::new(
                DomainErrorKind::Cancelled,
                "session publication was cancelled after PTY startup",
            ));
        }
        tracing::info!(component = "session", operation = "created", session_id = %session_id, name = %name, "Session created");
        Ok(entry)
    }

    fn rename_inner(
        &self,
        session_id: SessionId,
        new_name: SessionName,
    ) -> Result<SessionSummary, DaemonError> {
        if new_name.is_main() {
            return Err(reserved_main());
        }
        let entry = self.inner.rename(session_id, new_name)?;
        tracing::info!(component = "session", operation = "renamed", session_id = %session_id, "Session renamed");
        entry.summary()
    }

    fn close_inner(
        &self,
        session_id: SessionId,
        reason: SessionEndReason,
    ) -> Result<SessionSummary, DaemonError> {
        let entry = self.inner.entry(session_id)?;
        let summary = entry.summary()?;
        entry.actor.begin_end(reason);
        entry.actor.wait_finished()?;
        entry.actor.join_finished()?;
        Ok(summary)
    }

    pub(super) fn resolve(
        &self,
        selector: &SessionSelector,
    ) -> Result<Arc<SessionActor>, DaemonError> {
        self.inner.resolve(selector)
    }

    pub(super) fn summary(&self, session_id: SessionId) -> Result<SessionSummary, DaemonError> {
        self.inner.entry(session_id)?.summary()
    }

    pub(super) fn execute_replayed(
        &self,
        principal: AttachmentPrincipal,
        operation_id: OperationId,
        fingerprint: OperationFingerprint,
        operation: impl FnOnce() -> MutationResult,
    ) -> MutationResult {
        let key = ReplayKey::new(principal, operation_id.lease.ordinal);
        let registration = {
            let mut replay = lock(&self.replay, "operation replay")?;
            replay.register(key, operation_id, fingerprint)?
        };
        match registration {
            ReplayRegistration::Join(cell) => cell.wait(),
            ReplayRegistration::Execute(cell) => {
                let mut completion = OperationCompletionGuard::new(Arc::clone(&cell));
                let result = catch_unwind(AssertUnwindSafe(operation))
                    .unwrap_or_else(|_| Err(outcome_unknown()));
                completion.complete(result.clone());
                result
            }
        }
    }

    fn execute_takeover_replayed(
        &self,
        principal: AttachmentPrincipal,
        operation_id: OperationId,
        session_id: SessionId,
        attachment_id: AttachmentId,
        deadline: Instant,
    ) -> MutationResult {
        let replay_key = ReplayKey::new(principal, operation_id.lease.ordinal);
        let operation_key = ReplayOperationKey {
            replay_key,
            operation_id,
        };
        let registration = {
            let mut replay = lock(&self.replay, "operation replay")?;
            replay.register(
                replay_key,
                operation_id,
                OperationFingerprint::Takeover { session_id },
            )?
        };
        match registration {
            ReplayRegistration::Execute(cell) => {
                let mut completion = OperationCompletionGuard::new(Arc::clone(&cell));
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let actor = self.resolve(&SessionSelector::Id(session_id))?;
                    actor.request(deadline, |meta, reply| SessionCommand::Takeover {
                        meta,
                        principal,
                        attachment_id,
                        operation_key,
                        continuation: false,
                        reply,
                    })?;
                    self.summary(session_id)
                }))
                .unwrap_or_else(|_| Err(outcome_unknown()));
                completion.complete(result.clone());
                result
            }
            ReplayRegistration::Join(cell) => {
                let retained = cell.wait()?;
                let actor = self.resolve(&SessionSelector::Id(session_id))?;
                actor.request(deadline, |meta, reply| SessionCommand::Takeover {
                    meta,
                    principal,
                    attachment_id,
                    operation_key,
                    continuation: true,
                    reply,
                })?;
                Ok(retained)
            }
        }
    }
}
