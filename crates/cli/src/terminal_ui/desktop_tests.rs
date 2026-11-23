use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::task::Poll;

use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::pty::openpty;
use nix::sys::signal::{Signal as NixSignal, kill};
#[cfg(unix)]
use nix::sys::termios::tcgetattr;
#[cfg(target_os = "macos")]
#[cfg(unix)]
use nix::sys::termios::{LocalFlags, cfgetispeed, cfgetospeed};
use nix::unistd::Pid;

use super::*;
use super::{input_codec::find_bytes, viewport::ViewportState};

#[path = "startup_tests.rs"]
mod startup_tests;

#[derive(Default)]
struct ViewportFrameWriter {
    bytes: Vec<u8>,
    frames: Vec<Vec<u8>>,
    writes: usize,
    flushes: usize,
}

impl Write for ViewportFrameWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        self.bytes.extend_from_slice(bytes);
        self.frames.push(bytes.to_vec());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn stdin_queue_is_exactly_bounded_and_recovers_after_one_receive() {
    let pty = openpty(None, None).expect("open stdin-capacity PTY");
    let input_epoch = InputEpoch::new();
    let mut stdin_pump = StdinPump::start(&pty.slave, input_epoch.clone())
        .expect("start capacity-controlled stdin pump");

    for index in 0..STDIN_CHANNEL_CAPACITY {
        let byte = b'a' + u8::try_from(index).expect("stdin fixture index fits u8");
        assert!(
            stdin_pump
                .sender_for_test
                .try_send(StdinEvent::Bytes {
                    geometry: 0,
                    epoch: input_epoch.current(),
                    bytes: vec![byte],
                })
                .is_ok(),
            "every production stdin queue slot is admitted",
        );
    }
    assert_eq!(
        stdin_pump
            .receiver
            .as_ref()
            .expect("live stdin receiver")
            .len(),
        STDIN_CHANNEL_CAPACITY,
    );
    let Err(tokio::sync::mpsc::error::TrySendError::Full(backpressured)) =
        stdin_pump.sender_for_test.try_send(StdinEvent::Bytes {
            geometry: 0,
            epoch: input_epoch.current(),
            bytes: b"i".to_vec(),
        })
    else {
        panic!("the next stdin item must be backpressured at the exact bound");
    };
    assert!(matches!(
        backpressured,
        StdinEvent::Bytes { bytes, .. } if bytes == b"i"
    ));

    assert!(matches!(
        stdin_pump.recv().await,
        Some(StdinEvent::Bytes { bytes, .. }) if bytes == b"a"
    ));
    assert!(
        stdin_pump
            .sender_for_test
            .try_send(StdinEvent::Bytes {
                geometry: 0,
                epoch: input_epoch.current(),
                bytes: b"i".to_vec(),
            })
            .is_ok(),
        "receiving one item recovers one stdin queue slot",
    );
    assert_eq!(
        stdin_pump
            .receiver
            .as_ref()
            .expect("live recovered stdin receiver")
            .len(),
        STDIN_CHANNEL_CAPACITY,
    );
    stdin_pump
        .shutdown()
        .expect("shutdown capacity-controlled stdin pump");
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_active_fence_joins_live_reader_and_delivers_only_fresh_input() {
    let pty = openpty(None, None).expect("open input-fence PTY");
    let original = tcgetattr(&pty.slave).expect("input-fence attributes");
    let mut raw = original.clone();
    cfmakeraw(&mut raw);
    tcsetattr(&pty.slave, SetArg::TCSANOW, &raw).expect("input-fence raw mode");

    let input_epoch = InputEpoch::new();
    let mut current_epoch = input_epoch.current();
    let mut prefix = CommandMode::new();
    let (reader_test_seam, controls) = StdinReaderTestSeam::with_gates(4);
    let mut controls = VecDeque::from(controls);
    let mut stdin_pump = Some(
        StdinPump::start_with_test_seam(&pty.slave, input_epoch.clone(), reader_test_seam)
            .expect("start controlled stdin pump"),
    );

    for cycle in 0..2_u8 {
        let stale = vec![b's', b't', b'a', b'l', b'e', b'0' + cycle];
        rustix::io::write(&pty.master, &stale).expect("queue unsynchronized input");
        let stale_control = controls.pop_front().expect("stale-read gate");
        stale_control
            .observed
            .recv_timeout(Duration::from_secs(2))
            .expect("reader reached the deterministic poll/read seam");

        assert!(
            prefix
                .feed(b"\x1d", Instant::now(), false)
                .expect("prefix input")
                .is_empty()
        );
        let stale_epoch = current_epoch;
        let old_pump = stdin_pump.take().expect("live stdin pump");
        let cancellation_probe = duplicate_cloexec(
            &old_pump._cancellation_read_guard,
            "duplicate stdin-fence cancellation probe",
        )
        .expect("cancellation probe");
        let fence_input = duplicate_cloexec(&pty.slave, "duplicate input-fence PTY")
            .expect("input-fence PTY duplicate");
        let thread_epoch = input_epoch.clone();
        let (result_sender, result_receiver) = sync_channel(1);
        let mut retained = vec![b'k', b'e', b'y', b'0' + cycle];
        let mut viewport = ViewportController {
            state: ViewportState::ResumePending {
                retained_input: retained.clone(),
                snapshot_applied: true,
                presented_scroll_metrics: None,
            },
            content_size: TerminalSize::new(2, 20),
            live_metrics: None,
            gutter_column: None,
            drag_grab_row: None,
            drag_last_request: None,
            drag_deferred_target: None,
            last_presented_scroll_metrics: None,
            presented_gutter_column: None,
            discard_window_response: false,
            window_cache: ViewportCache::new(),
        };
        if cycle == 0 {
            let mut codec = HostInputCodec::new();
            assert!(
                codec
                    .feed(b"\x1b[20")
                    .expect("split paste prefix remains bounded")
                    .is_empty()
            );
            assert!(
                codec
                    .feed(b"0~paste-head\x1d.")
                    .expect("bounded partial paste")
                    .is_empty()
            );
            assert!(
                should_defer_active_for_paste(
                    TerminalViewTransportState::Active,
                    &viewport,
                    &codec,
                ),
                "an Active event after Snapshot must wait for the unread paste tail"
            );
            assert!(
                codec
                    .feed(b"-tail\x1b[201")
                    .expect("split paste suffix remains bounded")
                    .is_empty()
            );
            let events = codec
                .feed(b"~")
                .expect("the final delimiter completes one paste unit");
            let [HostInputEvent::Paste(paste)] = events.as_slice() else {
                panic!("split bracketed paste must produce exactly one Paste event");
            };
            assert!(!codec.paste_in_progress());
            viewport
                .retain_resume_input(paste)
                .expect("whole paste fits the resume-input bound");
            retained.extend_from_slice(paste);
        }
        let fence = std::thread::spawn(move || {
            let mut pump = old_pump;
            let mut epoch = stale_epoch;
            let mut parser = prefix;
            let result = transition_transport_input_state(
                &fence_input,
                &thread_epoch,
                &mut epoch,
                &mut pump,
                &mut parser,
                TerminalViewTransportState::Synchronizing,
                TerminalViewTransportState::Active,
                &mut viewport,
            );
            let _ = result_sender.send((result, pump, epoch, parser, viewport));
        });

        let bound = rustix::event::Timespec {
            tv_sec: 2,
            tv_nsec: 0,
        };
        let mut cancellation = [PollFd::new(&cancellation_probe, PollFlags::IN)];
        assert_eq!(
            poll(&mut cancellation, Some(&bound)).expect("observe old-pump cancellation"),
            1,
            "the Active fence must wake the old reader before joining it"
        );
        assert!(
            matches!(
                result_receiver.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ),
            "retained resume input must not become sendable before the old reader joins"
        );
        stale_control
            .release
            .send(())
            .expect("release the old reader after cancellation is visible");
        let (result, returned_pump, returned_epoch, returned_prefix, viewport) = result_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("Active fence completed within its bound");
        fence.join().expect("Active fence thread");
        assert_eq!(
            result.expect("replace stdin pump after Active fence"),
            Some(retained)
        );
        assert!(viewport.is_live());
        stdin_pump = Some(returned_pump);
        current_epoch = returned_epoch;
        prefix = returned_prefix;

        assert!(!input_epoch_is_current(stale_epoch, current_epoch));
        assert!(input_epoch_is_current(input_epoch.current(), current_epoch));
        assert_eq!(prefix.deadline(), None);

        let fresh = vec![b'f', b'r', b'e', b's', b'h', b'0' + cycle];
        rustix::io::write(&pty.master, &fresh).expect("write post-fence input");
        let fresh_control = controls.pop_front().expect("fresh-read gate");
        fresh_control
            .observed
            .recv_timeout(Duration::from_secs(2))
            .expect("replacement reader observed post-fence input");
        fresh_control
            .release
            .send(())
            .expect("release replacement reader");
        let mut codec = HostInputCodec::new();
        loop {
            let event = tokio::time::timeout(
                Duration::from_secs(2),
                stdin_pump.as_mut().expect("replacement pump").recv(),
            )
            .await
            .expect("bounded delivery")
            .expect("live pump");
            let StdinEvent::Bytes { epoch, bytes, .. } = event else {
                panic!("expected input");
            };
            let decoded = codec
                .feed_for_epoch(&bytes, epoch, current_epoch)
                .expect("decode epoch");
            if epoch != current_epoch {
                assert!(
                    decoded.is_empty(),
                    "stale keys are discarded after preserving reply framing"
                );
                continue;
            }
            assert_eq!(decoded, vec![HostInputEvent::Bytes(fresh.clone())]);
            break;
        }
    }

    stdin_pump
        .as_mut()
        .expect("final replacement pump")
        .shutdown()
        .expect("shutdown final replacement pump");
    tcsetattr(&pty.slave, SetArg::TCSANOW, &original).expect("restore input-fence attributes");
}
fn test_row(columns: u16, text: &str, style: TerminalStyle) -> TerminalSurfaceRow {
    let mut cells = vec![TerminalCell::default(); usize::from(columns)];
    if let Some(first) = cells.first_mut() {
        first.contents = text.to_owned();
        first.style = style;
    }
    TerminalSurfaceRow {
        cells,
        wrapped: false,
    }
}

fn test_snapshot(
    size: TerminalSize,
    active_screen: ActiveScreen,
    revision: Revision,
) -> TerminalSurfaceSnapshot {
    let rows = (0..size.rows)
        .map(|row| test_row(size.columns, &row.to_string(), TerminalStyle::default()))
        .collect();
    let scroll_metrics = (active_screen == ActiveScreen::Main).then_some(TerminalScrollMetrics {
        epoch: Revision::new(1),
        revision,
        offset_from_bottom: 0,
        max_offset_from_bottom: 10,
        viewport_rows: size.rows,
    });
    TerminalSurfaceSnapshot {
        revision,
        surface: TerminalSurface {
            application_title: String::new(),
            colors: Default::default(),

            size,
            active_screen,
            rows,
            cursor: TerminalCursor {
                presentation: Default::default(),
                row: 0,
                column: 0,
                visible: true,
                style: TerminalStyle::default(),
            },
            modes: TerminalModes::default(),
            scroll_metrics,
        },
    }
}

fn install_test_history_window(viewport: &mut ViewportController) {
    let ViewportEffect::RequestHistoryWindow(query) = viewport.navigate(true, 1) else {
        panic!("test history navigation must request a complete window");
    };
    let shape = query
        .response_shape(query.anchor)
        .expect("valid test query");
    let rows = (0..shape.row_count)
        .map(|index| {
            test_row(
                query.anchor.viewport.columns,
                &format!("history-{index}"),
                TerminalStyle::default(),
            )
        })
        .collect();
    let effect = viewport
        .apply_view_history_window(TerminalSurfaceHistoryWindowResult::Frame(
            TerminalSurfaceHistoryWindowFrame {
                colors: Default::default(),

                disposition: shape.disposition,
                anchor: query.anchor,
                target_offset_from_bottom: shape.target_offset_from_bottom,
                first_row_from_live_top: shape.first_row_from_live_top,
                rows,
            },
        ))
        .expect("install complete test history window");
    assert!(matches!(
        effect,
        ViewportEffect::Render | ViewportEffect::RenderAndRequestHistoryWindow(_)
    ));
}

#[test]
fn resume_snapshot_presents_all_live_rows_before_activation_or_more_input() {
    for route in [TerminalViewRoute::Local, TerminalViewRoute::Remote] {
        for new_output in [false, true] {
            let physical = TerminalSize::new(5, 24);
            let layout = ChromeLayout::new(physical, ActiveScreen::Main);
            let mut snapshot = test_snapshot(layout.child, ActiveScreen::Main, Revision::new(2));
            for (index, row) in snapshot.surface.rows.iter_mut().enumerate() {
                *row = test_row(
                    layout.child.columns,
                    &format!("live-{index}"),
                    TerminalStyle::default(),
                );
            }
            let mut surface = AttachmentSurface::from_snapshot(&snapshot)
                .expect("resume presentation fixture succeeds");
            let mut viewport =
                ViewportController::with_layout(layout, snapshot.surface.scroll_metrics);
            let status =
                StatusRenderer::new(TerminalViewTarget::for_display("peer", route), physical);
            let mut presenter = DesktopPresenter::default();
            let mut output = ViewportFrameWriter::default();
            install_test_history_window(&mut viewport);
            let _ = viewport.navigate(true, 1);
            present_surface_with_writer(
                &mut output,
                &surface,
                &mut presenter,
                &viewport,
                &status,
                TerminalViewTransportState::Active,
            )
            .expect("resume presentation fixture succeeds");
            viewport.observe_presentation();
            let history = presenter
                .baseline
                .clone()
                .expect("resume presentation fixture succeeds");
            assert!(matches!(
                viewport.navigate(false, 2),
                ViewportEffect::Resume
            ));
            viewport
                .retain_resume_input(b"\x1b[200~retained paste\x1b[201~")
                .expect("resume presentation fixture succeeds");
            viewport.observe_sync_required();
            assert!(
                !present_transport_transition_with_writer(
                    &mut output,
                    &surface,
                    &mut presenter,
                    &viewport,
                    &status,
                    TerminalViewTransportState::Synchronizing
                )
                .expect("resume presentation fixture succeeds")
            );
            assert_eq!(presenter.baseline.as_ref(), Some(&history));
            if new_output {
                snapshot.revision = Revision::new(3);
                snapshot
                    .surface
                    .scroll_metrics
                    .as_mut()
                    .expect("resume presentation fixture succeeds")
                    .revision = snapshot.revision;
                for row in 2..4 {
                    snapshot.surface.rows[row] = test_row(
                        layout.child.columns,
                        &format!("new-tail-{row}"),
                        TerminalStyle::default(),
                    );
                }
            }
            install_snapshot_with_writer(
                &mut output,
                &mut surface,
                &mut presenter,
                &snapshot,
                &mut viewport,
                &status,
                TerminalViewTransportState::Synchronizing,
            )
            .expect("resume presentation fixture succeeds");
            viewport.observe_presentation();
            assert!(
                viewport.is_resume_pending(),
                "painting live must not release input"
            );
            for row in 0..layout.child.rows {
                assert_eq!(
                    presenter
                        .baseline
                        .as_ref()
                        .expect("resume presentation fixture succeeds")
                        .rows[&row][..usize::from(layout.child.columns)],
                    snapshot.surface.rows[usize::from(row)].cells,
                    "route={route:?} new_output={new_output} row={row}"
                );
            }
            assert_eq!(
                viewport
                    .scroll_metrics()
                    .expect("resume presentation fixture succeeds")
                    .offset_from_bottom,
                0
            );
            assert!(
                !presenter
                    .baseline
                    .as_ref()
                    .expect("resume presentation fixture succeeds")
                    .cursor
                    .visible
            );
            assert_eq!(
                viewport.finish_resume(),
                Some(b"\x1b[200~retained paste\x1b[201~".to_vec())
            );
            assert_eq!(
                viewport.finish_resume(),
                None,
                "retained input is released once"
            );
            assert!(
                present_transport_transition_with_writer(
                    &mut output,
                    &surface,
                    &mut presenter,
                    &viewport,
                    &status,
                    TerminalViewTransportState::Active
                )
                .expect("resume presentation fixture succeeds"),
                "Active must restore the live cursor"
            );
            assert!(
                presenter
                    .baseline
                    .as_ref()
                    .expect("resume presentation fixture succeeds")
                    .cursor
                    .visible
            );
            let expected = ComposedFrame::compose(
                &surface.surface,
                presenter.baseline.as_ref(),
                &viewport,
                &status,
                TerminalViewTransportState::Active,
            )
            .expect("resume presentation fixture succeeds");
            assert_eq!(presenter.baseline.as_ref(), Some(&expected));
            let writes = output.writes;
            assert!(
                !present_transport_transition_with_writer(
                    &mut output,
                    &surface,
                    &mut presenter,
                    &viewport,
                    &status,
                    TerminalViewTransportState::Active
                )
                .expect("resume presentation fixture succeeds")
            );
            assert_eq!(output.writes, writes, "identical desired frames do no I/O");
        }
    }
}

#[test]
fn snapshot_flush_failure_keeps_resume_surface_layout_and_input_uncommitted() {
    struct FlushFailure;
    impl Write for FlushFailure {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("snapshot flush failure"))
        }
    }
    let physical = TerminalSize::new(5, 24);
    let layout = ChromeLayout::new(physical, ActiveScreen::Main);
    let snapshot = test_snapshot(layout.child, ActiveScreen::Main, Revision::new(2));
    let mut surface =
        AttachmentSurface::from_snapshot(&snapshot).expect("resume presentation fixture succeeds");
    let mut viewport = ViewportController::with_layout(layout, snapshot.surface.scroll_metrics);
    let status = StatusRenderer::new(
        TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
        physical,
    );
    let mut presenter = DesktopPresenter::default();
    let mut output = ViewportFrameWriter::default();
    install_test_history_window(&mut viewport);
    present_surface_with_writer(
        &mut output,
        &surface,
        &mut presenter,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("resume presentation fixture succeeds");
    viewport.observe_presentation();
    viewport
        .begin_resume(b"retained".to_vec())
        .expect("resume presentation fixture succeeds");
    let metrics = viewport.scroll_metrics();
    let anchor = viewport.window_cache.anchor();
    let replacement = test_snapshot(
        ChromeLayout::new(physical, ActiveScreen::Alternate).child,
        ActiveScreen::Alternate,
        Revision::new(3),
    );
    assert!(
        install_snapshot_with_writer(
            &mut FlushFailure,
            &mut surface,
            &mut presenter,
            &replacement,
            &mut viewport,
            &status,
            TerminalViewTransportState::Synchronizing
        )
        .is_err()
    );
    assert_eq!(surface.revision(), snapshot.revision);
    assert_eq!(viewport.content_size(), layout.child);
    assert_eq!(viewport.gutter_column, layout.gutter_column);
    assert_eq!(viewport.scroll_metrics(), metrics);
    assert_eq!(viewport.window_cache.anchor(), anchor);
    assert_eq!(viewport.finish_resume(), None);
    assert!(presenter.baseline.is_none());
    install_snapshot_with_writer(
        &mut output,
        &mut surface,
        &mut presenter,
        &replacement,
        &mut viewport,
        &status,
        TerminalViewTransportState::Synchronizing,
    )
    .expect("resume presentation fixture succeeds");
    assert_eq!(viewport.finish_resume(), Some(b"retained".to_vec()));
    assert_eq!(viewport.gutter_column, None);
}

#[test]
fn background_snapshot_preserves_pinned_history_cells() {
    let physical = TerminalSize::new(5, 24);
    let layout = ChromeLayout::new(physical, ActiveScreen::Main);
    let snapshot = test_snapshot(layout.child, ActiveScreen::Main, Revision::new(2));
    let mut surface =
        AttachmentSurface::from_snapshot(&snapshot).expect("resume presentation fixture succeeds");
    let mut viewport = ViewportController::with_layout(layout, snapshot.surface.scroll_metrics);
    let status = StatusRenderer::new(
        TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
        physical,
    );
    let mut presenter = DesktopPresenter::default();
    let mut output = ViewportFrameWriter::default();
    install_test_history_window(&mut viewport);
    present_surface_with_writer(
        &mut output,
        &surface,
        &mut presenter,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("resume presentation fixture succeeds");
    viewport.observe_presentation();
    let history = presenter
        .baseline
        .as_ref()
        .expect("resume presentation fixture succeeds")
        .rows
        .clone();
    viewport.observe_sync_required();
    let replacement = test_snapshot(layout.child, ActiveScreen::Main, Revision::new(3));
    install_snapshot_with_writer(
        &mut output,
        &mut surface,
        &mut presenter,
        &replacement,
        &mut viewport,
        &status,
        TerminalViewTransportState::Synchronizing,
    )
    .expect("resume presentation fixture succeeds");
    assert!(viewport.is_history());
    for row in 0..layout.child.rows {
        assert_eq!(
            presenter
                .baseline
                .as_ref()
                .expect("resume presentation fixture succeeds")
                .rows[&row],
            history[&row]
        );
    }
    assert_eq!(surface.revision(), replacement.revision);
}

fn finalize_test_selection(
    surface: &AttachmentSurface,
    viewport: &mut ViewportController,
    presenter: &mut DesktopPresenter,
    status: &StatusRenderer,
    output: &mut impl Write,
) -> SelectionController {
    let source = viewport
        .selection_source_identity(surface)
        .expect("test viewport has a presented selection source");
    let rows = viewport
        .selection_rows(surface)
        .expect("test viewport has presented semantic rows");
    let mut selection = SelectionController::default();
    selection
        .begin(source, TerminalTextPoint::new(0, 0), rows)
        .expect("begin test selection");
    selection
        .update(source, TerminalTextPoint::new(0, 1), rows)
        .expect("extend test selection");
    selection.finish();
    reconcile_presenter_selection(&mut selection, viewport, surface, presenter);
    present_surface_with_writer(
        output,
        surface,
        presenter,
        viewport,
        status,
        TerminalViewTransportState::Active,
    )
    .expect("present test selection");
    viewport.observe_presentation();
    assert!(selection.is_finalized());
    assert_eq!(presenter.presented_keyboard_flags().bits(), 7);
    selection
}

#[test]
fn palette_only_delta_recolors_live_and_pinned_history_without_losing_selection() {
    use voytty_core::terminal::{
        COLOR_BACKGROUND, COLOR_SELECTION_BACKGROUND, COLOR_SELECTION_FOREGROUND,
        TerminalColorValue,
    };
    for history in [false, true] {
        let physical = TerminalSize::new(5, 24);
        let layout = ChromeLayout::new(physical, ActiveScreen::Main);
        let snapshot = test_snapshot(layout.child, ActiveScreen::Main, Revision::new(2));
        let mut surface = AttachmentSurface::from_snapshot(&snapshot).expect("color baseline");
        let mut viewport = ViewportController::with_layout(layout, snapshot.surface.scroll_metrics);
        if history {
            install_test_history_window(&mut viewport);
        }
        let status = StatusRenderer::new(
            TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
            physical,
        );
        let mut presenter = DesktopPresenter::default();
        let mut output = ViewportFrameWriter::default();
        present_surface_with_writer(
            &mut output,
            &surface,
            &mut presenter,
            &viewport,
            &status,
            TerminalViewTransportState::Active,
        )
        .expect("initial frame");
        viewport.observe_presentation();
        let mut selection = finalize_test_selection(
            &surface,
            &mut viewport,
            &mut presenter,
            &status,
            &mut output,
        );
        let semantic = presenter
            .semantic_baseline
            .as_ref()
            .expect("semantic baseline")
            .rows
            .clone();
        let offset = viewport.scroll_metrics().map(|m| m.offset_from_bottom);
        let mut colors = snapshot.surface.colors.clone();
        colors.changed_at = Revision::new(3);
        colors.profile.values[COLOR_BACKGROUND] = TerminalColorValue::Rgb(4, 5, 6);
        colors.profile.values[COLOR_SELECTION_FOREGROUND] = TerminalColorValue::Rgb(11, 12, 13);
        colors.profile.values[COLOR_SELECTION_BACKGROUND] = TerminalColorValue::Rgb(14, 15, 16);
        let delta = TerminalSurfaceDelta {
            application_title: String::new(),
            colors,
            from_revision: Revision::new(2),
            to_revision: Revision::new(3),
            size: layout.child,
            active_screen: ActiveScreen::Main,
            row_patches: Vec::new(),
            cursor: snapshot.surface.cursor,
            modes: snapshot.surface.modes,
            scroll_metrics: snapshot
                .surface
                .scroll_metrics
                .map(|m| TerminalScrollMetrics {
                    revision: Revision::new(3),
                    ..m
                }),
        };
        output = ViewportFrameWriter::default();
        assert!(matches!(
            apply_delta_with_writer(
                &mut output,
                &mut surface,
                &mut presenter,
                &delta,
                &mut viewport,
                &mut selection,
                &status,
                TerminalViewTransportState::Active,
                false
            )
            .expect("color delta"),
            DeltaRender::Applied
        ));
        assert!(selection.is_finalized());
        assert_eq!(viewport.is_history(), history);
        assert_eq!(
            viewport.scroll_metrics().map(|m| m.offset_from_bottom),
            offset
        );
        assert_eq!(
            presenter
                .semantic_baseline
                .as_ref()
                .expect("semantic colors")
                .rows,
            semantic
        );
        let physical = presenter.baseline.as_ref().expect("painted colors");
        assert_eq!(
            physical.rows[&0][0].style.foreground,
            TerminalColor::Rgb(11, 12, 13)
        );
        assert_eq!(
            physical.rows[&0][2].style.background,
            TerminalColor::Rgb(4, 5, 6)
        );
        assert_eq!((output.writes, output.flushes), (1, 1));
    }
}
#[test]
fn resize_coalescer_retains_only_the_latest_non_active_viewport() {
    let initial = TerminalSize::new(24, 80);
    let latest = TerminalSize::new(40, 120);
    let mut coalescer = ResizeCoalescer::new(initial);
    assert_eq!(
        coalescer.observe(
            TerminalSize::new(20, 60),
            TerminalViewTransportState::Synchronizing,
        ),
        None
    );
    assert_eq!(
        coalescer.observe(latest, TerminalViewTransportState::Reconnecting),
        None
    );
    assert_eq!(
        coalescer.enter_transport_state(TerminalViewTransportState::Active),
        (TerminalViewTransportState::Synchronizing, Some(latest))
    );
    assert_eq!(
        coalescer.enter_transport_state(TerminalViewTransportState::Active),
        (TerminalViewTransportState::Active, None)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn inactive_waits_retain_physical_resizes_and_discard_startup_input() {
    // Count real terminal-size reads so each SIGWINCH is consumed
    // before the next resize, without depending on a timing delay.
    struct ResizeProbe<'a> {
        terminal: &'a std::os::fd::OwnedFd,
        reads: Arc<AtomicUsize>,
    }
    impl AsFd for ResizeProbe<'_> {
        fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.terminal.as_fd()
        }
    }

    for preserve_submitted_result in [true, false] {
        let pty = openpty(None, None).expect("inactive resize PTY");
        let reads = Arc::new(AtomicUsize::new(0));
        let stdout = ResizeProbe {
            terminal: &pty.slave,
            reads: Arc::clone(&reads),
        };
        let mut resize_signal = signal(SignalKind::window_change()).expect("SIGWINCH");
        let (_cancel_sender, mut cancellation_receiver) = watch::channel(None);
        let input_epoch = InputEpoch::new();
        let mut stdin_pump =
            StdinPump::start(&pty.slave, input_epoch.clone()).expect("inactive stdin pump");
        let input_sender = stdin_pump.sender_for_test.clone();
        let mut prefix = CommandMode::new();
        let mut physical_size = TerminalSize::new(40, 140);
        let (release, ready) = tokio::sync::oneshot::channel();
        let mut input_codec = HostInputCodec::new();
        let mut host_colors = HostColors::default();
        let mut presenter = DesktopPresenter::default();
        let mut output = Vec::new();
        let wait = await_while_inactive(
            async {
                ready.await.expect("release initial operation");
                Ok(())
            },
            InactiveWaitContext {
                presentation: InactivePresentation::None,
                input_codec: &mut input_codec,
                host_colors: &mut host_colors,
                presenter: &mut presenter,
                stdout: &stdout,
                resize_signal: &mut resize_signal,
                cancellation_receiver: &mut cancellation_receiver,
                stdin_pump: &mut stdin_pump,
                prefix: &mut prefix,
                physical_size: &mut physical_size,
                current_input_epoch: input_epoch.current(),
                preserve_submitted_result,
                report_key_events: false,
            },
            &mut output,
        );
        let resize = async {
            for (index, (rows, columns)) in [(50, 160), (42, 152)].into_iter().enumerate() {
                rustix::termios::tcsetwinsize(
                    &pty.master,
                    rustix::termios::Winsize {
                        ws_row: rows,
                        ws_col: columns,
                        ws_xpixel: 0,
                        ws_ypixel: 0,
                    },
                )
                .expect("resize inactive outer terminal");
                kill(Pid::this(), NixSignal::SIGWINCH).expect("notify inactive resize");
                input_sender
                    .send(StdinEvent::Bytes {
                        geometry: 0,
                        epoch: input_epoch.current(),
                        bytes: b"startup input".to_vec(),
                    })
                    .await
                    .expect("queue inactive input");
                while reads.load(Ordering::SeqCst) <= index
                    || input_sender.capacity() != STDIN_CHANNEL_CAPACITY
                {
                    tokio::task::yield_now().await;
                }
            }
            release.send(()).expect("complete initial operation");
        };
        let (result, ()) =
            tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(wait, resize) })
                .await
                .expect("inactive resize/input progress");
        assert!(matches!(
            result.expect("inactive wait"),
            InactiveWait::Ready(())
        ));
        assert_eq!(physical_size, TerminalSize::new(42, 152));
        assert!(
            stdin_pump
                .receiver
                .as_mut()
                .expect("live receiver")
                .try_recv()
                .is_err()
        );
        stdin_pump.shutdown().expect("stop inactive stdin pump");
    }
}

#[test]
fn attachment_surface_validates_snapshots_and_applies_deltas_transactionally() {
    let size = TerminalSize::new(2, 4);
    let snapshot = test_snapshot(size, ActiveScreen::Main, Revision::new(4));
    let retained = AttachmentSurface::from_snapshot(&snapshot).expect("valid snapshot");
    let styled = TerminalStyle {
        foreground: TerminalColor::Rgb(7, 8, 9),
        bold: true,
        ..TerminalStyle::default()
    };
    let delta = TerminalSurfaceDelta {
        application_title: String::new(),
        colors: Default::default(),

        from_revision: Revision::new(4),
        to_revision: Revision::new(5),
        size,
        active_screen: ActiveScreen::Main,
        row_patches: vec![TerminalSurfaceRowPatch {
            row: 1,
            replacement: test_row(size.columns, "changed", styled),
        }],
        cursor: TerminalCursor {
            presentation: Default::default(),
            row: 1,
            column: 3,
            visible: true,
            style: styled,
        },
        modes: TerminalModes {
            bracketed_paste: true,
            ..TerminalModes::default()
        },
        scroll_metrics: Some(TerminalScrollMetrics {
            epoch: Revision::new(1),
            revision: Revision::new(5),
            offset_from_bottom: 0,
            max_offset_from_bottom: 10,
            viewport_rows: size.rows,
        }),
    };
    let candidate = retained
        .candidate_after_delta(&delta)
        .expect("valid delta")
        .expect("contiguous delta");
    assert_eq!(retained.revision(), Revision::new(4));
    assert_eq!(candidate.revision(), Revision::new(5));
    assert_eq!(candidate.surface.rows[1].cells[0].contents, "changed");

    let gap = TerminalSurfaceDelta {
        application_title: String::new(),
        from_revision: Revision::new(3),
        ..delta.clone()
    };
    assert_eq!(
        retained
            .candidate_after_delta(&gap)
            .expect("gap is not malformed"),
        None
    );

    let malformed = TerminalSurfaceDelta {
        row_patches: vec![TerminalSurfaceRowPatch {
            row: 1,
            replacement: TerminalSurfaceRow {
                cells: vec![TerminalCell::default(); 3],
                wrapped: false,
            },
        }],
        ..delta
    };
    assert!(
        retained.candidate_after_delta(&malformed).is_err(),
        "a malformed patch must not promote a partial surface"
    );
    assert_eq!(
        retained,
        AttachmentSurface::from_snapshot(&snapshot).expect("original snapshot is valid")
    );
}

#[test]
fn semantic_history_uses_one_cache_and_moves_one_line_locally() {
    let physical = TerminalSize::new(4, 10);
    let layout = ChromeLayout::new(physical, ActiveScreen::Main);
    let live = TerminalScrollMetrics {
        epoch: Revision::new(2),
        revision: Revision::new(7),
        offset_from_bottom: 0,
        max_offset_from_bottom: 10,
        viewport_rows: layout.child.rows,
    };
    let mut viewport = ViewportController::with_layout(layout, Some(live));
    let ViewportEffect::RequestHistoryWindow(query) = viewport.navigate(true, 1) else {
        panic!("the first history miss must issue one semantic window request");
    };
    let shape = query.response_shape(query.anchor).expect("valid query");
    let rows = (0..shape.row_count)
        .map(|index| {
            test_row(
                query.anchor.viewport.columns,
                &index.to_string(),
                TerminalStyle::default(),
            )
        })
        .collect();
    let effect = viewport
        .apply_view_history_window(TerminalSurfaceHistoryWindowResult::Frame(
            TerminalSurfaceHistoryWindowFrame {
                colors: Default::default(),

                disposition: shape.disposition,
                anchor: query.anchor,
                target_offset_from_bottom: shape.target_offset_from_bottom,
                first_row_from_live_top: shape.first_row_from_live_top,
                rows,
            },
        ))
        .expect("install semantic window");
    assert!(matches!(
        effect,
        ViewportEffect::Render | ViewportEffect::RenderAndRequestHistoryWindow(_)
    ));
    assert_eq!(
        viewport
            .visible_semantic_history_rows()
            .expect("complete cached viewport")
            .0
            .len(),
        usize::from(layout.child.rows)
    );
    assert_eq!(viewport.window_cache.desired_offset_from_bottom(), 1);

    let effect = viewport.navigate(true, 1);
    assert!(matches!(
        effect,
        ViewportEffect::Render
            | ViewportEffect::RenderAndRequestHistoryWindow(_)
            | ViewportEffect::RequestHistoryWindow(_)
    ));
    assert_eq!(viewport.window_cache.desired_offset_from_bottom(), 2);
    assert!(matches!(
        viewport.navigate(false, 2),
        ViewportEffect::Resume
    ));
}

#[test]
fn resumed_history_waits_for_old_response_and_keeps_latest_scroll_target() {
    for drag in [false, true] {
        let layout = ChromeLayout::new(TerminalSize::new(4, 10), ActiveScreen::Main);
        let live = TerminalScrollMetrics {
            epoch: Revision::new(2),
            revision: Revision::new(7),
            offset_from_bottom: 0,
            max_offset_from_bottom: 10,
            viewport_rows: layout.child.rows,
        };
        let mut viewport = ViewportController::with_layout(layout, Some(live));
        let first = viewport
            .prefetch_live()
            .expect("first request is in flight");
        viewport.observe_sync_required();
        viewport.observe_snapshot(Some(TerminalScrollMetrics {
            revision: Revision::new(8),
            ..live
        }));
        assert_eq!(viewport.finish_resume(), Some(Vec::new()));
        assert!(
            viewport.prefetch_live().is_none(),
            "old reply still owns admission"
        );
        for target in 1..=3 {
            let effect = if drag {
                viewport.scroll_to_offset(target)
            } else {
                viewport.navigate(true, 1)
            };
            assert!(matches!(effect, ViewportEffect::None));
            assert_eq!(viewport.window_cache.desired_offset_from_bottom(), target);
        }
        let ViewportEffect::RequestHistoryWindow(next) = viewport
            .apply_view_history_window(TerminalSurfaceHistoryWindowResult::HistoryGap {
                epoch: first.anchor.epoch,
                revision: first.anchor.revision,
            })
            .expect("late obsolete response is discarded")
        else {
            panic!("the latest target must be requested after the old reply");
        };
        assert_eq!(next.target_offset_from_bottom, 3);
        assert_eq!(next.anchor.revision, Revision::new(8));
        assert!(
            viewport.refetch_history_window().is_none(),
            "only one refill"
        );
        let shape = next.response_shape(next.anchor).expect("valid query");
        let effect = viewport
            .apply_view_history_window(TerminalSurfaceHistoryWindowResult::Frame(
                TerminalSurfaceHistoryWindowFrame {
                    colors: Default::default(),
                    disposition: shape.disposition,
                    anchor: next.anchor,
                    target_offset_from_bottom: shape.target_offset_from_bottom,
                    first_row_from_live_top: shape.first_row_from_live_top,
                    rows: (0..shape.row_count)
                        .map(|_| {
                            test_row(
                                next.anchor.viewport.columns,
                                "history",
                                TerminalStyle::default(),
                            )
                        })
                        .collect(),
                },
            ))
            .expect("current response is correlated and usable");
        assert!(matches!(effect, ViewportEffect::Render));
        assert!(viewport.has_complete_cached_viewport());
        assert!(matches!(
            viewport.begin_resume(b"x".to_vec()).expect("input resumes"),
            ViewportEffect::Resume
        ));
        viewport.observe_snapshot(Some(live));
        assert_eq!(viewport.finish_resume(), Some(b"x".to_vec()));
    }
}

#[test]
fn obsolete_history_response_during_repeated_resume_waits_for_active() {
    let layout = ChromeLayout::new(TerminalSize::new(4, 10), ActiveScreen::Main);
    let live = TerminalScrollMetrics {
        epoch: Revision::new(2),
        revision: Revision::new(7),
        offset_from_bottom: 0,
        max_offset_from_bottom: 10,
        viewport_rows: layout.child.rows,
    };
    let mut viewport = ViewportController::with_layout(layout, Some(live));
    viewport
        .prefetch_live()
        .expect("first request is in flight");
    viewport.observe_sync_required();
    viewport.start_resume(Vec::new());
    assert!(
        viewport.discard_window_response,
        "second invalidation retains correlation"
    );
    assert!(matches!(
        viewport
            .apply_view_history_window(TerminalSurfaceHistoryWindowResult::HistoryGap {
                epoch: live.epoch,
                revision: live.revision,
            })
            .expect("obsolete response is still expected"),
        ViewportEffect::None
    ));
    assert!(
        viewport.prefetch_live().is_none(),
        "resume has no live anchor yet"
    );
    viewport.observe_snapshot(Some(live));
    assert_eq!(viewport.finish_resume(), Some(Vec::new()));
    assert!(
        viewport.prefetch_live().is_some(),
        "one refill after Active"
    );
    assert!(
        viewport.prefetch_live().is_none(),
        "refill is not duplicated"
    );
}

#[test]
fn live_prefetch_after_sync_resume_discards_the_overlapped_response() {
    let physical = TerminalSize::new(4, 10);
    let layout = ChromeLayout::new(physical, ActiveScreen::Main);
    let live = TerminalScrollMetrics {
        epoch: Revision::new(2),
        revision: Revision::new(7),
        offset_from_bottom: 0,
        max_offset_from_bottom: 10,
        viewport_rows: layout.child.rows,
    };
    let mut viewport = ViewportController::with_layout(layout, Some(live));
    let first = viewport
        .prefetch_live()
        .expect("live history issues one prefetch");
    assert!(
        viewport.prefetch_live().is_none(),
        "one prefetch is pending"
    );

    // A sync marker and its replacement snapshot arrive while the
    // first response is still in flight.
    viewport.observe_sync_required();
    viewport.observe_snapshot(Some(TerminalScrollMetrics {
        revision: Revision::new(8),
        ..live
    }));
    assert_eq!(viewport.finish_resume(), Some(Vec::new()));
    assert!(
        viewport.prefetch_live().is_none(),
        "the resumed view waits for the old request to release admission"
    );
    // The late first response is dropped, and its completion releases
    // exactly one replacement prefetch for the current anchor.
    let shape = first.response_shape(first.anchor).expect("valid query");
    let rows = (0..shape.row_count)
        .map(|index| {
            test_row(
                first.anchor.viewport.columns,
                &index.to_string(),
                TerminalStyle::default(),
            )
        })
        .collect();
    assert!(matches!(
        viewport.apply_view_history_window(TerminalSurfaceHistoryWindowResult::Frame(
            TerminalSurfaceHistoryWindowFrame {
                colors: Default::default(),
                disposition: shape.disposition,
                anchor: first.anchor,
                target_offset_from_bottom: shape.target_offset_from_bottom,
                first_row_from_live_top: shape.first_row_from_live_top,
                rows,
            },
        )),
        Ok(ViewportEffect::RequestHistoryWindow(query))
            if query.anchor.revision == Revision::new(8)
    ));
    assert!(
        viewport.prefetch_live().is_none(),
        "the replacement prefetch remains the only pending request"
    );
}

#[test]
fn composed_frame_owns_live_gutter_status_and_alternate_layout() {
    let physical = TerminalSize::new(4, 12);
    let main_layout = ChromeLayout::new(physical, ActiveScreen::Main);
    let mut viewport = ViewportController::with_layout(
        main_layout,
        Some(TerminalScrollMetrics {
            epoch: Revision::new(1),
            revision: Revision::new(3),
            offset_from_bottom: 0,
            max_offset_from_bottom: 4,
            viewport_rows: main_layout.child.rows,
        }),
    );
    let mut snapshot = test_snapshot(main_layout.child, ActiveScreen::Main, Revision::new(3));
    let rightmost = usize::from(main_layout.child.columns - 1);
    snapshot.surface.rows[0].cells[rightmost] = TerminalCell {
        contents: "x".to_owned(),
        style: TerminalStyle {
            foreground: TerminalColor::Indexed(2),
            ..TerminalStyle::default()
        },
        ..TerminalCell::default()
    };
    let surface = AttachmentSurface::from_snapshot(&snapshot).expect("test snapshot is valid");
    let mut status = StatusRenderer::new(
        TerminalViewTarget::for_display("node", TerminalViewRoute::Remote),
        physical,
    );
    status.path = TerminalViewConnectionPath::Direct;
    status.rtt_ms = Some(8);
    let frame = ComposedFrame::compose(
        &surface.surface,
        None,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("compose main frame");
    assert_eq!(frame.layout.gutter_column, Some(physical.columns));
    assert_eq!(frame.layout.status_row, Some(physical.rows - 1));
    assert_eq!(
        frame.rows[&0][rightmost],
        snapshot.surface.rows[0].cells[rightmost]
    );
    let status_cells = &frame.rows[&(physical.rows - 1)];
    assert!(status_cells.iter().all(|cell| cell.style.inverse));
    assert_eq!(
        status_cells
            .iter()
            .map(|cell| cell.contents.as_str())
            .collect::<String>(),
        "node | direc"
    );

    let alternate_layout = ChromeLayout::new(physical, ActiveScreen::Alternate);
    viewport.set_layout(alternate_layout);
    let alternate = test_snapshot(
        alternate_layout.child,
        ActiveScreen::Alternate,
        Revision::new(4),
    );
    let alternate_frame = ComposedFrame::compose(
        &alternate.surface,
        Some(&frame),
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("compose alternate frame");
    assert_eq!(alternate_frame.layout.gutter_column, None);
    assert_eq!(
        alternate_frame.layout.content_size.columns,
        physical.columns
    );
}

#[test]
fn live_height_handoff_preserves_minimum_pan_and_guards_unknown_width() {
    for screen in [ActiveScreen::Main, ActiveScreen::Alternate] {
        let old_layout = ChromeLayout::new(TerminalSize::new(8, 9), screen);
        let physical = TerminalSize::new(4, 9);
        let layout = ChromeLayout::new(physical, screen);
        let viewport = ViewportController::with_layout(layout, None);
        let status = StatusRenderer::new(
            TerminalViewTarget::for_display("fixture", TerminalViewRoute::Local),
            physical,
        );
        let mut snapshot = test_snapshot(old_layout.child, screen, Revision::new(1));
        for (row, line) in snapshot.surface.rows.iter_mut().enumerate() {
            line.cells[0].contents = row.to_string();
        }
        for (cursor, pan) in [(1, 0), (5, 3)] {
            snapshot.surface.cursor.row = cursor;
            snapshot.surface.cursor.visible = true;
            let before = ComposedFrame::compose(
                &snapshot.surface,
                None,
                &viewport,
                &status,
                TerminalViewTransportState::Active,
            )
            .expect("local crop/pan");
            assert_eq!(before.rows[&0][0].contents, pan.to_string());
            assert_eq!(before.cursor.row, cursor - pan);

            let mut resized = snapshot.surface.clone();
            resized.rows =
                resized.rows[usize::from(pan)..usize::from(pan + layout.child.rows)].to_vec();
            resized.size = layout.child;
            resized.cursor.row -= pan;
            let after = ComposedFrame::compose(
                &resized,
                Some(&before),
                &viewport,
                &status,
                TerminalViewTransportState::Active,
            )
            .expect("authoritative height handoff");
            assert_eq!(before, after, "no second displacement at the host handoff");
        }
        snapshot.surface.cursor.visible = false;
        let hidden = ComposedFrame::compose(
            &snapshot.surface,
            None,
            &viewport,
            &status,
            TerminalViewTransportState::Active,
        )
        .expect("hidden cursor");
        assert_eq!(hidden.rows[&0][0].contents, "0");
        snapshot.surface.cursor.visible = true;
        let mut wider = ViewportController::with_layout(layout, None);
        wider.set_layout(ChromeLayout::new(TerminalSize::new(4, 10), screen));
        let wider_status = StatusRenderer::new(
            TerminalViewTarget::for_display("fixture", TerminalViewRoute::Local),
            TerminalSize::new(4, 10),
        );
        let changed_width = ComposedFrame::compose(
            &snapshot.surface,
            None,
            &wider,
            &wider_status,
            TerminalViewTransportState::Active,
        )
        .expect("unknown width mapping");
        assert_eq!(changed_width.rows[&0][0].contents, "0");
    }
}

#[test]
fn universal_chrome_uses_exact_local_status_and_screen_specific_child_size() {
    let physical = TerminalSize::new(24, 80);
    let main = ChromeLayout::new(physical, ActiveScreen::Main);
    assert_eq!(main.child, TerminalSize::new(23, 79));
    assert_eq!(main.status_row, Some(23));
    assert_eq!(main.gutter_column, Some(80));

    let alternate = ChromeLayout::new(physical, ActiveScreen::Alternate);
    assert_eq!(alternate.child, TerminalSize::new(23, 80));
    assert_eq!(alternate.status_row, Some(23));
    assert_eq!(alternate.gutter_column, None);

    let local = StatusRenderer::new(
        TerminalViewTarget::for_display("Mac", TerminalViewRoute::Local),
        physical,
    );
    assert_eq!(
        local.composed_text(TerminalViewTransportState::Active),
        Some("Mac | local".to_owned())
    );
    assert_eq!(
        local.composed_text(TerminalViewTransportState::Reconnecting),
        Some("Mac | local".to_owned())
    );

    let mut remote = StatusRenderer::new(
        TerminalViewTarget::for_display("Mac", TerminalViewRoute::Remote),
        physical,
    );
    assert_eq!(
        remote.composed_text(TerminalViewTransportState::Active),
        Some("Mac | -- | --".to_owned())
    );
    remote.path = TerminalViewConnectionPath::Direct;
    remote.rtt_ms = Some(7);
    assert_eq!(
        remote.composed_text(TerminalViewTransportState::Active),
        Some("Mac | direct | 7 ms".to_owned())
    );
    assert_eq!(
        remote.composed_text(TerminalViewTransportState::Reconnecting),
        Some("Mac | -- | --".to_owned())
    );
    remote.path = TerminalViewConnectionPath::Relay;
    remote.rtt_ms = None;
    assert_eq!(
        remote.composed_text(TerminalViewTransportState::Active),
        Some("Mac | relay | --".to_owned())
    );

    let one_row = TerminalSize::new(1, 80);
    let fallback = ChromeLayout::new(one_row, ActiveScreen::Main);
    assert_eq!(fallback.child, one_row);
    assert_eq!(fallback.status_row, None);
    let local = StatusRenderer::new(
        TerminalViewTarget::for_display("Mac", TerminalViewRoute::Local),
        one_row,
    );
    assert_eq!(
        local.composed_text(TerminalViewTransportState::Active),
        None
    );
}

#[test]
fn status_row_fills_physical_width_across_resizes() {
    for screen in [ActiveScreen::Main, ActiveScreen::Alternate] {
        for route in [TerminalViewRoute::Local, TerminalViewRoute::Remote] {
            let physical = TerminalSize::new(24, 80);
            let layout = ChromeLayout::new(physical, screen);
            let snapshot = test_snapshot(layout.child, screen, Revision::new(2));
            let mut viewport = ViewportController::with_layout(layout, None);
            let mut status =
                StatusRenderer::new(TerminalViewTarget::for_display("Mac", route), physical);
            let mut previous = None;

            // Local chrome must resize before the child snapshot catches up,
            // including when the child has reached its resource limit.
            for columns in [80, 240, 241, 320, 480, 240, 80] {
                let physical = TerminalSize::new(24, columns);
                viewport.set_layout(ChromeLayout::new(physical, screen));
                status.resize(physical);
                let frame = ComposedFrame::compose(
                    &snapshot.surface,
                    previous.as_ref(),
                    &viewport,
                    &status,
                    TerminalViewTransportState::Active,
                )
                .expect("compose resized status row");
                let cells = &frame.rows[&(physical.rows - 1)];
                assert_eq!(cells.len(), usize::from(columns));
                assert!(cells.iter().all(|cell| cell.style.inverse));
                assert!(cells.last().expect("status padding").contents.is_empty());
                assert!(
                    frame.layout.content_size.columns
                        <= voytty_core::ResourceLimits::default().max_viewport_columns
                );
                previous = Some(frame);
            }
        }
    }
}

#[test]
fn command_status_is_bounded_and_restores_the_underlying_status() {
    for route in [TerminalViewRoute::Local, TerminalViewRoute::Remote] {
        for width in [1, 8, 24, 80, 240, 320] {
            for screen in [ActiveScreen::Main, ActiveScreen::Alternate] {
                let physical = TerminalSize::new(24, width);
                let layout = ChromeLayout::new(physical, screen);
                let snapshot = test_snapshot(layout.child, screen, Revision::new(2));
                let viewport = ViewportController::with_layout(layout, None);
                let mut status =
                    StatusRenderer::new(TerminalViewTarget::for_display("服务器", route), physical);
                if route == TerminalViewRoute::Remote {
                    status.upload = Some(upload::UploadStatus::preparing());
                }
                let normal = status.composed_text(TerminalViewTransportState::Active);
                status.command_pending = true;
                let text = status
                    .composed_text(TerminalViewTransportState::Active)
                    .expect("command status");
                assert_eq!(
                    unicode_width::UnicodeWidthStr::width(text.as_str()),
                    usize::from(width),
                );
                if width >= 80 {
                    assert!(text.contains("Ctrl+] command: . detach"));
                    assert_eq!(
                        text.contains("v upload"),
                        route == TerminalViewRoute::Remote
                    );
                    assert!(!text.contains("Preparing upload"));
                }
                let frame = ComposedFrame::compose(
                    &snapshot.surface,
                    None,
                    &viewport,
                    &status,
                    TerminalViewTransportState::Active,
                )
                .expect("command status frame");
                assert_eq!(frame.layout.content_size, layout.child);
                assert_eq!(frame.modes, snapshot.surface.modes);
                status.command_pending = false;
                assert_eq!(
                    status.composed_text(TerminalViewTransportState::Active),
                    normal,
                );
            }
        }
    }
    let mut one_row = StatusRenderer::new(
        TerminalViewTarget::for_display("Mac", TerminalViewRoute::Local),
        TerminalSize::new(1, 80),
    );
    one_row.command_pending = true;
    assert_eq!(
        one_row.composed_text(TerminalViewTransportState::Active),
        None
    );
}

#[test]
fn upload_status_stays_right_aligned_without_changing_child_geometry() {
    use voytty_core::upload::{UploadPhase, UploadProgress};
    for width in [4, 12, 24, 80, 120, 240, 241, 320, 480] {
        for screen in [ActiveScreen::Main, ActiveScreen::Alternate] {
            let physical = TerminalSize::new(24, width);
            let original = ChromeLayout::new(physical, screen);
            let mut status = StatusRenderer::new(
                TerminalViewTarget::for_display(
                    "服务器e\u{301}-long-name",
                    TerminalViewRoute::Remote,
                ),
                physical,
            );
            let mut upload = upload::UploadStatus::preparing();
            upload.observe(UploadProgress {
                phase: UploadPhase::Uploading,
                accepted_bytes: 25_000_000,
                total_bytes: Some(50_000_000),
            });
            let suffix = upload.text(usize::from(width));
            status.upload = Some(upload);
            let text = status
                .composed_text(TerminalViewTransportState::Active)
                .expect("status row");
            assert_eq!(
                unicode_width::UnicodeWidthStr::width(text.as_str()),
                usize::from(width)
            );
            assert!(text.ends_with(&suffix));
            assert!(text.contains("50%"));
            assert_eq!(ChromeLayout::new(physical, screen).child, original.child);
        }
    }
}

#[test]
fn composed_frame_preserves_wide_spans_combining_text_and_styled_blanks() {
    let size = TerminalSize::new(1, 5);
    let layout = ChromeLayout::new(size, ActiveScreen::Alternate);
    let viewport = ViewportController::with_layout(layout, None);
    let style = TerminalStyle {
        background: TerminalColor::Rgb(1, 2, 3),
        underline: voytty_core::terminal::TerminalUnderline::Single,
        ..TerminalStyle::default()
    };
    let mut snapshot = test_snapshot(size, ActiveScreen::Alternate, Revision::new(2));
    snapshot.surface.rows[0].cells = vec![
        TerminalCell {
            contents: "e\u{301}".to_owned(),
            style,
            ..TerminalCell::default()
        },
        TerminalCell {
            contents: "界".to_owned(),
            wide: true,
            style,
            ..TerminalCell::default()
        },
        TerminalCell {
            wide_continuation: true,
            style,
            ..TerminalCell::default()
        },
        TerminalCell::default(),
        TerminalCell {
            style,
            ..TerminalCell::default()
        },
    ];
    snapshot.validate().expect("valid exact semantic row");
    let status = StatusRenderer::new(
        TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
        size,
    );
    let frame = ComposedFrame::compose(
        &snapshot.surface,
        None,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("compose exact row");
    assert_eq!(frame.rows[&0], snapshot.surface.rows[0].cells);
    let blank = vec![TerminalCell::default(); 5];
    assert_eq!(
        semantic_dirty_runs(&snapshot.surface.rows[0].cells, &blank),
        vec![(0, 3), (4, 5)]
    );
}

#[test]
fn compositor_is_sparse_for_huge_physical_row_numbers() {
    let physical = TerminalSize::new(u16::MAX, 6);
    let layout = ChromeLayout::new(physical, ActiveScreen::Main);
    let snapshot = test_snapshot(layout.child, ActiveScreen::Main, Revision::new(2));
    let viewport = ViewportController::with_layout(layout, snapshot.surface.scroll_metrics);
    let status = StatusRenderer::new(
        TerminalViewTarget::for_display("node", TerminalViewRoute::Remote),
        physical,
    );
    let frame = ComposedFrame::compose(
        &snapshot.surface,
        None,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("bounded sparse frame");
    assert_eq!(
        frame.rows.len(),
        usize::from(layout.child.rows).saturating_add(1)
    );
    assert!(frame.rows.contains_key(&(u16::MAX - 1)));
}

#[test]
fn hidden_cursor_keeps_ime_position_and_moves_without_cell_changes() {
    for screen in [ActiveScreen::Main, ActiveScreen::Alternate] {
        let physical = TerminalSize::new(6, 24);
        let layout = ChromeLayout::new(physical, screen);
        let mut snapshot = test_snapshot(layout.child, screen, Revision::new(2));
        let viewport = ViewportController::with_layout(layout, snapshot.surface.scroll_metrics);
        let status = StatusRenderer::new(
            TerminalViewTarget::for_display("peer", TerminalViewRoute::Remote),
            physical,
        );
        let mut presenter = DesktopPresenter::default();

        for (row, column, visible) in [(2, 3, true), (2, 3, false), (3, 5, false), (3, 5, true)] {
            snapshot.surface.cursor = TerminalCursor {
                presentation: Default::default(),
                row,
                column,
                visible,
                style: TerminalStyle::default(),
            };
            let desired = ComposedFrame::compose(
                &snapshot.surface,
                presenter.semantic_baseline.as_ref(),
                &viewport,
                &status,
                TerminalViewTransportState::Active,
            )
            .expect("compose cursor-only update");
            if let Some(previous) = &presenter.semantic_baseline {
                assert_eq!(desired.rows, previous.rows, "only the cursor changed");
            }
            let first_frame = presenter.baseline.is_none();
            let mut output = ViewportFrameWriter::default();
            assert!(
                presenter
                    .present(&mut output, desired.clone(), None)
                    .expect("present cursor-only update"),
                "cursor position/visibility must reach the physical terminal"
            );
            let mut expected_tail = format!("\x1b[{};{}H", row + 1, column + 1).into_bytes();
            expected_tail.extend_from_slice(if visible { b"\x1b[?25h" } else { b"\x1b[?25l" });
            if first_frame {
                expected_tail.extend_from_slice(HOST_INPUT_CAPTURE);
            } else {
                for mode in [HOST_INPUT_CAPTURE, b"\x1b[?1003l", b"\x1b[?1004l", b" q"] {
                    assert!(find_bytes(&output.bytes, mode).is_none());
                }
            }
            expected_tail.extend_from_slice(HOST_SYNC_END);
            assert!(
                output.bytes.ends_with(&expected_tail),
                "screen={screen:?} cursor=({row},{column},{visible}) output={:?}",
                String::from_utf8_lossy(&output.bytes)
            );
            assert_eq!((output.writes, output.flushes), (1, 1));
            assert!(
                !presenter
                    .present(&mut output, desired, None)
                    .expect("unchanged cursor is a no-op")
            );
            assert_eq!((output.writes, output.flushes), (1, 1));
        }
    }
}

#[test]
fn desktop_presenter_is_the_single_atomic_commit_boundary() {
    let size = TerminalSize::new(2, 4);
    let snapshot = test_snapshot(size, ActiveScreen::Alternate, Revision::new(2));
    let surface = AttachmentSurface::from_snapshot(&snapshot).expect("test snapshot is valid");
    let viewport =
        ViewportController::with_layout(ChromeLayout::new(size, ActiveScreen::Alternate), None);
    let status = StatusRenderer::new(
        TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
        size,
    );
    let desired = ComposedFrame::compose(
        &surface.surface,
        None,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("test frame composes");
    let mut presenter = DesktopPresenter::default();
    let mut output = ViewportFrameWriter::default();
    assert!(
        presenter
            .present(&mut output, desired.clone(), None)
            .expect("initial frame presents")
    );
    assert_eq!((output.writes, output.flushes), (1, 1));
    assert!(output.bytes.starts_with(HOST_SYNC_BEGIN));
    assert!(output.bytes.ends_with(HOST_SYNC_END));
    assert!(find_bytes(&output.bytes, HOST_INPUT_CAPTURE).is_some());
    assert!(
        !presenter
            .present(&mut output, desired, None)
            .expect("identical frame is a no-op")
    );
    assert_eq!((output.writes, output.flushes), (1, 1));
}

#[test]
fn presenter_clipboard_write_is_canonical_and_does_not_advance_the_frame() {
    let mut presenter = DesktopPresenter::default();
    let size = TerminalSize::new(1, 2);
    let snapshot = test_snapshot(size, ActiveScreen::Alternate, Revision::new(2));
    let viewport =
        ViewportController::with_layout(ChromeLayout::new(size, ActiveScreen::Alternate), None);
    let status = StatusRenderer::new(
        TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
        size,
    );
    let frame = ComposedFrame::compose(
        &snapshot.surface,
        None,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("compose clipboard baseline frame");
    let mut visual = ViewportFrameWriter::default();
    presenter
        .present(&mut visual, frame, None)
        .expect("present clipboard baseline frame");
    let baseline = presenter.baseline.clone();

    let write = voytty_core::terminal::TerminalClipboardWrite::new("hello".to_owned())
        .expect("clipboard value");
    let mut clipboard = ViewportFrameWriter::default();
    presenter
        .write_clipboard(&mut clipboard, &write)
        .expect("write clipboard escape");
    assert_eq!(clipboard.bytes, b"\x1b]52;c;aGVsbG8=\x07");
    assert_eq!((clipboard.writes, clipboard.flushes), (1, 1));
    assert_eq!(presenter.baseline, baseline);
    for (notification, bytes) in [
        (
            voytty_core::terminal::TerminalNotification::osc9("结果: done".into()),
            "\x1b]9;结果: done\x1b\\",
        ),
        (
            voytty_core::terminal::TerminalNotification::osc777("标题".into(), "body;结果".into()),
            "\x1b]777;notify;标题;body;结果\x1b\\",
        ),
    ] {
        let mut output = ViewportFrameWriter::default();
        presenter
            .write_notification(&mut output, &notification.expect("valid"))
            .expect("notification output");
        assert_eq!(output.bytes, bytes.as_bytes());
        assert_eq!((output.writes, output.flushes), (1, 1));
        assert_eq!(presenter.baseline, baseline);
    }
}

#[test]
fn presenter_clipboard_failure_preserves_the_visual_baseline() {
    struct ClipboardWriteFailure;

    impl Write for ClipboardWriteFailure {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("injected clipboard failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let size = TerminalSize::new(1, 2);
    let snapshot = test_snapshot(size, ActiveScreen::Alternate, Revision::new(2));
    let viewport =
        ViewportController::with_layout(ChromeLayout::new(size, ActiveScreen::Alternate), None);
    let status = StatusRenderer::new(
        TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
        size,
    );
    let frame = ComposedFrame::compose(
        &snapshot.surface,
        None,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("compose clipboard failure baseline frame");
    let mut presenter = DesktopPresenter::default();
    let mut output = ViewportFrameWriter::default();
    presenter
        .present(&mut output, frame, None)
        .expect("present clipboard failure baseline frame");
    let baseline = presenter.baseline.clone();
    let write = voytty_core::terminal::TerminalClipboardWrite::new("secret".to_owned())
        .expect("clipboard value");

    let error = presenter
        .write_clipboard(&mut ClipboardWriteFailure, &write)
        .expect_err("injected clipboard write fails");
    assert!(error.to_string().contains("injected clipboard failure"));
    assert_eq!(presenter.baseline, baseline);
    assert!(!error.to_string().contains(write.as_str()));
    let notification =
        voytty_core::terminal::TerminalNotification::osc9("secret notification".into())
            .expect("valid");
    assert!(
        presenter
            .write_notification(&mut ClipboardWriteFailure, &notification)
            .is_err()
    );
    assert_eq!(presenter.baseline, baseline);
    struct FailFlush {
        writes: usize,
        flushes: usize,
    }
    impl Write for FailFlush {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Err(io::Error::other("injected flush failure"))
        }
    }
    let mut output = FailFlush {
        writes: 0,
        flushes: 0,
    };
    let error = presenter
        .write_notification(&mut output, &notification)
        .expect_err("ambiguous flush");
    assert!(!error.to_string().contains(notification.body()));
    assert_eq!((output.writes, output.flushes), (1, 1));
    assert_eq!(presenter.baseline, baseline);
}

#[test]
fn presenter_selection_overlay_and_keyboard_mode_share_one_frame_commit() {
    let size = TerminalSize::new(1, 4);
    let snapshot = test_snapshot(size, ActiveScreen::Alternate, Revision::new(2));
    let viewport =
        ViewportController::with_layout(ChromeLayout::new(size, ActiveScreen::Alternate), None);
    let status = StatusRenderer::new(
        TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
        size,
    );
    let frame = ComposedFrame::compose(
        &snapshot.surface,
        None,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("compose selection overlay frame");
    let mut presenter = DesktopPresenter::default();
    let source = SelectionSourceIdentity::Live {
        revision: snapshot.revision,
        screen: ActiveScreen::Alternate,
        viewport: size,
    };
    presenter.set_selection(
        Some(source),
        Some(voytty_core::terminal_selection::TerminalTextRange::new(
            TerminalTextPoint::new(0, 0),
            TerminalTextPoint::new(0, 1),
        )),
        true,
    );
    let mut output = ViewportFrameWriter::default();
    presenter
        .present(&mut output, frame, Some(source))
        .expect("present selection overlay frame");

    let committed = presenter.baseline.as_ref().expect("committed frame");
    assert!(committed.rows[&0][0].style.inverse);
    assert!(committed.rows[&0][1].style.inverse);
    assert!(!committed.rows[&0][2].style.inverse);
    assert_eq!(committed.modes.keyboard_flags.bits(), 7);
    assert!(find_bytes(&output.bytes, b"\x1b[=7u").is_some());

    presenter.set_selection(None, None, false);
    assert_eq!(
        presenter
            .outer_keyboard_flags(voytty_core::terminal::TerminalKeyboardFlags::default())
            .bits(),
        0
    );
    assert_eq!(presenter.presented_keyboard_flags().bits(), 7);
    let frame = ComposedFrame::compose(
        &snapshot.surface,
        presenter.baseline.as_ref(),
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("compose selection removal frame");
    presenter
        .present(&mut output, frame, Some(source))
        .expect("present selection removal frame");
    assert_eq!(presenter.presented_keyboard_flags().bits(), 0);
}

#[test]
fn hidden_history_delta_synchronizes_only_host_input_modes_and_preserves_selection_elevation() {
    let physical = TerminalSize::new(3, 8);
    let layout = ChromeLayout::new(physical, ActiveScreen::Main);
    let snapshot = test_snapshot(layout.child, ActiveScreen::Main, Revision::new(2));
    let mut surface = AttachmentSurface::from_snapshot(&snapshot).expect("valid history surface");
    let mut viewport = ViewportController::with_layout(layout, snapshot.surface.scroll_metrics);
    let status = StatusRenderer::new(
        TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
        physical,
    );
    let mut presenter = DesktopPresenter::default();
    let mut selection = SelectionController::default();
    let mut output = ViewportFrameWriter::default();

    present_surface_with_writer(
        &mut output,
        &surface,
        &mut presenter,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("present initial live frame");
    viewport.observe_presentation();
    install_test_history_window(&mut viewport);
    present_surface_with_writer(
        &mut output,
        &surface,
        &mut presenter,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("present pinned history frame");
    viewport.observe_presentation();

    let source = viewport
        .selection_source_identity(&surface)
        .expect("presented history has a stable source");
    let rows = viewport
        .selection_rows(&surface)
        .expect("presented history has semantic rows");
    selection
        .begin(source, TerminalTextPoint::new(0, 0), rows)
        .expect("begin history selection");
    selection
        .update(source, TerminalTextPoint::new(0, 1), rows)
        .expect("extend history selection");
    selection.finish();
    reconcile_presenter_selection(&mut selection, &viewport, &surface, &mut presenter);
    present_surface_with_writer(
        &mut output,
        &surface,
        &mut presenter,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("commit selection overlay and local keyboard elevation");
    viewport.observe_presentation();
    assert_eq!(presenter.presented_keyboard_flags().bits(), 7);
    let selected_rows = presenter
        .baseline
        .as_ref()
        .expect("selection frame is committed")
        .rows
        .clone();

    output = ViewportFrameWriter::default();
    let child_flags = voytty_core::terminal::TerminalKeyboardFlags::from_bits(9)
        .expect("valid child keyboard flags");
    let delta = TerminalSurfaceDelta {
        application_title: String::new(),
        colors: Default::default(),

        from_revision: Revision::new(2),
        to_revision: Revision::new(3),
        size: layout.child,
        active_screen: ActiveScreen::Main,
        row_patches: vec![TerminalSurfaceRowPatch {
            row: 0,
            replacement: test_row(
                layout.child.columns,
                "hidden-live-change",
                TerminalStyle::default(),
            ),
        }],
        cursor: snapshot.surface.cursor,
        modes: TerminalModes {
            application_cursor: true,
            application_keypad: true,
            bracketed_paste: true,
            focus_reporting: true,
            keyboard_flags: child_flags,
            ..TerminalModes::default()
        },
        scroll_metrics: Some(TerminalScrollMetrics {
            epoch: Revision::new(1),
            revision: Revision::new(3),
            offset_from_bottom: 0,
            max_offset_from_bottom: 10,
            viewport_rows: layout.child.rows,
        }),
    };
    assert_eq!(
        apply_delta_with_writer(
            &mut output,
            &mut surface,
            &mut presenter,
            &delta,
            &mut viewport,
            &mut selection,
            &status,
            TerminalViewTransportState::Active,
            false,
        )
        .expect("apply hidden history delta"),
        DeltaRender::Applied
    );
    assert_eq!(output.bytes, b"\x1b[?1h\x1b=\x1b[?2004h\x1b[?1004h\x1b[=9u");
    assert_eq!((output.writes, output.flushes), (1, 1));
    assert_eq!(surface.revision(), Revision::new(3));
    assert!(surface.modes().application_cursor);
    assert!(surface.modes().application_keypad);
    assert!(surface.modes().bracketed_paste);
    assert!(surface.modes().focus_reporting);
    assert_eq!(surface.modes().keyboard_flags, child_flags);
    assert_eq!(presenter.presented_keyboard_flags(), child_flags);
    let committed = presenter
        .baseline
        .as_ref()
        .expect("mode commit retains frame");
    assert_eq!(committed.rows, selected_rows);
    assert!(committed.modes.application_cursor);
    assert!(committed.modes.application_keypad);
    assert!(committed.modes.bracketed_paste);
    assert!(committed.modes.focus_reporting);
    assert_eq!(committed.modes.keyboard_flags, child_flags);
    assert!(find_bytes(&output.bytes, HOST_SYNC_BEGIN).is_none());
    assert!(
        !present_surface_with_writer(
            &mut output,
            &surface,
            &mut presenter,
            &viewport,
            &status,
            TerminalViewTransportState::Active,
        )
        .expect("accurate mode baseline makes the next full presentation a no-op")
    );
    assert_eq!((output.writes, output.flushes), (1, 1));

    output = ViewportFrameWriter::default();
    let delta = TerminalSurfaceDelta {
        application_title: String::new(),
        colors: Default::default(),

        from_revision: Revision::new(3),
        to_revision: Revision::new(4),
        size: layout.child,
        active_screen: ActiveScreen::Main,
        row_patches: Vec::new(),
        cursor: snapshot.surface.cursor,
        modes: TerminalModes::default(),
        scroll_metrics: Some(TerminalScrollMetrics {
            epoch: Revision::new(1),
            revision: Revision::new(4),
            offset_from_bottom: 0,
            max_offset_from_bottom: 10,
            viewport_rows: layout.child.rows,
        }),
    };
    assert_eq!(
        apply_delta_with_writer(
            &mut output,
            &mut surface,
            &mut presenter,
            &delta,
            &mut viewport,
            &mut selection,
            &status,
            TerminalViewTransportState::Active,
            false,
        )
        .expect("restore child legacy mode behind history"),
        DeltaRender::Applied
    );
    assert_eq!(output.bytes, b"\x1b[?1l\x1b>\x1b[?2004l\x1b[?1004l\x1b[=7u");
    assert_eq!((output.writes, output.flushes), (1, 1));
    assert_eq!(surface.modes().keyboard_flags.bits(), 0);
    assert_eq!(presenter.presented_keyboard_flags().bits(), 7);
    assert_eq!(
        presenter
            .baseline
            .as_ref()
            .expect("selection frame remains committed")
            .rows,
        selected_rows
    );

    output = ViewportFrameWriter::default();
    let routed_only = TerminalModes {
        alternate_scroll: true,
        mouse_mode: TerminalMouseMode::PressRelease,
        mouse_encoding: TerminalMouseEncoding::Sgr,
        ..TerminalModes::default()
    };
    let delta = TerminalSurfaceDelta {
        application_title: String::new(),
        colors: Default::default(),

        from_revision: Revision::new(4),
        to_revision: Revision::new(5),
        size: layout.child,
        active_screen: ActiveScreen::Main,
        row_patches: vec![TerminalSurfaceRowPatch {
            row: 0,
            replacement: test_row(
                layout.child.columns,
                "routed-live",
                TerminalStyle::default(),
            ),
        }],
        cursor: snapshot.surface.cursor,
        modes: routed_only,
        scroll_metrics: Some(TerminalScrollMetrics {
            epoch: Revision::new(1),
            revision: Revision::new(5),
            offset_from_bottom: 0,
            max_offset_from_bottom: 10,
            viewport_rows: layout.child.rows,
        }),
    };
    assert_eq!(
        apply_delta_with_writer(
            &mut output,
            &mut surface,
            &mut presenter,
            &delta,
            &mut viewport,
            &mut selection,
            &status,
            TerminalViewTransportState::Active,
            false,
        )
        .expect("install routed-only child modes behind history"),
        DeltaRender::Applied
    );
    assert_eq!((output.writes, output.flushes), (0, 0));
    assert!(output.bytes.is_empty());
    assert_eq!(surface.modes(), routed_only);
    let committed = presenter
        .baseline
        .as_ref()
        .expect("routed modes do not alter physical baseline");
    assert_eq!(committed.rows, selected_rows);
    assert!(!committed.modes.alternate_scroll);
    assert_eq!(committed.modes.mouse_mode, TerminalMouseMode::None);
    assert_eq!(
        committed.modes.mouse_encoding,
        TerminalMouseEncoding::default()
    );
    assert!(
        !present_surface_with_writer(
            &mut output,
            &surface,
            &mut presenter,
            &viewport,
            &status,
            TerminalViewTransportState::Active,
        )
        .expect("routed-only semantic modes never trigger a physical repaint")
    );
    assert_eq!((output.writes, output.flushes), (0, 0));
}

#[test]
fn hidden_incompatible_anchor_retires_selection_in_the_sole_mode_transaction() {
    for (case, epoch, maximum) in [
        ("epoch change", Revision::new(2), 10),
        ("history shrink", Revision::new(1), 0),
    ] {
        let physical = TerminalSize::new(3, 8);
        let layout = ChromeLayout::new(physical, ActiveScreen::Main);
        let snapshot = test_snapshot(layout.child, ActiveScreen::Main, Revision::new(2));
        let mut surface =
            AttachmentSurface::from_snapshot(&snapshot).expect("valid history surface");
        let mut viewport = ViewportController::with_layout(layout, snapshot.surface.scroll_metrics);
        let status = StatusRenderer::new(
            TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
            physical,
        );
        let mut presenter = DesktopPresenter::default();
        let mut setup_output = ViewportFrameWriter::default();
        present_surface_with_writer(
            &mut setup_output,
            &surface,
            &mut presenter,
            &viewport,
            &status,
            TerminalViewTransportState::Active,
        )
        .expect("present initial live frame");
        viewport.observe_presentation();
        install_test_history_window(&mut viewport);
        present_surface_with_writer(
            &mut setup_output,
            &surface,
            &mut presenter,
            &viewport,
            &status,
            TerminalViewTransportState::Active,
        )
        .expect("present pinned history frame");
        viewport.observe_presentation();
        let mut selection = finalize_test_selection(
            &surface,
            &mut viewport,
            &mut presenter,
            &status,
            &mut setup_output,
        );
        let selected_rows = presenter
            .baseline
            .as_ref()
            .expect("selected history frame is committed")
            .rows
            .clone();

        let delta = TerminalSurfaceDelta {
            application_title: String::new(),
            colors: Default::default(),

            from_revision: Revision::new(2),
            to_revision: Revision::new(3),
            size: layout.child,
            active_screen: ActiveScreen::Main,
            row_patches: vec![TerminalSurfaceRowPatch {
                row: 0,
                replacement: test_row(layout.child.columns, "hidden-new", TerminalStyle::default()),
            }],
            cursor: snapshot.surface.cursor,
            modes: TerminalModes::default(),
            scroll_metrics: Some(TerminalScrollMetrics {
                epoch,
                revision: Revision::new(3),
                offset_from_bottom: 0,
                max_offset_from_bottom: maximum,
                viewport_rows: layout.child.rows,
            }),
        };
        let mut output = ViewportFrameWriter::default();
        assert_eq!(
            apply_delta_with_writer(
                &mut output,
                &mut surface,
                &mut presenter,
                &delta,
                &mut viewport,
                &mut selection,
                &status,
                TerminalViewTransportState::Active,
                false,
            )
            .unwrap_or_else(|error| panic!("{case} delta failed: {error}")),
            DeltaRender::Applied
        );

        assert_eq!(output.bytes, b"\x1b[=0u", "{case}");
        assert_eq!((output.writes, output.flushes), (1, 1), "{case}");
        assert!(
            find_bytes(&output.bytes, HOST_SYNC_BEGIN).is_none(),
            "{case}"
        );
        assert_eq!(surface.revision(), Revision::new(3), "{case}");
        assert!(!selection.is_finalized(), "{case}");
        assert_eq!(viewport.selection_source_identity(&surface), None, "{case}");
        assert!(viewport.window_cache.presented_rows().is_none(), "{case}");
        assert_eq!(presenter.presented_keyboard_flags().bits(), 0, "{case}");
        let committed = presenter
            .baseline
            .as_ref()
            .expect("mode-only transaction retains the visual baseline");
        assert_eq!(committed.rows, selected_rows, "{case}");
        assert_eq!(committed.modes.keyboard_flags.bits(), 0, "{case}");

        let mut no_io = ViewportFrameWriter::default();
        assert!(
            !presenter
                .sync_input_modes(
                    &mut no_io,
                    surface.modes(),
                    selection.presentation(viewport.selection_source_identity(&surface)),
                )
                .expect("committed candidate needs no second mode sync"),
            "{case}"
        );
        assert_eq!((no_io.writes, no_io.flushes), (0, 0), "{case}");
    }
}

#[test]
fn hidden_delta_io_failure_keeps_every_semantic_candidate_uncommitted() {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum FailurePoint {
        Write,
        Flush,
    }

    struct TransactionFailure {
        point: FailurePoint,
        failed: bool,
        bytes: Vec<u8>,
        writes: usize,
        flushes: usize,
    }

    impl Write for TransactionFailure {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            if self.point == FailurePoint::Write && !self.failed {
                self.failed = true;
                return Err(io::Error::other("injected input mode write failure"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.point == FailurePoint::Flush && !self.failed {
                self.failed = true;
                return Err(io::Error::other("injected input mode flush failure"));
            }
            Ok(())
        }
    }

    for point in [FailurePoint::Write, FailurePoint::Flush] {
        let physical = TerminalSize::new(3, 8);
        let layout = ChromeLayout::new(physical, ActiveScreen::Main);
        let snapshot = test_snapshot(layout.child, ActiveScreen::Main, Revision::new(2));
        let mut surface =
            AttachmentSurface::from_snapshot(&snapshot).expect("valid history surface");
        let mut viewport = ViewportController::with_layout(layout, snapshot.surface.scroll_metrics);
        let status = StatusRenderer::new(
            TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
            physical,
        );
        let mut presenter = DesktopPresenter::default();
        let mut setup_output = ViewportFrameWriter::default();
        present_surface_with_writer(
            &mut setup_output,
            &surface,
            &mut presenter,
            &viewport,
            &status,
            TerminalViewTransportState::Active,
        )
        .expect("present initial frame");
        viewport.observe_presentation();
        install_test_history_window(&mut viewport);
        present_surface_with_writer(
            &mut setup_output,
            &surface,
            &mut presenter,
            &viewport,
            &status,
            TerminalViewTransportState::Active,
        )
        .expect("present history frame");
        viewport.observe_presentation();
        let mut selection = finalize_test_selection(
            &surface,
            &mut viewport,
            &mut presenter,
            &status,
            &mut setup_output,
        );
        let selection_before = selection;
        let anchor_before = viewport.window_cache.anchor();
        let desired_before = viewport.window_cache.desired_offset_from_bottom();
        let source_before = viewport.selection_source_identity(&surface);
        let metrics_before = viewport.live_metrics;

        let delta = TerminalSurfaceDelta {
            application_title: String::new(),
            colors: Default::default(),

            from_revision: Revision::new(2),
            to_revision: Revision::new(3),
            size: layout.child,
            active_screen: ActiveScreen::Main,
            row_patches: Vec::new(),
            cursor: snapshot.surface.cursor,
            modes: TerminalModes::default(),
            scroll_metrics: Some(TerminalScrollMetrics {
                epoch: Revision::new(2),
                revision: Revision::new(3),
                offset_from_bottom: 0,
                max_offset_from_bottom: 4,
                viewport_rows: layout.child.rows,
            }),
        };
        let mut failure = TransactionFailure {
            point,
            failed: false,
            bytes: Vec::new(),
            writes: 0,
            flushes: 0,
        };
        let error = apply_delta_with_writer(
            &mut failure,
            &mut surface,
            &mut presenter,
            &delta,
            &mut viewport,
            &mut selection,
            &status,
            TerminalViewTransportState::Active,
            false,
        )
        .expect_err("failed physical mode commit rejects every candidate state");
        assert!(error.to_string().contains(match point {
            FailurePoint::Write => "input mode write failure",
            FailurePoint::Flush => "input mode flush failure",
        }));
        assert_eq!(failure.writes, 1);
        match point {
            FailurePoint::Write => {
                assert!(failure.bytes.is_empty());
                assert_eq!(failure.flushes, 0);
            }
            FailurePoint::Flush => {
                assert_eq!(failure.bytes, b"\x1b[=0u");
                assert_eq!(failure.flushes, 1);
            }
        }
        assert_eq!(surface.revision(), Revision::new(2));
        assert_eq!(viewport.window_cache.anchor(), anchor_before);
        assert_eq!(
            viewport.window_cache.desired_offset_from_bottom(),
            desired_before
        );
        assert_eq!(viewport.selection_source_identity(&surface), source_before);
        assert_eq!(viewport.live_metrics, metrics_before);
        assert_eq!(selection, selection_before);
        assert!(selection.is_finalized());
        assert_eq!(presenter.presented_keyboard_flags().bits(), 7);
        assert!(presenter.baseline.is_none());

        let mut recovery = ViewportFrameWriter::default();
        assert!(
            present_surface_with_writer(
                &mut recovery,
                &surface,
                &mut presenter,
                &viewport,
                &status,
                TerminalViewTransportState::Active,
            )
            .expect("unknown baseline forces a complete pre-delta recovery frame")
        );
        assert!(find_bytes(&recovery.bytes, b"\x1b[2J").is_none());
        assert!(find_bytes(&recovery.bytes, HOST_INPUT_CAPTURE).is_some());
        assert!(find_bytes(&recovery.bytes, b"\x1b[=7u").is_some());
        assert_eq!(presenter.presented_keyboard_flags().bits(), 7);
    }
}

#[test]
fn live_delta_presentation_failure_keeps_surface_viewport_and_selection_uncommitted() {
    struct FlushFailure {
        bytes: Vec<u8>,
    }

    impl Write for FlushFailure {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("injected live delta flush failure"))
        }
    }

    let physical = TerminalSize::new(3, 8);
    let layout = ChromeLayout::new(physical, ActiveScreen::Main);
    let snapshot = test_snapshot(layout.child, ActiveScreen::Main, Revision::new(2));
    let mut surface = AttachmentSurface::from_snapshot(&snapshot).expect("valid live surface");
    let mut viewport = ViewportController::with_layout(layout, snapshot.surface.scroll_metrics);
    let status = StatusRenderer::new(
        TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
        physical,
    );
    let mut presenter = DesktopPresenter::default();
    let mut setup_output = ViewportFrameWriter::default();
    present_surface_with_writer(
        &mut setup_output,
        &surface,
        &mut presenter,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("present initial live frame");
    viewport.observe_presentation();
    let mut selection = finalize_test_selection(
        &surface,
        &mut viewport,
        &mut presenter,
        &status,
        &mut setup_output,
    );
    let selection_before = selection;
    let metrics_before = viewport.live_metrics;
    let anchor_before = viewport.window_cache.anchor();
    let source_before = viewport.selection_source_identity(&surface);

    let delta = TerminalSurfaceDelta {
        application_title: String::new(),
        colors: Default::default(),

        from_revision: Revision::new(2),
        to_revision: Revision::new(3),
        size: layout.child,
        active_screen: ActiveScreen::Main,
        row_patches: Vec::new(),
        cursor: snapshot.surface.cursor,
        modes: TerminalModes {
            application_cursor: true,
            ..TerminalModes::default()
        },
        scroll_metrics: Some(TerminalScrollMetrics {
            epoch: Revision::new(1),
            revision: Revision::new(3),
            offset_from_bottom: 0,
            max_offset_from_bottom: 11,
            viewport_rows: layout.child.rows,
        }),
    };
    let mut failure = FlushFailure { bytes: Vec::new() };
    let error = apply_delta_with_writer(
        &mut failure,
        &mut surface,
        &mut presenter,
        &delta,
        &mut viewport,
        &mut selection,
        &status,
        TerminalViewTransportState::Active,
        false,
    )
    .expect_err("failed live frame rejects every candidate state");
    assert!(
        error.to_string().contains("live delta flush failure"),
        "unexpected live delta failure: {error}"
    );
    assert_eq!(surface.revision(), Revision::new(2));
    assert_eq!(surface.active_screen(), ActiveScreen::Main);
    assert_eq!(viewport.content_size, layout.child);
    assert_eq!(viewport.gutter_column, layout.gutter_column);
    assert_eq!(viewport.live_metrics, metrics_before);
    assert_eq!(viewport.window_cache.anchor(), anchor_before);
    assert_eq!(viewport.selection_source_identity(&surface), source_before);
    assert_eq!(selection, selection_before);
    assert!(selection.is_finalized());
    assert_eq!(presenter.presented_keyboard_flags().bits(), 7);
    assert!(presenter.baseline.is_none());

    let mut recovery = ViewportFrameWriter::default();
    assert!(
        present_surface_with_writer(
            &mut recovery,
            &surface,
            &mut presenter,
            &viewport,
            &status,
            TerminalViewTransportState::Active,
        )
        .expect("full recovery presents the complete pre-delta state")
    );
    assert!(find_bytes(&recovery.bytes, b"\x1b[2J").is_none());
    assert!(find_bytes(&recovery.bytes, b"\x1b[=7u").is_some());
    assert_eq!(presenter.presented_keyboard_flags().bits(), 7);
}

#[test]
fn presenter_forgets_baseline_after_flush_failure_and_repaints_fully() {
    struct FlushFailOnce {
        bytes: Vec<u8>,
        writes: usize,
        flushes: usize,
        fail_next_flush: bool,
    }

    impl Write for FlushFailOnce {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.fail_next_flush {
                self.fail_next_flush = false;
                Err(io::Error::other("injected flush failure"))
            } else {
                Ok(())
            }
        }
    }

    let size = TerminalSize::new(1, 3);
    let snapshot = test_snapshot(size, ActiveScreen::Alternate, Revision::new(2));
    let viewport =
        ViewportController::with_layout(ChromeLayout::new(size, ActiveScreen::Alternate), None);
    let status = StatusRenderer::new(
        TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
        size,
    );
    let desired = ComposedFrame::compose(
        &snapshot.surface,
        None,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("test frame composes");
    let mut presenter = DesktopPresenter::default();
    let mut writer = FlushFailOnce {
        bytes: Vec::new(),
        writes: 0,
        flushes: 0,
        fail_next_flush: true,
    };
    assert!(
        presenter
            .present(&mut writer, desired.clone(), None)
            .is_err()
    );
    assert!(presenter.baseline.is_none());

    writer.bytes.clear();
    assert!(
        presenter
            .present(&mut writer, desired, None)
            .expect("retry frame presents")
    );
    assert!(
        find_bytes(&writer.bytes, b"\x1b[2J").is_none(),
        "unknown baseline writes final cells without an intermediate screen erase"
    );
    assert!(presenter.baseline.is_some());
}

#[test]
fn presenter_forgets_baseline_after_partial_write_failure_and_repaints_fully() {
    struct PartialWriteFailOnce {
        bytes: Vec<u8>,
        writes: usize,
        flushes: usize,
    }

    impl Write for PartialWriteFailOnce {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            match self.writes {
                1 => {
                    let written = bytes.len().min(4);
                    self.bytes.extend_from_slice(&bytes[..written]);
                    Ok(written)
                }
                2 => Err(io::Error::other("injected partial write failure")),
                _ => {
                    self.bytes.extend_from_slice(bytes);
                    Ok(bytes.len())
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    let size = TerminalSize::new(1, 3);
    let snapshot = test_snapshot(size, ActiveScreen::Alternate, Revision::new(2));
    let viewport =
        ViewportController::with_layout(ChromeLayout::new(size, ActiveScreen::Alternate), None);
    let status = StatusRenderer::new(
        TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
        size,
    );
    let desired = ComposedFrame::compose(
        &snapshot.surface,
        None,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("test frame composes");
    let mut presenter = DesktopPresenter::default();
    let mut writer = PartialWriteFailOnce {
        bytes: Vec::new(),
        writes: 0,
        flushes: 0,
    };
    let error = presenter
        .present(&mut writer, desired.clone(), None)
        .expect_err("the injected partial write fails");
    assert!(error.to_string().contains("injected partial write failure"));
    assert!(presenter.baseline.is_none());
    assert!(writer.bytes.ends_with(HOST_SYNC_END));

    writer.bytes.clear();
    assert!(
        presenter
            .present(&mut writer, desired, None)
            .expect("retry frame presents")
    );
    assert!(
        find_bytes(&writer.bytes, b"\x1b[2J").is_none(),
        "unknown baseline writes final cells without an intermediate screen erase"
    );
    assert!(presenter.baseline.is_some());
}

#[tokio::test]
async fn lone_escape_is_delivered_without_another_read() {
    let mut codec = HostInputCodec::new();
    assert!(
        codec
            .feed_for_epoch(b"\x1b", 1, 1)
            .expect("escape")
            .is_empty()
    );
    let events = tokio::time::timeout(
        Duration::from_secs(1),
        codec.read(std::future::pending(), 1, 0),
    )
    .await
    .expect("idle escape deadline")
    .expect("read")
    .expect("input");
    assert_eq!(events, [HostInputEvent::Bytes(vec![0x1b])]);
    assert_eq!(
        codec
            .feed_for_epoch(b"]text", 1, 1)
            .expect("ordinary input"),
        [HostInputEvent::Bytes(b"]text".to_vec())]
    );
}

#[test]
fn escape_preserves_the_next_sequence_and_its_input_epoch() {
    let mouse = b"\x1b[<35;2;3M";
    let mut codec = HostInputCodec::new();
    let mut combined = vec![0x1b];
    combined.extend_from_slice(mouse);
    let escape = HostInputEvent::Bytes(vec![0x1b]);
    assert_eq!(
        codec.feed_for_epoch(&combined, 1, 1).expect("same chunk"),
        [escape.clone(), HostInputEvent::Mouse(sgr_mouse(mouse))]
    );
    assert!(
        codec
            .feed_for_epoch(b"\x1b", 1, 1)
            .expect("escape")
            .is_empty()
    );
    assert_eq!(
        codec.feed_for_epoch(PAGE_UP, 1, 1).expect("separate reads"),
        [escape, HostInputEvent::PageUp]
    );
    assert!(
        codec
            .feed_for_epoch(b"\x1b", 1, 1)
            .expect("escape")
            .is_empty()
    );
    assert_eq!(
        codec.feed_for_epoch(b"fresh", 2, 2).expect("new epoch"),
        [HostInputEvent::Bytes(b"fresh".to_vec())]
    );
    assert!(
        codec
            .feed_for_epoch(b"\x1b", 2, 2)
            .expect("escape")
            .is_empty()
    );
    assert_eq!(
        codec
            .feed_for_epoch(b"[5~", 2, 2)
            .expect("fragmented page up"),
        [HostInputEvent::PageUp]
    );
    assert!(
        codec
            .feed_for_epoch(b"\x1b", 2, 2)
            .expect("reply prefix")
            .is_empty()
    );
    assert!(matches!(
        codec
            .feed_for_epoch(b"]11;rgb:f/f/f\x07", 3, 3)
            .expect("host reply")
            .as_slice(),
        [HostInputEvent::TerminalReply {
            reply: HostReply::Color(..),
            ..
        }]
    ));
}

#[test]
fn geometry_generation_retires_mouse_without_discarding_keyboard_or_paste() {
    let mut codec = HostInputCodec::new();
    let stale = codec
        .feed_for_lifetimes(b"key\x1b[<0;2;3M", 7, 7, 1, 3)
        .expect("input");
    assert_eq!(stale, vec![HostInputEvent::Bytes(b"key".to_vec())]);
    assert!(
        codec
            .feed_for_lifetimes(b"\x1b[<0;", 7, 7, 3, 3)
            .expect("partial mouse")
            .is_empty()
    );
    let split = codec
        .feed_for_lifetimes(b"2;3Mnext", 7, 7, 4, 4)
        .expect("geometry changed within mouse");
    assert_eq!(split, vec![HostInputEvent::Bytes(b"next".to_vec())]);
    let fresh = codec
        .feed_for_lifetimes(b"\x1b[<0;2;3M", 7, 7, 4, 4)
        .expect("fresh mouse");
    assert!(matches!(fresh.as_slice(), [HostInputEvent::Mouse(_)]));
    codec
        .feed_for_lifetimes(b"\x1b[200~pre", 7, 7, 4, 4)
        .expect("paste start");
    let paste = codec
        .feed_for_lifetimes(b"edit\x1b[201~", 7, 7, 5, 5)
        .expect("resize in paste");
    assert_eq!(
        paste,
        vec![HostInputEvent::Paste(b"\x1b[200~preedit\x1b[201~".to_vec())]
    );
}

#[test]
fn host_input_routing_is_application_neutral_and_has_one_mouse_owner() {
    let mut codec = HostInputCodec::new();
    let wheel_raw = b"\x1b[<64;5;4M";
    let events = codec.feed(wheel_raw).expect("SGR wheel input");
    let Some(HostInputEvent::Mouse(wheel)) = events.first() else {
        panic!("wheel must decode as one mouse event");
    };
    let shell_modes = TerminalModes::default();
    assert!(history_owns_gestures(ActiveScreen::Main, shell_modes));
    assert_eq!(
        route_mouse_to_child(wheel, ActiveScreen::Main, shell_modes),
        None
    );

    let child_mouse = TerminalModes {
        mouse_mode: TerminalMouseMode::PressRelease,
        mouse_encoding: TerminalMouseEncoding::Sgr,
        ..TerminalModes::default()
    };
    assert!(!history_owns_gestures(ActiveScreen::Main, child_mouse));
    assert_eq!(
        route_mouse_to_child(wheel, ActiveScreen::Main, child_mouse),
        Some(wheel_raw.to_vec())
    );

    let alternate_scroll = TerminalModes {
        alternate_scroll: true,
        application_cursor: true,
        ..TerminalModes::default()
    };
    assert_eq!(
        route_mouse_to_child(wheel, ActiveScreen::Alternate, alternate_scroll),
        Some(b"\x1bOA".to_vec())
    );
}

#[test]
fn host_codec_bounds_and_preserves_fragmented_csi_u_and_malformed_csi() {
    let mut codec = HostInputCodec::new();
    assert!(
        codec
            .feed(b"\x1b[99:67;")
            .expect("bounded CSI-u prefix")
            .is_empty()
    );
    let events = codec.feed(b"6:2u").expect("complete CSI-u event");
    let [HostInputEvent::EnhancedKey(key)] = events.as_slice() else {
        panic!("fragmented enhanced key must decode once");
    };
    assert_eq!(key.raw, b"\x1b[99:67;6:2u");
    assert_eq!(key.kind, KeyEventKind::Repeat);

    let malformed = codec
        .feed(b"\x1b[99;0u")
        .expect("malformed CSI-u input is preserved");
    assert_eq!(
        malformed,
        vec![HostInputEvent::Opaque(b"\x1b[99;0u".to_vec())]
    );
    let malformed_with_etx = codec
        .feed(b"\x1b[99;\x03u")
        .expect("malformed CSI-u containing ETX is preserved");
    assert_eq!(
        malformed_with_etx,
        vec![HostInputEvent::Opaque(b"\x1b[99;\x03u".to_vec())],
        "an ETX inside a malformed framed sequence must not be guessed as a copy key"
    );
    assert_eq!(
        codec.feed(b"a\x03b").expect("legacy bytes decode"),
        vec![
            HostInputEvent::Bytes(b"a".to_vec()),
            HostInputEvent::LegacyCtrlC,
            HostInputEvent::Bytes(b"b".to_vec()),
        ]
    );

    let oversized = [b"\x1b[".as_slice(), &[b'1'; HOST_SEQUENCE_BOUND]].concat();
    let drained = codec
        .feed(&oversized)
        .expect("oversized host sequence drains safely");
    assert!(!drained.is_empty());
}

#[test]
fn typed_console_reports_follow_the_child_keyboard_mode() {
    let legacy = TerminalModes::default();
    let enhanced = TerminalModes {
        keyboard_flags: voytty_core::terminal::TerminalKeyboardFlags::from_bits(11)
            .expect("all keys with event types"),
        ..legacy
    };
    for (report, modes, expected) in [
        (b"\x1b[97;1:1u".as_slice(), legacy, b"a".as_slice()),
        (b"\x1b[97;1:2u", legacy, b"a"),
        (b"\x1b[97;1:3u", legacy, b""),
        (b"\x1b[13;1:1u", legacy, b"\r"),
        (b"\x1b[97;1:3u", enhanced, b"\x1b[97;1:3u"),
    ] {
        let key = EnhancedKey::parse(report.to_vec()).expect("typed key report");
        let KeyboardRoute::Forward { bytes, .. } = route_enhanced_input(
            &key,
            modes,
            HostKeyboard::WindowsConsole,
            false,
            &mut CopyKeyLease::default(),
        ) else {
            panic!("typed key should be forwarded");
        };
        assert_eq!(bytes, expected);
    }
}

#[test]
fn enhanced_keyboard_router_copies_once_and_preserves_forwarding_contracts() {
    let flags_seven = voytty_core::terminal::TerminalKeyboardFlags::from_bits(7)
        .expect("protocol-complete local copy mode");
    let child_legacy = TerminalModes::default();

    let press = EnhancedKey::parse(b"\x1b[99:67;6:1u".to_vec()).expect("Ctrl+Shift+C press");
    let repeat = EnhancedKey::parse(b"\x1b[99:67;6:2u".to_vec()).expect("Ctrl+Shift+C repeat");
    let release = EnhancedKey::parse(b"\x1b[99:67;6:3u".to_vec()).expect("Ctrl+Shift+C release");
    let mut lease = CopyKeyLease::default();
    assert!(matches!(
        route_enhanced_input(
            &press,
            child_legacy,
            HostKeyboard::Terminal(flags_seven),
            true,
            &mut lease
        ),
        KeyboardRoute::Copy
    ));
    assert!(matches!(
        route_enhanced_input(
            &repeat,
            child_legacy,
            HostKeyboard::Terminal(flags_seven),
            true,
            &mut lease
        ),
        KeyboardRoute::Consume
    ));
    assert!(matches!(
        route_enhanced_input(
            &release,
            child_legacy,
            HostKeyboard::Terminal(flags_seven),
            true,
            &mut lease
        ),
        KeyboardRoute::Consume
    ));

    let super_copy = EnhancedKey::parse(b"\x1b[99;9:1u".to_vec()).expect("Super+C press");
    assert!(matches!(
        route_enhanced_input(
            &super_copy,
            child_legacy,
            HostKeyboard::Terminal(flags_seven),
            true,
            &mut CopyKeyLease::default(),
        ),
        KeyboardRoute::Copy
    ));

    let raw = b"\x1b[120;5:1u";
    let ctrl_x = EnhancedKey::parse(raw.to_vec()).expect("Ctrl+X");
    let child_enhanced = TerminalModes {
        keyboard_flags: flags_seven,
        ..TerminalModes::default()
    };
    let KeyboardRoute::Forward {
        bytes,
        clear_selection,
        reinterpret_legacy,
    } = route_enhanced_input(
        &ctrl_x,
        child_enhanced,
        HostKeyboard::Terminal(flags_seven),
        true,
        &mut CopyKeyLease::default(),
    )
    else {
        panic!("matching modes must forward a non-copy key");
    };
    assert_eq!(bytes, raw);
    assert!(clear_selection);
    assert!(!reinterpret_legacy);

    let KeyboardRoute::Forward {
        bytes,
        clear_selection,
        reinterpret_legacy,
    } = route_enhanced_input(
        &ctrl_x,
        child_legacy,
        HostKeyboard::Terminal(flags_seven),
        true,
        &mut CopyKeyLease::default(),
    )
    else {
        panic!("temporary local elevation must downgrade a non-copy key");
    };
    assert_eq!(bytes, vec![0x18]);
    assert!(clear_selection);
    assert!(reinterpret_legacy);

    let raw_unknown = b"\x1b[57358;1:2u";
    let unknown = EnhancedKey::parse(raw_unknown.to_vec()).expect("unknown key");
    let KeyboardRoute::Forward {
        bytes,
        reinterpret_legacy,
        ..
    } = route_enhanced_input(
        &unknown,
        child_legacy,
        HostKeyboard::Terminal(flags_seven),
        true,
        &mut CopyKeyLease::default(),
    )
    else {
        panic!("unknown input must remain forwardable");
    };
    assert_eq!(bytes, raw_unknown);
    assert!(reinterpret_legacy);
    assert_eq!(
        host_events_from_legacy_bytes(bytes),
        vec![HostInputEvent::Bytes(raw_unknown.to_vec())]
    );

    let child_enhanced = TerminalModes {
        keyboard_flags: voytty_core::terminal::TerminalKeyboardFlags::from_bits(9)
            .expect("valid child keyboard flags"),
        ..TerminalModes::default()
    };
    let KeyboardRoute::Forward {
        bytes,
        reinterpret_legacy,
        ..
    } = route_enhanced_input(
        &ctrl_x,
        child_enhanced,
        HostKeyboard::Terminal(voytty_core::terminal::TerminalKeyboardFlags::default()),
        false,
        &mut CopyKeyLease::default(),
    )
    else {
        panic!("an arbitrary physical/child mismatch remains byte preserving");
    };
    assert_eq!(bytes, raw);
    assert!(!reinterpret_legacy);
}

fn sgr_mouse(raw: &[u8]) -> SgrMouse {
    SgrMouse::parse(raw.to_vec()).expect("valid SGR mouse fixture")
}

#[test]
fn pointer_router_selects_exactly_one_owner_for_selection_child_history_and_gutter() {
    let physical = TerminalSize::new(3, 8);
    let layout = ChromeLayout::new(physical, ActiveScreen::Main);
    let snapshot = test_snapshot(layout.child, ActiveScreen::Main, Revision::new(7));
    let mut surface = AttachmentSurface::from_snapshot(&snapshot).expect("valid pointer surface");
    let mut viewport = ViewportController::with_layout(layout, snapshot.surface.scroll_metrics);
    let mut selection = SelectionController::default();

    assert!(matches!(
        route_pointer(
            &sgr_mouse(b"\x1b[<0;1;1M"),
            &mut viewport,
            &surface,
            &mut selection,
            true,
        )
        .expect("route selection press"),
        PointerRoute::SelectionChanged
    ));
    assert!(matches!(
        route_pointer(
            &sgr_mouse(b"\x1b[<32;3;1M"),
            &mut viewport,
            &surface,
            &mut selection,
            true,
        )
        .expect("route selection motion"),
        PointerRoute::SelectionChanged
    ));
    assert!(matches!(
        route_pointer(
            &sgr_mouse(b"\x1b[<0;3;1m"),
            &mut viewport,
            &surface,
            &mut selection,
            true,
        )
        .expect("route selection release"),
        PointerRoute::SelectionChanged
    ));
    assert!(selection.is_finalized());

    assert!(matches!(
        route_pointer(
            &sgr_mouse(b"\x1b[<64;2;2M"),
            &mut viewport,
            &surface,
            &mut selection,
            true,
        )
        .expect("route history wheel"),
        PointerRoute::Viewport(_)
    ));
    assert!(!selection.is_finalized());

    surface.surface.modes.mouse_mode = TerminalMouseMode::PressRelease;
    surface.surface.modes.mouse_encoding = TerminalMouseEncoding::Sgr;
    let child_raw = b"\x1b[<0;2;2M";
    assert!(matches!(
        route_pointer(
            &sgr_mouse(child_raw),
            &mut viewport,
            &surface,
            &mut selection,
            true,
        )
        .expect("route child press after local selection"),
        PointerRoute::Ignore
    ));

    let mut viewport = ViewportController::with_layout(layout, snapshot.surface.scroll_metrics);
    let mut selection = SelectionController::default();
    assert!(matches!(
        route_pointer(
            &sgr_mouse(child_raw),
            &mut viewport,
            &surface,
            &mut selection,
            true,
        )
        .expect("route child press"),
        PointerRoute::Child(bytes) if bytes == child_raw
    ));

    surface.surface.modes = TerminalModes::default();
    let gutter = layout.gutter_column.expect("main layout gutter");
    assert!(matches!(
        route_pointer(
            &sgr_mouse(format!("\x1b[<0;{gutter};2M").as_bytes()),
            &mut viewport,
            &surface,
            &mut selection,
            true,
        )
        .expect("route gutter press"),
        PointerRoute::Viewport(_)
    ));
    assert!(viewport.gutter_drag_active());
    assert!(matches!(
        route_pointer(
            &sgr_mouse(b"\x1b[<32;1;1M"),
            &mut viewport,
            &surface,
            &mut selection,
            true,
        )
        .expect("route captured gutter motion"),
        PointerRoute::Viewport(_)
    ));
}

#[test]
fn pointer_router_preserves_modified_native_selection_and_cancelled_drag_capture() {
    let physical = TerminalSize::new(2, 7);
    let layout = ChromeLayout::new(physical, ActiveScreen::Main);
    let snapshot = test_snapshot(layout.child, ActiveScreen::Main, Revision::new(3));
    let surface = AttachmentSurface::from_snapshot(&snapshot).expect("valid pointer surface");
    let mut viewport = ViewportController::with_layout(layout, snapshot.surface.scroll_metrics);
    let mut selection = SelectionController::default();

    assert!(matches!(
        route_pointer(
            &sgr_mouse(b"\x1b[<4;1;1M"),
            &mut viewport,
            &surface,
            &mut selection,
            true,
        )
        .expect("route modified native selection"),
        PointerRoute::Ignore
    ));
    assert!(matches!(
        route_pointer(
            &sgr_mouse(b"\x1b[<0;1;1M"),
            &mut viewport,
            &surface,
            &mut selection,
            true,
        )
        .expect("route cancellable selection press"),
        PointerRoute::SelectionChanged
    ));
    selection.reconcile(None);
    assert!(selection.owns_pointer_sequence());
    assert!(matches!(
        route_pointer(
            &sgr_mouse(b"\x1b[<0;1;1m"),
            &mut viewport,
            &surface,
            &mut selection,
            true,
        )
        .expect("route cancelled selection release"),
        PointerRoute::SelectionChanged
    ));
    assert!(!selection.owns_pointer_sequence());
}

#[test]
fn pointer_router_invalidates_a_pinned_selection_before_history_navigation() {
    let physical = TerminalSize::new(3, 8);
    let layout = ChromeLayout::new(physical, ActiveScreen::Main);
    let snapshot = test_snapshot(layout.child, ActiveScreen::Main, Revision::new(7));
    let surface = AttachmentSurface::from_snapshot(&snapshot).expect("valid history surface");
    let mut viewport = ViewportController::with_layout(layout, snapshot.surface.scroll_metrics);
    let ViewportEffect::RequestHistoryWindow(query) = viewport.navigate(true, 1) else {
        panic!("initial history navigation requests a window");
    };
    let shape = query
        .response_shape(query.anchor)
        .expect("valid query shape");
    let rows = (0..shape.row_count)
        .map(|index| {
            test_row(
                query.anchor.viewport.columns,
                &format!("history-{index}"),
                TerminalStyle::default(),
            )
        })
        .collect();
    assert!(matches!(
        viewport
            .apply_view_history_window(TerminalSurfaceHistoryWindowResult::Frame(
                TerminalSurfaceHistoryWindowFrame {
                    colors: Default::default(),

                    disposition: shape.disposition,
                    anchor: query.anchor,
                    target_offset_from_bottom: shape.target_offset_from_bottom,
                    first_row_from_live_top: shape.first_row_from_live_top,
                    rows,
                },
            ))
            .expect("install history fixture"),
        ViewportEffect::Render | ViewportEffect::RenderAndRequestHistoryWindow(_)
    ));
    viewport.observe_presentation();

    let source = viewport
        .selection_source_identity(&surface)
        .expect("presented history identity");
    let rows = viewport
        .selection_rows(&surface)
        .expect("presented history rows");
    let mut selection = SelectionController::default();
    selection
        .begin(source, TerminalTextPoint::new(0, 0), rows)
        .expect("begin history selection");
    selection
        .update(source, TerminalTextPoint::new(0, 1), rows)
        .expect("extend history selection");
    selection.finish();
    assert!(selection.is_finalized());

    assert!(matches!(
        route_pointer(
            &sgr_mouse(b"\x1b[<64;2;2M"),
            &mut viewport,
            &surface,
            &mut selection,
            true,
        )
        .expect("route pinned history wheel"),
        PointerRoute::Viewport(_)
    ));
    assert!(
        !selection.is_finalized(),
        "viewport navigation must retire coordinates from the previously presented slice"
    );
}

#[test]
fn generic_nested_tui_wheel_update_preserves_the_styled_rightmost_cell() {
    let size = TerminalSize::new(1, 5);
    let child_mouse = TerminalModes {
        mouse_mode: TerminalMouseMode::PressRelease,
        mouse_encoding: TerminalMouseEncoding::Sgr,
        ..TerminalModes::default()
    };
    let mut snapshot = test_snapshot(size, ActiveScreen::Alternate, Revision::new(2));
    snapshot.surface.modes = child_mouse;
    snapshot.surface.rows[0].cells[4] = TerminalCell {
        contents: "x".to_owned(),
        style: TerminalStyle {
            foreground: TerminalColor::Indexed(2),
            ..TerminalStyle::default()
        },
        ..TerminalCell::default()
    };
    let viewport =
        ViewportController::with_layout(ChromeLayout::new(size, ActiveScreen::Alternate), None);
    let status = StatusRenderer::new(
        TerminalViewTarget::for_display("local", TerminalViewRoute::Local),
        size,
    );
    let initial = ComposedFrame::compose(
        &snapshot.surface,
        None,
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("compose generic nested TUI frame");
    let mut presenter = DesktopPresenter::default();
    let mut output = ViewportFrameWriter::default();
    presenter
        .present(&mut output, initial, None)
        .expect("present initial right-edge cell");

    let wheel_raw = b"\x1b[<64;5;1M";
    let Some(HostInputEvent::Mouse(wheel)) = HostInputCodec::new()
        .feed(wheel_raw)
        .expect("decode generic nested TUI wheel")
        .into_iter()
        .next()
    else {
        panic!("wheel must decode as one mouse report");
    };
    assert!(!history_owns_gestures(ActiveScreen::Alternate, child_mouse));
    assert_eq!(
        route_mouse_to_child(&wheel, ActiveScreen::Alternate, child_mouse),
        Some(wheel_raw.to_vec())
    );

    snapshot.surface.rows[0].cells[4].contents = "y".to_owned();
    snapshot.surface.rows[0].cells[4].style.background = TerminalColor::Rgb(1, 2, 3);
    let updated = ComposedFrame::compose(
        &snapshot.surface,
        presenter.baseline.as_ref(),
        &viewport,
        &status,
        TerminalViewTransportState::Active,
    )
    .expect("compose child-owned wheel update");
    output = ViewportFrameWriter::default();
    presenter
        .present(&mut output, updated, None)
        .expect("present child-owned wheel update");
    assert_eq!((output.writes, output.flushes), (1, 1));
    assert!(find_bytes(&output.bytes, b"\x1b[1;5H").is_some());
    assert!(output.bytes.contains(&b'y'));
    assert!(find_bytes(&output.bytes, b"\x1b[K").is_none());
    assert!(find_bytes(&output.bytes, b"\x1b[2K").is_none());
}

#[test]
fn session_end_projects_fixed_content_free_normal_mode_diagnostics() {
    let natural = terminal_end_completion(TerminalViewEndReason::NaturalExit)
        .expect("natural end is a successful terminal completion");
    assert_eq!(
        natural,
        TerminalCompletion::SessionEnded(TerminalViewEndReason::NaturalExit)
    );
    let diagnostic = completion_diagnostic(natural)
        .expect("natural diagnostic")
        .expect("natural end is visible");
    assert!(!diagnostic.contains(&b'\x1b'));
    assert!(!diagnostic.windows(4).any(|window| window == b"cwd="));
    assert!(!diagnostic.windows(6).any(|window| window == b"route="));
    assert!(!diagnostic.windows(7).any(|window| window == b"ticket="));

    let error = terminal_end_completion(TerminalViewEndReason::DriverFailure)
        .expect_err("driver failure remains a typed CLI error");
    assert!(matches!(error, CliError::TerminalDriverFailure));
    assert_eq!(error.to_string(), "the daemon-owned terminal driver failed");
    assert!(matches!(
        completion_diagnostic(TerminalCompletion::SessionEnded(
            TerminalViewEndReason::DriverFailure
        )),
        Err(CliError::TerminalDriverFailure)
    ));
}

#[test]
fn created_session_attach_failure_retains_the_exact_live_session_identity() {
    let session_id = voytty_core::SessionId::from_array([0xc1; 16]);
    let source = DaemonError::new(
        DomainErrorKind::SessionOccupied,
        "the exact created Session could not be attached",
    );
    let error = preserve_created_session::<()>(session_id, Err(source))
        .expect_err("attach failure preserves the committed create");
    assert!(matches!(
        error,
        CliError::CreatedSessionAttach {
            session_id: actual,
            source,
        } if actual == session_id && source.kind() == DomainErrorKind::SessionOccupied
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_stateful_prepare_preserves_every_exact_terminal_result() {
    let session_id = voytty_core::SessionId::from_array([0xc2; 16]);
    let dropped = Arc::new(AtomicBool::new(false));
    let (submitted_sender, submitted_receiver) = tokio::sync::oneshot::channel();
    let (release_sender, release_receiver) = tokio::sync::oneshot::channel();
    let drop_probe = FutureDropProbe(Arc::clone(&dropped));
    let mut operation = Box::pin(async move {
        let _ = submitted_sender.send(());
        let _ = release_receiver.await;
        drop(drop_probe);
        Ok::<SessionId, CliError>(session_id)
    });
    poll_once_pending(operation.as_mut()).await;
    submitted_receiver
        .await
        .expect("stateful operation crossed its submission barrier");

    let mut finishing = Box::pin(finish_submitted_after_cancellation(
        operation.as_mut(),
        InactiveCancellation::LocalDetach,
    ));
    poll_once_pending(finishing.as_mut()).await;
    assert!(
        !dropped.load(Ordering::SeqCst),
        "cancellation must retain the submitted operation future"
    );
    release_sender
        .send(())
        .expect("release exact committed result");
    let exact = finishing.await.expect("exact committed result");
    assert!(matches!(
        exact,
        InactiveWait::CompletedAfterCancellation {
            value,
            cancellation: InactiveCancellation::LocalDetach,
        } if value == session_id
    ));
    assert!(dropped.load(Ordering::SeqCst));

    let created_error = CliError::CreatedSessionAttach {
        session_id,
        source: DaemonError::new(
            DomainErrorKind::SessionOccupied,
            "the exact created Session remains live",
        ),
    };
    let created_error = cancelled_submitted_error(created_error).await;
    assert!(matches!(
        created_error,
        CliError::CreatedSessionAttach {
            session_id: actual,
            source,
        } if actual == session_id && source.kind() == DomainErrorKind::SessionOccupied
    ));

    let unknown = cancelled_submitted_error(CliError::Daemon(DaemonError::new(
        DomainErrorKind::OperationOutcomeUnknown,
        "the submitted Session result could not be proven",
    )))
    .await;
    assert!(matches!(
        unknown,
        CliError::Daemon(error)
            if error.kind() == DomainErrorKind::OperationOutcomeUnknown
    ));
}

#[test]
fn cancelled_prepared_session_identity_is_user_visible_and_truthful() {
    let session_id = voytty_core::SessionId::from_array([0xc3; 16]);
    let completion =
        inactive_cancellation_result(InactiveCancellation::LocalDetach, Some(session_id))
            .expect("prefix detach remains a successful completion");
    let diagnostic = completion_diagnostic(completion)
        .expect("safe completion projection")
        .expect("prepared detach is user-visible");
    let diagnostic = String::from_utf8(diagnostic).expect("ASCII completion diagnostic");
    assert!(diagnostic.contains(&session_id.to_string()));
    assert!(diagnostic.contains("remains live"));

    let signal = inactive_cancellation_result(
        InactiveCancellation::Signal(TerminalSignalCancellation::new("SIGTERM", Some(()))),
        Some(session_id),
    )
    .expect_err("external signal retains cancelled semantics");
    assert!(matches!(
        signal,
        CliError::Daemon(ref error) if error.kind() == DomainErrorKind::Cancelled
    ));
    assert!(signal.to_string().contains(&session_id.to_string()));
    assert!(signal.to_string().contains("remains live"));
}

struct FutureDropProbe(Arc<AtomicBool>);

impl Drop for FutureDropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn cancelled_submitted_error(error: CliError) -> CliError {
    let dropped = Arc::new(AtomicBool::new(false));
    let (submitted_sender, submitted_receiver) = tokio::sync::oneshot::channel();
    let (release_sender, release_receiver) = tokio::sync::oneshot::channel();
    let drop_probe = FutureDropProbe(Arc::clone(&dropped));
    let mut operation = Box::pin(async move {
        let _ = submitted_sender.send(());
        let _ = release_receiver.await;
        drop(drop_probe);
        Err::<SessionId, CliError>(error)
    });
    poll_once_pending(operation.as_mut()).await;
    submitted_receiver
        .await
        .expect("error operation crossed its submission barrier");

    let mut finishing = Box::pin(finish_submitted_after_cancellation(
        operation.as_mut(),
        InactiveCancellation::LocalDetach,
    ));
    poll_once_pending(finishing.as_mut()).await;
    assert!(!dropped.load(Ordering::SeqCst));
    release_sender
        .send(())
        .expect("release exact terminal error");
    let result = finishing.await;
    assert!(dropped.load(Ordering::SeqCst));
    match result {
        Err(error) => error,
        Ok(_) => panic!("submitted error unexpectedly became success"),
    }
}

async fn poll_once_pending<F: Future>(mut future: Pin<&mut F>) {
    std::future::poll_fn(|context| match future.as_mut().poll(context) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("barrier future completed before its release"),
    })
    .await;
}

#[test]
fn terminal_guard_restores_only_owned_appearance_subscription() {
    for owned in [false, true] {
        let pty = openpty(None, None).expect("appearance guard PTY");
        let mut guard = TerminalGuard::enter(&pty.slave, &pty.slave).expect("enter guard");
        guard.appearance_owned.store(owned, Ordering::Release);
        guard.restore().expect("restore guard");
        fcntl(pty.master.as_raw_fd(), FcntlArg::F_SETFL(OFlag::O_NONBLOCK))
            .expect("nonblocking capture");
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            match rustix::io::read(&pty.master, &mut buffer) {
                Ok(0) | Err(rustix::io::Errno::AGAIN) => break,
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                Err(error) => panic!("read guard output: {error}"),
            }
        }
        assert_eq!(find_bytes(&bytes, b"\x1b[?2031l").is_some(), owned);
        assert_eq!(
            bytes.windows(2).filter(|bytes| *bytes == b"\x1b]").count(),
            1
        );
        assert!(find_bytes(&bytes, b"\x1b]8;;\x1b\\").is_some());
        assert!(find_bytes(&bytes, b"\x1b[22;2t").is_some());
        assert!(find_bytes(&bytes, b"\x1b[23;2t").is_some());
    }
}
#[test]
fn terminal_guard_restores_attributes_on_success_error_cancel_and_panic() {
    for path in [
        GuardPath::Success,
        GuardPath::Error,
        GuardPath::Cancel,
        GuardPath::Panic,
    ] {
        let pty = openpty(None, None).expect("open PTY");
        let original = tcgetattr(&pty.slave).expect("original attributes");
        match path {
            GuardPath::Success => {
                let mut guard = TerminalGuard::enter(&pty.slave, &pty.slave).expect("guard");
                assert_descriptor_cloexec(&guard.input);
                assert_descriptor_cloexec(&guard.output);
                guard.restore().expect("restore");
            }
            GuardPath::Error => {
                fn fail(input: &OwnedFd) -> Result<(), CliError> {
                    let _guard = TerminalGuard::enter(input, input)?;
                    Err(CliError::Io("fixture error".to_owned()))
                }
                assert!(fail(&pty.slave).is_err());
            }
            GuardPath::Cancel => {
                let guard = TerminalGuard::enter(&pty.slave, &pty.slave).expect("guard");
                drop(guard);
            }
            GuardPath::Panic => {
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    let _guard = TerminalGuard::enter(&pty.slave, &pty.slave).expect("guard");
                    panic!("fixture panic");
                }));
                assert!(result.is_err());
            }
        }
        assert_restored_termios(
            tcgetattr(&pty.slave).expect("restored attributes"),
            original,
        );
        let mut master = File::from(pty.master);
        let mut cleanup = vec![0_u8; ENTER_TERMINAL_UI.len() + RESTORE_TERMINAL_UI.len()];
        master.read_exact(&mut cleanup).expect("terminal UI bytes");
        assert_eq!(cleanup, [ENTER_TERMINAL_UI, RESTORE_TERMINAL_UI].concat());
    }
}

#[tokio::test]
async fn task_abort_restores_termios_and_terminal_ui_after_an_entry_barrier() {
    let pty = openpty(None, None).expect("open cancellation PTY");
    let mut master = File::from(pty.master);
    let slave = File::from(pty.slave);
    let probe = slave.try_clone().expect("cancellation termios probe");
    let original = tcgetattr(&probe).expect("cancellation original attributes");
    let task_terminal = slave.try_clone().expect("cancellation task terminal");
    let (entered_sender, entered_receiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _guard = TerminalGuard::enter(&task_terminal, &task_terminal).expect("task guard");
        let _ = entered_sender.send(());
        std::future::pending::<()>().await;
    });

    entered_receiver
        .await
        .expect("task crossed the raw-terminal entry barrier");
    task.abort();
    assert!(
        task.await
            .expect_err("aborted terminal task")
            .is_cancelled(),
        "the task must end through Tokio cancellation"
    );
    assert_restored_termios(
        tcgetattr(&probe).expect("task-cancel restored attributes"),
        original,
    );
    let mut cleanup = vec![0_u8; ENTER_TERMINAL_UI.len() + RESTORE_TERMINAL_UI.len()];
    master
        .read_exact(&mut cleanup)
        .expect("task-cancel terminal UI bytes");
    assert_eq!(cleanup, [ENTER_TERMINAL_UI, RESTORE_TERMINAL_UI].concat());
}

#[cfg(not(target_os = "macos"))]
fn assert_restored_termios(actual: Termios, expected: Termios) {
    assert_eq!(actual, expected);
}

#[cfg(target_os = "macos")]
fn assert_restored_termios(actual: Termios, expected: Termios) {
    // The macOS PTY kernel may maintain PENDIN independently when
    // attributes are read back. Production still restores the exact
    // saved Termios object with TCSANOW; only this assertion ignores
    // that one kernel-owned bit while comparing every stable field.
    assert_eq!(actual.input_flags, expected.input_flags);
    assert_eq!(actual.output_flags, expected.output_flags);
    assert_eq!(actual.control_flags, expected.control_flags);
    assert_eq!(
        actual.local_flags - LocalFlags::PENDIN,
        expected.local_flags - LocalFlags::PENDIN
    );
    assert_eq!(actual.control_chars, expected.control_chars);
    assert_eq!(cfgetispeed(&actual), cfgetispeed(&expected));
    assert_eq!(cfgetospeed(&actual), cfgetospeed(&expected));
}

#[derive(Clone, Copy)]
enum GuardPath {
    Success,
    Error,
    Cancel,
    Panic,
}

#[test]
fn stdin_pump_wakes_and_joins_without_changing_stdin_status_flags() {
    let pty = openpty(None, None).expect("open PTY");
    let original = OFlag::from_bits_truncate(
        fcntl(pty.slave.as_raw_fd(), FcntlArg::F_GETFL).expect("original flags"),
    );
    let mut pump = StdinPump::start(&pty.slave, InputEpoch::new()).expect("stdin pump");
    assert_descriptor_cloexec(&pump._cancellation_read_guard);
    assert_descriptor_cloexec(
        pump.cancellation_write
            .as_ref()
            .expect("cancellation writer"),
    );
    let while_running = OFlag::from_bits_truncate(
        fcntl(pty.slave.as_raw_fd(), FcntlArg::F_GETFL).expect("running flags"),
    );
    assert_eq!(while_running, original);
    pump.shutdown().expect("pump shutdown");
    let after_join = OFlag::from_bits_truncate(
        fcntl(pty.slave.as_raw_fd(), FcntlArg::F_GETFL).expect("joined flags"),
    );
    assert_eq!(after_join, original);
}

fn assert_descriptor_cloexec(fd: &impl AsFd) {
    let flags = rustix::io::fcntl_getfd(fd).expect("descriptor flags");
    assert!(
        flags.contains(rustix::io::FdFlags::CLOEXEC),
        "owned terminal descriptors must not survive exec"
    );
}

#[test]
fn signals_restore_termios_and_ui_before_the_normal_mode_diagnostic() {
    if std::env::var_os("VOYTTY_TERMINAL_SIGNAL_CHILD").is_some() {
        return;
    }
    for signal in [
        NixSignal::SIGINT,
        NixSignal::SIGTERM,
        NixSignal::SIGHUP,
        NixSignal::SIGTSTP,
    ] {
        assert_signal_restoration(signal);
    }
}

fn assert_signal_restoration(signal: NixSignal) {
    let pty = openpty(None, None).expect("open signal PTY");
    let master = File::from(pty.master);
    let slave = File::from(pty.slave);
    let probe = slave.try_clone().expect("signal termios probe");
    let original = tcgetattr(&probe).expect("signal original termios");
    let child_stdin = slave.try_clone().expect("signal child stdin");
    let child_stdout = slave.try_clone().expect("signal child stdout");
    let child_stderr = slave.try_clone().expect("signal child stderr");
    let mut child = Command::new(std::env::current_exe().expect("current test binary"))
        .arg("--exact")
        .arg("terminal_ui::desktop::tests::signal_restore_child")
        .arg("--ignored")
        .arg("--nocapture")
        .env("VOYTTY_TERMINAL_SIGNAL_CHILD", "1")
        .stdin(Stdio::from(child_stdin))
        .stdout(Stdio::from(child_stdout))
        .stderr(Stdio::from(child_stderr))
        .spawn()
        .expect("spawn isolated signal child");
    drop(slave);

    let (entered_sender, entered_receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut master = master;
        let mut bytes = Vec::new();
        let mut entered = false;
        let mut buffer = [0_u8; 1024];
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    bytes.extend_from_slice(&buffer[..read]);
                    if !entered && contains_bytes(&bytes, ENTER_TERMINAL_UI) {
                        entered = true;
                        let _ = entered_sender.send(());
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if linux_pty_master_closed(&error) => break,
                Err(error) => panic!("read signal child PTY: {error}"),
            }
        }
        bytes
    });

    if entered_receiver
        .recv_timeout(Duration::from_secs(5))
        .is_err()
    {
        let _ = child.kill();
        let _ = child.wait();
        drop(probe);
        let _ = reader.join();
        panic!("signal child did not enter raw terminal mode");
    }
    kill(Pid::from_raw(child.id() as i32), signal).expect("send terminal signal");
    let status = wait_for_signal_child(&mut child);
    assert_restored_termios(
        tcgetattr(&probe).expect("signal restored termios"),
        original,
    );
    drop(probe);
    let bytes = reader.join().expect("signal PTY reader");
    assert!(status.success(), "isolated signal child failed: {signal:?}");
    let restore =
        find_bytes(&bytes, RESTORE_TERMINAL_UI).expect("signal cleanup bytes are written");
    let diagnostic = find_bytes(&bytes, b"VOYTTY_SAFE_SIGNAL_DIAGNOSTIC")
        .expect("normal-mode signal diagnostic is written");
    assert!(
        restore < diagnostic,
        "terminal cleanup must precede the normal-mode diagnostic"
    );
}

fn wait_for_signal_child(child: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().expect("poll signal child") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait().expect("reap timed-out signal child");
            panic!("signal child did not exit before its deadline: {status}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    find_bytes(haystack, needle).is_some()
}

fn linux_pty_master_closed(error: &io::Error) -> bool {
    cfg!(target_os = "linux") && error.raw_os_error() == Some(nix::errno::Errno::EIO as i32)
}

#[test]
#[ignore = "isolated terminal-signal helper"]
fn signal_restore_child() {
    if std::env::var_os("VOYTTY_TERMINAL_SIGNAL_CHILD").is_none() {
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("signal child runtime");
    let result = runtime.block_on(async {
        let TerminalSignals {
            resize: _resize,
            mut interrupt,
            mut terminate,
            mut hangup,
            mut suspend,
        } = TerminalSignals::install()?;
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut guard = TerminalGuard::enter(&stdin, &stdout)?;
        let (cancellation_sender, mut cancellation_receiver) = watch::channel(None);
        let result = select_terminal_termination(
            async move {
                let cancellation = receive_terminal_cancellation(&mut cancellation_receiver).await;
                Err(cancellation.error(None))
            },
            &mut interrupt,
            &mut terminate,
            &mut hangup,
            &mut suspend,
            cancellation_sender,
        )
        .await;
        guard.restore()?;
        result
    });
    let error = result.expect_err("signal interrupts the guarded view");
    assert!(matches!(
        error,
        CliError::Daemon(ref error) if error.kind() == DomainErrorKind::Cancelled
    ));
    write_all_fd(&io::stderr(), b"VOYTTY_SAFE_SIGNAL_DIAGNOSTIC")
        .expect("write normal-mode diagnostic marker");
}

#[test]
fn panic_restores_termios_and_ui_before_the_normal_mode_diagnostic() {
    if std::env::var_os("VOYTTY_TERMINAL_PANIC_CHILD").is_some() {
        return;
    }
    let pty = openpty(None, None).expect("open panic PTY");
    let master = File::from(pty.master);
    let slave = File::from(pty.slave);
    let probe = slave.try_clone().expect("panic termios probe");
    let original = tcgetattr(&probe).expect("panic original termios");
    let mut child = Command::new(std::env::current_exe().expect("current test binary"))
        .arg("--exact")
        .arg("terminal_ui::desktop::tests::panic_restore_child")
        .arg("--ignored")
        .arg("--nocapture")
        .env("VOYTTY_TERMINAL_PANIC_CHILD", "1")
        .stdin(Stdio::from(slave.try_clone().expect("panic child stdin")))
        .stdout(Stdio::from(slave.try_clone().expect("panic child stdout")))
        .stderr(Stdio::from(slave.try_clone().expect("panic child stderr")))
        .spawn()
        .expect("spawn isolated panic child");
    drop(slave);

    let reader = std::thread::spawn(move || {
        let mut master = master;
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => bytes.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if linux_pty_master_closed(&error) => break,
                Err(error) => panic!("read panic child PTY: {error}"),
            }
        }
        bytes
    });
    let status = wait_for_signal_child(&mut child);
    assert_restored_termios(tcgetattr(&probe).expect("panic restored termios"), original);
    drop(probe);
    let bytes = reader.join().expect("panic PTY reader");
    assert!(status.success(), "isolated panic child failed");
    let restore = find_bytes(&bytes, RESTORE_TERMINAL_UI).expect("panic cleanup bytes are written");
    let diagnostic = find_bytes(&bytes, b"VOYTTY_SAFE_PANIC_DIAGNOSTIC")
        .expect("normal-mode panic diagnostic is written");
    assert!(
        restore < diagnostic,
        "terminal cleanup must precede the panic diagnostic"
    );
}

#[test]
#[ignore = "isolated terminal-panic helper"]
fn panic_restore_child() {
    if std::env::var_os("VOYTTY_TERMINAL_PANIC_CHILD").is_none() {
        return;
    }
    let panic_hook = ScopedPanicHook::suppress();
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let _guard = TerminalGuard::enter(&stdin, &stdout).expect("panic child guard");
        panic!("redacted terminal fixture panic");
    }));
    assert!(result.is_err());
    drop(panic_hook);
    write_all_fd(&io::stderr(), b"VOYTTY_SAFE_PANIC_DIAGNOSTIC")
        .expect("write normal-mode panic diagnostic marker");
}

#[test]
fn panic_hook_restoration_is_proved_in_an_isolated_process() {
    if std::env::var_os("VOYTTY_PANIC_HOOK_CHILD").is_some() {
        return;
    }
    let status = Command::new(std::env::current_exe().expect("current test binary"))
        .arg("--exact")
        .arg("terminal_ui::desktop::tests::panic_hook_child")
        .arg("--ignored")
        .env("VOYTTY_PANIC_HOOK_CHILD", "1")
        .status()
        .expect("run isolated panic-hook child");
    assert!(status.success());
}

#[test]
#[ignore = "isolated panic-hook helper"]
fn panic_hook_child() {
    if std::env::var_os("VOYTTY_PANIC_HOOK_CHILD").is_none() {
        return;
    }
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let process_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {
        CALLS.fetch_add(1, Ordering::SeqCst);
    }));
    {
        let _suppressed = ScopedPanicHook::suppress();
        let result = std::panic::catch_unwind(|| panic!("redacted fixture panic"));
        assert!(result.is_err());
    }
    let result = std::panic::catch_unwind(|| panic!("post-restore fixture panic"));
    assert!(result.is_err());
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    let custom = std::panic::take_hook();
    drop(custom);
    std::panic::set_hook(process_hook);
}

#[test]
fn suppressed_panic_hook_records_process_panicked_diagnostics_without_payload() {
    if std::env::var_os("VOYTTY_PANIC_DIAGNOSTICS_CHILD").is_some() {
        return;
    }
    let output = Command::new(std::env::current_exe().expect("current test binary"))
        .arg("--exact")
        .arg("terminal_ui::desktop::tests::panic_diagnostics_child")
        .arg("--ignored")
        .env("VOYTTY_PANIC_DIAGNOSTICS_CHILD", "1")
        .output()
        .expect("run isolated panic-diagnostics child");
    assert!(
        output.status.success(),
        "isolated panic-diagnostics child failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "isolated panic-diagnostics helper"]
fn panic_diagnostics_child() {
    if std::env::var_os("VOYTTY_PANIC_DIAGNOSTICS_CHILD").is_none() {
        return;
    }
    struct CaptureSink(Mutex<Vec<String>>);
    impl voytty_diagnostics::Sink for CaptureSink {
        fn append(&self, detail: bool, bytes: &[u8]) -> io::Result<()> {
            assert!(!detail, "ProcessPanicked must record on the primary lane");
            self.0
                .lock()
                .expect("capture sink")
                .push(String::from_utf8_lossy(bytes).into_owned());
            Ok(())
        }
        fn control(&self) -> io::Result<voytty_diagnostics::Control> {
            Ok(voytty_diagnostics::Control::disabled())
        }
    }

    let captured = Arc::new(CaptureSink(Mutex::new(Vec::new())));
    let recorder =
        voytty_diagnostics::Recorder::new(captured.clone()).expect("diagnostic recorder");
    assert!(
        voytty_diagnostics::install(recorder),
        "isolated child owns global diagnostic installation"
    );
    static PREVIOUS_CALLS: AtomicUsize = AtomicUsize::new(0);
    let process_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {
        PREVIOUS_CALLS.fetch_add(1, Ordering::SeqCst);
    }));
    {
        let _suppressed = ScopedPanicHook::suppress();
        let result = std::panic::catch_unwind(|| panic!("TOP_SECRET_TERMINAL_PANIC_PAYLOAD"));
        assert!(result.is_err());
    }
    let custom = std::panic::take_hook();
    drop(custom);
    std::panic::set_hook(process_hook);
    assert_eq!(PREVIOUS_CALLS.load(Ordering::SeqCst), 0);
    let records = captured.0.lock().expect("capture sink");
    assert_eq!(records.len(), 1, "expected one ProcessPanicked record");
    let decoded =
        voytty_diagnostics::Record::decode(records[0].as_bytes()).expect("valid diagnostic JSON");
    assert_eq!(decoded.event, voytty_diagnostics::Kind::ProcessPanicked);
    assert_eq!(decoded.level, voytty_diagnostics::Level::Error);
    assert!(!records[0].contains("TOP_SECRET_TERMINAL_PANIC_PAYLOAD"));
}
