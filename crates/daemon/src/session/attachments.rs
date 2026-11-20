//! Session actor command dispatch and attachment state transitions.

use super::*;

pub(super) fn dispatch_command(
    actor: &Arc<SessionActor>,
    runtime: &mut SessionRuntime,
    command: SessionCommand,
) {
    match command {
        SessionCommand::PrepareAttach {
            base_colors,
            meta,
            principal,
            takeover,
            resume,
            reply,
        } => respond(actor, meta, reply, || {
            prepare_attach(actor, runtime, principal, takeover, resume, base_colors)
        }),
        #[cfg(any(unix, windows))]
        SessionCommand::DetachForRemoteResume {
            meta,
            attachment_id,
            reply,
        } => respond(actor, meta, reply, || {
            detach_for_remote_resume(actor, runtime, attachment_id)
        }),
        SessionCommand::SnapshotApplied {
            meta,
            attachment_id,
            revision,
            reply,
        } => respond(actor, meta, reply, || {
            let was_active = runtime
                .attachments
                .get(&attachment_id)
                .is_some_and(|attachment| attachment.ever_active);
            let result = snapshot_applied(runtime, attachment_id, revision);
            if result.is_ok() && voytty_diagnostics::detail_enabled() {
                voytty_diagnostics::record(
                    voytty_diagnostics::Event::new(voytty_diagnostics::Kind::SyncChanged)
                        .level(voytty_diagnostics::Level::Debug)
                        .session(actor.id)
                        .attachment(attachment_id)
                        .stage(voytty_diagnostics::Stage::SnapshotAcknowledged)
                        .epoch(revision.get()),
                );
            }
            if result.is_ok()
                && !was_active
                && runtime
                    .attachments
                    .get(&attachment_id)
                    .is_some_and(|attachment| attachment.ever_active)
            {
                tracing::info!(component = "session", operation = "controller_attached", session_id = %actor.id, attachment_id = %attachment_id, "Controller attached");
            }
            result
        }),
        SessionCommand::NextUpdate {
            meta,
            attachment_id,
            reply,
        } => respond(actor, meta, reply, || next_update(runtime, attachment_id)),
        #[cfg(any(unix, windows))]
        SessionCommand::FinalUpdate {
            meta,
            attachment_id,
            reply,
        } => respond(actor, meta, reply, || final_update(runtime, attachment_id)),
        SessionCommand::SyncLatest {
            meta,
            attachment_id,
            known_revision,
            reply,
        } => respond(actor, meta, reply, || {
            sync_latest(runtime, attachment_id, known_revision)
        }),
        #[cfg(any(unix, windows))]
        SessionCommand::HistoryWindow {
            meta,
            attachment_id,
            query,
            reply,
        } => {
            let deadline = meta.deadline;
            respond(actor, meta, reply, || {
                history_window(runtime, attachment_id, query, deadline)
            })
        }
        #[cfg(any(unix, windows))]
        SessionCommand::ValidateHistoryController {
            meta,
            attachment_id,
            reply,
        } => respond(actor, meta, reply, || {
            require_existing_visual_sync_controller(runtime, attachment_id).map(|_| ())
        }),
        #[cfg(any(unix, windows))]
        SessionCommand::AdmitUpload {
            meta,
            principal,
            attachment_id,
            reply,
        } => respond(actor, meta, reply, || {
            let generation = require_existing_visual_sync_controller(runtime, attachment_id)?;
            let attachment = runtime
                .attachments
                .get(&attachment_id)
                .ok_or_else(lease_lost)?;
            if attachment.principal != principal || attachment.detached.load(Ordering::Acquire) {
                return Err(principal_mismatch());
            }
            Ok(UploadAdmission {
                generation,
                lifecycle: attachment.lifecycle.subscribe(),
                detached: Arc::clone(&attachment.detached),
            })
        }),
        SessionCommand::WriteInput {
            meta,
            attachment_id,
            bytes,
            reply,
        } => respond(actor, meta, reply, || {
            write_input(runtime, attachment_id, &bytes)
        }),
        SessionCommand::UpdateColors {
            meta,
            attachment_id,
            sequence,
            base_colors,
            reply,
        } => respond(actor, meta, reply, || {
            require_resize_controller(runtime, attachment_id)?;
            let attachment = runtime
                .attachments
                .get_mut(&attachment_id)
                .ok_or_else(lease_lost)?;
            if sequence == 0 || sequence <= attachment.color_sequence {
                return Err(invalid_session("stale color observation sequence"));
            }
            runtime
                .driver
                .as_ref()
                .ok_or_else(session_not_found)?
                .update_base_colors(base_colors.clone())
                .map_err(map_driver_error)?;
            attachment.base_colors = base_colors;
            attachment.color_sequence = sequence;
            actor.update_cached(runtime, false);
            Ok(())
        }),
        SessionCommand::Resize {
            meta,
            attachment_id,
            size,
            reply,
        } => respond(actor, meta, reply, || {
            resize(actor, runtime, attachment_id, size)
        }),
        SessionCommand::Takeover {
            meta,
            principal,
            attachment_id,
            operation_key,
            continuation,
            reply,
        } => respond(actor, meta, reply, || {
            takeover(
                actor,
                runtime,
                principal,
                attachment_id,
                operation_key,
                continuation,
            )
        }),
        SessionCommand::DetachRemotePrincipal {
            meta,
            device_id,
            #[cfg(test)]
            processed,
            reply,
        } => respond(actor, meta, reply, || {
            let outcome = detach_remote_principal(actor, runtime, device_id)?;
            #[cfg(test)]
            if let Some(processed) = processed {
                let _ = processed.send(());
            }
            Ok(outcome)
        }),
        #[cfg(any(unix, windows))]
        SessionCommand::CountRemoteAttachments {
            meta,
            device_id,
            reply,
        } => respond(actor, meta, reply, || {
            Ok(count_remote_attachments(runtime, device_id))
        }),
        #[cfg(all(test, unix))]
        SessionCommand::BlockPtyEffect {
            meta,
            entered,
            release,
            executions,
            reply,
        } => respond(actor, meta, reply, || {
            executions.fetch_add(1, Ordering::AcqRel);
            entered
                .send(())
                .map_err(|_| synchronization_error("test PTY-effect entry"))?;
            release
                .recv()
                .map_err(|_| synchronization_error("test PTY-effect release"))?;
            Ok(())
        }),
        #[cfg(all(test, unix))]
        SessionCommand::PanicWorker => panic!("injected session actor worker panic"),
        SessionCommand::Wake => {}
    }
}

fn respond<R>(
    actor: &SessionActor,
    meta: CommandMeta,
    reply: SyncSender<Result<R, DaemonError>>,
    operation: impl FnOnce() -> Result<R, DaemonError>,
) {
    let result = match actor.try_start_command(&meta) {
        CommandStart::Started => operation(),
        CommandStart::Expired => Err(deadline_error(
            "session command expired before starting its side effect",
        )),
        CommandStart::Ending => Err(session_not_found()),
    };
    let _ = reply.send(result);
}

fn prepare_attach(
    actor: &Arc<SessionActor>,
    runtime: &mut SessionRuntime,
    principal: AttachmentPrincipal,
    takeover: bool,
    resume: Option<RemoteResumeRequest>,
    base_colors: TerminalColorProfile,
) -> Result<PreparedAttachment, DaemonError> {
    reap_detached(actor, runtime)?;
    #[cfg(any(unix, windows))]
    let resume_view_id = resume.map(|request| request.view_id);
    let resumed_terminal = take_resume_terminal(actor, runtime, principal, resume);
    if runtime.controller.is_some() && !takeover {
        #[cfg(any(unix, windows))]
        if let Some(request) = resume
            && let Some(controller) = runtime.controller
            && runtime
                .attachments
                .get(&controller.attachment_id)
                .is_some_and(|attachment| {
                    attachment.principal == principal
                        && attachment.resume_view_id == Some(request.view_id)
                })
        {
            // The same view may detect a one-sided transport loss before its
            // old stream releases the lease. Only this exact owner may wait;
            // a different controller is a definitive occupied result and must
            // not leave the retired view silently reclaiming a later vacancy.
            return Err(DaemonError::new(
                DomainErrorKind::TransportUnavailable,
                "the previous transport for this remote view has not released control",
            ));
        }
        return Err(DaemonError::new(
            DomainErrorKind::SessionOccupied,
            "session already has a controller; use explicit takeover",
        ));
    }
    if runtime.controller.is_some()
        && runtime.attachments.values().any(|attachment| {
            matches!(
                attachment.sync,
                AttachmentSync::Awaiting {
                    target: SyncTarget::PreparedTakeover,
                    ..
                } | AttachmentSync::PreparedTakeover
            )
        })
    {
        return Err(DaemonError::new(
            DomainErrorKind::SessionOccupied,
            "session already has one pending takeover",
        ));
    }
    let attachment_id = next_attachment_id(&runtime.attachments)?;
    let driver = runtime.driver.as_ref().ok_or_else(session_not_found)?;
    if runtime.controller.is_none() {
        runtime
            .next_generation
            .checked_add(1)
            .ok_or_else(|| resource_error("controller generation exhausted"))?;
        driver
            .update_base_colors(base_colors.clone())
            .map_err(map_driver_error)?;
    }
    let mut terminal = resumed_terminal.unwrap_or_else(|| driver.attach());
    #[cfg(any(unix, windows))]
    let effect_broker = terminal.effect_broker();
    #[cfg(any(unix, windows))]
    let effect_wakeup = effect_broker.subscribe();
    let revisions = terminal.revision_watch();
    let initial_state = if resume.is_some() && terminal.checkpoint_revision().is_some() {
        match terminal.sync_latest().map_err(map_driver_error)? {
            TerminalSurfaceDeltaResult::Delta(delta) => {
                let revision = delta.to_revision;
                let snapshot = terminal.latest_snapshot().map_err(map_driver_error)?;
                (snapshot, Some(delta), revision)
            }
            TerminalSurfaceDeltaResult::Resync(snapshot) => {
                let revision = snapshot.revision;
                (snapshot, None, revision)
            }
        }
    } else {
        terminal.discard_checkpoint();
        let snapshot = full_sync(&mut terminal)?;
        let revision = snapshot.revision;
        (snapshot, None, revision)
    };
    #[cfg(any(unix, windows))]
    let (snapshot, initial_delta, initial_revision) = initial_state;
    #[cfg(not(any(unix, windows)))]
    let (snapshot, _, initial_revision) = initial_state;
    let target = if runtime.controller.is_none() {
        runtime.next_generation = runtime
            .next_generation
            .checked_add(1)
            .ok_or_else(|| resource_error("controller generation exhausted"))?;
        let generation = runtime.next_generation;
        runtime.controller = Some(ControllerLease {
            attachment_id,
            generation,
        });
        runtime.controller_operation = None;
        SyncTarget::Active { generation }
    } else {
        SyncTarget::PreparedTakeover
    };
    let (lifecycle, lifecycle_receiver) = watch::channel(AttachmentLifecycle::AwaitingSnapshot {
        revision: initial_revision,
    });
    let detached = Arc::new(AtomicBool::new(false));
    let final_update = Arc::new(Mutex::new(None));
    runtime.attachments.insert(
        attachment_id,
        ActorAttachment {
            base_colors,
            color_sequence: 0,
            pending_color_snapshot: None,
            principal,
            #[cfg(any(unix, windows))]
            resume_view_id,
            terminal,
            ever_active: false,
            prepared_snapshot_applied: false,
            applied_snapshot_revision: None,
            sync: AttachmentSync::Awaiting {
                revision: initial_revision,
                target,
            },
            lifecycle,
            detached: Arc::clone(&detached),
            final_update: Arc::clone(&final_update),
        },
    );
    reconcile_effect_target(runtime)?;
    actor.update_cached(runtime, false);
    Ok(PreparedAttachment {
        attachment: Arc::new(SessionAttachment {
            actor: Arc::clone(actor),
            attachment_id,
            detached,
            revisions,
            lifecycle: lifecycle_receiver,
            #[cfg(any(unix, windows))]
            effect_broker,
            #[cfg(any(unix, windows))]
            effect_wakeup,
            #[cfg(any(unix, windows))]
            final_update,
        }),
        snapshot,
        #[cfg(any(unix, windows))]
        initial_delta,
    })
}

fn take_resume_terminal(
    actor: &SessionActor,
    runtime: &mut SessionRuntime,
    principal: AttachmentPrincipal,
    request: Option<RemoteResumeRequest>,
) -> Option<TerminalAttachment> {
    let Some(request) = request else {
        runtime.resume = None;
        return None;
    };

    let candidate = runtime.resume.take();
    candidate.and_then(|candidate| {
        (candidate.principal == principal
            && candidate.session_id == actor.id
            && candidate.view_id == request.view_id
            && request.known_revision == Some(candidate.revision))
        .then_some(candidate.terminal)
    })
}

#[cfg(any(unix, windows))]
fn detach_for_remote_resume(
    actor: &SessionActor,
    runtime: &mut SessionRuntime,
    attachment_id: AttachmentId,
) -> Result<bool, DaemonError> {
    let Some(attachment) = runtime.attachments.remove(&attachment_id) else {
        return Err(lease_lost());
    };
    attachment.detached.store(true, Ordering::Release);
    let was_controller = runtime
        .controller
        .is_some_and(|controller| controller.attachment_id == attachment_id);
    let active_controller =
        was_controller && matches!(attachment.sync, AttachmentSync::Active { .. });
    if was_controller {
        runtime.controller = None;
        runtime.controller_operation = None;
    }

    let saved = match (
        active_controller,
        attachment.principal,
        attachment.resume_view_id,
        attachment.terminal.checkpoint_revision(),
    ) {
        (
            true,
            principal @ AttachmentPrincipal::RemoteEndpoint { .. },
            Some(view_id),
            Some(revision),
        ) => {
            runtime.resume = Some(RemoteResumeCheckpoint {
                principal,
                session_id: actor.id,
                view_id,
                revision,
                terminal: attachment.terminal,
            });
            true
        }
        _ => {
            runtime.resume = None;
            false
        }
    };
    reconcile_effect_target(runtime)?;
    actor.update_cached(runtime, false);
    if was_controller {
        tracing::info!(component = "session", operation = "controller_detached", session_id = %actor.id, attachment_id = %attachment_id, reason = "transport_closed", "Controller detached");
    }
    Ok(saved)
}

fn snapshot_applied(
    runtime: &mut SessionRuntime,
    attachment_id: AttachmentId,
    revision: Revision,
) -> Result<Option<TerminalSurfaceSnapshot>, DaemonError> {
    let attachment = runtime
        .attachments
        .get_mut(&attachment_id)
        .ok_or_else(lease_lost)?;
    let AttachmentSync::Awaiting {
        revision: expected,
        target,
    } = attachment.sync
    else {
        // Two snapshots can cross an ACK in the opposite stream direction.
        // A stale ACK may request a replacement at a revision already on the
        // wire, so both delivered snapshots legitimately receive the same ACK.
        if attachment.applied_snapshot_revision == Some(revision) {
            return Ok(None);
        }
        return Err(not_synchronized("attachment is not awaiting a snapshot"));
    };
    if revision != expected && attachment.applied_snapshot_revision == Some(revision) {
        // A repeated old ACK cannot release or replace a newer sync fence.
        return Ok(None);
    }
    attachment.pending_color_snapshot = None;
    if revision != expected {
        attachment.terminal.discard_checkpoint();
        let snapshot = full_sync(&mut attachment.terminal)?;
        attachment.sync = AttachmentSync::Awaiting {
            revision: snapshot.revision,
            target,
        };
        attachment
            .lifecycle
            .send_replace(AttachmentLifecycle::AwaitingSnapshot {
                revision: snapshot.revision,
            });
        reconcile_effect_target(runtime)?;
        return Ok(Some(snapshot));
    }
    attachment.applied_snapshot_revision = Some(revision);
    let lifecycle = match target {
        SyncTarget::Active { generation } => {
            attachment.ever_active = true;
            attachment.sync = AttachmentSync::Active { generation };
            AttachmentLifecycle::Active { generation }
        }
        SyncTarget::PreparedTakeover => {
            attachment.prepared_snapshot_applied = true;
            attachment.sync = AttachmentSync::PreparedTakeover;
            AttachmentLifecycle::PreparedTakeover
        }
    };
    reconcile_effect_target(runtime)?;
    runtime
        .attachments
        .get(&attachment_id)
        .ok_or_else(lease_lost)?
        .lifecycle
        .send_replace(lifecycle);
    Ok(None)
}

fn next_update(
    runtime: &mut SessionRuntime,
    attachment_id: AttachmentId,
) -> Result<Option<AttachmentUpdate>, DaemonError> {
    let attachment = runtime
        .attachments
        .get_mut(&attachment_id)
        .ok_or_else(lease_lost)?;
    if let Some(snapshot) = attachment.pending_color_snapshot.take() {
        return Ok(Some(AttachmentUpdate::Snapshot(snapshot)));
    }
    let target = match attachment.sync {
        AttachmentSync::Active { generation } => SyncTarget::Active { generation },
        AttachmentSync::PreparedTakeover => SyncTarget::PreparedTakeover,
        AttachmentSync::Awaiting { .. } => return Ok(None),
    };
    match attachment
        .terminal
        .sync_changed()
        .map_err(map_driver_error)?
    {
        None => Ok(None),
        Some(TerminalSurfaceDeltaResult::Delta(delta)) => Ok(Some(AttachmentUpdate::Delta(delta))),
        Some(TerminalSurfaceDeltaResult::Resync(snapshot)) => {
            attachment.sync = AttachmentSync::Awaiting {
                revision: snapshot.revision,
                target,
            };
            attachment
                .lifecycle
                .send_replace(AttachmentLifecycle::AwaitingSnapshot {
                    revision: snapshot.revision,
                });
            reconcile_effect_target(runtime)?;
            Ok(Some(AttachmentUpdate::Snapshot(snapshot)))
        }
    }
}

#[cfg(any(unix, windows))]
fn final_update(
    runtime: &mut SessionRuntime,
    attachment_id: AttachmentId,
) -> Result<Option<AttachmentUpdate>, DaemonError> {
    let attachment = runtime
        .attachments
        .get_mut(&attachment_id)
        .ok_or_else(lease_lost)?;
    Ok(attachment
        .terminal
        .sync_changed()
        .map_err(map_driver_error)?
        .map(semantic_update_value))
}

pub(super) fn final_terminal_update(
    terminal: &mut TerminalAttachment,
) -> Result<Option<AttachmentUpdate>, DaemonError> {
    Ok(terminal
        .sync_changed()
        .map_err(map_driver_error)?
        .map(semantic_update_value))
}

fn semantic_update_value(update: TerminalSurfaceDeltaResult) -> AttachmentUpdate {
    match update {
        TerminalSurfaceDeltaResult::Delta(delta) => AttachmentUpdate::Delta(delta),
        TerminalSurfaceDeltaResult::Resync(snapshot) => AttachmentUpdate::Snapshot(snapshot),
    }
}

fn sync_latest(
    runtime: &mut SessionRuntime,
    attachment_id: AttachmentId,
    _known_revision: Revision,
) -> Result<TerminalSurfaceSnapshot, DaemonError> {
    let attachment = runtime
        .attachments
        .get_mut(&attachment_id)
        .ok_or_else(lease_lost)?;
    attachment.pending_color_snapshot = None;
    let target = match attachment.sync {
        AttachmentSync::Active { generation } => SyncTarget::Active { generation },
        AttachmentSync::PreparedTakeover => SyncTarget::PreparedTakeover,
        AttachmentSync::Awaiting { target, .. } => target,
    };
    attachment.terminal.discard_checkpoint();
    let snapshot = full_sync(&mut attachment.terminal)?;
    attachment.sync = AttachmentSync::Awaiting {
        revision: snapshot.revision,
        target,
    };
    attachment
        .lifecycle
        .send_replace(AttachmentLifecycle::AwaitingSnapshot {
            revision: snapshot.revision,
        });
    reconcile_effect_target(runtime)?;
    Ok(snapshot)
}

#[cfg(any(unix, windows))]
fn history_window(
    runtime: &SessionRuntime,
    attachment_id: AttachmentId,
    query: TerminalHistoryWindowQuery,
    deadline: Instant,
) -> Result<TerminalHistoryRead, DaemonError> {
    require_existing_visual_sync_controller(runtime, attachment_id)?;
    runtime
        .attachments
        .get(&attachment_id)
        .ok_or_else(lease_lost)?
        .terminal
        .begin_history_window(query, deadline)
        .map_err(map_driver_error)
}

fn write_input(
    runtime: &SessionRuntime,
    attachment_id: AttachmentId,
    bytes: &[u8],
) -> Result<(), DaemonError> {
    require_existing_visual_sync_controller(runtime, attachment_id)?;
    runtime
        .driver
        .as_ref()
        .ok_or_else(session_not_found)?
        .write_input(bytes)
        .map_err(map_driver_error)
}

fn resize(
    actor: &Arc<SessionActor>,
    runtime: &mut SessionRuntime,
    attachment_id: AttachmentId,
    size: TerminalSize,
) -> Result<Revision, DaemonError> {
    validate_viewport(actor.limits, size)?;
    require_resize_controller(runtime, attachment_id)?;
    let revision = runtime
        .driver
        .as_ref()
        .ok_or_else(session_not_found)?
        .resize(size)
        .map_err(map_driver_error)?;
    runtime.viewport = size;
    runtime.last_revision = revision;
    actor.update_cached(runtime, false);
    if voytty_diagnostics::detail_enabled() {
        voytty_diagnostics::record(
            voytty_diagnostics::Event::new(voytty_diagnostics::Kind::ViewportChanged)
                .level(voytty_diagnostics::Level::Debug)
                .session(actor.id)
                .attachment(attachment_id)
                .stage(voytty_diagnostics::Stage::Resize)
                .epoch(revision.get())
                .viewport(size.columns, size.rows),
        );
    }
    Ok(revision)
}

fn takeover(
    actor: &Arc<SessionActor>,
    runtime: &mut SessionRuntime,
    principal: AttachmentPrincipal,
    attachment_id: AttachmentId,
    operation_key: ReplayOperationKey,
    continuation: bool,
) -> Result<(), DaemonError> {
    let owner = runtime
        .attachments
        .get(&attachment_id)
        .map(|attachment| attachment.principal)
        .ok_or_else(lease_lost)?;
    if owner != principal {
        return Err(principal_mismatch());
    }
    if runtime
        .controller
        .is_some_and(|controller| controller.attachment_id == attachment_id)
    {
        // The previous controller can detach before an explicit takeover's
        // attach request arrives. That attach then reserves the vacant lease.
        // Once its snapshot is applied, committing takeover is a no-op, not a
        // synchronization failure. Retain the operation key for reconnect replay.
        require_existing_visual_sync_controller(runtime, attachment_id)?;
        if !continuation {
            runtime.controller_operation = Some(operation_key);
        }
        return Ok(());
    }
    let attachment = runtime
        .attachments
        .get_mut(&attachment_id)
        .ok_or_else(lease_lost)?;
    let ready = match attachment.sync {
        AttachmentSync::PreparedTakeover => true,
        AttachmentSync::Awaiting {
            target: SyncTarget::PreparedTakeover,
            ..
        } if attachment.prepared_snapshot_applied => false,
        _ => {
            return Err(not_synchronized(
                "takeover attachment has not applied its prepared snapshot",
            ));
        }
    };
    if continuation {
        match runtime.controller {
            None => {}
            Some(_) if runtime.controller_operation == Some(operation_key) => {}
            Some(_) => return Err(outcome_unknown()),
        }
    }
    let generation = runtime
        .next_generation
        .checked_add(1)
        .ok_or_else(|| resource_error("controller generation exhausted"))?;
    let driver = runtime.driver.as_ref().ok_or_else(session_not_found)?;
    let snapshot = driver
        .commit_takeover(
            attachment.base_colors.clone(),
            &mut attachment.terminal,
            |colors_changed| {
                (attachment.ever_active || ready && !colors_changed).then_some(attachment_id)
            },
        )
        .map_err(map_driver_error)?;

    // All fallible work is complete. The session actor owns both leases.
    let notification = if let Some(snapshot) = snapshot {
        let revision = snapshot.revision;
        attachment.pending_color_snapshot = Some(snapshot);
        attachment.sync = AttachmentSync::Awaiting {
            revision,
            target: SyncTarget::Active { generation },
        };
        Some(AttachmentLifecycle::AwaitingSnapshot { revision })
    } else if let AttachmentSync::Awaiting { revision, .. } = attachment.sync {
        attachment.sync = AttachmentSync::Awaiting {
            revision,
            target: SyncTarget::Active { generation },
        };
        None
    } else {
        attachment.sync = AttachmentSync::Active { generation };
        attachment.ever_active = true;
        Some(AttachmentLifecycle::Active { generation })
    };
    let lifecycle = attachment.lifecycle.clone();
    let effect_target = attachment.ever_active.then_some(attachment_id);
    let old = runtime.controller.replace(ControllerLease {
        attachment_id,
        generation,
    });
    runtime.next_generation = generation;
    runtime.controller_operation = Some(operation_key);
    if let Some(old) = old
        && let Some(old) = runtime.attachments.remove(&old.attachment_id)
    {
        old.lifecycle
            .send_replace(AttachmentLifecycle::LeaseLost { generation });
    }
    cancel_inactive_history(runtime, effect_target);
    actor.update_cached(runtime, false);
    if let Some(notification) = notification {
        lifecycle.send_replace(notification);
    }
    tracing::info!(component = "session", operation = "controller_taken_over", session_id = %actor.id, attachment_id = %attachment_id, "Controller taken over");
    Ok(())
}

pub(super) fn reap_detached(
    actor: &SessionActor,
    runtime: &mut SessionRuntime,
) -> Result<(), DaemonError> {
    let detached = runtime
        .attachments
        .iter()
        .filter_map(|(attachment_id, attachment)| {
            attachment
                .detached
                .load(Ordering::Acquire)
                .then_some(*attachment_id)
        })
        .collect::<Vec<_>>();
    if detached.is_empty() {
        return Ok(());
    }
    let mut detached_controller = None;
    for attachment_id in detached {
        runtime.attachments.remove(&attachment_id);
        if runtime
            .controller
            .is_some_and(|lease| lease.attachment_id == attachment_id)
        {
            runtime.controller = None;
            runtime.controller_operation = None;
            detached_controller = Some(attachment_id);
        }
    }
    reconcile_effect_target(runtime)?;
    actor.update_cached(runtime, false);
    if let Some(attachment_id) = detached_controller {
        tracing::info!(component = "session", operation = "controller_detached", session_id = %actor.id, attachment_id = %attachment_id, reason = "detached", "Controller detached");
    }
    Ok(())
}

fn detach_remote_principal(
    actor: &SessionActor,
    runtime: &mut SessionRuntime,
    device_id: DeviceId,
) -> Result<PrincipalDetachOutcome, DaemonError> {
    let resume_removed = runtime.resume.as_ref().is_some_and(|resume| {
        matches!(
            resume.principal,
            AttachmentPrincipal::RemoteEndpoint {
                device_id: remote,
                ..
            } if remote == device_id
        )
    });
    if resume_removed {
        runtime.resume = None;
    }
    let removed = runtime
        .attachments
        .iter()
        .filter(|(_, attachment)| {
            matches!(
                attachment.principal,
                AttachmentPrincipal::RemoteEndpoint {
                    device_id: remote,
                    ..
                } if remote == device_id
            )
        })
        .map(|(attachment_id, _)| *attachment_id)
        .collect::<Vec<_>>();

    let mut outcome = PrincipalDetachOutcome::default();
    for attachment_id in removed {
        if let Some(attachment) = runtime.attachments.remove(&attachment_id) {
            attachment.detached.store(true, Ordering::Release);
            outcome.attachments_removed += 1;
        }
        if runtime
            .controller
            .is_some_and(|lease| lease.attachment_id == attachment_id)
        {
            runtime.controller = None;
            runtime.controller_operation = None;
            outcome.controller_released = true;
        }
    }
    if outcome.attachments_removed > 0 || resume_removed {
        reconcile_effect_target(runtime)?;
        actor.update_cached(runtime, false);
    }
    Ok(outcome)
}

fn reconcile_effect_target(runtime: &SessionRuntime) -> Result<(), DaemonError> {
    let target = runtime.controller.and_then(|controller| {
        runtime
            .attachments
            .get(&controller.attachment_id)
            .filter(|attachment| {
                attachment.ever_active
                    && matches!(
                        attachment.sync,
                        AttachmentSync::Active { generation }
                            | AttachmentSync::Awaiting {
                                target: SyncTarget::Active { generation },
                                ..
                            } if generation == controller.generation
                    )
            })
            .map(|_| controller.attachment_id)
    });
    cancel_inactive_history(runtime, target);
    runtime
        .driver
        .as_ref()
        .ok_or_else(session_not_found)?
        .set_effect_target(target)
        .map_err(map_driver_error)
}

fn cancel_inactive_history(runtime: &SessionRuntime, target: Option<AttachmentId>) {
    for (id, attachment) in &runtime.attachments {
        if Some(*id) != target {
            attachment.terminal.cancel_pending_history();
        }
    }
}

#[cfg(any(unix, windows))]
fn count_remote_attachments(runtime: &SessionRuntime, device_id: DeviceId) -> usize {
    runtime
        .attachments
        .values()
        .filter(|attachment| {
            !attachment.detached.load(Ordering::Acquire)
                && matches!(
                    attachment.principal,
                    AttachmentPrincipal::RemoteEndpoint {
                        device_id: remote,
                        ..
                    } if remote == device_id
                )
        })
        .count()
}

fn require_resize_controller(
    runtime: &SessionRuntime,
    attachment_id: AttachmentId,
) -> Result<u64, DaemonError> {
    // Snapshot delivery and controller commands travel in opposite directions
    // on one duplex stream. Resize is replaceable controller state and may be
    // admitted while either the initial or a later Active snapshot is in
    // flight. Input and read-only presentation requests reuse this controller
    // check only after proving that the same attachment was active before its
    // current visual-sync window. First attach and takeover remain fenced.
    let attachment = runtime
        .attachments
        .get(&attachment_id)
        .ok_or_else(lease_lost)?;
    let generation = match attachment.sync {
        AttachmentSync::Active { generation } => generation,
        AttachmentSync::Awaiting {
            target: SyncTarget::Active { generation },
            ..
        } => generation,
        AttachmentSync::Awaiting {
            target: SyncTarget::PreparedTakeover,
            ..
        }
        | AttachmentSync::PreparedTakeover => {
            return Err(not_synchronized(
                "snapshot must be applied before controller operations",
            ));
        }
    };
    match runtime.controller {
        Some(ControllerLease {
            attachment_id: owner,
            generation: current,
        }) if owner == attachment_id && current == generation => Ok(generation),
        _ => Err(lease_lost()),
    }
}

fn require_existing_visual_sync_controller(
    runtime: &SessionRuntime,
    attachment_id: AttachmentId,
) -> Result<u64, DaemonError> {
    let attachment = runtime
        .attachments
        .get(&attachment_id)
        .ok_or_else(lease_lost)?;
    if matches!(attachment.sync, AttachmentSync::Awaiting { .. }) && !attachment.ever_active {
        return Err(not_synchronized(
            "snapshot must be applied before controller operations",
        ));
    }
    require_resize_controller(runtime, attachment_id)
}

fn full_sync(attachment: &mut TerminalAttachment) -> Result<TerminalSurfaceSnapshot, DaemonError> {
    match attachment.sync_latest().map_err(map_driver_error)? {
        TerminalSurfaceDeltaResult::Resync(snapshot) => Ok(snapshot),
        TerminalSurfaceDeltaResult::Delta(_) => Err(DaemonError::new(
            DomainErrorKind::MalformedFrame,
            "discarded attachment checkpoint did not produce a full snapshot",
        )),
    }
}

fn next_attachment_id(
    attachments: &BTreeMap<AttachmentId, ActorAttachment>,
) -> Result<AttachmentId, DaemonError> {
    for _ in 0..16 {
        let candidate = AttachmentId::from_array(random_16());
        if !attachments.contains_key(&candidate) {
            return Ok(candidate);
        }
    }
    Err(resource_error(
        "unable to allocate a unique attachment identity",
    ))
}
