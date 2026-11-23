use super::*;
#[cfg(any(unix, windows))]
use crate::model::ResolvedSessionTarget;
#[cfg(any(unix, windows))]
use std::collections::VecDeque;
#[cfg(any(unix, windows))]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(any(unix, windows))]
use voytty_core::DeviceId;
#[cfg(any(unix, windows))]
use voytty_proto::{DecodedFrame, FrameDecoder, WireKind, encode_message, v2};

#[cfg(any(unix, windows))]
#[tokio::test]
async fn upload_paste_is_rejected_at_driver_when_attachment_changed() {
    let target = ResolvedSessionTarget::device(DeviceId::from_array([0xe1; 32]));
    let (client, mut peer) = SessionClient::terminal_driver_test_pair(
        target,
        SessionId::from_array([0xe2; 16]),
        AttachmentId::from_array([0xe3; 16]),
    );
    let (events, writer) = spawn_terminal_driver(
        client,
        TerminalViewTransportState::Active,
        TerminalViewRoute::Remote,
        false,
    )
    .split();
    let origin = writer
        .upload_origin()
        .await
        .expect("capture current driver origin");
    assert_eq!(origin.target, target);
    let stale = crate::upload::UploadOrigin {
        binding: voytty_core::upload::UploadBinding {
            attachment_id: AttachmentId::from_array([0xe4; 16]),
            ..origin.binding
        },
        ..origin
    };
    let error = writer
        .write_upload_input(stale, b"/tmp/voytty-1/file.png".to_vec())
        .await
        .expect_err("retired attachment cannot receive upload input");
    assert_eq!(error.kind(), DomainErrorKind::LeaseLost);
    let mut bytes = [0; 1];
    assert!(
        tokio::time::timeout(Duration::from_millis(10), peer.read(&mut bytes))
            .await
            .is_err(),
        "stale paste sends no command or replay-lease request"
    );
    drop(events);
}

#[cfg(any(unix, windows))]
#[tokio::test(start_paused = true)]
async fn full_command_queue_expires_and_dequeued_expired_input_is_not_written() {
    let (sender, mut queue) = tokio::sync::mpsc::channel(1);
    let (_outcome_sender, terminal_outcome_queued) = tokio::sync::watch::channel(false);
    let writer = TerminalViewCommandWriter {
        sender,
        terminal_outcome_queued,
        applied_revision: Arc::new(AtomicU64::new(1)),
    };
    let (response, received) = tokio::sync::oneshot::channel();
    writer
        .sender
        .send(PendingTerminalCommand {
            command: TerminalDriverCommand::Input {
                bytes: b"expired".to_vec(),
            },
            deadline: crate::protocol::control_deadline(),
            response,
        })
        .await
        .expect("fill single slot");
    let started = tokio::time::Instant::now();
    assert_eq!(
        writer
            .write_input(b"never admitted".to_vec())
            .await
            .expect_err("queue admission expires")
            .kind(),
        DomainErrorKind::DeadlineExceeded
    );
    assert_eq!(
        tokio::time::Instant::now() - started,
        crate::protocol::DEFAULT_DEADLINE
    );

    let (mut client, mut peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        SessionId::from_array([1; 16]),
        AttachmentId::from_array([2; 16]),
    );
    assert!(matches!(
        handle_terminal_driver_command(queue.recv().await, &mut client).await,
        TerminalDriverCommandResult::Continue
    ));
    assert_eq!(
        received
            .await
            .expect("expired command completes")
            .expect_err("expired before starting")
            .kind(),
        DomainErrorKind::DeadlineExceeded
    );
    assert!(
        queue.try_recv().is_err(),
        "timed-out queue admission retained no command"
    );
    use tokio::io::AsyncReadExt;
    assert!(
        futures_util::FutureExt::now_or_never(peer.read(&mut [0_u8; 1])).is_none(),
        "expired input never reached the transport"
    );
}

#[cfg(any(unix, windows))]
#[tokio::test(start_paused = true)]
async fn command_write_uses_remaining_queue_budget_and_releases_the_owner() {
    let (mut client, mut peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        SessionId::from_array([1; 16]),
        AttachmentId::from_array([2; 16]),
    );
    let deadline = crate::protocol::control_deadline();
    tokio::time::advance(Duration::from_secs(4)).await;
    let (response, received) = tokio::sync::oneshot::channel();
    let result = handle_terminal_driver_command(
        Some(PendingTerminalCommand {
            command: TerminalDriverCommand::Input {
                bytes: vec![b'x'; 900_000],
            },
            deadline,
            response,
        }),
        &mut client,
    )
    .await;
    assert!(matches!(result, TerminalDriverCommandResult::Stop));
    assert_eq!(
        tokio::time::Instant::now(),
        deadline,
        "write cannot start a fresh five-second budget"
    );
    assert_eq!(
        received
            .await
            .expect("command result")
            .expect_err("blocked write expired")
            .kind(),
        DomainErrorKind::DeadlineExceeded
    );
    let mut bytes = Vec::new();
    peer.read_to_end(&mut bytes)
        .await
        .expect("expired owner closes stream");
    assert!(bytes.len() < 900_000);
}
#[test]
fn terminal_end_debug_redacts_the_platform_signal_text() {
    let ended = TerminalViewEnded {
        reason: TerminalViewEndReason::NaturalExit,
        exit_code: 1,
        signal: "SENSITIVE_TERMINAL_SIGNAL_SENTINEL".to_owned(),
    };
    let debug = format!("{ended:?}");
    assert!(debug.contains("has_signal: true"));
    assert!(!debug.contains("SENSITIVE_TERMINAL_SIGNAL_SENTINEL"));
}

#[cfg(any(unix, windows))]
#[test]
fn connection_status_projection_is_route_explicit_and_redacts_private_identity() {
    const ATTACHMENT_SENTINEL: &[u8; AttachmentId::LENGTH] = b"STATUS_ID_SENTIN";
    let attachment_id = AttachmentId::from_array(*ATTACHMENT_SENTINEL);
    let project = |path, rtt_ms| {
        terminal_event_from_local(
            LocalAttachmentEvent::ConnectionStatus(v2::TerminalConnectionStatusEvent {
                attachment_id: Some(attachment_id.into()),
                path,
                rtt_ms,
            }),
            TerminalViewRoute::Remote,
        )
        .expect("same-UID status projects")
        .expect("same-UID status remains visible")
    };

    for (event, expected_path, expected_rtt) in [
        (
            project(v2::TerminalConnectionPath::Unknown as i32, None),
            TerminalViewConnectionPath::Unknown,
            None,
        ),
        (
            project(v2::TerminalConnectionPath::Direct as i32, Some(7)),
            TerminalViewConnectionPath::Direct,
            Some(7),
        ),
        (
            project(v2::TerminalConnectionPath::Relay as i32, Some(19)),
            TerminalViewConnectionPath::Relay,
            Some(19),
        ),
    ] {
        let TerminalViewEvent::ConnectionStatus(status) = event else {
            panic!("connection status projects to its typed view event");
        };
        assert_eq!(status.path(), expected_path);
        assert_eq!(status.rtt_ms(), expected_rtt);
        let rendered = format!("{status:?}");
        assert!(!rendered.contains(
            std::str::from_utf8(ATTACHMENT_SENTINEL).expect("ASCII attachment sentinel")
        ));
    }

    let error = terminal_event_from_local(
        LocalAttachmentEvent::ConnectionStatus(v2::TerminalConnectionStatusEvent {
            attachment_id: Some(attachment_id.into()),
            path: v2::TerminalConnectionPath::Direct as i32,
            rtt_ms: Some(7),
        }),
        TerminalViewRoute::Local,
    )
    .expect_err("local-only views cannot accept remote connection status");
    assert_eq!(error.kind(), DomainErrorKind::MalformedFrame);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn terminal_command_closure_prefers_typed_end_and_normalizes_plain_eof() {
    let session_id = SessionId::from_array([0xc1; SessionId::LENGTH]);
    let attachment_id = AttachmentId::from_array([0xc2; AttachmentId::LENGTH]);
    let (mut client, mut peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        session_id,
        attachment_id,
    );
    peer.write_all(
        &encode_message(
            WireKind::TerminalSessionEnded,
            0,
            0,
            &v2::TerminalSessionEnded {
                session_id: Some(session_id.into()),
                attachment_id: Some(attachment_id.into()),
                reason: v2::TerminalSessionEndReason::NaturalExit as i32,
                exit_code: 0,
                signal: String::new(),
            },
        )
        .expect("encode typed terminal end"),
    )
    .await
    .expect("queue typed terminal end before command closure");
    peer.flush().await.expect("flush typed terminal end");

    let mut pending = VecDeque::new();
    let mut takeover_pending = false;
    let mut awaiting_snapshot_ack = false;
    let mut last_state = TerminalViewTransportState::Active;
    let (effects, _effect_wakeup) = TerminalEffectSlot::new();
    let (response, received) = tokio::sync::oneshot::channel();
    correlate_terminal_command_closure(
        &mut client,
        &mut pending,
        TerminalViewRoute::Local,
        &mut takeover_pending,
        &mut awaiting_snapshot_ack,
        &mut last_state,
        response,
        &effects,
    )
    .await;
    assert_eq!(received.await.expect("command response owner"), Ok(()));
    assert!(matches!(
        pending.pop_front(),
        Some(Ok(TerminalViewEvent::SessionEnded(TerminalViewEnded {
            reason: TerminalViewEndReason::NaturalExit,
            ..
        })))
    ));
    assert!(pending.is_empty());

    let (mut eof_client, eof_peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        session_id,
        attachment_id,
    );
    drop(eof_peer);
    let mut eof_pending = VecDeque::new();
    let (response, received) = tokio::sync::oneshot::channel();
    correlate_terminal_command_closure(
        &mut eof_client,
        &mut eof_pending,
        TerminalViewRoute::Local,
        &mut takeover_pending,
        &mut awaiting_snapshot_ack,
        &mut last_state,
        response,
        &effects,
    )
    .await;
    assert_eq!(received.await.expect("EOF command response owner"), Ok(()));
    let Some(Err(error)) = eof_pending.pop_front() else {
        panic!("plain attachment EOF must become one normalized typed error");
    };
    assert_eq!(error.kind(), DomainErrorKind::DaemonStopped);
    assert!(!error.detail().contains("Broken pipe"));
    assert!(!error.detail().contains("os error"));
    assert!(eof_pending.is_empty());
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn notification_reader_drains_coalesced_wakes_and_prioritizes_lifecycle() {
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    let (effects, effect_wakeup) = TerminalEffectSlot::new();
    let mut reader = TerminalViewEventReader {
        receiver,
        deferred_visual: None,
        effects: Arc::clone(&effects),
        effect_wakeup,
    };
    for index in 0..40 {
        effects.push(TerminalHostEffect::Notification(
            TerminalNotification::osc9(format!("event {index}")).expect("valid"),
        ));
    }
    sender
        .send(Ok(TerminalViewEvent::TransportState(
            TerminalViewTransportState::Active,
        )))
        .await
        .expect("lifecycle");
    assert!(matches!(
        reader.read_event().await.expect("read"),
        Some(TerminalViewEvent::TransportState(_))
    ));
    for index in 8..40 {
        let event = tokio::time::timeout(Duration::from_secs(1), reader.read_event())
            .await
            .expect("no stranded wake")
            .expect("event");
        assert!(
            matches!(event, Some(TerminalViewEvent::Notification(value)) if value.body() == format!("event {index}"))
        );
    }
    effects.push(TerminalHostEffect::Notification(
        TerminalNotification::osc9("during redraw".into()).expect("valid"),
    ));
    for _ in 0..8 {
        sender
            .send(Ok(TerminalViewEvent::ConnectionStatus(
                TerminalViewConnectionStatus {
                    path: TerminalViewConnectionPath::Direct,
                    rtt_ms: Some(1),
                },
            )))
            .await
            .expect("visual update");
    }
    assert!(matches!(
        reader.read_event().await.expect("no starvation"),
        Some(TerminalViewEvent::Notification(_))
    ));
    for _ in 0..8 {
        assert!(matches!(
            reader.read_event().await.expect("visual retained"),
            Some(TerminalViewEvent::ConnectionStatus(_))
        ));
    }
    effects.push(TerminalHostEffect::Notification(
        TerminalNotification::osc9("stale".into()).expect("valid"),
    ));
    let mut pending = VecDeque::new();
    let mut takeover = false;
    let mut awaiting_snapshot_ack = false;
    let mut state = TerminalViewTransportState::Active;
    queue_local_attachment_event(
        Ok(LocalAttachmentEvent::TransportState(
            v2::TerminalTransportStateEvent {
                attachment_id: Some(AttachmentId::from_array([1; 16]).into()),
                state: v2::TerminalTransportState::Reconnecting as i32,
            },
        )),
        &mut pending,
        TerminalViewRoute::Remote,
        &mut takeover,
        &mut awaiting_snapshot_ack,
        &mut state,
        &effects,
    );
    assert!(effects.take().is_none());
    effects.push(TerminalHostEffect::Notification(
        TerminalNotification::osc9("closing".into()).expect("valid"),
    ));
    drop(sender);
    assert!(reader.read_event().await.expect("closed reader").is_none());
    assert!(effects.take().is_none());
}

#[cfg(any(unix, windows))]
#[test]
fn clipboard_events_bypass_the_bounded_semantic_queue_and_keep_only_latest() {
    let (effects, mut wakeup) = TerminalEffectSlot::new();
    let mut pending = VecDeque::from_iter((0..TERMINAL_DRIVER_CAPACITY).map(|_| {
        Ok(TerminalViewEvent::TransportState(
            TerminalViewTransportState::Active,
        ))
    }));
    let mut takeover_pending = false;
    let mut awaiting_snapshot_ack = false;
    let mut last_state = TerminalViewTransportState::Active;
    let first =
        TerminalClipboardWrite::new("first".to_owned()).expect("valid first clipboard value");
    let latest =
        TerminalClipboardWrite::new("latest".to_owned()).expect("valid latest clipboard value");

    assert!(!queue_local_attachment_event(
        Ok(LocalAttachmentEvent::ClipboardWrite(first)),
        &mut pending,
        TerminalViewRoute::Local,
        &mut takeover_pending,
        &mut awaiting_snapshot_ack,
        &mut last_state,
        &effects,
    ));
    assert!(!queue_local_attachment_event(
        Ok(LocalAttachmentEvent::ClipboardWrite(latest)),
        &mut pending,
        TerminalViewRoute::Local,
        &mut takeover_pending,
        &mut awaiting_snapshot_ack,
        &mut last_state,
        &effects,
    ));
    assert_eq!(pending.len(), TERMINAL_DRIVER_CAPACITY);
    assert!(wakeup.has_changed().expect("open effects wakeup"));
    wakeup.borrow_and_update();
    let Some(TerminalHostEffect::ClipboardWrite(write)) = effects.take() else {
        panic!("clipboard effect");
    };
    assert_eq!(write.as_str(), "latest");
    assert!(effects.take().is_none());
}

#[cfg(any(unix, windows))]
#[test]
fn clipboard_slot_is_cleared_by_reconnect_and_malformed_transport_state() {
    let attachment_id = AttachmentId::from_array([0xd2; AttachmentId::LENGTH]);

    for (state, terminal) in [
        (v2::TerminalTransportState::Reconnecting as i32, false),
        (i32::MAX, true),
    ] {
        let (effects, _effect_wakeup) = TerminalEffectSlot::new();
        effects.push(TerminalHostEffect::ClipboardWrite(
            TerminalClipboardWrite::new("stale epoch clipboard".to_owned())
                .expect("valid clipboard fixture"),
        ));
        let mut pending = VecDeque::new();
        let mut takeover_pending = false;
        let mut awaiting_snapshot_ack = false;
        let mut last_state = TerminalViewTransportState::Active;

        assert_eq!(
            queue_local_attachment_event(
                Ok(LocalAttachmentEvent::TransportState(
                    v2::TerminalTransportStateEvent {
                        attachment_id: Some(attachment_id.into()),
                        state,
                    },
                )),
                &mut pending,
                TerminalViewRoute::Local,
                &mut takeover_pending,
                &mut awaiting_snapshot_ack,
                &mut last_state,
                &effects,
            ),
            terminal,
        );
        assert!(
            effects.take().is_none(),
            "an epoch boundary or terminal protocol error must discard pending effects content"
        );
    }
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn legal_resize_after_terminal_event_was_queued_preserves_the_typed_outcome() {
    let session_id = SessionId::from_array([0xc5; SessionId::LENGTH]);
    let attachment_id = AttachmentId::from_array([0xc6; AttachmentId::LENGTH]);
    let (client, mut peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        session_id,
        attachment_id,
    );
    let view = spawn_terminal_driver(
        client,
        TerminalViewTransportState::Active,
        TerminalViewRoute::Local,
        false,
    );
    let (mut events, writer) = view.split();
    assert!(matches!(
        events.read_event().await.expect("initial terminal event"),
        Some(TerminalViewEvent::TransportState(
            TerminalViewTransportState::Active
        ))
    ));

    peer.write_all(
        &encode_message(
            WireKind::TerminalSessionEnded,
            0,
            0,
            &v2::TerminalSessionEnded {
                session_id: Some(session_id.into()),
                attachment_id: Some(attachment_id.into()),
                reason: v2::TerminalSessionEndReason::NaturalExit as i32,
                exit_code: 0,
                signal: String::new(),
            },
        )
        .expect("encode queued terminal outcome"),
    )
    .await
    .expect("queue terminal outcome");
    peer.flush().await.expect("flush terminal outcome");
    drop(peer);

    tokio::time::timeout(Duration::from_secs(1), writer.sender.closed())
        .await
        .expect("terminal driver must close its command owner after queuing the outcome");
    writer
        .resize(TerminalSize::new(31, 97))
        .await
        .expect("a legal resize must defer to the already queued terminal outcome");
    assert!(matches!(
        events
            .read_event()
            .await
            .expect("authoritative terminal event"),
        Some(TerminalViewEvent::SessionEnded(TerminalViewEnded {
            reason: TerminalViewEndReason::NaturalExit,
            ..
        }))
    ));

    let (command_sender, mut command_receiver) =
        tokio::sync::mpsc::channel(TERMINAL_DRIVER_CAPACITY);
    let (outcome_sender, outcome_receiver) = tokio::sync::watch::channel(false);
    let (event_sender, mut event_receiver) = tokio::sync::mpsc::channel::<TerminalViewEvent>(1);
    let response_owner = tokio::spawn(async move {
        let command = command_receiver
            .recv()
            .await
            .expect("receive the accepted resize command");
        event_sender
            .send(TerminalViewEvent::LeaseLost { generation: 41 })
            .await
            .expect("queue authoritative response-owner outcome");
        outcome_sender.send_replace(true);
        drop(command);
    });
    let response_closed_writer = TerminalViewCommandWriter {
        sender: command_sender,
        terminal_outcome_queued: outcome_receiver,
        applied_revision: Arc::new(AtomicU64::new(0)),
    };
    response_closed_writer
        .resize(TerminalSize::new(31, 97))
        .await
        .expect("a closed response owner must use the same event-side correlation");
    response_owner.await.expect("response-owner schedule");
    assert!(matches!(
        event_receiver.recv().await,
        Some(TerminalViewEvent::LeaseLost { generation: 41 })
    ));

    let (closed_sender, closed_receiver) = tokio::sync::mpsc::channel(TERMINAL_DRIVER_CAPACITY);
    drop(closed_receiver);
    let (unavailable_sender, unavailable_receiver) = tokio::sync::watch::channel(false);
    drop(unavailable_sender);
    let unavailable_writer = TerminalViewCommandWriter {
        sender: closed_sender,
        terminal_outcome_queued: unavailable_receiver,
        applied_revision: Arc::new(AtomicU64::new(0)),
    };
    let error = unavailable_writer
        .resize(TerminalSize::new(31, 97))
        .await
        .expect_err("closure without an authoritative event must stay a bounded error");
    assert_eq!(error.kind(), DomainErrorKind::DaemonStopped);
    assert!(!error.detail().contains("terminal attachment driver closed"));
    assert!(!error.detail().contains("Broken pipe"));
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn dropped_terminal_event_reader_never_confirms_a_closed_command() {
    let session_id = SessionId::from_array([0xc7; SessionId::LENGTH]);
    let attachment_id = AttachmentId::from_array([0xc8; AttachmentId::LENGTH]);
    let (client, peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        session_id,
        attachment_id,
    );
    let view = spawn_terminal_driver(
        client,
        TerminalViewTransportState::Active,
        TerminalViewRoute::Local,
        false,
    );
    let (events, writer) = view.split();
    drop(events);

    tokio::time::timeout(Duration::from_secs(1), writer.sender.closed())
        .await
        .expect("dropping the event owner must stop the terminal driver");
    let error = writer
        .resize(TerminalSize::new(31, 97))
        .await
        .expect_err("no command may be confirmed after its event owner was dropped");
    assert_eq!(error.kind(), DomainErrorKind::DaemonStopped);
    assert!(!error.detail().contains("terminal attachment driver closed"));
    assert!(!error.detail().contains("Broken pipe"));
    drop(peer);
}

#[cfg(any(unix, windows))]
async fn closed_resize_schedule(
    session_id: SessionId,
    attachment_id: AttachmentId,
    frames: impl IntoIterator<Item = Vec<u8>>,
    route: TerminalViewRoute,
) -> (
    VecDeque<Result<TerminalViewEvent, DaemonError>>,
    TerminalViewTransportState,
) {
    let (mut client, mut peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        session_id,
        attachment_id,
    );
    for frame in frames {
        peer.write_all(&frame)
            .await
            .expect("queue closure-schedule frame");
    }
    peer.flush().await.expect("flush closure-schedule frames");
    drop(peer);

    let mut pending = VecDeque::new();
    let mut takeover_pending = false;
    let mut awaiting_snapshot_ack = false;
    let mut last_state = TerminalViewTransportState::Active;
    let mut stop_after_pending = false;
    let (effects, _effect_wakeup) = TerminalEffectSlot::new();
    let (response, received) = tokio::sync::oneshot::channel();
    assert!(
        !apply_terminal_driver_command(
            Some(PendingTerminalCommand {
                command: TerminalDriverCommand::Resize {
                    size: TerminalSize::new(31, 97)
                },
                deadline: crate::protocol::control_deadline(),
                response
            }),
            &mut client,
            &mut pending,
            route,
            &mut takeover_pending,
            &mut awaiting_snapshot_ack,
            &mut last_state,
            &mut stop_after_pending,
            &effects,
        )
        .await,
        "a correlated closure drains its typed outcome before the driver stops"
    );
    assert!(stop_after_pending);
    assert_eq!(received.await.expect("resize response owner"), Ok(()));
    (pending, last_state)
}

#[cfg(any(unix, windows))]
fn closure_schedule_frame<Message: prost::Message>(kind: WireKind, message: &Message) -> Vec<u8> {
    encode_message(kind, 0, 0, message).expect("encode closure-schedule frame")
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn resize_closure_schedules_preserve_typed_end_lease_error_and_remote_resync_order() {
    let session_id = SessionId::from_array([0xc3; SessionId::LENGTH]);
    let attachment_id = AttachmentId::from_array([0xc4; AttachmentId::LENGTH]);

    let (mut ended, _) = closed_resize_schedule(
        session_id,
        attachment_id,
        [closure_schedule_frame(
            WireKind::TerminalSessionEnded,
            &v2::TerminalSessionEnded {
                session_id: Some(session_id.into()),
                attachment_id: Some(attachment_id.into()),
                reason: v2::TerminalSessionEndReason::DaemonStop as i32,
                exit_code: 0,
                signal: String::new(),
            },
        )],
        TerminalViewRoute::Local,
    )
    .await;
    assert!(matches!(
        ended.pop_front(),
        Some(Ok(TerminalViewEvent::SessionEnded(TerminalViewEnded {
            reason: TerminalViewEndReason::DaemonStop,
            ..
        })))
    ));
    assert!(ended.is_empty());

    let (mut lost, _) = closed_resize_schedule(
        session_id,
        attachment_id,
        [closure_schedule_frame(
            WireKind::TerminalLeaseLost,
            &v2::TerminalLeaseLost {
                attachment_id: Some(attachment_id.into()),
                generation: 23,
            },
        )],
        TerminalViewRoute::Local,
    )
    .await;
    assert!(matches!(
        lost.pop_front(),
        Some(Ok(TerminalViewEvent::LeaseLost { generation: 23 }))
    ));
    assert!(lost.is_empty());

    let (mut resync, last_state) = closed_resize_schedule(
        session_id,
        attachment_id,
        [
            closure_schedule_frame(
                WireKind::TerminalTransportStateEvent,
                &v2::TerminalTransportStateEvent {
                    attachment_id: Some(attachment_id.into()),
                    state: v2::TerminalTransportState::Reconnecting as i32,
                },
            ),
            closure_schedule_frame(
                WireKind::TerminalConnectionStatusEvent,
                &v2::TerminalConnectionStatusEvent {
                    attachment_id: Some(attachment_id.into()),
                    path: v2::TerminalConnectionPath::Unknown as i32,
                    rtt_ms: None,
                },
            ),
            closure_schedule_frame(
                WireKind::TerminalSyncRequired,
                &v2::TerminalSyncRequired {
                    attachment_id: Some(attachment_id.into()),
                    latest_revision: 29,
                },
            ),
        ],
        TerminalViewRoute::Remote,
    )
    .await;
    assert!(matches!(
        resync.pop_front(),
        Some(Ok(TerminalViewEvent::TransportState(
            TerminalViewTransportState::Reconnecting
        )))
    ));
    assert!(matches!(
        resync.pop_front(),
        Some(Ok(TerminalViewEvent::ConnectionStatus(_)))
    ));
    assert!(matches!(
        resync.pop_front(),
        Some(Ok(TerminalViewEvent::TransportState(
            TerminalViewTransportState::Synchronizing
        )))
    ));
    assert!(matches!(
        resync.pop_front(),
        Some(Ok(TerminalViewEvent::SyncRequired {
            latest_revision
        })) if latest_revision == Revision::new(29)
    ));
    let Some(Err(resync_closed)) = resync.pop_front() else {
        panic!("an incomplete resync followed by EOF is normalized once");
    };
    assert_eq!(resync_closed.kind(), DomainErrorKind::DaemonStopped);
    assert!(!resync_closed.detail().contains("Broken pipe"));
    assert!(resync.is_empty());
    assert_eq!(last_state, TerminalViewTransportState::Synchronizing);

    let typed_error = DaemonError::new(
        DomainErrorKind::Unauthorized,
        "BUFFERED_TYPED_ERROR_SENTINEL",
    );
    let (mut failed, _) = closed_resize_schedule(
        session_id,
        attachment_id,
        [closure_schedule_frame(
            WireKind::ServiceErrorResponse,
            &v2::ServiceError {
                code: typed_error.kind().code().to_owned(),
                message: typed_error.detail().to_owned(),
            },
        )],
        TerminalViewRoute::Local,
    )
    .await;
    let Some(Err(error)) = failed.pop_front() else {
        panic!("buffered typed service error wins the resize closure race");
    };
    assert_eq!(error.kind(), DomainErrorKind::Unauthorized);
    assert_eq!(error.detail(), "BUFFERED_TYPED_ERROR_SENTINEL");
    assert!(!error.detail().contains("Broken pipe"));
    assert!(failed.is_empty());
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn terminal_driver_local_exact_ack_activates_once_with_bounded_channels() {
    let (prepared, mut daemon, _, attachment_id) = terminal_test_view(false);
    let server = tokio::spawn(async move {
        let mut decoder = FrameDecoder::new();
        let mut queued = VecDeque::new();
        let initial_ack = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        assert_eq!(initial_ack.kind, WireKind::TerminalSnapshotApplied);
        let initial: v2::TerminalSnapshotApplied = initial_ack
            .decode_message(WireKind::TerminalSnapshotApplied)
            .expect("initial snapshot acknowledgement");
        assert_eq!(initial.revision, 1);

        let repeated_ack = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        assert_eq!(repeated_ack.kind, WireKind::TerminalSnapshotApplied);
        daemon
            .write_all(
                &encode_message(
                    WireKind::TerminalLeaseLost,
                    0,
                    0,
                    &v2::TerminalLeaseLost {
                        attachment_id: Some(attachment_id.into()),
                        generation: 7,
                    },
                )
                .expect("bounded lease-lost marker"),
            )
            .await
            .expect("write lease-lost marker");

        let mut after_terminal = [0_u8; 1];
        assert_eq!(
            daemon
                .read(&mut after_terminal)
                .await
                .expect("read the driver's terminal-event shutdown"),
            0,
            "the typed lease-loss event closes the local driver"
        );
    });

    let view = prepared
        .acknowledge_initial()
        .await
        .expect("exact initial acknowledgement");
    let (mut events, writer) = view.split();
    assert_eq!(writer.sender.max_capacity(), TERMINAL_DRIVER_CAPACITY);
    assert!(matches!(
        events.read_event().await.expect("initial event"),
        Some(TerminalViewEvent::TransportState(
            TerminalViewTransportState::Active
        ))
    ));
    writer
        .snapshot_applied(Revision::new(2))
        .await
        .expect("repeated acknowledgement is serialized");
    assert!(matches!(
        events.read_event().await.expect("post-ack marker"),
        Some(TerminalViewEvent::LeaseLost { generation: 7 })
    ));
    drop(writer);
    server.await.expect("local driver server");
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn overlapping_history_window_request_is_rejected_without_ending_the_driver() {
    let (prepared, mut daemon, _, attachment_id) = terminal_test_view(false);
    let server = tokio::spawn(async move {
        let mut decoder = FrameDecoder::new();
        let mut queued = VecDeque::new();
        let initial = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        assert_eq!(initial.kind, WireKind::TerminalSnapshotApplied);
        let request = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        assert_eq!(request.kind, WireKind::TerminalHistoryWindowRequest);
        // The rejected overlap writes nothing; the next frame is input.
        let input = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        assert_eq!(input.kind, WireKind::TerminalInput);
        daemon
            .write_all(
                &encode_message(
                    WireKind::TerminalLeaseLost,
                    0,
                    0,
                    &v2::TerminalLeaseLost {
                        attachment_id: Some(attachment_id.into()),
                        generation: 7,
                    },
                )
                .expect("bounded lease-lost marker"),
            )
            .await
            .expect("write lease-lost marker");
        daemon
    });

    let view = prepared
        .acknowledge_initial()
        .await
        .expect("exact initial acknowledgement");
    let (mut events, writer) = view.split();
    assert!(matches!(
        events.read_event().await.expect("initial state"),
        Some(TerminalViewEvent::TransportState(
            TerminalViewTransportState::Active
        ))
    ));
    let query = TerminalHistoryWindowQuery {
        anchor: voytty_core::terminal::TerminalHistoryWindowAnchor {
            epoch: Revision::new(1),
            revision: Revision::new(2),
            max_offset_from_bottom: 12,
            viewport: voytty_core::terminal::TerminalSize::new(4, 10),
        },
        target_offset_from_bottom: 0,
        older_margin_rows: 8,
        newer_margin_rows: 0,
    };
    writer
        .request_history_window(query)
        .await
        .expect("first bounded window request");
    // A frontend whose cache was invalidated may ask again before the
    // first response arrives. The rejection is local and leaves the
    // attachment intact.
    let error = writer
        .request_history_window(query)
        .await
        .expect_err("only one window response may be outstanding");
    assert_eq!(error.kind(), DomainErrorKind::ResourceExhausted);
    writer
        .write_input(b"x".to_vec())
        .await
        .expect("the attachment still accepts input");
    assert!(matches!(
        events.read_event().await.expect("post-rejection marker"),
        Some(TerminalViewEvent::LeaseLost { generation: 7 })
    ));
    drop(writer);
    let _daemon = server.await.expect("history overlap server");
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn queued_resume_barrier_survives_view_state_transitions() {
    let (prepared, _peer, _, _) = terminal_test_view(false);
    let snapshot = prepared.initial_snapshot();
    let delta = TerminalViewDelta {
        application_title: String::new(),
        colors: Default::default(),

        from_revision: snapshot.revision,
        to_revision: Revision::new(snapshot.revision.get() + 1),
        size: snapshot.surface.size,
        active_screen: snapshot.surface.active_screen,
        row_patches: Vec::new(),
        cursor: snapshot.surface.cursor,
        modes: snapshot.surface.modes,
        scroll_metrics: None,
    };
    for route in [TerminalViewRoute::Local, TerminalViewRoute::Remote] {
        for initial in [
            TerminalViewTransportState::Active,
            TerminalViewTransportState::Synchronizing,
        ] {
            let mut pending = VecDeque::new();
            let mut takeover_pending = false;
            let mut awaiting_snapshot_ack = false;
            let mut state = initial;
            let (effects, _wakeup) = TerminalEffectSlot::new();
            for event in [
                LocalAttachmentEvent::Delta(delta.clone()),
                LocalAttachmentEvent::ResumeDelta(delta.clone()),
            ] {
                assert!(!queue_local_attachment_event(
                    Ok(event),
                    &mut pending,
                    route,
                    &mut takeover_pending,
                    &mut awaiting_snapshot_ack,
                    &mut state,
                    &effects
                ));
            }
            assert!(matches!(
                pending.pop_front(),
                Some(Ok(TerminalViewEvent::Delta(_)))
            ));
            if initial == TerminalViewTransportState::Active {
                assert!(matches!(
                    pending.pop_front(),
                    Some(Ok(TerminalViewEvent::TransportState(
                        TerminalViewTransportState::Synchronizing
                    )))
                ));
            }
            assert!(matches!(
                pending.pop_front(),
                Some(Ok(TerminalViewEvent::ResumeDelta(_)))
            ));
            assert!(pending.is_empty());
        }
    }
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn lost_remote_snapshot_ack_stays_synchronizing_until_resume() {
    let target = DeviceId::from_array([0xd1; DeviceId::LENGTH]);
    let (mut client, mut dead_peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(target),
        SessionId::from_array([0xd2; SessionId::LENGTH]),
        AttachmentId::from_array([0xd3; AttachmentId::LENGTH]),
    );
    dead_peer
        .shutdown()
        .await
        .expect("close the simulated remote epoch");
    drop(dead_peer);

    let mut pending = VecDeque::new();
    let mut takeover_pending = false;
    let mut awaiting_snapshot_ack = false;
    let mut last_state = TerminalViewTransportState::Synchronizing;
    let mut stop_after_pending = false;
    let (effects, _effect_wakeup) = TerminalEffectSlot::new();
    let (response, received) = tokio::sync::oneshot::channel();
    assert!(
        !apply_terminal_driver_command(
            Some(PendingTerminalCommand {
                command: TerminalDriverCommand::SnapshotApplied {
                    revision: Revision::new(2)
                },
                deadline: crate::protocol::control_deadline(),
                response
            }),
            &mut client,
            &mut pending,
            TerminalViewRoute::Remote,
            &mut takeover_pending,
            &mut awaiting_snapshot_ack,
            &mut last_state,
            &mut stop_after_pending,
            &effects,
        )
        .await
    );
    assert_eq!(received.await.expect("snapshot ack response owner"), Ok(()));
    assert!(client.reconnect_pending());
    assert_eq!(last_state, TerminalViewTransportState::Synchronizing);
    assert!(pending.is_empty(), "a dead epoch must not emit Active");
    assert!(!stop_after_pending);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn lost_initial_remote_snapshot_ack_starts_driver_reconnecting_not_active() {
    let target_device = DeviceId::from_array([0xd4; DeviceId::LENGTH]);
    let session_id = SessionId::from_array([0xd5; SessionId::LENGTH]);
    let attachment_id = AttachmentId::from_array([0xd6; AttachmentId::LENGTH]);
    let (mut client, mut dead_peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::device(target_device),
        session_id,
        attachment_id,
    );
    let initial_snapshot = client
        .take_initial_snapshot()
        .expect("new attachment retains its initial snapshot");
    dead_peer
        .shutdown()
        .await
        .expect("close the simulated initial remote epoch");
    drop(dead_peer);
    let prepared = PreparedTerminalView {
        session_id,
        attachment_id,
        initial_snapshot,
        takeover: false,
        target: TerminalViewTarget::for_display("peer", TerminalViewRoute::Remote),
        client,
    };

    let view = prepared
        .acknowledge_initial()
        .await
        .expect("lost initial ack enters recovery instead of failing locally");
    let (mut events, writer) = view.split();
    assert!(matches!(
        events
            .read_event()
            .await
            .expect("initial reconnecting event"),
        Some(TerminalViewEvent::TransportState(
            TerminalViewTransportState::Reconnecting
        ))
    ));
    drop(events);
    drop(writer);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn terminal_driver_takeover_activates_only_after_correlated_response() {
    let (prepared, mut daemon, session_id, _) = terminal_test_view(true);
    let server = tokio::spawn(async move {
        let mut decoder = FrameDecoder::new();
        let mut queued = VecDeque::new();
        let ack = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        assert_eq!(ack.kind, WireKind::TerminalSnapshotApplied);
        let lease_request = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        assert_eq!(lease_request.kind, WireKind::SessionOperationLeaseRequest);
        daemon
            .write_all(
                &encode_message(
                    WireKind::SessionOperationLeaseResponse,
                    lease_request.request_id,
                    0,
                    &test_operation_lease(),
                )
                .expect("bounded operation lease response"),
            )
            .await
            .expect("write operation lease response");
        let takeover = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        assert_eq!(takeover.kind, WireKind::SessionTakeoverRequest);
        daemon
            .write_all(
                &encode_message(
                    WireKind::SessionMutateResponse,
                    takeover.request_id,
                    0,
                    &test_takeover_response(session_id),
                )
                .expect("bounded correlated takeover response"),
            )
            .await
            .expect("write correlated takeover response");
        let detach = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        assert_eq!(detach.kind, WireKind::TerminalDetach);
        let mut after_detach = [0_u8; 1];
        assert_eq!(
            daemon
                .read(&mut after_detach)
                .await
                .expect("read the takeover driver's detach shutdown"),
            0,
            "the takeover driver half-closes after the detach frame"
        );
    });

    let view = prepared
        .acknowledge_initial()
        .await
        .expect("begin exact takeover");
    let (mut events, writer) = view.split();
    assert!(matches!(
        events.read_event().await.expect("initial takeover state"),
        Some(TerminalViewEvent::TransportState(
            TerminalViewTransportState::Synchronizing
        ))
    ));
    assert!(matches!(
        events
            .read_event()
            .await
            .expect("correlated takeover state"),
        Some(TerminalViewEvent::TransportState(
            TerminalViewTransportState::Active
        ))
    ));
    writer.detach().await.expect("detach takeover view");
    server.await.expect("takeover server");
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn terminal_driver_outcome_unknown_is_terminal_and_never_replays_takeover() {
    let (prepared, mut daemon, _, _) = terminal_test_view(true);
    let server = tokio::spawn(async move {
        let mut decoder = FrameDecoder::new();
        let mut queued = VecDeque::new();
        let ack = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        assert_eq!(ack.kind, WireKind::TerminalSnapshotApplied);
        let lease_request = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        daemon
            .write_all(
                &encode_message(
                    WireKind::SessionOperationLeaseResponse,
                    lease_request.request_id,
                    0,
                    &test_operation_lease(),
                )
                .expect("bounded operation lease response"),
            )
            .await
            .expect("write operation lease response");
        let takeover = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        assert_eq!(takeover.kind, WireKind::SessionTakeoverRequest);
        daemon
            .write_all(
                &encode_message(
                    WireKind::ServiceErrorResponse,
                    takeover.request_id,
                    0,
                    &v2::ServiceError {
                        code: DomainErrorKind::OperationOutcomeUnknown.code().to_owned(),
                        message: "takeover outcome is unknown".to_owned(),
                    },
                )
                .expect("bounded outcome-unknown response"),
            )
            .await
            .expect("write outcome-unknown response");
        let mut unexpected = [0_u8; 1];
        assert_eq!(
            daemon.read(&mut unexpected).await.expect("driver closes"),
            0,
            "the terminal driver must not retry under a fresh lease"
        );
    });

    let view = prepared
        .acknowledge_initial()
        .await
        .expect("begin takeover before ambiguous result");
    let (mut events, _writer) = view.split();
    assert!(matches!(
        events.read_event().await.expect("initial takeover state"),
        Some(TerminalViewEvent::TransportState(
            TerminalViewTransportState::Synchronizing
        ))
    ));
    let error = events
        .read_event()
        .await
        .expect_err("outcome unknown is a typed terminal driver error");
    assert_eq!(error.kind(), DomainErrorKind::OperationOutcomeUnknown);
    server.await.expect("outcome-unknown server");
}

#[cfg(any(unix, windows))]
fn terminal_test_view(
    takeover: bool,
) -> (
    PreparedTerminalView,
    crate::session::TestStream,
    SessionId,
    AttachmentId,
) {
    let session_id = SessionId::from_array([0xb1; SessionId::LENGTH]);
    let attachment_id = AttachmentId::from_array([0xb2; AttachmentId::LENGTH]);
    let (mut client, peer) = SessionClient::terminal_driver_test_pair(
        ResolvedSessionTarget::local(),
        session_id,
        attachment_id,
    );
    let initial_snapshot = client
        .take_initial_snapshot()
        .expect("new attachment retains its initial snapshot");
    (
        PreparedTerminalView {
            session_id,
            attachment_id,
            initial_snapshot,
            takeover,
            target: TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
            client,
        },
        peer,
        session_id,
        attachment_id,
    )
}

#[cfg(any(unix, windows))]
async fn read_terminal_test_frame(
    stream: &mut crate::session::TestStream,
    decoder: &mut FrameDecoder,
    queued: &mut VecDeque<DecodedFrame>,
) -> DecodedFrame {
    if let Some(frame) = queued.pop_front() {
        return frame;
    }
    let mut bytes = [0_u8; 4096];
    loop {
        let read = stream.read(&mut bytes).await.expect("read driver frame");
        assert_ne!(
            read, 0,
            "terminal driver stream closed before the next frame"
        );
        queued.extend(decoder.feed(&bytes[..read]).expect("decode driver frame"));
        if let Some(frame) = queued.pop_front() {
            return frame;
        }
    }
}

#[cfg(any(unix, windows))]
fn test_operation_lease() -> v2::SessionOperationLeaseResponse {
    v2::SessionOperationLeaseResponse {
        lease: Some(v2::OperationLease {
            daemon_incarnation: vec![0xb3; 16],
            ordinal: 1,
        }),
    }
}

#[cfg(any(unix, windows))]
fn test_takeover_response(session_id: SessionId) -> v2::SessionMutateResponse {
    v2::SessionMutateResponse {
        session: Some(v2::SessionSummary {
            session_id: Some(session_id.into()),
            name: "main".to_owned(),
            revision: 1,
            has_controller: true,
            working_directory: String::new(),
            viewport: Some(v2::TerminalViewport {
                rows: 24,
                columns: 80,
            }),
        }),
    }
}

#[cfg(any(unix, windows))]
fn receipt_test_delta(attachment_id: AttachmentId, from: u64, frame_sequence: u64) -> Vec<u8> {
    let delta = voytty_core::terminal::TerminalSurfaceDelta {
        application_title: String::new(),
        colors: Default::default(),
        from_revision: Revision::new(from),
        to_revision: Revision::new(from + 1),
        size: TerminalSize::new(24, 80),
        active_screen: voytty_core::terminal::ActiveScreen::Main,
        row_patches: Vec::new(),
        cursor: voytty_core::terminal::TerminalCursor {
            presentation: Default::default(),
            row: 0,
            column: 0,
            visible: true,
            style: Default::default(),
        },
        modes: voytty_core::terminal::TerminalModes::default(),
        scroll_metrics: Some(voytty_core::terminal::TerminalScrollMetrics {
            epoch: Revision::ZERO,
            revision: Revision::new(from + 1),
            offset_from_bottom: 0,
            max_offset_from_bottom: 0,
            viewport_rows: 24,
        }),
    };
    let mut message = voytty_proto::terminal_surface_delta_message(attachment_id, delta);
    message.frame_sequence = frame_sequence;
    encode_message(WireKind::TerminalSemanticDelta, 0, 0, &message).expect("bounded delta")
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn terminal_driver_reports_numbered_surface_frames_and_rejects_gaps() {
    let (mut prepared, mut daemon, _, attachment_id) = terminal_test_view(false);
    prepared.client.negotiate_surface_frame_receipts_for_test();
    let server = tokio::spawn(async move {
        let mut decoder = FrameDecoder::new();
        let mut queued = VecDeque::new();
        let initial = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        assert_eq!(initial.kind, WireKind::TerminalSnapshotApplied);
        // An unnumbered frame (older host) is never acknowledged.
        daemon
            .write_all(&receipt_test_delta(attachment_id, 1, 0))
            .await
            .expect("write unnumbered delta");
        daemon
            .write_all(&receipt_test_delta(attachment_id, 2, 1))
            .await
            .expect("write first numbered delta");
        let receipt = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        assert_eq!(receipt.kind, WireKind::TerminalSurfaceFramesReceived);
        let receipt: v2::TerminalSurfaceFramesReceived = receipt
            .decode_message(WireKind::TerminalSurfaceFramesReceived)
            .expect("receipt");
        assert_eq!(receipt.frame_sequence, 1);
        assert_eq!(
            AttachmentId::try_from(receipt.attachment_id.expect("attachment")).expect("id"),
            attachment_id
        );
        daemon
            .write_all(&receipt_test_delta(attachment_id, 3, 2))
            .await
            .expect("write second numbered delta");
        let receipt = read_terminal_test_frame(&mut daemon, &mut decoder, &mut queued).await;
        let receipt: v2::TerminalSurfaceFramesReceived = receipt
            .decode_message(WireKind::TerminalSurfaceFramesReceived)
            .expect("second receipt");
        assert_eq!(receipt.frame_sequence, 2);
        // A skipped sequence is a protocol violation, not a silent gap.
        daemon
            .write_all(&receipt_test_delta(attachment_id, 4, 4))
            .await
            .expect("write gapped delta");
        daemon
    });

    let view = prepared
        .acknowledge_initial()
        .await
        .expect("exact initial acknowledgement");
    let (mut events, writer) = view.split();
    assert!(matches!(
        events.read_event().await.expect("initial state"),
        Some(TerminalViewEvent::TransportState(
            TerminalViewTransportState::Active
        ))
    ));
    let mut deltas = 0;
    let failure = loop {
        match tokio::time::timeout(Duration::from_secs(5), events.read_event())
            .await
            .expect("driver event deadline")
        {
            Ok(Some(TerminalViewEvent::Delta(_))) => deltas += 1,
            Ok(Some(_)) => {}
            Ok(None) => panic!("driver closed without reporting the gap"),
            Err(error) => break error,
        }
    };
    assert_eq!(deltas, 3, "the gapped frame is not delivered");
    assert_eq!(failure.kind(), DomainErrorKind::MalformedFrame);
    drop(writer);
    let _daemon = server.await.expect("receipt test server");
}

#[test]
fn takeover_reply_does_not_mark_active_while_a_new_snapshot_awaits_ack() {
    let mut pending = std::collections::VecDeque::new();
    let mut local_takeover_pending = true;
    let mut awaiting_snapshot_ack = false;
    let mut last_state = TerminalViewTransportState::Synchronizing;
    let (effects, _wake) = TerminalEffectSlot::new();
    let attachment_id = AttachmentId::from_array([0x51; 16]);
    let session_id = SessionId::from_array([0x52; 16]);

    assert!(!queue_local_attachment_event(
        Ok(LocalAttachmentEvent::SyncRequired(
            v2::TerminalSyncRequired {
                attachment_id: Some(attachment_id.into()),
                latest_revision: 5,
            },
        )),
        &mut pending,
        TerminalViewRoute::Local,
        &mut local_takeover_pending,
        &mut awaiting_snapshot_ack,
        &mut last_state,
        &effects,
    ));
    assert!(awaiting_snapshot_ack);
    assert_eq!(last_state, TerminalViewTransportState::Synchronizing);

    assert!(!queue_local_attachment_event(
        Ok(LocalAttachmentEvent::Takeover(
            crate::model::SessionSummary {
                session_id,
                name: voytty_core::SessionName::new("takeover").expect("name"),
                revision: Revision::new(5),
                has_controller: true,
                working_directory: std::path::PathBuf::from("/tmp"),
                viewport: TerminalSize::new(24, 80),
            },
        )),
        &mut pending,
        TerminalViewRoute::Local,
        &mut local_takeover_pending,
        &mut awaiting_snapshot_ack,
        &mut last_state,
        &effects,
    ));
    assert!(!local_takeover_pending);
    assert!(awaiting_snapshot_ack);
    assert_eq!(
        last_state,
        TerminalViewTransportState::Synchronizing,
        "unconfirmed sync snapshot must keep the view synchronizing until SnapshotApplied"
    );
    assert!(!pending.iter().any(|event| matches!(
        event,
        Ok(TerminalViewEvent::TransportState(
            TerminalViewTransportState::Active
        ))
    )));
}
