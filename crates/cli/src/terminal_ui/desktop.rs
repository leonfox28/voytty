//! Interactive session lifecycle and rendering for Unix and Windows consoles.

use std::collections::VecDeque;
use std::future::Future;
use std::io::{self, IsTerminal, Write};
#[cfg(unix)]
use std::os::fd::{AsFd, OwnedFd};
#[cfg(windows)]
use std::os::windows::io::AsHandle as AsFd;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
#[cfg(all(test, unix))]
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(all(test, unix))]
use std::sync::mpsc::{Receiver as TestReceiver, SyncSender as TestSender, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
#[cfg(unix)]
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
#[cfg(unix)]
use rustix::event::{PollFd, PollFlags, poll};
#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::{mpsc, watch};
use voytty_client::progress::{ProgressFailure, ProgressHistory, ProgressObserver};
#[cfg(all(test, unix))]
use voytty_core::Revision;
use voytty_core::terminal::{
    ActiveScreen, TerminalHistoryWindowAnchor, TerminalHistoryWindowQuery, TerminalModes,
    TerminalMouseEncoding, TerminalMouseMode, TerminalScrollMetrics, TerminalSize,
    TerminalSurfaceDelta, TerminalSurfaceHistoryWindowResult, TerminalSurfaceRow,
    TerminalSurfaceSnapshot, TerminalViewportDisposition,
};
#[cfg(all(test, unix))]
use voytty_core::terminal::{
    TerminalCell, TerminalColor, TerminalCursor, TerminalStyle, TerminalSurface,
    TerminalSurfaceHistoryWindowFrame, TerminalSurfaceRowPatch,
};
use voytty_core::terminal_selection::{TerminalTextPoint, TerminalTextSelectionError};
use voytty_core::viewport_cache::{
    CachedViewportWindow, ViewportAnchorObservation, ViewportCache, ViewportCacheUpdate,
};
use voytty_core::{DomainErrorKind, SessionId, connection_progress::ConnectionStage};
use voytty_daemon::client::view::{
    PreparedTerminalView, TerminalViewConnectionPath, TerminalViewConnectionStatus,
    TerminalViewEndReason, TerminalViewEvent, TerminalViewHistoryWindow, TerminalViewRoute,
    TerminalViewTarget, TerminalViewTransportState,
};
use voytty_daemon::error::DaemonError;
use voytty_daemon::operations::LocalRuntime;

use super::super::{CliError, TerminalRequest, TerminalRequestKind};

mod startup;
use startup::{InactivePresentation, StartupProgress};
#[cfg(windows)]
mod windows_console;
#[cfg(windows)]
use windows_console::{Signal, StdinPump, TerminalGuard, terminal_size, write_all_fd};

mod host_colors;
use host_colors::{HostColors, HostReply};

#[path = "session.rs"]
mod ui_session;
use ui_session::TerminalUiSession;

mod upload;

mod ansi_presenter;
#[path = "surface.rs"]
mod attachment_surface;
mod composition;
mod keyboard;
mod prefix;
use prefix::{CommandMode, LocalCommand, PrefixAction};

mod selection;

use ansi_presenter::DesktopPresenter;
#[cfg(all(test, unix))]
use ansi_presenter::semantic_dirty_runs;
use attachment_surface::AttachmentSurface;
use composition::{
    ChromeLayout, ComposedFrame, LiveViewportProjection, ScrollbarGeometry, normalize_composed_row,
};
use keyboard::{CopyKeyLease, EnhancedKey, KeyEventKind};
use selection::{SelectionController, SelectionSourceIdentity};

const STDIN_CHANNEL_CAPACITY: usize = 8;
#[cfg(unix)]
const STDIN_CHUNK_BYTES: usize = 4 * 1024;
const CONTROL_PREFIX_TIMEOUT: Duration = Duration::from_secs(1);
const DETACH_TIMEOUT: Duration = Duration::from_secs(2);
const DRAG_REQUEST_INTERVAL: Duration = Duration::from_millis(33);
const MIN_VIEWPORT_PRESENT_INTERVAL: Duration = Duration::from_millis(16);
const HOST_SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const HOST_SYNC_END: &[u8] = b"\x1b[?2026l";
const HOST_INPUT_CAPTURE: &[u8] = b"\x1b[?1003h\x1b[?1006h";
const ENTER_TERMINAL_UI: &[u8] = b"\x1b[22;2t\x1b[?1049h\x1b[?25l\x1b[>0u\x1b[?1003h\x1b[?1006h";
const HOST_SEQUENCE_BOUND: usize = 64;
use voytty_client::input::RESUME_INPUT_BOUND;
const PAGE_UP: &[u8] = b"\x1b[5~";
const PAGE_DOWN: &[u8] = b"\x1b[6~";
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";
const RESTORE_TERMINAL_UI: &[u8] = concat!(
    "\x1b]8;;\x1b\\",
    "\x1b[23;2t",
    "\x1b[?2026l",
    "\x1b[?9l",
    "\x1b[?1000l",
    "\x1b[?1001l",
    "\x1b[?1002l",
    "\x1b[?1003l",
    "\x1b[?1004l",
    "\x1b[?1005l",
    "\x1b[?1006l",
    "\x1b[?1007l",
    "\x1b[?1015l",
    "\x1b[?1016l",
    "\x1b[?2004l",
    "\x1b[?1l",
    "\x1b>",
    "\x1b[<u",
    "\x1b[0m",
    "\x1b[0 q",
    "\x1b[?25h",
    "\x1b[?1049l"
)
.as_bytes();

pub(super) async fn run(request: TerminalRequest, runtime: &LocalRuntime) -> Result<(), CliError> {
    let (progress, history) = runtime.connection_progress();
    progress.report(ConnectionStage::Starting);
    let runtime = runtime.with_connection_progress(progress.clone());
    let result = run_observed(request, &runtime, progress.clone(), history).await;
    let startup_finished = !progress.is_active();
    finish_startup_progress(&progress, &result);
    if startup_finished && let Err(error) = &result {
        use voytty_diagnostics::{Event, FrontendFailure, Kind, Level};
        let event = Event::new(Kind::TerminalFailed).level(Level::Error);
        let event = match error {
            CliError::Daemon(error)
            | CliError::CreatedSessionAttach { source: error, .. }
            | CliError::SessionOperation { source: error, .. } => event.error(error.kind()),
            CliError::Usage(_) => event.frontend_error(FrontendFailure::InvalidUsage),
            CliError::Io(_) => event.frontend_error(FrontendFailure::TerminalIo),
            CliError::TerminalDriverFailure => {
                event.frontend_error(FrontendFailure::TerminalDriver)
            }
        };
        voytty_diagnostics::record(event);
    }
    result.and_then(emit_completion_diagnostic)
}

fn finish_startup_progress(
    progress: &ProgressObserver,
    result: &Result<TerminalCompletion, CliError>,
) {
    if progress.is_active() {
        match result {
            Ok(TerminalCompletion::Detached | TerminalCompletion::PreparedThenDetached(_)) => {
                progress.report(ConnectionStage::Cancelled)
            }
            Ok(TerminalCompletion::SessionEnded(_)) => {
                progress.report(ConnectionStage::SessionEnded)
            }
            Err(
                CliError::Daemon(error)
                | CliError::CreatedSessionAttach { source: error, .. }
                | CliError::SessionOperation { source: error, .. },
            ) => progress.fail(error.kind()),
            Err(CliError::Usage(_)) => progress.fail(ProgressFailure::InvalidUsage),
            Err(CliError::Io(_)) => progress.fail(ProgressFailure::TerminalIo),
            Err(CliError::TerminalDriverFailure) => progress.fail(ProgressFailure::TerminalDriver),
        }
        progress.stop();
    }
}

async fn run_observed(
    request: TerminalRequest,
    runtime: &LocalRuntime,
    progress: ProgressObserver,
    history: watch::Receiver<ProgressHistory>,
) -> Result<TerminalCompletion, CliError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    if !stdin.is_terminal() || !stdout.is_terminal() {
        return Err(CliError::Usage(
            "interactive attachment requires both stdin and stdout to be TTYs".to_owned(),
        ));
    }

    let panic_hook = ScopedPanicHook::suppress();
    let result = AssertUnwindSafe(run_guarded_terminal(request, runtime, progress, history))
        .catch_unwind()
        .await;
    drop(panic_hook);
    match result {
        Ok(completion) => completion,
        Err(payload) => {
            drop(payload);
            Err(CliError::Io(
                "interactive terminal aborted after an internal panic".to_owned(),
            ))
        }
    }
}

async fn run_guarded_terminal(
    request: TerminalRequest,
    runtime: &LocalRuntime,
    progress: ProgressObserver,
    history: watch::Receiver<ProgressHistory>,
) -> Result<TerminalCompletion, CliError> {
    progress.report(ConnectionStage::InitializingTerminal);
    io::stdout()
        .flush()
        .map_err(|error| terminal_io("flush stdout before raw mode", error))?;
    let TerminalSignals {
        mut resize,
        mut interrupt,
        mut terminate,
        mut hangup,
    } = TerminalSignals::install()?;
    let (cancellation_sender, mut cancellation_receiver) = watch::channel(None);
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut guard = TerminalGuard::enter(&stdin, &stdout)?;
    let result = select_terminal_termination(
        run_view(
            request,
            runtime,
            &stdin,
            &stdout,
            &mut resize,
            &mut cancellation_receiver,
            Arc::clone(&guard.appearance_owned),
            progress,
            history,
        ),
        &mut interrupt,
        &mut terminate,
        &mut hangup,
        cancellation_sender,
    )
    .await;
    let restoration = guard.restore();
    match (result, restoration) {
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
        (Ok(completion), Ok(())) => Ok(completion),
    }
}

async fn select_terminal_termination(
    view: impl Future<Output = Result<TerminalCompletion, CliError>>,
    interrupt: &mut Signal,
    terminate: &mut Signal,
    hangup: &mut Signal,
    cancellation_sender: watch::Sender<Option<TerminalSignalCancellation>>,
) -> Result<TerminalCompletion, CliError> {
    tokio::pin!(view);
    let cancellation = tokio::select! {
        biased;
        signal = interrupt.recv() => TerminalSignalCancellation::new("SIGINT", signal),
        signal = terminate.recv() => TerminalSignalCancellation::new("SIGTERM", signal),
        signal = hangup.recv() => TerminalSignalCancellation::new("SIGHUP", signal),
        result = &mut view => return result,
    };
    let _ = cancellation_sender.send(Some(cancellation));
    view.await
}

#[allow(clippy::too_many_arguments)]
async fn run_view(
    request: TerminalRequest,
    runtime: &LocalRuntime,
    stdin: &io::Stdin,
    stdout: &io::Stdout,
    resize_signal: &mut Signal,
    cancellation_receiver: &mut watch::Receiver<Option<TerminalSignalCancellation>>,
    appearance_owned: Arc<std::sync::atomic::AtomicBool>,
    progress: ProgressObserver,
    history: watch::Receiver<ProgressHistory>,
) -> Result<TerminalCompletion, CliError> {
    if let Some(cancellation) = current_terminal_cancellation(cancellation_receiver) {
        return Err(cancellation.error(None));
    }
    let mut physical_size = terminal_size(stdout)?;
    let initial_layout = ChromeLayout::new(physical_size, ActiveScreen::Main);
    let initial_size = initial_layout.child;
    let stateful_prepare = matches!(
        &request.kind,
        TerminalRequestKind::Create { .. }
            | TerminalRequestKind::Attach {
                create_main: true,
                ..
            }
    );
    let input_epoch = InputEpoch::new();
    let current_input_epoch = input_epoch.current();
    let mut stdin_pump = StdinPump::start(stdin, input_epoch.clone())?;
    let mut prefix = CommandMode::new();
    let mut input_codec = HostInputCodec::new();
    let mut presenter = DesktopPresenter::default();
    let mut startup = StartupProgress::new(&request, progress.clone(), history);
    startup.present(&mut stdout.lock(), &mut presenter, physical_size)?;
    let mut host_colors = HostColors::for_host(appearance_owned, cfg!(unix));
    host_colors.flush_commands(&mut presenter, &mut stdout.lock())?;
    let initial_color_deadline = Instant::now() + Duration::from_millis(250);
    // Collect before any create/attach side effect, so a child's first query
    // observes this controller's base. Unsupported hosts cost at most 250 ms.
    loop {
        host_colors.expire(Instant::now());
        if host_colors.initial_complete() || Instant::now() >= initial_color_deadline {
            break;
        }
        let deadline = Some(
            host_colors
                .deadline()
                .unwrap_or(initial_color_deadline)
                .min(initial_color_deadline),
        );
        tokio::select! {
            cancellation = receive_terminal_cancellation(cancellation_receiver) => return Err(cancellation.error(None)),
            signal = resize_signal.recv() => {
                if signal.is_none() {
                    return Err(terminal_daemon_error(DomainErrorKind::Cancelled, "SIGWINCH handler closed"));
                }
                physical_size = terminal_size(stdout)?;
                startup.present(&mut stdout.lock(), &mut presenter, physical_size)?;
            }
            () = wait_for_prefix_deadline(deadline) => {},
            input = input_codec.read(stdin_pump.recv(), current_input_epoch, input_epoch.geometry()) => match input? {
                Some(events) => {
                    for event in events {
                        if let HostInputEvent::TerminalReply { reply, bytes } = event {
                            host_colors.observe(reply, bytes, Instant::now());
                        } else {
                            for action in prefix.route(event, Instant::now(), false)? {
                                if matches!(action, PrefixAction::Command(LocalCommand::Detach)) { return Ok(TerminalCompletion::Detached); }
                            }
                        }
                    }
                },
                None => return Ok(TerminalCompletion::Detached),
            }
        }
        host_colors.flush_commands(&mut presenter, &mut stdout.lock())?;
    }

    progress.report(ConnectionStage::TerminalInitialized);
    let transport_state = TerminalViewTransportState::Synchronizing;
    let prepared = match await_while_inactive(
        prepare(request, runtime, initial_size, host_colors.profile.clone()),
        InactiveWaitContext {
            presentation: InactivePresentation::Startup(&mut startup),
            stdout,
            resize_signal,
            cancellation_receiver,
            stdin_pump: &mut stdin_pump,
            input_codec: &mut input_codec,
            host_colors: &mut host_colors,
            presenter: &mut presenter,
            prefix: &mut prefix,
            physical_size: &mut physical_size,
            current_input_epoch,
            preserve_submitted_result: stateful_prepare,
            report_key_events: false,
        },
        &mut io::stdout(),
    )
    .await?
    {
        InactiveWait::Ready(prepared) => prepared,
        InactiveWait::Cancelled(cancellation) => {
            stdin_pump.shutdown()?;
            return inactive_cancellation_result(cancellation, None);
        }
        InactiveWait::CompletedAfterCancellation {
            value: prepared,
            cancellation,
        } => {
            let session_id = prepared.session_id();
            drop(prepared);
            stdin_pump.shutdown()?;
            return inactive_cancellation_result(cancellation, Some(session_id));
        }
    };
    let session_id = prepared.session_id();
    physical_size = terminal_size(stdout)?;
    let latest_layout = ChromeLayout::new(
        physical_size,
        prepared.initial_snapshot().surface.active_screen,
    );
    let view_target = prepared.target().clone();
    let surface = AttachmentSurface::from_snapshot(prepared.initial_snapshot())
        .map_err(|_| semantic_surface_error("semantic terminal snapshot is invalid"))?;
    // The creation hint does not resize a retained Session. Only the host
    // snapshot establishes the actual geometry for resize deduplication.
    let mut resize_coalescer = ResizeCoalescer::new(surface.surface.size);
    let mut selection = SelectionController::default();
    let initial_scroll_metrics = prepared.initial_snapshot().surface.scroll_metrics;
    let mut viewport = ViewportController::with_layout(latest_layout, initial_scroll_metrics);
    let mut status_renderer = StatusRenderer::new(view_target, physical_size);
    status_renderer.initial_synchronizing = true;
    reconcile_presenter_selection(&mut selection, &viewport, &surface, &mut presenter);
    progress.report(ConnectionStage::DisplayingTerminal);
    present_surface_stdout(
        &surface,
        &mut presenter,
        &viewport,
        &status_renderer,
        transport_state,
    )?;
    viewport.observe_presentation();
    progress.report(ConnectionStage::SynchronizingTerminal);
    let mut viewport_pacer = ViewportPresentationPacer::default();
    viewport_pacer.mark_presented(Instant::now());
    if let Some(cancellation) = current_terminal_cancellation(cancellation_receiver) {
        drop(prepared);
        stdin_pump.shutdown()?;
        return inactive_cancellation_result(
            InactiveCancellation::Signal(cancellation),
            Some(session_id),
        );
    }
    let report_key_events = presenter
        .presented_keyboard_flags()
        .contains(voytty_core::terminal::TerminalKeyboardFlags::REPORT_EVENT_TYPES);
    let view = match await_while_inactive(
        async move { prepared.acknowledge_initial().await.map_err(Into::into) },
        InactiveWaitContext {
            presentation: InactivePresentation::Synchronizing {
                surface: &surface,
                viewport: &mut viewport,
                status: &mut status_renderer,
            },
            stdout,
            resize_signal,
            cancellation_receiver,
            stdin_pump: &mut stdin_pump,
            input_codec: &mut input_codec,
            host_colors: &mut host_colors,
            presenter: &mut presenter,
            prefix: &mut prefix,
            physical_size: &mut physical_size,
            current_input_epoch,
            preserve_submitted_result: false,
            report_key_events,
        },
        &mut io::stdout(),
    )
    .await?
    {
        InactiveWait::Ready(view) => view,
        InactiveWait::Cancelled(cancellation) => {
            stdin_pump.shutdown()?;
            return inactive_cancellation_result(cancellation, Some(session_id));
        }
        InactiveWait::CompletedAfterCancellation {
            value: view,
            cancellation,
        } => {
            drop(view);
            stdin_pump.shutdown()?;
            return inactive_cancellation_result(cancellation, Some(session_id));
        }
    };
    // Both inactive waits retain physical sizes, without assuming Main.
    // Sample again because a ready future may win over a pending SIGWINCH.
    // Reconcile once, using the known screen, before the first Active event.
    physical_size = terminal_size(stdout)?;
    let layout = ChromeLayout::new(physical_size, surface.active_screen());
    viewport.set_layout(layout);
    status_renderer.resize(physical_size);
    let _ = resize_coalescer.observe(layout.child, transport_state);
    let (events, writer) = view.split();
    let sync_requested = false;
    let copy_key_lease = CopyKeyLease::default();
    let deferred_active = false;

    TerminalUiSession {
        upload: None,
        upload_connector: runtime.upload_connector(),
        progress,
        session_id,
        events,
        writer,
        input_epoch,
        current_input_epoch,
        stdin_pump,
        prefix,
        transport_state,
        healthy_resize: false,
        resize_coalescer,
        physical_size,
        surface,
        presenter,
        selection,
        viewport,
        status_renderer,
        viewport_pacer,
        sync_requested,
        input_codec,
        host_colors,
        copy_key_lease,
        deferred_active,
    }
    .run(stdin, stdout, resize_signal, cancellation_receiver)
    .await
}

struct InactiveWaitContext<'a, Output> {
    presentation: InactivePresentation<'a>,
    stdout: &'a Output,
    resize_signal: &'a mut Signal,
    cancellation_receiver: &'a mut watch::Receiver<Option<TerminalSignalCancellation>>,
    stdin_pump: &'a mut StdinPump,
    input_codec: &'a mut HostInputCodec,
    host_colors: &'a mut HostColors,
    presenter: &'a mut DesktopPresenter,
    prefix: &'a mut CommandMode,
    physical_size: &'a mut TerminalSize,
    current_input_epoch: u64,
    preserve_submitted_result: bool,
    report_key_events: bool,
}

async fn await_while_inactive<T>(
    future: impl Future<Output = Result<T, CliError>>,
    context: InactiveWaitContext<'_, impl AsFd>,
    output: &mut impl Write,
) -> Result<InactiveWait<T>, CliError> {
    let InactiveWaitContext {
        mut presentation,
        stdout,
        resize_signal,
        cancellation_receiver,
        stdin_pump,
        input_codec,
        host_colors,
        presenter,
        prefix,
        physical_size,
        current_input_epoch,
        preserve_submitted_result,
        report_key_events,
    } = context;
    // Fail the initial paint before polling an operation that can create a
    // Session. Once submitted, subsequent paint failures retain its result.
    presentation.set_command_pending(prefix.deadline().is_some());
    presentation.present(output, presenter, *physical_size)?;
    tokio::pin!(future);
    let mut pending_cancellation = None;
    let mut input_open = true;
    loop {
        let now = Instant::now();
        if pending_cancellation.is_none() {
            host_colors.flush_commands(presenter, output)?;
        }
        let color_deadline = pending_cancellation
            .is_none()
            .then(|| host_colors.deadline())
            .flatten();
        let prefix_deadline = prefix.deadline();
        if prefix_deadline.is_some_and(|deadline| now >= deadline) {
            prefix.cancel();
        }
        let cancellation = tokio::select! {
            biased;
            result = &mut future => return match pending_cancellation {
                Some(cancellation) => finish_submitted_after_cancellation(std::future::ready(result), cancellation).await,
                None => result.map(InactiveWait::Ready),
            },
            cancellation = receive_terminal_cancellation(cancellation_receiver), if pending_cancellation.is_none() => {
                Some(InactiveCancellation::Signal(cancellation))
            }
            () = presentation.changed() => None,
            signal = resize_signal.recv() => {
                if signal.is_none() {
                    return Err(terminal_daemon_error(DomainErrorKind::Cancelled, "SIGWINCH handler closed"));
                }
                *physical_size = terminal_size(stdout)?;
                None
            }
            () = wait_for_prefix_deadline(prefix_deadline), if prefix_deadline.is_some() => {
                prefix.cancel();
                None
            }
            () = wait_for_prefix_deadline(color_deadline), if color_deadline.is_some() => {
                host_colors.expire(Instant::now());
                None
            }
            input = input_codec.read(stdin_pump.recv(), current_input_epoch, 0), if input_open => {
                match input {
                    Ok(Some(decoded)) => {
                        let mut cancellation = None;
                        for event in decoded {
                            if let HostInputEvent::TerminalReply { reply, bytes } = event {
                                host_colors.observe(reply, bytes, Instant::now());
                            } else if pending_cancellation.is_none() {
                                for action in prefix.route(event, Instant::now(), report_key_events)? {
                                    if let PrefixAction::Command(LocalCommand::Detach) = action {
                                        cancellation = Some(InactiveCancellation::LocalDetach);
                                    }
                                }
                            }
                        }
                        cancellation
                    }
                    Ok(None) => {
                        input_open = false;
                        prefix.clear_pending();
                        pending_cancellation.is_none().then_some(InactiveCancellation::LocalDetach)
                    }
                    Err(_) if pending_cancellation.is_some() => { input_open = false; None }
                    Err(error) => return Err(error),
                }
            }
        };
        if let Some(cancellation) = cancellation {
            if !preserve_submitted_result {
                return Ok(InactiveWait::Cancelled(cancellation));
            }
            // Retain the submitted operation while continuing to paint and
            // drain physical replies. Cancellation never creates an input epoch.
            pending_cancellation = Some(cancellation);
            prefix.clear_pending();
            presentation.cancelling();
        }
        presentation.set_command_pending(prefix.deadline().is_some());
        if let Err(error) = presentation.present(output, presenter, *physical_size) {
            if preserve_submitted_result {
                let cancellation = pending_cancellation.unwrap_or_else(|| {
                    InactiveCancellation::PresentationFailure(error.to_string())
                });
                return finish_submitted_after_cancellation(&mut future, cancellation).await;
            }
            return Err(error);
        }
    }
}

async fn finish_submitted_after_cancellation<T>(
    future: impl Future<Output = Result<T, CliError>>,
    cancellation: InactiveCancellation,
) -> Result<InactiveWait<T>, CliError> {
    future
        .await
        .map(|value| InactiveWait::CompletedAfterCancellation {
            value,
            cancellation,
        })
}

fn transition_input_state(
    stdin: &impl AsFd,
    input_epoch: &InputEpoch,
    current_input_epoch: &mut u64,
    stdin_pump: &mut StdinPump,
    prefix: &mut CommandMode,
    next: TerminalViewTransportState,
) -> Result<(), CliError> {
    if next == TerminalViewTransportState::Active {
        return stdin_pump.replace_after_active_fence(
            stdin,
            input_epoch,
            current_input_epoch,
            prefix,
        );
    }
    *current_input_epoch = input_epoch.advance();
    prefix.clear_pending();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn transition_transport_input_state(
    stdin: &impl AsFd,
    input_epoch: &InputEpoch,
    current_input_epoch: &mut u64,
    stdin_pump: &mut StdinPump,
    prefix: &mut CommandMode,
    previous: TerminalViewTransportState,
    next: TerminalViewTransportState,
    viewport: &mut ViewportController,
) -> Result<Option<Vec<u8>>, CliError> {
    if next != previous
        && (!viewport.is_resume_pending() || next == TerminalViewTransportState::Active)
    {
        transition_input_state(
            stdin,
            input_epoch,
            current_input_epoch,
            stdin_pump,
            prefix,
            next,
        )?;
    }
    Ok((next == TerminalViewTransportState::Active)
        .then(|| viewport.finish_resume())
        .flatten())
}

fn should_defer_active_for_paste(
    next: TerminalViewTransportState,
    viewport: &ViewportController,
    input_codec: &HostInputCodec,
) -> bool {
    next == TerminalViewTransportState::Active
        && viewport.is_resume_pending()
        && input_codec.paste_in_progress()
}

#[cfg(all(test, unix))]
const fn input_epoch_is_current(observed: u64, current: u64) -> bool {
    observed == current
}

async fn finish_terminal_view(
    loop_result: Result<TerminalCompletion, CliError>,
    stdin_pump: &mut StdinPump,
    writer: &voytty_daemon::client::view::TerminalViewCommandWriter,
) -> Result<TerminalCompletion, CliError> {
    let stdin_result = stdin_pump.shutdown();
    let session_already_ended = matches!(&loop_result, Ok(TerminalCompletion::SessionEnded(_)));
    let detach_result = if session_already_ended {
        Ok(())
    } else {
        match tokio::time::timeout(DETACH_TIMEOUT, writer.detach()).await {
            Ok(result) => result.map_err(CliError::from),
            Err(_) => Err(terminal_daemon_error(
                DomainErrorKind::DeadlineExceeded,
                "timed out detaching the local terminal view",
            )),
        }
    };
    match (loop_result, stdin_result, detach_result) {
        (Err(error), _, _) => Err(error),
        (Ok(_), Err(error), _) => Err(error),
        (Ok(_), Ok(()), Err(error)) => Err(error),
        (Ok(completion), Ok(()), Ok(())) => Ok(completion),
    }
}

async fn prepare(
    request: TerminalRequest,
    runtime: &LocalRuntime,
    viewport: TerminalSize,
    base_colors: voytty_core::terminal::TerminalColorProfile,
) -> Result<PreparedTerminalView, CliError> {
    match request.kind {
        TerminalRequestKind::Attach {
            target,
            selector,
            create_main,
            takeover,
        } => runtime
            .attach_with_colors(
                &target,
                selector.as_deref(),
                create_main,
                takeover,
                Some(viewport),
                base_colors.clone(),
            )
            .await
            .map_err(Into::into),
        TerminalRequestKind::Create {
            target,
            name,
            working_directory,
        } => {
            let created = runtime
                .session_create_for_attach_with_colors(
                    &target,
                    &name,
                    working_directory.as_deref(),
                    Some(viewport),
                    base_colors.clone(),
                )
                .await?;
            let session_id = created.summary().session_id;
            preserve_created_session(
                session_id,
                runtime
                    .attach_created_with_colors(&created, Some(viewport), base_colors)
                    .await,
            )
        }
    }
}

fn preserve_created_session<T>(
    session_id: voytty_core::SessionId,
    result: Result<T, DaemonError>,
) -> Result<T, CliError> {
    result.map_err(|source| CliError::CreatedSessionAttach { session_id, source })
}

struct TerminalSignals {
    resize: Signal,
    interrupt: Signal,
    terminate: Signal,
    hangup: Signal,
}

#[cfg(unix)]
impl TerminalSignals {
    fn install() -> Result<Self, CliError> {
        Ok(Self {
            resize: signal(SignalKind::window_change())
                .map_err(|error| terminal_io("install SIGWINCH handler", error))?,
            interrupt: signal(SignalKind::interrupt())
                .map_err(|error| terminal_io("install SIGINT handler", error))?,
            terminate: signal(SignalKind::terminate())
                .map_err(|error| terminal_io("install SIGTERM handler", error))?,
            hangup: signal(SignalKind::hangup())
                .map_err(|error| terminal_io("install SIGHUP handler", error))?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TerminalSignalCancellation {
    name: &'static str,
    received: bool,
}

impl TerminalSignalCancellation {
    const fn new(name: &'static str, received: Option<()>) -> Self {
        Self {
            name,
            received: received.is_some(),
        }
    }

    fn error(self, prepared_session: Option<SessionId>) -> CliError {
        let detail = match (self.received, prepared_session) {
            (true, Some(session_id)) => format!(
                "terminal attachment interrupted by {} after Session {session_id} was prepared; the Session remains live",
                self.name
            ),
            (false, Some(session_id)) => format!(
                "{} handler closed after Session {session_id} was prepared; the Session remains live",
                self.name
            ),
            (true, None) => format!("terminal attachment interrupted by {}", self.name),
            (false, None) => format!("{} handler closed", self.name),
        };
        terminal_daemon_error(DomainErrorKind::Cancelled, &detail)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum InactiveCancellation {
    LocalDetach,
    Signal(TerminalSignalCancellation),
    PresentationFailure(String),
}

enum InactiveWait<T> {
    Ready(T),
    Cancelled(InactiveCancellation),
    CompletedAfterCancellation {
        value: T,
        cancellation: InactiveCancellation,
    },
}

fn current_terminal_cancellation(
    receiver: &mut watch::Receiver<Option<TerminalSignalCancellation>>,
) -> Option<TerminalSignalCancellation> {
    *receiver.borrow_and_update()
}

async fn receive_terminal_cancellation(
    receiver: &mut watch::Receiver<Option<TerminalSignalCancellation>>,
) -> TerminalSignalCancellation {
    loop {
        if let Some(cancellation) = current_terminal_cancellation(receiver) {
            return cancellation;
        }
        if receiver.changed().await.is_err() {
            return TerminalSignalCancellation::new("terminal signal coordinator", None);
        }
    }
}

fn inactive_cancellation_result(
    cancellation: InactiveCancellation,
    prepared_session: Option<SessionId>,
) -> Result<TerminalCompletion, CliError> {
    match cancellation {
        InactiveCancellation::LocalDetach => Ok(prepared_session.map_or(
            TerminalCompletion::Detached,
            TerminalCompletion::PreparedThenDetached,
        )),
        InactiveCancellation::Signal(signal) => Err(signal.error(prepared_session)),
        InactiveCancellation::PresentationFailure(detail) => {
            Err(CliError::Io(prepared_session.map_or_else(
                || detail.clone(),
                |session_id| {
                    format!("{detail}; session {session_id} was prepared and remains running")
                },
            )))
        }
    }
}

fn terminal_daemon_error(kind: DomainErrorKind, detail: &str) -> CliError {
    CliError::Daemon(DaemonError::new(kind, detail))
}

fn semantic_surface_error(detail: &str) -> CliError {
    terminal_daemon_error(DomainErrorKind::MalformedFrame, detail)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalCompletion {
    Detached,
    PreparedThenDetached(SessionId),
    SessionEnded(TerminalViewEndReason),
}

fn terminal_end_completion(reason: TerminalViewEndReason) -> Result<TerminalCompletion, CliError> {
    if reason == TerminalViewEndReason::DriverFailure {
        Err(CliError::TerminalDriverFailure)
    } else {
        Ok(TerminalCompletion::SessionEnded(reason))
    }
}

fn emit_completion_diagnostic(completion: TerminalCompletion) -> Result<(), CliError> {
    let Some(message) = completion_diagnostic(completion)? else {
        return Ok(());
    };
    write_all_fd(&io::stderr(), &message)
        .map_err(|error| terminal_io("write terminal completion diagnostic", error))
}

fn completion_diagnostic(completion: TerminalCompletion) -> Result<Option<Vec<u8>>, CliError> {
    Ok(Some(match completion {
        TerminalCompletion::Detached => return Ok(None),
        TerminalCompletion::PreparedThenDetached(session_id) => {
            format!("voytty: detached after Session {session_id} was prepared; it remains live.\n")
                .into_bytes()
        }
        TerminalCompletion::SessionEnded(TerminalViewEndReason::NaturalExit) => {
            b"voytty: Session ended after its root shell exited.\n".to_vec()
        }
        TerminalCompletion::SessionEnded(TerminalViewEndReason::ExplicitClose) => {
            b"voytty: Session was closed explicitly.\n".to_vec()
        }
        TerminalCompletion::SessionEnded(TerminalViewEndReason::DaemonStop) => {
            b"voytty: Session ended because its daemon stopped.\n".to_vec()
        }
        TerminalCompletion::SessionEnded(TerminalViewEndReason::DriverFailure) => {
            return Err(CliError::TerminalDriverFailure);
        }
    }))
}

fn terminal_io(operation: &'static str, error: io::Error) -> CliError {
    CliError::Io(format!("{operation}: {error}"))
}

#[cfg(unix)]
fn terminal_size(terminal: &impl AsFd) -> Result<TerminalSize, CliError> {
    let size = rustix::termios::tcgetwinsize(terminal)
        .map_err(|error| terminal_io("read terminal viewport", error.into()))?;
    if size.ws_row == 0 || size.ws_col == 0 {
        return Err(CliError::Usage(
            "interactive terminal viewport must have non-zero rows and columns".to_owned(),
        ));
    }
    Ok(TerminalSize::new(size.ws_row, size.ws_col))
}

type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static>;

struct ScopedPanicHook {
    previous: Option<PanicHook>,
}

impl ScopedPanicHook {
    fn suppress() -> Self {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        Self {
            previous: Some(previous),
        }
    }
}

impl Drop for ScopedPanicHook {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            std::panic::set_hook(previous);
        }
    }
}

#[cfg(unix)]
struct TerminalGuard {
    input: OwnedFd,
    output: OwnedFd,
    original: Termios,
    restored: bool,
    appearance_owned: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(unix)]
impl TerminalGuard {
    fn enter(input: &impl AsFd, output: &impl AsFd) -> Result<Self, CliError> {
        let input = duplicate_cloexec(input, "duplicate terminal stdin")?;
        let output = duplicate_cloexec(output, "duplicate terminal stdout")?;
        let original = tcgetattr_retry(&input)
            .map_err(|error| terminal_io("read terminal attributes", error.into()))?;
        let mut raw = original.clone();
        cfmakeraw(&mut raw);
        tcsetattr_retry(&input, SetArg::TCSANOW, &raw)
            .map_err(|error| terminal_io("enable terminal raw mode", error.into()))?;
        let mut guard = Self {
            input,
            output,
            original,
            restored: false,
            appearance_owned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        if let Err(error) = write_all_fd(&guard.output, ENTER_TERMINAL_UI) {
            let _ = guard.restore();
            return Err(terminal_io("enter terminal UI", error));
        }
        Ok(guard)
    }

    fn restore(&mut self) -> Result<(), CliError> {
        if self.restored {
            return Ok(());
        }
        let appearance = if self.appearance_owned.load(Ordering::Acquire) {
            write_all_fd(&self.output, b"\x1b[?2031l")
        } else {
            Ok(())
        };
        let output = appearance
            .and_then(|()| write_all_fd(&self.output, RESTORE_TERMINAL_UI))
            .map_err(|error| terminal_io("restore terminal display", error));
        let attributes = tcsetattr_retry(&self.input, SetArg::TCSANOW, &self.original)
            .map_err(|error| terminal_io("restore terminal attributes", error.into()));
        if output.is_ok() && attributes.is_ok() {
            self.restored = true;
        }
        match (output, attributes) {
            (_, Err(error)) => Err(error),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }
}

#[cfg(unix)]
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(unix)]
fn tcgetattr_retry(fd: &impl AsFd) -> Result<Termios, nix::errno::Errno> {
    loop {
        match tcgetattr(fd) {
            Err(nix::errno::Errno::EINTR) => {}
            result => return result,
        }
    }
}

#[cfg(unix)]
fn tcsetattr_retry(
    fd: &impl AsFd,
    action: SetArg,
    attributes: &Termios,
) -> Result<(), nix::errno::Errno> {
    loop {
        match tcsetattr(fd, action, attributes) {
            Err(nix::errno::Errno::EINTR) => {}
            result => return result,
        }
    }
}

#[cfg(unix)]
fn write_all_fd(fd: &impl AsFd, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        match rustix::io::write(fd, bytes) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error == rustix::io::Errno::INTR => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn duplicate_cloexec(fd: &impl AsFd, operation: &'static str) -> Result<OwnedFd, CliError> {
    rustix::io::fcntl_dupfd_cloexec(fd, 0).map_err(|error| terminal_io(operation, error.into()))
}

#[cfg(unix)]
fn cancellation_pipe() -> Result<(OwnedFd, OwnedFd), CliError> {
    #[cfg(target_os = "linux")]
    {
        rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)
            .map_err(|error| terminal_io("create terminal stdin cancellation pipe", error.into()))
    }

    #[cfg(not(target_os = "linux"))]
    {
        // macOS has no rustix `pipe_with` boundary. Set both descriptor
        // flags through safe fcntl calls before publishing either end or
        // starting the reader thread.
        let (read, write) = rustix::pipe::pipe().map_err(|error| {
            terminal_io("create terminal stdin cancellation pipe", error.into())
        })?;
        set_cloexec(&read, "protect terminal stdin cancellation reader")?;
        set_cloexec(&write, "protect terminal stdin cancellation writer")?;
        Ok((read, write))
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn set_cloexec(fd: &impl AsFd, operation: &'static str) -> Result<(), CliError> {
    let flags =
        rustix::io::fcntl_getfd(fd).map_err(|error| terminal_io(operation, error.into()))?;
    rustix::io::fcntl_setfd(fd, flags | rustix::io::FdFlags::CLOEXEC)
        .map_err(|error| terminal_io(operation, error.into()))
}

#[derive(Clone)]
struct InputEpoch(Arc<AtomicU64>, Arc<AtomicU64>);

impl InputEpoch {
    fn new() -> Self {
        Self(Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)))
    }

    fn geometry(&self) -> u64 {
        self.1.load(Ordering::Acquire)
    }

    fn invalidate_geometry(&self) {
        self.1
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .expect("terminal geometry generation exhausted");
    }

    fn current(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    fn advance(&self) -> u64 {
        let previous = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .expect("terminal input epoch exhausted");
        previous + 1
    }
}

enum StdinEvent {
    Bytes {
        epoch: u64,
        geometry: u64,
        bytes: Vec<u8>,
    },
    Eof,
    Error(String),
}

#[cfg(all(test, unix))]
struct StdinReaderTestGate {
    observed: TestSender<()>,
    release: TestReceiver<()>,
}

#[cfg(all(test, unix))]
struct StdinReaderTestControl {
    observed: TestReceiver<()>,
    release: TestSender<()>,
}

#[cfg(all(test, unix))]
#[derive(Clone)]
struct StdinReaderTestSeam {
    gates: Arc<Mutex<VecDeque<StdinReaderTestGate>>>,
}

#[cfg(all(test, unix))]
impl StdinReaderTestSeam {
    fn with_gates(count: usize) -> (Self, Vec<StdinReaderTestControl>) {
        let mut gates = VecDeque::with_capacity(count);
        let mut controls = Vec::with_capacity(count);
        for _ in 0..count {
            let (observed_sender, observed) = sync_channel(1);
            let (release, release_receiver) = sync_channel(1);
            gates.push_back(StdinReaderTestGate {
                observed: observed_sender,
                release: release_receiver,
            });
            controls.push(StdinReaderTestControl { observed, release });
        }
        (
            Self {
                gates: Arc::new(Mutex::new(gates)),
            },
            controls,
        )
    }

    fn after_readable_poll(&self) -> Result<(), String> {
        let gate = self
            .gates
            .lock()
            .map_err(|_| "terminal stdin test seam was poisoned".to_owned())?
            .pop_front();
        let Some(gate) = gate else {
            return Ok(());
        };
        gate.observed
            .send(())
            .map_err(|_| "terminal stdin test observer was dropped".to_owned())?;
        gate.release
            .recv()
            .map_err(|_| "terminal stdin test release was dropped".to_owned())
    }
}

#[cfg(unix)]
struct StdinPump {
    receiver: Option<mpsc::Receiver<StdinEvent>>,
    retained: VecDeque<StdinEvent>,
    #[cfg(all(test, unix))]
    sender_for_test: mpsc::Sender<StdinEvent>,
    _cancellation_read_guard: OwnedFd,
    cancellation_write: Option<OwnedFd>,
    handle: Option<JoinHandle<Result<Option<StdinEvent>, String>>>,
    #[cfg(all(test, unix))]
    reader_test_seam: Option<StdinReaderTestSeam>,
}

#[cfg(unix)]
impl StdinPump {
    fn start(input: &impl AsFd, input_epoch: InputEpoch) -> Result<Self, CliError> {
        Self::start_inner(
            input,
            input_epoch,
            #[cfg(all(test, unix))]
            None,
        )
    }

    fn start_inner(
        input: &impl AsFd,
        input_epoch: InputEpoch,
        #[cfg(all(test, unix))] reader_test_seam: Option<StdinReaderTestSeam>,
    ) -> Result<Self, CliError> {
        let fd = duplicate_cloexec(input, "duplicate terminal stdin reader")?;
        let (cancellation_read_guard, cancellation_write) = cancellation_pipe()?;
        let cancellation_read = duplicate_cloexec(
            &cancellation_read_guard,
            "duplicate terminal stdin cancellation reader",
        )?;
        let (sender, receiver) = mpsc::channel(STDIN_CHANNEL_CAPACITY);
        #[cfg(all(test, unix))]
        let sender_for_test = sender.clone();
        #[cfg(all(test, unix))]
        let thread_reader_test_seam = reader_test_seam.clone();
        let handle = std::thread::Builder::new()
            .name("voytty-cli-stdin".to_owned())
            .spawn(move || {
                stdin_reader(
                    fd,
                    cancellation_read,
                    sender,
                    input_epoch,
                    #[cfg(all(test, unix))]
                    thread_reader_test_seam,
                )
            })
            .map_err(|error| terminal_io("start terminal stdin reader", error))?;
        Ok(Self {
            receiver: Some(receiver),
            retained: VecDeque::new(),
            #[cfg(all(test, unix))]
            sender_for_test,
            _cancellation_read_guard: cancellation_read_guard,
            cancellation_write: Some(cancellation_write),
            handle: Some(handle),
            #[cfg(all(test, unix))]
            reader_test_seam,
        })
    }

    #[cfg(all(test, unix))]
    fn start_with_test_seam(
        input: &impl AsFd,
        input_epoch: InputEpoch,
        reader_test_seam: StdinReaderTestSeam,
    ) -> Result<Self, CliError> {
        Self::start_inner(input, input_epoch, Some(reader_test_seam))
    }

    async fn recv(&mut self) -> Option<StdinEvent> {
        if let Some(event) = self.retained.pop_front() {
            return Some(event);
        }
        self.receiver
            .as_mut()
            .expect("terminal stdin receiver exists while its pump is live")
            .recv()
            .await
    }

    fn replace_after_active_fence(
        &mut self,
        input: &impl AsFd,
        input_epoch: &InputEpoch,
        current_input_epoch: &mut u64,
        prefix: &mut CommandMode,
    ) -> Result<(), CliError> {
        #[cfg(all(test, unix))]
        let reader_test_seam = self.reader_test_seam.clone();

        let mut retained = self.stop_reader()?;
        // No concurrent reader exists here. Consume complete bounded chunks
        // under the old epoch; the persistent codec later retains replies
        // and discards keyboard events, including split escape sequences.
        let mut drained = 0usize;
        loop {
            let mut descriptors = [PollFd::new(input, PollFlags::IN)];
            match poll(
                &mut descriptors,
                Some(&rustix::event::Timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                }),
            ) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) if error == rustix::io::Errno::INTR => continue,
                Err(error) => {
                    return Err(terminal_io("drain inactive terminal input", error.into()));
                }
            }
            if !descriptors[0].revents().contains(PollFlags::IN) {
                break;
            }
            if drained >= 64 * 1024 {
                return Err(terminal_daemon_error(
                    DomainErrorKind::ResourceExhausted,
                    "inactive terminal input exceeded drain bound",
                ));
            }
            let mut buffer = [0; STDIN_CHUNK_BYTES];
            match rustix::io::read(input, &mut buffer) {
                Ok(0) => break,
                Ok(length) => {
                    drained += length;
                    retained.push_back(StdinEvent::Bytes {
                        geometry: input_epoch.geometry(),
                        epoch: *current_input_epoch,
                        bytes: buffer[..length].to_vec(),
                    });
                }
                Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => {
                    continue;
                }
                Err(error) => {
                    return Err(terminal_io("drain inactive terminal input", error.into()));
                }
            }
        }
        // Repeated ACK fences may occur before the consumer drains retained
        // events. Bound the aggregate, not just each kernel read round.
        let retained_bytes = retained.iter().try_fold(0usize, |total, event| {
            let length = match event {
                StdinEvent::Bytes { bytes, .. } => bytes.len().max(1),
                StdinEvent::Error(detail) => detail.len().max(1),
                StdinEvent::Eof => 1,
            };
            total.checked_add(length)
        });
        if retained_bytes.is_none_or(|length| length > RESUME_INPUT_BOUND) {
            return Err(terminal_daemon_error(
                DomainErrorKind::ResourceExhausted,
                "inactive terminal input exceeded retained bound",
            ));
        }
        *current_input_epoch = input_epoch.advance();
        prefix.clear_pending();
        let mut replacement = Self::start_inner(
            input,
            input_epoch.clone(),
            #[cfg(all(test, unix))]
            reader_test_seam,
        )?;
        replacement.retained = retained;
        *self = replacement;
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), CliError> {
        self.stop_reader().map(drop)
    }

    fn stop_reader(&mut self) -> Result<VecDeque<StdinEvent>, CliError> {
        let mut receiver = self.receiver.take();
        if let Some(receiver) = &mut receiver {
            receiver.close();
        }
        let mut retained = std::mem::take(&mut self.retained);
        let Some(handle) = self.handle.take() else {
            return Ok(retained);
        };
        let wake = self
            .cancellation_write
            .take()
            .map(|writer| {
                let result = write_all_fd(&writer, &[1])
                    .map_err(|error| terminal_io("wake terminal stdin reader", error));
                drop(writer);
                result
            })
            .unwrap_or(Ok(()));
        let joined = match handle.join() {
            Ok(Ok(tail)) => Ok(tail),
            Ok(Err(detail)) => Err(CliError::Io(detail)),
            Err(payload) => {
                drop(payload);
                Err(CliError::Io(
                    "terminal stdin reader panicked during shutdown".to_owned(),
                ))
            }
        };
        if let Some(receiver) = &mut receiver {
            while let Ok(event) = receiver.try_recv() {
                retained.push_back(event);
            }
        }
        match (wake, joined) {
            (_, Err(error)) | (Err(error), Ok(_)) => Err(error),
            (Ok(()), Ok(tail)) => {
                retained.extend(tail);
                Ok(retained)
            }
        }
    }
}

#[cfg(unix)]
impl Drop for StdinPump {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(unix)]
fn stdin_reader(
    input: OwnedFd,
    cancellation: OwnedFd,
    sender: mpsc::Sender<StdinEvent>,
    input_epoch: InputEpoch,
    #[cfg(all(test, unix))] reader_test_seam: Option<StdinReaderTestSeam>,
) -> Result<Option<StdinEvent>, String> {
    loop {
        let mut descriptors = [
            PollFd::new(&cancellation, PollFlags::IN),
            PollFd::new(&input, PollFlags::IN),
        ];
        match poll(&mut descriptors, None) {
            Ok(_) => {}
            Err(error) if error == rustix::io::Errno::INTR => continue,
            Err(error) => return Err(format!("poll terminal stdin: {error}")),
        }
        if !descriptors[0].revents().is_empty() {
            return Ok(None);
        }

        let input_events = descriptors[1].revents();
        if input_events.contains(PollFlags::NVAL) {
            return Err("poll terminal stdin: descriptor became invalid".to_owned());
        }
        if !input_events.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
            continue;
        }
        #[cfg(all(test, unix))]
        if let Some(reader_test_seam) = &reader_test_seam {
            reader_test_seam.after_readable_poll()?;
        }
        let epoch = input_epoch.current();
        let geometry = input_epoch.geometry();
        let mut buffer = [0_u8; STDIN_CHUNK_BYTES];
        match rustix::io::read(&input, &mut buffer) {
            Ok(0) => {
                let _ = sender.blocking_send(StdinEvent::Eof);
                return Ok(None);
            }
            Ok(read) => {
                if let Err(error) = sender.blocking_send(StdinEvent::Bytes {
                    geometry,
                    epoch,
                    bytes: buffer[..read].to_vec(),
                }) {
                    return Ok(Some(error.0));
                }
            }
            Err(error) if error == rustix::io::Errno::INTR => {}
            Err(error) if error == rustix::io::Errno::AGAIN => {}
            Err(error) => {
                let detail = error.to_string();
                if sender
                    .blocking_send(StdinEvent::Error(detail.clone()))
                    .is_err()
                {
                    return Ok(None);
                }
                return Err(format!("read terminal stdin: {detail}"));
            }
        }
    }
}

async fn wait_for_prefix_deadline(deadline: Option<Instant>) {
    let Some(deadline) = deadline else {
        std::future::pending::<()>().await;
        return;
    };
    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
}

async fn wait_for_viewport_deadline(deadline: Option<Instant>) {
    let Some(deadline) = deadline else {
        std::future::pending::<()>().await;
        return;
    };
    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
}

struct ResizeCoalescer {
    pending: Option<TerminalSize>,
    last_submitted: TerminalSize,
}

impl ResizeCoalescer {
    const fn new(initial_size: TerminalSize) -> Self {
        Self {
            pending: None,
            last_submitted: initial_size,
        }
    }

    fn observe(
        &mut self,
        size: TerminalSize,
        state: TerminalViewTransportState,
    ) -> Option<TerminalSize> {
        if state == TerminalViewTransportState::Active {
            self.pending = None;
            self.submit_if_changed(size)
        } else {
            self.pending = Some(size);
            None
        }
    }

    fn enter_transport_state(
        &mut self,
        state: TerminalViewTransportState,
    ) -> (TerminalViewTransportState, Option<TerminalSize>) {
        let pending = (state == TerminalViewTransportState::Active)
            .then(|| self.pending.take())
            .flatten()
            .and_then(|size| self.submit_if_changed(size));
        let effective = if pending.is_some() {
            TerminalViewTransportState::Synchronizing
        } else {
            state
        };
        (effective, pending)
    }

    fn submit_if_changed(&mut self, size: TerminalSize) -> Option<TerminalSize> {
        if size == self.last_submitted {
            return None;
        }
        self.last_submitted = size;
        Some(size)
    }
}

struct StatusRenderer {
    upload: Option<upload::UploadStatus>,
    command_pending: bool,
    initial_synchronizing: bool,
    target: TerminalViewTarget,
    physical_size: TerminalSize,
    path: TerminalViewConnectionPath,
    rtt_ms: Option<u32>,
}

impl StatusRenderer {
    fn new(target: TerminalViewTarget, physical_size: TerminalSize) -> Self {
        Self {
            upload: None,
            command_pending: false,
            initial_synchronizing: false,
            target,
            physical_size,
            path: TerminalViewConnectionPath::Unknown,
            rtt_ms: None,
        }
    }

    fn enabled(&self) -> bool {
        self.physical_size.rows > 1
    }

    fn is_remote(&self) -> bool {
        self.target.route() == TerminalViewRoute::Remote
    }

    fn resize(&mut self, physical_size: TerminalSize) {
        self.physical_size = physical_size;
    }

    fn observe(&mut self, status: TerminalViewConnectionStatus) -> Result<(), CliError> {
        if !self.is_remote() {
            return Err(terminal_daemon_error(
                DomainErrorKind::MalformedFrame,
                "local terminal received remote connection status",
            ));
        }
        self.path = status.path();
        self.rtt_ms = status.rtt_ms();
        Ok(())
    }

    fn reset_for_reconnect(&mut self) {
        // A replacement stream has a new path-observation epoch. Do not
        // let an old direct/relay sample reappear while it synchronizes.
        self.path = TerminalViewConnectionPath::Unknown;
        self.rtt_ms = None;
    }

    fn composed_text(&self, transport_state: TerminalViewTransportState) -> Option<String> {
        let left = self.left_text(transport_state)?;
        let width = usize::from(self.physical_size.columns);
        let right = if self.command_pending {
            upload::truncate(
                if self.is_remote() {
                    "Ctrl+] command: . detach  v upload  c cancel"
                } else {
                    "Ctrl+] command: . detach"
                },
                width,
            )
        } else if let Some(upload) = &self.upload {
            upload.text(width)
        } else {
            return Some(left);
        };
        let right_width = unicode_width::UnicodeWidthStr::width(right.as_str());
        let left = upload::truncate(&left, width.saturating_sub(right_width + 1));
        let padding = width
            .saturating_sub(unicode_width::UnicodeWidthStr::width(left.as_str()) + right_width);
        Some(format!("{left}{}{right}", " ".repeat(padding)))
    }

    fn left_text(&self, transport_state: TerminalViewTransportState) -> Option<String> {
        if !self.enabled() {
            return None;
        }
        if self.initial_synchronizing
            && transport_state == TerminalViewTransportState::Synchronizing
        {
            return Some(format!(
                "Synchronizing terminal | {}",
                self.target.display_name()
            ));
        }
        let device = self.target.display_name();
        if !self.is_remote() {
            return Some(format!("{device} | local"));
        }
        let (path, latency) = if transport_state != TerminalViewTransportState::Reconnecting {
            match self.path {
                TerminalViewConnectionPath::Direct => {
                    ("direct", self.rtt_ms.map(|rtt| format!("{rtt} ms")))
                }
                TerminalViewConnectionPath::Relay => {
                    ("relay", self.rtt_ms.map(|rtt| format!("{rtt} ms")))
                }
                TerminalViewConnectionPath::Unknown => ("--", None),
            }
        } else {
            ("--", None)
        };
        Some(format!(
            "{device} | {path} | {}",
            latency.as_deref().unwrap_or("--")
        ))
    }
}

mod viewport;
use viewport::{ViewportController, ViewportEffect, ViewportPresentationPacer};

fn append_resume_input(retained: &mut Vec<u8>, bytes: &[u8]) -> Result<(), CliError> {
    voytty_client::input::append_resume_input(retained, bytes).map_err(|_| resume_input_overflow())
}

fn resume_input_overflow() -> CliError {
    terminal_daemon_error(
        DomainErrorKind::ResourceExhausted,
        "terminal input retained while returning from history exceeded its fixed bound",
    )
}

mod input_codec;
use input_codec::{HostInputCodec, HostInputEvent, SgrMouse, parse_decimal, push_host_bytes};

fn history_owns_gestures(active_screen: ActiveScreen, modes: TerminalModes) -> bool {
    active_screen == ActiveScreen::Main && modes.mouse_mode == TerminalMouseMode::None
}

enum PointerRoute {
    Viewport(ViewportEffect),
    Child(Vec<u8>),
    SelectionChanged,
    Ignore,
}

fn route_pointer(
    mouse: &SgrMouse,
    viewport: &mut ViewportController,
    surface: &AttachmentSurface,
    selection: &mut SelectionController,
    allow_live_navigation: bool,
) -> Result<PointerRoute, CliError> {
    if viewport.gutter_drag_active() {
        selection.clear();
        return Ok(PointerRoute::Viewport(
            viewport
                .handle_gutter_mouse(mouse, allow_live_navigation)
                .unwrap_or(ViewportEffect::None),
        ));
    }

    if selection.owns_pointer_sequence() {
        if mouse.release {
            selection.finish();
            return Ok(PointerRoute::SelectionChanged);
        }
        if mouse.is_motion() {
            let source = viewport.selection_source_identity(surface);
            let point = mouse.content_point(viewport.content_size());
            let rows = viewport.selection_rows(surface);
            match (source, point, rows) {
                (Some(source), Some(point), Some(rows)) => selection
                    .update(source, point, rows)
                    .map_err(selection_error)?,
                _ => selection.cancel(),
            }
            return Ok(PointerRoute::SelectionChanged);
        }
        return Ok(PointerRoute::Ignore);
    }

    if viewport.gutter_hit(mouse) {
        selection.clear();
        return Ok(PointerRoute::Viewport(
            viewport
                .handle_gutter_mouse(mouse, allow_live_navigation)
                .unwrap_or(ViewportEffect::None),
        ));
    }
    if viewport.outside_layout(mouse) {
        return Ok(PointerRoute::Ignore);
    }

    if viewport.is_history() {
        if mouse.is_wheel() {
            selection.clear();
            return Ok(PointerRoute::Viewport(
                viewport.navigate(mouse.wheel_is_up(), 1),
            ));
        }
        if mouse.is_left_press() && mouse.is_unmodified() {
            return begin_pointer_selection(mouse, viewport, surface, selection);
        }
        return Ok(PointerRoute::Ignore);
    }

    let modes = surface.modes();
    if modes.mouse_mode != TerminalMouseMode::None {
        selection.clear();
        return Ok(route_mouse_to_child(mouse, surface.active_screen(), modes)
            .map(PointerRoute::Child)
            .unwrap_or(PointerRoute::Ignore));
    }
    if mouse.is_wheel()
        && surface.active_screen() == ActiveScreen::Alternate
        && modes.alternate_scroll
    {
        return Ok(PointerRoute::Child(emulated_wheel_cursor_keys(
            mouse.wheel_is_up(),
            modes.application_cursor,
        )));
    }
    if mouse.is_wheel()
        && allow_live_navigation
        && history_owns_gestures(surface.active_screen(), modes)
    {
        selection.clear();
        return Ok(PointerRoute::Viewport(
            viewport.navigate(mouse.wheel_is_up(), 1),
        ));
    }
    if mouse.is_left_press() && mouse.is_unmodified() {
        return begin_pointer_selection(mouse, viewport, surface, selection);
    }
    Ok(PointerRoute::Ignore)
}

fn begin_pointer_selection(
    mouse: &SgrMouse,
    viewport: &ViewportController,
    surface: &AttachmentSurface,
    selection: &mut SelectionController,
) -> Result<PointerRoute, CliError> {
    let Some(source) = viewport.selection_source_identity(surface) else {
        selection.clear();
        return Ok(PointerRoute::Ignore);
    };
    let Some(rows) = viewport.selection_rows(surface) else {
        selection.clear();
        return Ok(PointerRoute::Ignore);
    };
    let Some(point) = mouse.content_point(viewport.content_size()) else {
        return Ok(PointerRoute::Ignore);
    };
    selection
        .begin(source, point, rows)
        .map_err(selection_error)?;
    Ok(PointerRoute::SelectionChanged)
}

fn selection_error(error: TerminalTextSelectionError) -> CliError {
    let kind = match error {
        TerminalTextSelectionError::Clipboard(_) => DomainErrorKind::ResourceExhausted,
        TerminalTextSelectionError::InvalidRange | TerminalTextSelectionError::InvalidSurface => {
            DomainErrorKind::MalformedFrame
        }
    };
    terminal_daemon_error(kind, "terminal selection could not be projected")
}

const fn live_history_navigation_allowed(state: TerminalViewTransportState) -> bool {
    matches!(state, TerminalViewTransportState::Active)
}

fn route_mouse_to_child(
    mouse: &SgrMouse,
    active_screen: ActiveScreen,
    modes: TerminalModes,
) -> Option<Vec<u8>> {
    let input = voytty_client::mouse::MouseInput {
        code: mouse.code,
        column: mouse.column,
        row: mouse.row,
        release: mouse.release,
    };
    let encoded = input.route(active_screen, modes)?;
    // Preserve byte-exact physical SGR passthrough, including legal padding.
    Some(
        if modes.mouse_mode != TerminalMouseMode::None
            && modes.mouse_encoding == TerminalMouseEncoding::Sgr
        {
            mouse.raw.clone()
        } else {
            encoded
        },
    )
}

use voytty_client::mouse::cursor_wheel as emulated_wheel_cursor_keys;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]

enum DeltaRender {
    Applied,
    Gap,
}

enum KeyboardRoute {
    Copy,
    Consume,
    Forward {
        bytes: Vec<u8>,
        clear_selection: bool,
        reinterpret_legacy: bool,
    },
}

enum HostKeyboard {
    Terminal(voytty_core::terminal::TerminalKeyboardFlags),
    WindowsConsole,
}

fn route_enhanced_input(
    key: &EnhancedKey,
    child_modes: TerminalModes,
    host: HostKeyboard,
    selection_finalized: bool,
    lease: &mut CopyKeyLease,
) -> KeyboardRoute {
    if lease.consume(key) {
        return KeyboardRoute::Consume;
    }
    if selection_finalized && key.kind == KeyEventKind::Press && key.is_copy_shortcut() {
        lease.begin(key);
        return KeyboardRoute::Copy;
    }
    let (bytes, reinterpret_legacy) = match host {
        HostKeyboard::WindowsConsole => (
            key.input().encode(child_modes),
            child_modes.keyboard_flags.is_empty(),
        ),
        HostKeyboard::Terminal(flags)
            if selection_finalized
                && child_modes.keyboard_flags.is_empty()
                && flags
                    == keyboard::desired_outer_keyboard_flags(child_modes.keyboard_flags, true) =>
        {
            (key.input().legacy_bytes(child_modes), true)
        }
        HostKeyboard::Terminal(_) => (key.raw.clone(), false),
    };
    KeyboardRoute::Forward {
        bytes,
        clear_selection: selection_finalized && key.kind != KeyEventKind::Release,
        reinterpret_legacy,
    }
}

fn host_events_from_legacy_bytes(bytes: Vec<u8>) -> Vec<HostInputEvent> {
    if bytes == PAGE_UP {
        return vec![HostInputEvent::PageUp];
    }
    if bytes == PAGE_DOWN {
        return vec![HostInputEvent::PageDown];
    }
    let mut events = Vec::new();
    push_host_bytes(&mut events, bytes);
    events
}

fn reconcile_presenter_selection(
    selection: &mut SelectionController,
    viewport: &ViewportController,
    surface: &AttachmentSurface,
    presenter: &mut DesktopPresenter,
) {
    let source = viewport.selection_source_identity(surface);
    selection.reconcile(source);
    presenter.set_selection(
        source,
        selection.range_for(source),
        selection.is_finalized(),
    );
}

fn reconcile_presenter_selection_for_next_frame(
    selection: &mut SelectionController,
    viewport: &ViewportController,
    surface: &AttachmentSurface,
    presenter: &mut DesktopPresenter,
) {
    let source = viewport.next_frame_selection_source_identity(surface);
    selection.reconcile(source);
    presenter.set_selection(
        source,
        selection.range_for(source),
        selection.is_finalized(),
    );
}

fn write_selection_clipboard_stdout(
    selection: &SelectionController,
    viewport: &ViewportController,
    surface: &AttachmentSurface,
    presenter: &mut DesktopPresenter,
) -> Result<bool, CliError> {
    let Some(source) = viewport.selection_source_identity(surface) else {
        return Ok(false);
    };
    let Some(rows) = viewport.selection_rows(surface) else {
        return Ok(false);
    };
    let Some(write) = selection.extract(source, rows).map_err(selection_error)? else {
        return Ok(false);
    };
    let stdout = io::stdout();
    let mut output = stdout.lock();
    presenter.write_clipboard(&mut output, &write)?;
    Ok(true)
}

fn invalidate_selection_stdout(
    selection: &mut SelectionController,
    viewport: &mut ViewportController,
    surface: &AttachmentSurface,
    presenter: &mut DesktopPresenter,
    status: &StatusRenderer,
    transport_state: TerminalViewTransportState,
    pacer: &mut ViewportPresentationPacer,
) -> Result<bool, CliError> {
    let source = viewport.selection_source_identity(surface);
    let had_overlay = selection.range_for(source).is_some();
    let previous_flags = presenter.presented_keyboard_flags();
    selection.cancel();
    reconcile_presenter_selection(selection, viewport, surface, presenter);
    let flags_changed =
        previous_flags != presenter.outer_keyboard_flags(surface.modes().keyboard_flags);
    if !had_overlay && !flags_changed {
        return Ok(false);
    }
    pacer.cancel();
    present_surface_stdout(surface, presenter, viewport, status, transport_state)?;
    viewport.observe_presentation();
    pacer.mark_presented(Instant::now());
    Ok(true)
}

fn present_surface_with_writer(
    writer: &mut impl Write,
    surface: &AttachmentSurface,
    presenter: &mut DesktopPresenter,
    viewport: &ViewportController,
    status: &StatusRenderer,
    transport_state: TerminalViewTransportState,
) -> Result<bool, CliError> {
    let desired = ComposedFrame::compose(
        &surface.surface,
        presenter.semantic_baseline.as_ref(),
        viewport,
        status,
        transport_state,
    )?;
    presenter.present(
        writer,
        desired,
        viewport.next_frame_selection_source_identity(surface),
    )
}

fn present_surface_stdout(
    surface: &AttachmentSurface,
    presenter: &mut DesktopPresenter,
    viewport: &ViewportController,
    status: &StatusRenderer,
    transport_state: TerminalViewTransportState,
) -> Result<bool, CliError> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    present_surface_with_writer(
        &mut output,
        surface,
        presenter,
        viewport,
        status,
        transport_state,
    )
}

fn install_snapshot_stdout(
    surface: &mut AttachmentSurface,
    presenter: &mut DesktopPresenter,
    snapshot: &TerminalSurfaceSnapshot,
    viewport: &mut ViewportController,
    status: &StatusRenderer,
    transport_state: TerminalViewTransportState,
) -> Result<(), CliError> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    install_snapshot_with_writer(
        &mut output,
        surface,
        presenter,
        snapshot,
        viewport,
        status,
        transport_state,
    )
}

fn install_snapshot_with_writer(
    writer: &mut impl Write,
    surface: &mut AttachmentSurface,
    presenter: &mut DesktopPresenter,
    snapshot: &TerminalSurfaceSnapshot,
    viewport: &mut ViewportController,
    status: &StatusRenderer,
    transport_state: TerminalViewTransportState,
) -> Result<(), CliError> {
    let candidate = AttachmentSurface::from_snapshot(snapshot)
        .map_err(|_| semantic_surface_error("semantic terminal snapshot is invalid"))?;
    let layout = ChromeLayout::new(
        status.physical_size,
        viewport.effective_screen(candidate.active_screen()),
    );
    // ResumePending fences input, but its replacement frame must already
    // contain live cells. Background sync of pinned history keeps its view.
    let desired = if viewport.is_history() {
        ComposedFrame::compose(
            &candidate.surface,
            presenter.semantic_baseline.as_ref(),
            viewport,
            status,
            transport_state,
        )?
    } else {
        ComposedFrame::compose_live_candidate(
            &candidate.surface,
            presenter.semantic_baseline.as_ref(),
            viewport,
            LiveViewportProjection::new(
                layout.child,
                layout.gutter_column,
                snapshot.surface.scroll_metrics,
            ),
            status,
            transport_state,
        )?
    };
    let source = viewport.is_live().then_some(SelectionSourceIdentity::Live {
        revision: candidate.revision(),
        screen: candidate.active_screen(),
        viewport: layout.child,
    });
    presenter.present(writer, desired, source)?;
    viewport.set_layout(layout);
    viewport.observe_snapshot(snapshot.surface.scroll_metrics);
    *surface = candidate;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_delta_stdout(
    surface: &mut AttachmentSurface,
    presenter: &mut DesktopPresenter,
    delta: &TerminalSurfaceDelta,
    viewport: &mut ViewportController,
    selection: &mut SelectionController,
    status: &StatusRenderer,
    transport_state: TerminalViewTransportState,
    resume_barrier: bool,
) -> Result<DeltaRender, CliError> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    apply_delta_with_writer(
        &mut output,
        surface,
        presenter,
        delta,
        viewport,
        selection,
        status,
        transport_state,
        resume_barrier,
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_delta_with_writer(
    writer: &mut impl Write,
    surface: &mut AttachmentSurface,
    presenter: &mut DesktopPresenter,
    delta: &TerminalSurfaceDelta,
    viewport: &mut ViewportController,
    selection: &mut SelectionController,
    status: &StatusRenderer,
    transport_state: TerminalViewTransportState,
    resume_barrier: bool,
) -> Result<DeltaRender, CliError> {
    let Some(candidate) = surface
        .candidate_after_delta(delta)
        .map_err(|_| semantic_surface_error("semantic terminal delta is incompatible"))?
    else {
        return Ok(DeltaRender::Gap);
    };
    let render_live = viewport.is_live() || (resume_barrier && viewport.is_resume_pending());
    let live_layout =
        render_live.then(|| ChromeLayout::new(status.physical_size, candidate.active_screen()));
    let viewport_plan = viewport.preview_delta(&candidate, delta.scroll_metrics, live_layout);
    let mut candidate_selection = *selection;
    if render_live {
        if surface.surface.colors != candidate.surface.colors
            && surface.surface.rows == candidate.surface.rows
            && surface.surface.size == candidate.surface.size
            && surface.active_screen() == candidate.active_screen()
        {
            candidate_selection.rebase_live_revision(candidate.revision());
        } else {
            candidate_selection.cancel();
        }
    }
    candidate_selection.reconcile(viewport_plan.selection_source);
    let selection_presentation = candidate_selection.presentation(viewport_plan.selection_source);

    if render_live {
        let desired = ComposedFrame::compose_live_candidate(
            &candidate.surface,
            presenter.semantic_baseline.as_ref(),
            viewport,
            LiveViewportProjection::new(
                viewport_plan.content_size,
                viewport_plan.gutter_column,
                viewport_plan
                    .live_metrics
                    .filter(|metrics| metrics.is_valid()),
            ),
            status,
            transport_state,
        )?;
        presenter.present_candidate(
            writer,
            desired,
            viewport_plan.selection_source,
            selection_presentation,
        )?;
    } else if candidate.surface.colors != surface.surface.colors {
        let desired = ComposedFrame::compose(
            &candidate.surface,
            presenter.semantic_baseline.as_ref(),
            viewport,
            status,
            transport_state,
        )?;
        presenter.present_candidate(
            writer,
            desired,
            viewport_plan.selection_source,
            selection_presentation,
        )?;
    } else {
        presenter.sync_input_modes(writer, candidate.modes(), selection_presentation)?;
    }

    viewport.commit_delta(viewport_plan);
    if resume_barrier {
        viewport.observe_snapshot(candidate.surface.scroll_metrics);
    }
    *selection = candidate_selection;
    *surface = candidate;
    Ok(DeltaRender::Applied)
}

#[derive(Clone, Copy)]
struct CachedPresentationRequest {
    now: Instant,
    force: bool,
}

fn mark_cached_viewport_dirty(
    viewport: &ViewportController,
    pacer: &mut ViewportPresentationPacer,
    now: Instant,
) -> bool {
    if !viewport.has_complete_presentable_view() {
        pacer.cancel();
        return false;
    }
    pacer.mark_dirty(now);
    true
}

fn cancel_unpresentable_cached_viewport(
    viewport: &ViewportController,
    pacer: &mut ViewportPresentationPacer,
) {
    if !viewport.has_complete_presentable_view() {
        pacer.cancel();
    }
}

fn present_cached_viewport_stdout(
    surface: &AttachmentSurface,
    presenter: &mut DesktopPresenter,
    viewport: &mut ViewportController,
    status: &StatusRenderer,
    transport_state: TerminalViewTransportState,
    pacer: &mut ViewportPresentationPacer,
    request: CachedPresentationRequest,
) -> Result<bool, CliError> {
    if !request.force && !pacer.due(request.now) {
        return Ok(false);
    }
    if !viewport.has_complete_presentable_view() {
        pacer.cancel();
        return Ok(false);
    }
    let stdout = io::stdout();
    let mut output = stdout.lock();
    present_surface_with_writer(
        &mut output,
        surface,
        presenter,
        viewport,
        status,
        transport_state,
    )?;
    viewport.observe_presentation();
    pacer.mark_presented(request.now);
    Ok(true)
}

fn present_transport_transition_with_writer(
    writer: &mut impl Write,
    surface: &AttachmentSurface,
    presenter: &mut DesktopPresenter,
    viewport: &ViewportController,
    status: &StatusRenderer,
    transport_state: TerminalViewTransportState,
) -> Result<bool, CliError> {
    let resume_sync_is_visually_unchanged = viewport.is_resume_pending()
        && transport_state == TerminalViewTransportState::Synchronizing;
    if resume_sync_is_visually_unchanged {
        return Ok(false);
    }
    present_surface_with_writer(
        writer,
        surface,
        presenter,
        viewport,
        status,
        transport_state,
    )
}

fn present_transport_transition_stdout(
    surface: &AttachmentSurface,
    presenter: &mut DesktopPresenter,
    viewport: &ViewportController,
    status: &StatusRenderer,
    transport_state: TerminalViewTransportState,
) -> Result<bool, CliError> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    present_transport_transition_with_writer(
        &mut output,
        surface,
        presenter,
        viewport,
        status,
        transport_state,
    )
}

#[allow(clippy::too_many_arguments)]
async fn apply_viewport_effect(
    effect: ViewportEffect,
    viewport: &mut ViewportController,
    surface: &AttachmentSurface,
    presenter: &mut DesktopPresenter,
    writer: &voytty_daemon::client::view::TerminalViewCommandWriter,
    status: &StatusRenderer,
    transport_state: TerminalViewTransportState,
    pacer: &mut ViewportPresentationPacer,
    defer_cached_presentation: bool,
) -> Result<bool, CliError> {
    match effect {
        ViewportEffect::None => {
            cancel_unpresentable_cached_viewport(viewport, pacer);
            Ok(false)
        }
        ViewportEffect::Render => {
            let now = Instant::now();
            if mark_cached_viewport_dirty(viewport, pacer, now) {
                if !defer_cached_presentation {
                    let _ = present_cached_viewport_stdout(
                        surface,
                        presenter,
                        viewport,
                        status,
                        transport_state,
                        pacer,
                        CachedPresentationRequest { now, force: false },
                    )?;
                }
            } else {
                pacer.cancel();
                present_surface_stdout(surface, presenter, viewport, status, transport_state)?;
                viewport.observe_presentation();
                pacer.mark_presented(now);
            }
            Ok(false)
        }
        ViewportEffect::RequestHistoryWindow(query) => {
            cancel_unpresentable_cached_viewport(viewport, pacer);
            writer.request_history_window(query).await?;
            Ok(false)
        }
        ViewportEffect::RenderAndRequestHistoryWindow(query) => {
            writer.request_history_window(query).await?;
            let now = Instant::now();
            if mark_cached_viewport_dirty(viewport, pacer, now) && !defer_cached_presentation {
                let _ = present_cached_viewport_stdout(
                    surface,
                    presenter,
                    viewport,
                    status,
                    transport_state,
                    pacer,
                    CachedPresentationRequest { now, force: false },
                )?;
            }
            Ok(false)
        }
        ViewportEffect::Resume => {
            pacer.cancel();
            writer.request_sync(surface.revision()).await?;
            Ok(true)
        }
    }
}

#[cfg(all(test, unix))]
#[path = "../../tests/support/outer_terminal.rs"]
mod outer_terminal;

#[cfg(all(test, unix))]
#[path = "desktop_tests.rs"]
mod tests;
