//! Concurrent input, history and surface delivery on an attached terminal stream.

use super::*;

/// Validates a transport receipt without applying a session operation.
fn accept_surface_frames_received(
    frame: &DecodedFrame,
    context: &AttachmentReaderContext,
) -> Result<(), DaemonError> {
    let receipts = context
        .surface_receipts
        .as_ref()
        .ok_or_else(|| malformed("terminal surface frame receipts were not negotiated"))?;
    let receipt: v2::TerminalSurfaceFramesReceived = frame
        .decode_message(WireKind::TerminalSurfaceFramesReceived)
        .map_err(protocol_error)?;
    require_attachment_id(receipt.attachment_id, &context.attachment)?;
    receipts.accept(receipt.frame_sequence)
}

pub(super) async fn attachment_reader<Reader>(
    mut reader: Reader,
    mut decoder: FrameDecoder,
    mut queued: VecDeque<DecodedFrame>,
    context: AttachmentReaderContext,
) -> Result<AttachmentTaskEnd, DaemonError>
where
    Reader: AsyncRead + Unpin,
{
    let mut history_tasks = JoinSet::new();
    let (failed_request, error) = loop {
        let frame = tokio::select! {
            biased;
            Some(result) = history_tasks.join_next(), if !history_tasks.is_empty() => {
                finish_history_task(result)?;
                continue;
            }
            result = voytty_client::framing::read_queued_frame(&mut reader, &mut decoder, &mut queued, |e| daemon_io("read terminal stream", e)) => result,
        };
        let frame = match frame {
            Ok(Some(frame)) => frame,
            Ok(None) => match decoder.finish() {
                Ok(()) => return Ok(AttachmentTaskEnd::TransportEof),
                Err(error) => break (0, protocol_error(error)),
            },
            Err(error) if error.kind() == DomainErrorKind::DaemonStopped => return Err(error),
            Err(error) => break (0, error),
        };
        let request_id = frame.request_id;
        let result = if frame.kind == WireKind::TerminalSurfaceFramesReceived {
            accept_surface_frames_received(&frame, &context).map(|()| false)
        } else {
            process_attachment_frame(
                frame,
                &context.server,
                &context.attachment,
                &context.request_context,
                &context.outbound,
                context.limits,
                &mut history_tasks,
            )
            .await
        };
        match result {
            Ok(false) => continue,
            Ok(true) => return Ok(AttachmentTaskEnd::Explicit),
            Err(error) => break (request_id, error),
        }
    };
    flush_attachment_error(
        &context.outbound,
        failed_request,
        &error,
        context.limits.operation_timeout,
    )
    .await;
    Err(error)
}

pub(super) fn finish_history_task(
    result: Result<Result<(), DaemonError>, tokio::task::JoinError>,
) -> Result<(), DaemonError> {
    match result {
        Ok(Ok(())) => Ok(()),
        // Recoverable history-window errors already flushed their correlated
        // ServiceErrorResponse; keep the live terminal attachment open.
        Ok(Err(error))
            if matches!(
                error.kind(),
                DomainErrorKind::ResourceExhausted | DomainErrorKind::DeadlineExceeded
            ) =>
        {
            Ok(())
        }
        Ok(Err(error)) => Err(error),
        Err(_) => Err(DaemonError::new(
            DomainErrorKind::StoreUnavailable,
            "terminal history worker failed",
        )),
    }
}

async fn flush_attachment_error(
    outbound: &mpsc::Sender<AttachmentOutbound>,
    request_id: u64,
    error: &DaemonError,
    operation_timeout: Duration,
) {
    let (flushed, wait_for_flush) = oneshot::channel();
    let deadline = Instant::now() + operation_timeout;
    let flush = async {
        send_attachment_outbound_until(
            outbound,
            AttachmentOutbound {
                bytes: ServiceReply::error(request_id, error).bytes,
                flushed: Some(flushed),
                deadline,
                synchronize_before_reply: false,
                has_surface: false,
            },
            deadline,
        )
        .await
        .map_err(|_| ())?;
        wait_for_flush.await.map_err(|_| ())
    };
    let _ = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), flush).await;
}

pub(super) async fn process_attachment_frame(
    frame: DecodedFrame,
    server: &SessionWireServer,
    attachment: &Arc<SessionAttachment>,
    request_context: &SessionRequestContext,
    outbound: &mpsc::Sender<AttachmentOutbound>,
    limits: SessionWireLimits,
    history_tasks: &mut JoinSet<Result<(), DaemonError>>,
) -> Result<bool, DaemonError> {
    let deadline = Instant::now() + limits.request_deadline(frame.deadline_ms);
    match frame.kind {
        WireKind::TerminalSnapshotApplied => {
            let request: v2::TerminalSnapshotApplied = frame
                .decode_message(WireKind::TerminalSnapshotApplied)
                .map_err(protocol_error)?;
            require_attachment_id(request.attachment_id, attachment)?;
            let attachment_worker = Arc::clone(attachment);
            let revision = Revision::new(request.revision);
            let snapshot = request_context
                .run_effect(&server.sessions, deadline, move |_sessions, _principal| {
                    attachment_worker.snapshot_applied_until(revision, deadline)
                })
                .await?;
            if let Some(snapshot) = snapshot {
                send_resync(frame.request_id, attachment, snapshot, outbound, deadline).await?;
            }
            Ok(false)
        }
        WireKind::TerminalSyncRequest => {
            let request: v2::TerminalSyncRequest = frame
                .decode_message(WireKind::TerminalSyncRequest)
                .map_err(protocol_error)?;
            require_attachment_id(request.attachment_id, attachment)?;
            let attachment_worker = Arc::clone(attachment);
            let known_revision = Revision::new(request.known_revision);
            let snapshot = request_context
                .run_effect(&server.sessions, deadline, move |_sessions, _principal| {
                    attachment_worker.sync_latest_until(known_revision, deadline)
                })
                .await?;
            send_resync(frame.request_id, attachment, snapshot, outbound, deadline).await?;
            Ok(false)
        }
        WireKind::TerminalHistoryWindowRequest => {
            if !history_tasks.is_empty() {
                let error = DaemonError::new(
                    DomainErrorKind::ResourceExhausted,
                    "terminal history read already pending",
                );
                flush_attachment_error(
                    outbound,
                    frame.request_id,
                    &error,
                    limits.operation_timeout,
                )
                .await;
                return Ok(false);
            }
            let request: v2::TerminalHistoryWindowRequest = frame
                .decode_message(WireKind::TerminalHistoryWindowRequest)
                .map_err(protocol_error)?;
            let query = terminal_history_window_query(&request)?;
            require_attachment_id(request.attachment_id, attachment)?;
            let attachment_worker = Arc::clone(attachment);
            let read = request_context
                .run_effect(&server.sessions, deadline, move |_sessions, _principal| {
                    attachment_worker.begin_history_window_until(query, deadline)
                })
                .await?;
            let request_context = request_context.clone();
            let sessions = server.sessions.clone();
            let attachment = Arc::clone(attachment);
            let outbound = outbound.clone();
            history_tasks.spawn(async move {
                let result = async {
                    // No Session actor or remote authorization commit is held
                    // while a child batch finishes. Input/ACK reads continue.
                    let result = run_blocking_until(deadline, move || {
                        read.wait_until(deadline)
                            .map_err(crate::session::map_driver_error)
                    })
                    .await?;
                    let worker = Arc::clone(&attachment);
                    request_context
                        .run_effect(&sessions, deadline, move |_, _| {
                            worker.validate_history_controller_until(deadline)
                        })
                        .await?;
                    let message = voytty_proto::terminal_surface_history_window_frame_message(
                        attachment.attachment_id(),
                        result,
                    );
                    let bytes = encode_message(
                        WireKind::TerminalSemanticHistoryWindowFrame,
                        frame.request_id,
                        0,
                        &message,
                    )
                    .map_err(protocol_error)?;
                    send_attachment_outbound_until(
                        &outbound,
                        AttachmentOutbound::surface(bytes, deadline),
                        deadline,
                    )
                    .await
                }
                .await;
                if let Err(error) = &result {
                    flush_attachment_error(
                        &outbound,
                        frame.request_id,
                        error,
                        limits.operation_timeout,
                    )
                    .await;
                }
                result
            });
            Ok(false)
        }
        WireKind::TerminalInput => {
            let request: v2::TerminalInput = frame
                .decode_message(WireKind::TerminalInput)
                .map_err(protocol_error)?;
            require_attachment_id(request.attachment_id, attachment)?;
            let attachment_worker = Arc::clone(attachment);
            request_context
                .run_effect(&server.sessions, deadline, move |_sessions, _principal| {
                    attachment_worker.write_input_until(&request.bytes, deadline)
                })
                .await?;
            Ok(false)
        }
        WireKind::TerminalBaseColors => {
            let request: v2::TerminalBaseColors = frame
                .decode_message(WireKind::TerminalBaseColors)
                .map_err(protocol_error)?;
            require_attachment_id(request.attachment_id, attachment)?;
            let base = request
                .profile
                .ok_or_else(|| {
                    DaemonError::new(DomainErrorKind::MalformedFrame, "missing base colors")
                })?
                .try_into()
                .map_err(protocol_error)?;
            let worker = Arc::clone(attachment);
            match request_context
                .run_effect(&server.sessions, deadline, move |_, _| {
                    worker.update_colors_until(request.sequence, base, deadline)
                })
                .await
            {
                Ok(()) => Ok(false),
                // Stale color observation sequences are recoverable per-request
                // rejections and must not tear down an active attachment.
                Err(error)
                    if error.kind() == DomainErrorKind::SessionNotFound
                        && error.detail() == "stale color observation sequence" =>
                {
                    flush_attachment_error(
                        outbound,
                        frame.request_id,
                        &error,
                        limits.operation_timeout,
                    )
                    .await;
                    Ok(false)
                }
                Err(error) => Err(error),
            }
        }
        WireKind::TerminalResize => {
            let request: v2::TerminalResize = frame
                .decode_message(WireKind::TerminalResize)
                .map_err(protocol_error)?;
            require_attachment_id(request.attachment_id, attachment)?;
            let size = v2::TerminalViewport {
                rows: request.rows,
                columns: request.columns,
            }
            .try_into()
            .map_err(protocol_error)?;
            let attachment_worker = Arc::clone(attachment);
            request_context
                .run_effect(&server.sessions, deadline, move |_sessions, _principal| {
                    attachment_worker.resize_until(size, deadline)
                })
                .await?;
            Ok(false)
        }
        WireKind::TerminalDetach => {
            let request: v2::TerminalDetach = frame
                .decode_message(WireKind::TerminalDetach)
                .map_err(protocol_error)?;
            require_attachment_id(request.attachment_id, attachment)?;
            let attachment_worker = Arc::clone(attachment);
            request_context
                .run_effect(&server.sessions, deadline, move |_sessions, _principal| {
                    attachment_worker.detach();
                    Ok(())
                })
                .await?;
            Ok(true)
        }
        WireKind::SessionOperationLeaseRequest => {
            let request: v2::SessionOperationLeaseRequest = frame
                .decode_message(WireKind::SessionOperationLeaseRequest)
                .map_err(protocol_error)?;
            request_context.require_terminal_target(request.target)?;
            let lease = request_context
                .run_effect(&server.sessions, deadline, |sessions, principal| {
                    sessions.issue_operation_lease(principal)
                })
                .await?;
            send_attachment_outbound_until(
                outbound,
                AttachmentOutbound::queued(
                    encode_message(
                        WireKind::SessionOperationLeaseResponse,
                        frame.request_id,
                        0,
                        &v2::SessionOperationLeaseResponse {
                            lease: Some(lease.into()),
                        },
                    )
                    .map_err(protocol_error)?,
                    deadline,
                ),
                deadline,
            )
            .await?;
            Ok(false)
        }
        WireKind::SessionTakeoverRequest => {
            let request: v2::SessionTakeoverRequest = frame
                .decode_message(WireKind::SessionTakeoverRequest)
                .map_err(protocol_error)?;
            request_context.require_terminal_target(request.target.clone())?;
            let session_id: SessionId = request
                .session_id
                .clone()
                .ok_or_else(|| malformed("takeover omitted session_id"))?
                .try_into()
                .map_err(protocol_error)?;
            if session_id != attachment.session_id() {
                return Err(malformed("takeover session_id does not match this stream"));
            }
            require_attachment_id(request.attachment_id.clone(), attachment)?;
            let operation_id = request
                .operation_id
                .ok_or_else(|| malformed("takeover omitted operation_id"))?
                .try_into()
                .map_err(protocol_error)?;
            let attachment_worker = Arc::clone(attachment);
            let summary = request_context
                .run_effect(&server.sessions, deadline, move |sessions, principal| {
                    sessions.takeover_until(principal, operation_id, &attachment_worker, deadline)
                })
                .await?;
            let message = v2::SessionMutateResponse {
                session: Some(session_summary_proto(summary)),
            };
            send_attachment_outbound_until(
                outbound,
                AttachmentOutbound::takeover_reply(
                    encode_message(
                        WireKind::SessionMutateResponse,
                        frame.request_id,
                        0,
                        &message,
                    )
                    .map_err(protocol_error)?,
                    deadline,
                ),
                deadline,
            )
            .await?;
            Ok(false)
        }
        _ => Err(malformed(format!(
            "wire kind {:?} is invalid on a terminal attachment",
            frame.kind
        ))),
    }
}

pub(super) async fn attachment_writer<Writer>(
    mut writer: Writer,
    server: SessionWireServer,
    attachment: Arc<SessionAttachment>,
    request_context: SessionRequestContext,
    mut outbound: mpsc::Receiver<AttachmentOutbound>,
    operation_timeout: Duration,
    mut output: SurfaceOutput,
) -> Result<AttachmentTaskEnd, DaemonError>
where
    Writer: AsyncWrite + Unpin,
{
    let mut revisions = attachment.revision_watch()?;
    let mut lifecycle = attachment.lifecycle_watch()?;
    let mut effects = attachment.effect_watch();
    let mut revisions_open = true;
    let mut yield_after_effect = false;
    let initial_lifecycle = lifecycle.borrow().clone();
    if write_lifecycle_event(
        &mut writer,
        &server,
        &attachment,
        &request_context,
        initial_lifecycle,
        Instant::now() + operation_timeout,
        &mut output,
    )
    .await?
    {
        return Ok(AttachmentTaskEnd::Terminal);
    }
    loop {
        // Optional revision-driven updates wait for receipts; responses,
        // lifecycle, effects, takeover and final updates are never withheld.
        // A withheld revision stays marked changed in its watch, and the next
        // capture is latest-only, so delayed output is coalesced, not lost.
        let surface_room = output.frames.has_room();
        tokio::select! {
            biased;
            message = outbound.recv() => {
                let Some(mut message) = message else {
                    return Ok(AttachmentTaskEnd::ReaderClosed);
                };
                if message.synchronize_before_reply
                    && let Some(update) = attachment_next_update(
                        &server,
                        Arc::clone(&attachment),
                        &request_context,
                        message.deadline,
                    ).await?
                {
                    // Takeover can replace controller colors and require a
                    // fresh snapshot ACK. This sole writer must emit that
                    // snapshot before the success response enables commands;
                    // the biased response queue must not overtake lifecycle.
                    write_terminal_update(&mut writer, &attachment, update, message.deadline, &mut output).await?;
                }
                if message.has_surface {
                    *message.bytes = output.encoding.encode(std::mem::take(&mut *message.bytes), message.deadline).await?;
                }
                write_attachment_bytes_until(
                    &mut writer,
                    &message.bytes,
                    message.deadline,
                    "terminal response exceeded its absolute deadline",
                    "write terminal response",
                ).await?;
                if let Some(flushed) = message.flushed {
                    let _ = flushed.send(());
                }
            }
            changed = lifecycle.changed() => {
                changed.map_err(|_| attachment_cancelled())?;
                let event = lifecycle.borrow_and_update().clone();
                if write_lifecycle_event(
                    &mut writer,
                    &server,
                    &attachment,
                    &request_context,
                    event,
                    Instant::now() + operation_timeout,
                    &mut output,
                ).await? {
                    return Ok(AttachmentTaskEnd::Terminal);
                }
            }
            effect = next_host_effect(&attachment, &mut effects), if !yield_after_effect => {
                let deadline = Instant::now() + operation_timeout;
                let effect = effect?;
                request_context
                    .run_effect(&server.sessions, deadline, |_, _| Ok(()))
                    .await?;
                write_host_effect(&mut writer, attachment.attachment_id(), effect, deadline).await?;
                yield_after_effect = true;
            }
            changed = revisions.changed(), if revisions_open && surface_room => {
                yield_after_effect = false;
                if changed.is_err() {
                    // Driver finalization closes its revision watch before the actor publishes
                    // the final drained update and SessionEnded lifecycle value. Keep the stream
                    // alive for that authoritative terminal event instead of reporting a
                    // connection-local cancellation.
                    revisions_open = false;
                } else {
                    let deadline = Instant::now() + operation_timeout;
                    match attachment_next_update(
                        &server,
                        Arc::clone(&attachment),
                        &request_context,
                        deadline,
                    ).await {
                        Ok(Some(update)) => {
                            write_terminal_update(
                                &mut writer,
                                &attachment,
                                update,
                                deadline,
                                &mut output,
                            ).await?;
                        }
                        Ok(None) => {}
                        Err(error) if error.kind() == DomainErrorKind::SessionNotFound => {
                            // A revision notification can race the actor's transition into final
                            // drain. The lifecycle channel owns the terminal event and final
                            // checkpoint, so stop polling revisions and wait for it.
                            revisions_open = false;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            () = output.frames.receipt(), if revisions_open && !surface_room => {}
            () = std::future::ready(()), if yield_after_effect => {
                // A flood cannot monopolize visual/lifecycle work; an idle screen
                // does not require another revision to drain the notification FIFO.
                yield_after_effect = false;
                tokio::task::yield_now().await;
            }
        }
    }
}

async fn next_host_effect(
    attachment: &SessionAttachment,
    wake: &mut tokio::sync::watch::Receiver<()>,
) -> Result<voytty_core::terminal::TerminalHostEffect, DaemonError> {
    loop {
        // Observe before take: a coalesced wake never strands pending FIFO entries.
        wake.borrow_and_update();
        if let Some(effect) = attachment.take_host_effect()? {
            return Ok(effect);
        }
        wake.changed().await.map_err(|_| attachment_cancelled())?;
    }
}

pub(super) async fn write_host_effect<Writer>(
    writer: &mut Writer,
    attachment_id: AttachmentId,
    effect: voytty_core::terminal::TerminalHostEffect,
    deadline: Instant,
) -> Result<(), DaemonError>
where
    Writer: AsyncWrite + Unpin,
{
    use voytty_core::terminal::TerminalHostEffect;
    let bytes = match effect {
        TerminalHostEffect::ClipboardWrite(write) => encode_message(
            WireKind::TerminalClipboardWrite,
            0,
            0,
            &voytty_proto::terminal_clipboard_write_message(attachment_id, write),
        ),
        TerminalHostEffect::Notification(notification) => encode_message(
            WireKind::TerminalNotification,
            0,
            0,
            &voytty_proto::terminal_notification_message(attachment_id, notification),
        ),
    }
    .map_err(protocol_error)?;
    write_attachment_bytes_until(
        writer,
        &bytes,
        deadline,
        "terminal effect exceeded its absolute deadline",
        "write terminal effect",
    )
    .await
}

pub(super) async fn write_lifecycle_event<Writer>(
    writer: &mut Writer,
    server: &SessionWireServer,
    attachment: &Arc<SessionAttachment>,
    request_context: &SessionRequestContext,
    event: AttachmentLifecycle,
    deadline: Instant,
    output: &mut SurfaceOutput,
) -> Result<bool, DaemonError>
where
    Writer: AsyncWrite + Unpin,
{
    match event {
        AttachmentLifecycle::AwaitingSnapshot { .. }
        | AttachmentLifecycle::Active { .. }
        | AttachmentLifecycle::PreparedTakeover => {
            match attachment_next_update(server, Arc::clone(attachment), request_context, deadline)
                .await
            {
                Ok(Some(update)) => {
                    write_terminal_update(writer, attachment, update, deadline, output).await?
                }
                Ok(None) => {}
                // This notification may already have been superseded by SessionEnded.
                Err(error) if error.kind() == DomainErrorKind::SessionNotFound => {}
                Err(error) => return Err(error),
            }
            Ok(false)
        }
        AttachmentLifecycle::LeaseLost { generation } => {
            let message = v2::TerminalLeaseLost {
                attachment_id: Some(attachment.attachment_id().into()),
                generation,
            };
            let bytes = encode_message(WireKind::TerminalLeaseLost, 0, 0, &message)
                .map_err(protocol_error)?;
            write_attachment_bytes_until(
                writer,
                &bytes,
                deadline,
                "lease-lost event exceeded its absolute deadline",
                "write lease-lost event",
            )
            .await?;
            Ok(true)
        }
        AttachmentLifecycle::SessionEnded(reason) => {
            let final_attachment = Arc::clone(attachment);
            if let Ok(Some(update)) = request_context
                .run_effect(&server.sessions, deadline, move |_sessions, _principal| {
                    final_attachment.final_update_until(deadline)
                })
                .await
            {
                write_terminal_update(writer, attachment, update, deadline, output).await?;
            }
            let message = session_ended_message(attachment, reason);
            let bytes = encode_message(WireKind::TerminalSessionEnded, 0, 0, &message)
                .map_err(protocol_error)?;
            write_attachment_bytes_until(
                writer,
                &bytes,
                deadline,
                "session-ended event exceeded its absolute deadline",
                "write session-ended event",
            )
            .await?;
            Ok(true)
        }
    }
}

async fn attachment_next_update(
    server: &SessionWireServer,
    attachment: Arc<SessionAttachment>,
    request_context: &SessionRequestContext,
    deadline: Instant,
) -> Result<Option<AttachmentUpdate>, DaemonError> {
    let result = request_context
        .run_effect(&server.sessions, deadline, move |_sessions, _principal| {
            attachment.next_update_until(deadline)
        })
        .await;
    match result {
        // A queued Active/Awaiting/revision notification may race takeover.
        // The lifecycle watch owns the structured LeaseLost event; do not
        // close this writer before it can deliver that terminal notification.
        Err(error) if error.kind() == DomainErrorKind::LeaseLost => Ok(None),
        result => result,
    }
}

async fn write_terminal_update<Writer>(
    writer: &mut Writer,
    attachment: &SessionAttachment,
    update: AttachmentUpdate,
    deadline: Instant,
    output: &mut SurfaceOutput,
) -> Result<(), DaemonError>
where
    Writer: AsyncWrite + Unpin,
{
    let synchronization = match &update {
        AttachmentUpdate::Snapshot(snapshot) => Some((snapshot.revision, snapshot.surface.size)),
        AttachmentUpdate::Delta(_) => None,
    };
    let record_snapshot = |stage| {
        if voytty_diagnostics::detail_enabled()
            && let Some((revision, size)) = synchronization
        {
            voytty_diagnostics::record(
                voytty_diagnostics::Event::new(voytty_diagnostics::Kind::SyncChanged)
                    .level(voytty_diagnostics::Level::Debug)
                    .session(attachment.session_id())
                    .attachment(attachment.attachment_id())
                    .stage(stage)
                    .epoch(revision.get())
                    .viewport(size.columns, size.rows),
            );
        }
    };
    record_snapshot(voytty_diagnostics::Stage::Snapshot);
    let frame_sequence = output.frames.next_sequence();
    let (bytes, operation) = match update {
        AttachmentUpdate::Delta(delta) => {
            let bytes = encode_delta(0, attachment.attachment_id(), delta, frame_sequence)?;
            (bytes, "write terminal delta")
        }
        AttachmentUpdate::Snapshot(snapshot) => {
            let required = v2::TerminalSyncRequired {
                attachment_id: Some(attachment.attachment_id().into()),
                latest_revision: snapshot.revision.get(),
            };
            let mut bytes = encode_message(WireKind::TerminalSyncRequired, 0, 0, &required)
                .map_err(protocol_error)?;
            let snapshot = encode_snapshot(
                0,
                attachment.session_id(),
                attachment.attachment_id(),
                snapshot,
                frame_sequence,
            )?;
            bytes.extend_from_slice(&snapshot);
            (bytes, "write terminal resync update")
        }
    };
    let bytes = output.encoding.encode(bytes, deadline).await?;
    output.frames.record(frame_sequence, bytes.len());
    write_attachment_bytes_until(
        writer,
        &bytes,
        deadline,
        "terminal update exceeded its absolute deadline",
        operation,
    )
    .await?;
    // This observes stream flushing, not remote snapshot application.
    record_snapshot(voytty_diagnostics::Stage::SnapshotSent);
    Ok(())
}

async fn send_resync(
    request_id: u64,
    attachment: &SessionAttachment,
    snapshot: TerminalSurfaceSnapshot,
    outbound: &mpsc::Sender<AttachmentOutbound>,
    deadline: Instant,
) -> Result<(), DaemonError> {
    let required = v2::TerminalSyncRequired {
        attachment_id: Some(attachment.attachment_id().into()),
        latest_revision: snapshot.revision.get(),
    };
    let mut bytes = encode_message(WireKind::TerminalSyncRequired, request_id, 0, &required)
        .map_err(protocol_error)?;
    bytes.extend_from_slice(&encode_snapshot(
        request_id,
        attachment.session_id(),
        attachment.attachment_id(),
        snapshot,
        // Request-correlated replacements are not optional live output and
        // are never counted by the receipt window.
        0,
    )?);
    send_attachment_outbound_until(
        outbound,
        AttachmentOutbound::surface(bytes, deadline),
        deadline,
    )
    .await
}
