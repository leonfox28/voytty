//! Controlled wire schedules through the real shared driver and Android reducer.
use super::*;
use std::{sync::Mutex, time::Duration};
use zterm_client::{
    error::ClientError,
    transport::{
        AttachmentConnector, AttachmentIo, AttachmentTransport, AttachmentTransportItem,
        TransportFuture,
    },
};
use zterm_core::{AttachmentId, DeviceId, DomainErrorKind, Revision};
use zterm_proto::{DecodedFrame, FrameDecoder, WireKind, encode_message, v2};

const SESSION: SessionId = SessionId::from_array([0x93; 16]);
const ATTACHMENT: AttachmentId = AttachmentId::from_array([0x94; 16]);

struct Wire {
    incoming: mpsc::Receiver<DecodedFrame>,
    outgoing: mpsc::Sender<DecodedFrame>,
}

impl AttachmentIo for Wire {
    fn queued_session_count(&self) -> usize {
        self.incoming.len()
    }
    fn write<'a>(&'a mut self, bytes: &'a [u8]) -> TransportFuture<'a, Result<(), ClientError>> {
        Box::pin(async move {
            for frame in FrameDecoder::new()
                .feed(bytes)
                .expect("valid client command")
            {
                self.outgoing
                    .send(frame)
                    .await
                    .expect("fixture retains commands");
            }
            Ok(())
        })
    }
    fn read(&mut self) -> TransportFuture<'_, Result<AttachmentTransportItem, ClientError>> {
        Box::pin(async move {
            self.incoming
                .recv()
                .await
                .map(AttachmentTransportItem::Session)
                .ok_or_else(|| {
                    ClientError::new(DomainErrorKind::TransportUnavailable, "fixture closed")
                })
        })
    }
    fn shutdown(&mut self) -> TransportFuture<'_, Result<(), ClientError>> {
        Box::pin(async { Ok(()) })
    }
}

struct Connector(Mutex<Option<Wire>>);
impl AttachmentConnector for Connector {
    fn open(
        &self,
        _: ResolvedSessionTarget,
    ) -> TransportFuture<'_, Result<AttachmentTransport, ClientError>> {
        Box::pin(async move {
            Ok(AttachmentTransport::new(
                self.0
                    .lock()
                    .expect("fixture lock")
                    .take()
                    .expect("one attachment"),
            ))
        })
    }
}
impl zterm_client::upload::UploadConnector for Connector {
    fn open_upload(
        &self,
        _: ResolvedSessionTarget,
    ) -> TransportFuture<'_, Result<zterm_client::upload::UploadConnection, ClientError>> {
        Box::pin(async { panic!("synchronization fixture must not open an upload") })
    }
}

fn snapshot(revision: u64, size: TerminalSize) -> TerminalSurfaceSnapshot {
    TerminalSurfaceSnapshot {
        revision: Revision::new(revision),
        surface: TerminalSurface {
            application_title: String::new(),
            colors: TerminalColorSnapshot::default(),
            size,
            active_screen: ActiveScreen::Main,
            rows: (0..size.rows)
                .map(|_| TerminalSurfaceRow {
                    cells: vec![TerminalCell::default(); usize::from(size.columns)],
                    wrapped: false,
                })
                .collect(),
            cursor: TerminalCursor {
                presentation: Default::default(),
                row: 0,
                column: 0,
                visible: true,
                style: TerminalStyle::default(),
            },
            modes: TerminalModes::default(),
            scroll_metrics: Some(TerminalScrollMetrics {
                epoch: Revision::new(revision),
                revision: Revision::new(revision),
                max_offset_from_bottom: 0,
                offset_from_bottom: 0,
                viewport_rows: size.rows,
            }),
        },
    }
}

fn snapshot_frame(revision: u64, size: TerminalSize, request: u64) -> DecodedFrame {
    let bytes = encode_message(
        WireKind::TerminalSemanticSnapshot,
        request,
        0,
        &zterm_proto::terminal_surface_snapshot_message(
            SESSION,
            ATTACHMENT,
            snapshot(revision, size),
        ),
    )
    .expect("snapshot encoding");
    FrameDecoder::new()
        .feed(&bytes)
        .expect("snapshot decoding")
        .remove(0)
}

async fn next_command(commands: &mut mpsc::Receiver<DecodedFrame>, kind: WireKind) -> DecodedFrame {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let frame = commands.recv().await.expect("driver is alive");
            if frame.kind == kind {
                return frame;
            }
            assert_eq!(
                frame.kind,
                WireKind::TerminalBaseColors,
                "unexpected command while waiting for {kind:?}"
            );
        }
    })
    .await
    .expect("command must progress")
}

async fn frame_where(
    terminal: &NativeTerminal,
    predicate: impl Fn(&NativeFrame) -> bool,
) -> NativeFrame {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut frame = terminal.current_frame();
        loop {
            if predicate(&frame) {
                return frame;
            }
            assert!(
                !matches!(frame.state.as_str(), "closed" | "lease_lost" | "ended"),
                "terminal unexpectedly retired"
            );
            frame = terminal
                .wait_for_frame(frame.generation)
                .await
                .expect("next native frame");
        }
    })
    .await
    .expect("Android synchronization must progress")
}

async fn start() -> (
    Arc<NativeTerminal>,
    mpsc::Sender<DecodedFrame>,
    mpsc::Receiver<DecodedFrame>,
    tokio_util::task::AbortOnDropHandle<()>,
) {
    let (incoming, reader) = mpsc::channel(64);
    let (writer, mut outgoing) = mpsc::channel(64);
    let connector = Arc::new(Connector(Mutex::new(Some(Wire {
        incoming: reader,
        outgoing: writer,
    }))));
    let connect = tokio::spawn({
        let connector = connector.clone();
        async move {
            SessionClient::connect(
                connector,
                ResolvedSessionTarget::device(DeviceId::from_array([0x95; 32])),
                Some(SessionSelector::Id(SESSION)),
                false,
                false,
                Some(TerminalSize::new(48, 65)),
                base_colors(true),
            )
            .await
            .expect("connect fixture")
        }
    });
    let request = next_command(&mut outgoing, WireKind::TerminalAttachRequest).await;
    incoming
        .send(snapshot_frame(
            1,
            TerminalSize::new(48, 65),
            request.request_id,
        ))
        .await
        .expect("initial snapshot");
    let prepared = PreparedTerminalView::new(
        connect.await.expect("connect task"),
        false,
        TerminalViewTarget::for_display("fixture", TerminalViewRoute::Remote),
    )
    .expect("prepared view");
    let surface =
        AttachmentSurface::from_snapshot(prepared.initial_snapshot()).expect("initial surface");
    let (frame, _) = watch::channel(project(
        &surface,
        1,
        "synchronizing",
        None,
        true,
        &surface.surface.rows,
    ));
    let (reader, writer) = prepared
        .acknowledge_initial()
        .await
        .expect("initial ACK")
        .split();
    next_command(&mut outgoing, WireKind::TerminalSnapshotApplied).await;
    let (commands, receiver) = mpsc::channel(32);
    let cancel = CancellationToken::new();
    let upload_input_generation = Arc::new(AtomicU64::new(0));
    let notifications = Arc::new(NotificationBridge::new());
    let terminal = Arc::new(NativeTerminal {
        session_id: SESSION,
        commands,
        frame: frame.clone(),
        cancel: cancel.clone(),
        upload_input_generation: upload_input_generation.clone(),
        notifications: notifications.clone(),
    });
    let task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(run(
        SESSION,
        surface,
        reader,
        writer,
        receiver,
        frame,
        cancel,
        true,
        connector,
        upload_input_generation,
        notifications,
    )));
    frame_where(&terminal, |frame| frame.state == "active").await;
    (terminal, incoming, outgoing, task)
}

#[tokio::test]
async fn queued_snapshots_and_resize_intent_converge_in_android_reducer() {
    let (terminal, incoming, mut outgoing, _task) = start().await;
    let initial_epoch = terminal.current_frame().input_epoch;
    terminal
        .resize(NativeViewport {
            rows: 31,
            columns: 101,
        })
        .await
        .expect("first resize");
    next_command(&mut outgoing, WireKind::TerminalResize).await;
    let waiting = terminal.current_frame();
    assert_eq!(waiting.state, "synchronizing");
    assert!(waiting.input_ready);
    // While delivery is suspended, newer intent must be coalesced. Adjacent
    // snapshots model delayed/repeated resync replies already on the stream.
    terminal
        .resize(NativeViewport {
            rows: 32,
            columns: 102,
        })
        .await
        .expect("coalesced resize");
    for revision in 2..=18 {
        incoming
            .send(snapshot_frame(revision, TerminalSize::new(31, 101), 0))
            .await
            .expect("queued snapshot");
    }
    let resize = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let frame = outgoing.recv().await.expect("driver commands");
            if frame.kind == WireKind::TerminalResize {
                break frame;
            }
        }
    })
    .await
    .expect("latest resize leaves synchronization");
    let resize: v2::TerminalResize = resize
        .decode_message(WireKind::TerminalResize)
        .expect("resize");
    assert_eq!((resize.rows, resize.columns), (32, 102));
    incoming
        .send(snapshot_frame(19, TerminalSize::new(32, 102), 0))
        .await
        .expect("latest viewport");
    let frame = frame_where(&terminal, |frame| {
        frame.state == "active" && frame.viewport.rows == 32 && frame.viewport.columns == 102
    })
    .await;
    assert_eq!(frame.input_epoch, initial_epoch);
    assert!(frame.input_ready);
    terminal
        .commit_text(initial_epoch, "after-sync".into(), 0, false)
        .await
        .expect("original controller input");
    let input = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let frame = outgoing.recv().await.expect("driver commands");
            if frame.kind == WireKind::TerminalInput {
                break frame;
            }
        }
    })
    .await
    .expect("input reaches original stream");
    let input: v2::TerminalInput = input
        .decode_message(WireKind::TerminalInput)
        .expect("input");
    assert_eq!(input.bytes, b"after-sync");
    assert_eq!(terminal.session_id, SESSION);
}
