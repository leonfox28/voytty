//! Typed frontend commands and events above the single Session client owner.
use crate::error::ClientError as DaemonError;
#[cfg(any(unix, windows))]
use crate::{
    protocol::{is_attachment_command_stream_closed, is_attachment_stream_closed_without_event},
    session::{LocalAttachmentEvent, SessionClient},
};
use std::fmt;
#[cfg(any(unix, windows))]
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use voytty_core::terminal::{
    TerminalClipboardWrite, TerminalHistoryWindowQuery, TerminalNotification, TerminalSize,
    TerminalSurfaceDelta, TerminalSurfaceHistoryWindowResult, TerminalSurfaceSnapshot,
};
#[cfg(any(unix, windows))]
use voytty_core::terminal::{TerminalHostEffect, TerminalHostEffects};
use voytty_core::{AttachmentId, DomainErrorKind, Revision, SessionId};
#[cfg(not(any(unix, windows)))]
fn unsupported_command_platform() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::UnsupportedPlatform,
        "terminal views require a supported desktop platform",
    )
}
#[cfg(any(unix, windows))]
const TERMINAL_DRIVER_CAPACITY: usize = 8;
#[cfg(any(unix, windows))]
const TERMINAL_CLOSURE_CORRELATION_WINDOW: Duration = Duration::from_millis(100);

/// Replacement state consumed by the one CLI compositor.
pub type TerminalViewSnapshot = TerminalSurfaceSnapshot;

/// Contiguous semantic update from one exact acknowledged revision.
pub type TerminalViewDelta = TerminalSurfaceDelta;

/// Monotonic attachment transport state owned by the frontend Session client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalViewTransportState {
    /// The frontend is acquiring the attachment transport.
    Preparing,
    /// A full state or resume delta is awaiting exact acknowledgement.
    Synchronizing,
    /// Input and resize may be sent.
    Active,
    /// The frontend is replacing a lost remote stream.
    Reconnecting,
}

/// Address-free selected network path for one remote terminal view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalViewConnectionPath {
    /// No current selected path has been observed.
    Unknown,
    /// Iroh selected a direct IP path.
    Direct,
    /// Iroh selected a relay path.
    Relay,
}

/// Immutable route chosen before opening one terminal view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalViewRoute {
    /// One direct same-UID Session IPC stream to this daemon.
    Local,
    /// One same-UID opaque tunnel to a remote daemon Session stream.
    Remote,
}

/// Redaction-safe presentation metadata frozen with the exact resolved target.
#[derive(Clone, Eq, PartialEq)]
pub struct TerminalViewTarget {
    pub(crate) display_name: String,
    pub(crate) route: TerminalViewRoute,
}

impl fmt::Debug for TerminalViewTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalViewTarget")
            .field("display_name", &"[REDACTED]")
            .field("display_name_len", &self.display_name.len())
            .field("route", &self.route)
            .finish()
    }
}

impl TerminalViewTarget {
    /// Builds presentation-only metadata. This value contains no routing
    /// authority; Session target selection remains frozen separately.
    #[must_use]
    pub fn for_display(display_name: impl Into<String>, route: TerminalViewRoute) -> Self {
        Self {
            display_name: display_name.into(),
            route,
        }
    }

    /// Frozen configured device name or exact remote alias for presentation.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Explicit transport route; display text never selects behavior.
    #[must_use]
    pub const fn route(&self) -> TerminalViewRoute {
        self.route
    }
}

/// Current selected path and RTT for a remote terminal view.
#[derive(Clone, Eq, PartialEq)]
pub struct TerminalViewConnectionStatus {
    path: TerminalViewConnectionPath,
    rtt_ms: Option<u32>,
}

impl fmt::Debug for TerminalViewConnectionStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalViewConnectionStatus")
            .field("path", &self.path)
            .field("rtt_ms", &self.rtt_ms)
            .finish()
    }
}

impl TerminalViewConnectionStatus {
    /// Current selected Iroh path class.
    #[must_use]
    pub const fn path(&self) -> TerminalViewConnectionPath {
        self.path
    }

    /// Current selected-path round-trip time in integer milliseconds.
    #[must_use]
    pub const fn rtt_ms(&self) -> Option<u32> {
        self.rtt_ms
    }
}

/// Stable reason that the daemon-owned Session ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalViewEndReason {
    /// The root shell exited naturally.
    NaturalExit,
    /// An explicit Session close ended the PTY.
    ExplicitClose,
    /// Daemon shutdown ended the PTY.
    DaemonStop,
    /// The retained terminal driver failed.
    DriverFailure,
}

/// Terminal lifecycle details containing no rendered terminal bytes.
#[derive(Clone, Eq, PartialEq)]
pub struct TerminalViewEnded {
    /// Stable Session end reason.
    pub reason: TerminalViewEndReason,
    /// Exit code when the platform reported one.
    pub exit_code: u32,
    /// Bounded platform signal name when reported.
    pub signal: String,
}

/// Stateless renderer-neutral history-window response.
pub type TerminalViewHistoryWindow = TerminalSurfaceHistoryWindowResult;

impl fmt::Debug for TerminalViewEnded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalViewEnded")
            .field("reason", &self.reason)
            .field("exit_code", &self.exit_code)
            .field("has_signal", &!self.signal.is_empty())
            .finish_non_exhaustive()
    }
}

/// One typed event from the frontend-owned terminal driver.
#[derive(Clone, Eq, PartialEq)]
pub enum TerminalViewEvent {
    /// Attachment transport state changed.
    TransportState(TerminalViewTransportState),
    /// Selected path and RTT changed for this remote view.
    ConnectionStatus(TerminalViewConnectionStatus),
    /// Replace the local rendered state atomically.
    Snapshot(TerminalViewSnapshot),
    /// Apply one merged update only when its baseline is contiguous.
    Delta(TerminalViewDelta),
    /// Correlated reconnect barrier; acknowledge once after successful application.
    ResumeDelta(TerminalViewDelta),
    /// One correlated stateless bounded history-window outcome.
    HistoryWindow(TerminalViewHistoryWindow),
    /// One validated latest-only child clipboard write.
    ClipboardWrite(TerminalClipboardWrite),
    /// One validated, non-replayed ordinary terminal notification.
    Notification(TerminalNotification),
    /// The following snapshot replaces the current live rendering baseline.
    SyncRequired {
        /// Latest host revision declared by the synchronization marker.
        latest_revision: Revision,
    },
    /// Another controller replaced this attachment.
    LeaseLost {
        /// Controller generation which replaced this view.
        generation: u64,
    },
    /// The daemon-owned Session and PTY ended.
    SessionEnded(TerminalViewEnded),
}

impl fmt::Debug for TerminalViewEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TransportState(state) => formatter
                .debug_tuple("TransportState")
                .field(state)
                .finish(),
            Self::ConnectionStatus(status) => formatter
                .debug_tuple("ConnectionStatus")
                .field(status)
                .finish(),
            Self::Snapshot(snapshot) => formatter.debug_tuple("Snapshot").field(snapshot).finish(),
            Self::Delta(delta) => formatter.debug_tuple("Delta").field(delta).finish(),
            Self::ResumeDelta(delta) => formatter.debug_tuple("ResumeDelta").field(delta).finish(),
            Self::HistoryWindow(window) => formatter
                .debug_tuple("HistoryWindow")
                .field(window)
                .finish(),
            Self::Notification(value) => {
                formatter.debug_tuple("Notification").field(value).finish()
            }
            Self::ClipboardWrite(write) => formatter
                .debug_tuple("ClipboardWrite")
                .field(write)
                .finish(),
            Self::SyncRequired { latest_revision } => formatter
                .debug_struct("SyncRequired")
                .field("latest_revision", latest_revision)
                .finish(),
            Self::LeaseLost { generation } => formatter
                .debug_struct("LeaseLost")
                .field("generation", generation)
                .finish(),
            Self::SessionEnded(ended) => {
                formatter.debug_tuple("SessionEnded").field(ended).finish()
            }
        }
    }
}

/// Opaque prepared terminal view handed to the raw-terminal UI.
///
/// The socket, frame codec, target token, operation lease, and takeover replay
/// owner remain private to the daemon crate.
pub struct PreparedTerminalView {
    session_id: SessionId,
    attachment_id: AttachmentId,
    initial_snapshot: TerminalViewSnapshot,
    takeover: bool,
    target: TerminalViewTarget,
    #[cfg(any(unix, windows))]
    client: SessionClient,
}

impl fmt::Debug for PreparedTerminalView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedTerminalView")
            .field("session_id", &self.session_id)
            .field("attachment_id", &self.attachment_id)
            .field("initial_revision", &self.initial_snapshot.revision)
            .field("takeover", &self.takeover)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl PreparedTerminalView {
    #[cfg(any(unix, windows))]
    /// Takes ownership of a validated initial attachment, pending display acknowledgement.
    pub fn new(
        mut client: SessionClient,
        takeover: bool,
        target: TerminalViewTarget,
    ) -> Result<Self, DaemonError> {
        let initial_snapshot = client.take_initial_snapshot().ok_or_else(|| {
            terminal_protocol_error("prepared attachment already transferred its initial snapshot")
        })?;
        Ok(Self {
            session_id: client.session_id(),
            attachment_id: client.attachment_id(),
            initial_snapshot,
            takeover,
            target,
            client,
        })
    }

    /// Stable Session selected by this prepared view.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Target-issued attachment identity for this initial stream epoch.
    #[must_use]
    pub const fn attachment_id(&self) -> AttachmentId {
        self.attachment_id
    }

    /// Revision of the initial authoritative snapshot retained internally.
    #[must_use]
    pub const fn initial_revision(&self) -> Revision {
        self.initial_snapshot.revision
    }

    /// Initial authoritative state which must be flushed before acknowledgement.
    #[must_use]
    pub const fn initial_snapshot(&self) -> &TerminalViewSnapshot {
        &self.initial_snapshot
    }

    /// Frozen route and redaction-safe device label used by universal chrome.
    #[must_use]
    pub const fn target(&self) -> &TerminalViewTarget {
        &self.target
    }

    /// Acknowledges the exact flushed initial state and starts one typed driver.
    ///
    /// A requested takeover is initiated before the driver starts, so the same
    /// owner retains its daemon-issued operation lease, response correlation,
    /// decoder, and socket for the complete transition.
    #[cfg(any(unix, windows))]
    pub async fn acknowledge_initial(mut self) -> Result<TerminalViewIo, DaemonError> {
        self.client
            .snapshot_applied(self.initial_snapshot.revision)
            .await?;
        if self.takeover {
            self.client.begin_takeover().await?;
        }
        let initial_state = if self.client.reconnect_pending() {
            TerminalViewTransportState::Reconnecting
        } else if self.takeover {
            TerminalViewTransportState::Synchronizing
        } else {
            TerminalViewTransportState::Active
        };
        Ok(spawn_terminal_driver(
            self.client,
            initial_state,
            self.target.route,
            self.takeover,
        ))
    }

    /// Unsupported targets have no raw terminal implementation.
    #[cfg(not(any(unix, windows)))]
    pub async fn acknowledge_initial(self) -> Result<TerminalViewIo, DaemonError> {
        let _ = self;
        Err(unsupported_command_platform())
    }
}

/// Running typed terminal view, split only above the single daemon driver owner.
pub struct TerminalViewIo {
    reader: TerminalViewEventReader,
    writer: TerminalViewCommandWriter,
}

impl fmt::Debug for TerminalViewIo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalViewIo")
            .finish_non_exhaustive()
    }
}

impl TerminalViewIo {
    /// Separates typed event consumption from typed command submission.
    #[must_use]
    pub fn split(self) -> (TerminalViewEventReader, TerminalViewCommandWriter) {
        (self.reader, self.writer)
    }
}

/// Bounded event side of one frontend-owned terminal driver.
pub struct TerminalViewEventReader {
    #[cfg(any(unix, windows))]
    receiver: tokio::sync::mpsc::Receiver<Result<TerminalViewEvent, DaemonError>>,
    #[cfg(any(unix, windows))]
    deferred_visual: Option<TerminalViewEvent>,
    #[cfg(any(unix, windows))]
    effects: Arc<TerminalEffectSlot>,
    #[cfg(any(unix, windows))]
    effect_wakeup: tokio::sync::watch::Receiver<()>,
}

impl fmt::Debug for TerminalViewEventReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalViewEventReader")
            .finish_non_exhaustive()
    }
}

impl TerminalViewEventReader {
    /// Waits for the next typed event; active reads have no synthetic timeout.
    pub async fn read_event(&mut self) -> Result<Option<TerminalViewEvent>, DaemonError> {
        #[cfg(any(unix, windows))]
        {
            if let Some(event) = self.deferred_visual.take() {
                return Ok(Some(event));
            }
            tokio::select! {
                    biased;
                    event = self.receiver.recv() => {
                        if event.is_none() { self.effects.clear(); }
                        if let Some(Ok(event)) = event {
                            // Alternate ordinary visual updates with pending effects. Lifecycle
                            // transitions keep priority; continuous repaint cannot starve a request.
                            if matches!(event, TerminalViewEvent::Snapshot(_) | TerminalViewEvent::Delta(_)
                                | TerminalViewEvent::ResumeDelta(_) | TerminalViewEvent::HistoryWindow(_)
                                | TerminalViewEvent::ConnectionStatus(_))
                                && let Some(effect) = self.effects.take()
                            {
                                self.deferred_visual = Some(event);
                                return Ok(Some(view_host_effect(effect)));
                            }
                            return Ok(Some(event));
                        }
                        event.transpose()
                    }
                    effect = self.effects.next(&mut self.effect_wakeup) => {
                        Ok(Some(view_host_effect(effect)))
                    }
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(unsupported_command_platform())
        }
    }
}

#[cfg(any(unix, windows))]
fn view_host_effect(effect: TerminalHostEffect) -> TerminalViewEvent {
    match effect {
        TerminalHostEffect::ClipboardWrite(write) => TerminalViewEvent::ClipboardWrite(write),
        TerminalHostEffect::Notification(notification) => {
            TerminalViewEvent::Notification(notification)
        }
    }
}

#[cfg(any(unix, windows))]
struct TerminalEffectSlot {
    pending: Mutex<TerminalHostEffects>,
    wake: tokio::sync::watch::Sender<()>,
}

#[cfg(any(unix, windows))]
impl TerminalEffectSlot {
    fn new() -> (Arc<Self>, tokio::sync::watch::Receiver<()>) {
        let (wake, receiver) = tokio::sync::watch::channel(());
        (
            Arc::new(Self {
                pending: Mutex::new(TerminalHostEffects::default()),
                wake,
            }),
            receiver,
        )
    }

    fn push(&self, effect: TerminalHostEffect) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(effect);
        self.wake.send_replace(());
    }

    fn take(&self) -> Option<TerminalHostEffect> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop()
    }

    fn clear(&self) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    async fn next(&self, wake: &mut tokio::sync::watch::Receiver<()>) -> TerminalHostEffect {
        loop {
            wake.borrow_and_update();
            if let Some(effect) = self.take() {
                return effect;
            }
            // This owner retains the sender while a reader is alive.
            if wake.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// Cloneable typed command side of one frontend-owned terminal driver.
#[derive(Clone)]
pub struct TerminalViewCommandWriter {
    #[cfg(any(unix, windows))]
    sender: tokio::sync::mpsc::Sender<PendingTerminalCommand>,
    #[cfg(any(unix, windows))]
    terminal_outcome_queued: tokio::sync::watch::Receiver<bool>,
    #[cfg(any(unix, windows))]
    applied_revision: Arc<AtomicU64>,
}

impl fmt::Debug for TerminalViewCommandWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalViewCommandWriter")
            .finish_non_exhaustive()
    }
}

impl TerminalViewCommandWriter {
    /// Captures the current resolved upload destination at the terminal driver.
    #[cfg(any(unix, windows))]
    pub async fn upload_origin(&self) -> Result<crate::upload::UploadOrigin, DaemonError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.submit(TerminalDriverCommand::UploadOrigin { reply })
            .await?;
        result
            .await
            .map_err(|_| terminal_command_outcome_unavailable())?
    }

    /// Writes a completed upload's paste only if the original attachment still owns this driver.
    #[cfg(any(unix, windows))]
    pub async fn write_upload_input(
        &self,
        origin: crate::upload::UploadOrigin,
        bytes: Vec<u8>,
    ) -> Result<(), DaemonError> {
        self.submit(TerminalDriverCommand::UploadInput { origin, bytes })
            .await
    }

    /// Records the exact revision successfully installed by the frontend.
    /// This is local state for remote resume and sends no Session frame.
    pub fn revision_applied(&self, revision: Revision) {
        #[cfg(any(unix, windows))]
        self.applied_revision
            .fetch_max(revision.get(), Ordering::AcqRel);
        #[cfg(not(any(unix, windows)))]
        let _ = revision;
    }

    /// Acknowledges an exactly flushed replacement snapshot or resume delta.
    pub async fn snapshot_applied(&self, revision: Revision) -> Result<(), DaemonError> {
        #[cfg(any(unix, windows))]
        {
            self.submit(TerminalDriverCommand::SnapshotApplied { revision })
                .await
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = revision;
            Err(unsupported_command_platform())
        }
    }

    /// Sends ordinary controller input. Callers must gate this on `Active`.
    pub async fn write_input(&self, bytes: Vec<u8>) -> Result<(), DaemonError> {
        #[cfg(any(unix, windows))]
        {
            self.submit(TerminalDriverCommand::Input { bytes }).await
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = bytes;
            Err(unsupported_command_platform())
        }
    }

    /// Sends the latest validated viewport. Callers coalesce while non-active.
    pub async fn resize(&self, size: TerminalSize) -> Result<(), DaemonError> {
        #[cfg(any(unix, windows))]
        {
            self.submit(TerminalDriverCommand::Resize { size }).await
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = size;
            Err(unsupported_command_platform())
        }
    }

    /// Publishes current controller color observations.
    pub async fn update_colors(
        &self,
        profile: voytty_core::terminal::TerminalColorProfile,
    ) -> Result<(), DaemonError> {
        #[cfg(any(unix, windows))]
        {
            self.submit(TerminalDriverCommand::Colors { profile }).await
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = profile;
            Err(unsupported_command_platform())
        }
    }

    /// Requests a full replacement after a revision gap.
    pub async fn request_sync(&self, known_revision: Revision) -> Result<(), DaemonError> {
        #[cfg(any(unix, windows))]
        {
            self.submit(TerminalDriverCommand::RequestSync { known_revision })
                .await
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = known_revision;
            Err(unsupported_command_platform())
        }
    }

    /// Requests one stateless bounded history window.
    pub async fn request_history_window(
        &self,
        query: TerminalHistoryWindowQuery,
    ) -> Result<(), DaemonError> {
        #[cfg(any(unix, windows))]
        {
            self.submit(TerminalDriverCommand::RequestHistoryWindow { query })
                .await
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = query;
            Err(unsupported_command_platform())
        }
    }

    /// Detaches this view while leaving the Session and PTY running.
    pub async fn detach(&self) -> Result<(), DaemonError> {
        #[cfg(any(unix, windows))]
        {
            self.submit(TerminalDriverCommand::Detach).await
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(unsupported_command_platform())
        }
    }

    #[cfg(any(unix, windows))]
    async fn submit(&self, command: TerminalDriverCommand) -> Result<(), DaemonError> {
        let deadline = crate::protocol::control_deadline();
        let (response, receiver) = tokio::sync::oneshot::channel();
        let pending = PendingTerminalCommand {
            command,
            deadline,
            response,
        };
        tokio::time::timeout_at(deadline, async {
            if self.sender.send(pending).await.is_err() {
                return self.correlate_terminal_outcome().await;
            }
            match receiver.await {
                Ok(result) => result,
                Err(_) => self.correlate_terminal_outcome().await,
            }
        })
        .await
        .unwrap_or_else(|_| Err(crate::protocol::control_timeout()))
    }

    #[cfg(any(unix, windows))]
    async fn correlate_terminal_outcome(&self) -> Result<(), DaemonError> {
        let mut queued = self.terminal_outcome_queued.clone();
        if *queued.borrow_and_update() {
            return Ok(());
        }
        match tokio::time::timeout(TERMINAL_CLOSURE_CORRELATION_WINDOW, queued.changed()).await {
            Ok(Ok(())) if *queued.borrow_and_update() => Ok(()),
            Ok(Ok(()) | Err(_)) | Err(_) => Err(terminal_command_outcome_unavailable()),
        }
    }
}

#[cfg(any(unix, windows))]
struct PendingTerminalCommand {
    command: TerminalDriverCommand,
    deadline: tokio::time::Instant,
    response: tokio::sync::oneshot::Sender<Result<(), DaemonError>>,
}

#[cfg(any(unix, windows))]
enum TerminalDriverCommand {
    UploadOrigin {
        reply: tokio::sync::oneshot::Sender<Result<crate::upload::UploadOrigin, DaemonError>>,
    },
    UploadInput {
        origin: crate::upload::UploadOrigin,
        bytes: Vec<u8>,
    },
    Colors {
        profile: voytty_core::terminal::TerminalColorProfile,
    },
    SnapshotApplied {
        revision: Revision,
    },
    Input {
        bytes: Vec<u8>,
    },
    Resize {
        size: TerminalSize,
    },
    RequestSync {
        known_revision: Revision,
    },
    RequestHistoryWindow {
        query: TerminalHistoryWindowQuery,
    },
    Detach,
}

#[cfg(any(unix, windows))]
struct TerminalDriverInitial {
    state: TerminalViewTransportState,
    route: TerminalViewRoute,
    takeover: bool,
}

#[cfg(any(unix, windows))]
fn spawn_terminal_driver(
    client: SessionClient,
    initial_state: TerminalViewTransportState,
    route: TerminalViewRoute,
    takeover: bool,
) -> TerminalViewIo {
    let applied_revision = client.applied_revision_tracker();
    let (command_sender, command_receiver) = tokio::sync::mpsc::channel(TERMINAL_DRIVER_CAPACITY);
    let (event_sender, event_receiver) = tokio::sync::mpsc::channel(TERMINAL_DRIVER_CAPACITY);
    let (effects, effect_wakeup) = TerminalEffectSlot::new();
    let (terminal_outcome_sender, terminal_outcome_receiver) = tokio::sync::watch::channel(false);
    tokio::spawn(run_terminal_driver(
        client,
        command_receiver,
        event_sender,
        Arc::clone(&effects),
        terminal_outcome_sender,
        TerminalDriverInitial {
            state: initial_state,
            route,
            takeover,
        },
    ));
    TerminalViewIo {
        reader: TerminalViewEventReader {
            receiver: event_receiver,
            deferred_visual: None,
            effects,
            effect_wakeup,
        },
        writer: TerminalViewCommandWriter {
            sender: command_sender,
            terminal_outcome_queued: terminal_outcome_receiver,
            applied_revision,
        },
    }
}

#[cfg(any(unix, windows))]
async fn run_terminal_driver(
    mut client: SessionClient,
    mut commands: tokio::sync::mpsc::Receiver<PendingTerminalCommand>,
    events: tokio::sync::mpsc::Sender<Result<TerminalViewEvent, DaemonError>>,
    effects: Arc<TerminalEffectSlot>,
    terminal_outcome_queued: tokio::sync::watch::Sender<bool>,
    initial: TerminalDriverInitial,
) {
    use std::collections::VecDeque;

    let TerminalDriverInitial {
        state: initial_state,
        route,
        takeover,
    } = initial;
    let mut pending = VecDeque::from([Ok(TerminalViewEvent::TransportState(initial_state))]);
    if route == TerminalViewRoute::Remote {
        pending.push_back(Ok(TerminalViewEvent::ConnectionStatus(
            TerminalViewConnectionStatus {
                path: TerminalViewConnectionPath::Unknown,
                rtt_ms: None,
            },
        )));
    }
    let mut stop_after_pending = false;
    let mut commands_open = true;
    let mut local_takeover_pending = takeover;
    let mut awaiting_snapshot_ack = false;
    let mut last_state = initial_state;
    let mut diagnostic_state = None;

    loop {
        if diagnostic_state != Some(last_state) {
            diagnostic_state = Some(last_state);
            use voytty_diagnostics::{Event, Kind, Stage};
            let stage = match last_state {
                TerminalViewTransportState::Preparing => Stage::Preparing,
                TerminalViewTransportState::Synchronizing => Stage::Synchronizing,
                TerminalViewTransportState::Active => Stage::Active,
                TerminalViewTransportState::Reconnecting => Stage::Reconnecting,
            };
            voytty_diagnostics::record(
                Event::new(Kind::ViewStateChanged)
                    .session(client.session_id())
                    .attachment(client.attachment_id())
                    .stage(stage),
            );
        }
        if pending.is_empty() {
            tokio::select! {
                command = commands.recv() => {
                    if apply_terminal_driver_command(
                        command,
                        &mut client,
                        &mut pending,
                        route,
                        &mut local_takeover_pending,
                        &mut awaiting_snapshot_ack,
                        &mut last_state,
                        &mut stop_after_pending,
                        &effects,
                    ).await {
                        effects.clear();
                        return;
                    }
                }
                () = events.closed() => {
                    effects.clear();
                    // Retire input admission and queued command replies before
                    // waiting for the peer to finish this attachment's detach.
                    drop(commands);
                    let _ = client.detach().await;
                    return;
                }
                event = client.read_next_event() => {
                    let delivered = event.is_ok();
                    if queue_local_attachment_event(
                        event,
                        &mut pending,
                        route,
                        &mut local_takeover_pending,
                        &mut awaiting_snapshot_ack,
                        &mut last_state,
                        &effects,
                    ) {
                        stop_after_pending = true;
                    }
                    if delivered && !stop_after_pending {
                        // The read above is cancellable; this handler is not, so
                        // the receipt write cannot be abandoned mid-frame. A
                        // write failure is classified by the read half.
                        let _ = client.report_surface_frames().await;
                    }
                }
            }
            continue;
        }

        tokio::select! {
            command = commands.recv(), if commands_open => {
                if stop_after_pending {
                    // A writer may be waiting for its command before reading the
                    // terminal event. Wake it without replacing the known outcome.
                    if let Some(command) = command {
                        let _ = command.response.send(Ok(()));
                    } else {
                        commands_open = false;
                    }
                    continue;
                }
                if apply_terminal_driver_command(
                    command,
                    &mut client,
                    &mut pending,
                    route,
                    &mut local_takeover_pending,
                    &mut awaiting_snapshot_ack,
                    &mut last_state,
                    &mut stop_after_pending,
                    &effects,
                ).await {
                    effects.clear();
                    return;
                }
            }
            permit = events.reserve() => {
                let Ok(permit) = permit else {
                    effects.clear();
                    drop(commands);
                    let _ = client.detach().await;
                    return;
                };
                let event = pending.pop_front().expect("pending event was checked above");
                permit.send(event);
                if pending.is_empty() && stop_after_pending {
                    effects.clear();
                    terminal_outcome_queued.send_replace(true);
                    return;
                }
            }
        }
    }
}

#[cfg(any(unix, windows))]
#[allow(clippy::too_many_arguments)]
async fn apply_terminal_driver_command(
    command: Option<PendingTerminalCommand>,
    client: &mut SessionClient,
    pending: &mut std::collections::VecDeque<Result<TerminalViewEvent, DaemonError>>,
    route: TerminalViewRoute,
    local_takeover_pending: &mut bool,
    awaiting_snapshot_ack: &mut bool,
    last_state: &mut TerminalViewTransportState,
    stop_after_pending: &mut bool,
    effects: &TerminalEffectSlot,
) -> bool {
    match handle_terminal_driver_command(command, client).await {
        TerminalDriverCommandResult::Continue => false,
        TerminalDriverCommandResult::SnapshotApplied => {
            *awaiting_snapshot_ack = false;
            if !client.reconnect_pending()
                && !*local_takeover_pending
                && *last_state != TerminalViewTransportState::Active
            {
                *last_state = TerminalViewTransportState::Active;
                pending.push_back(Ok(TerminalViewEvent::TransportState(*last_state)));
            }
            false
        }
        TerminalDriverCommandResult::CommandStreamClosed { response } => {
            correlate_terminal_command_closure(
                client,
                pending,
                route,
                local_takeover_pending,
                awaiting_snapshot_ack,
                last_state,
                response,
                effects,
            )
            .await;
            *stop_after_pending = true;
            false
        }
        TerminalDriverCommandResult::Stop => true,
    }
}

#[cfg(any(unix, windows))]
#[allow(clippy::too_many_arguments)]
async fn correlate_terminal_command_closure(
    client: &mut SessionClient,
    pending: &mut std::collections::VecDeque<Result<TerminalViewEvent, DaemonError>>,
    route: TerminalViewRoute,
    local_takeover_pending: &mut bool,
    awaiting_snapshot_ack: &mut bool,
    last_state: &mut TerminalViewTransportState,
    response: tokio::sync::oneshot::Sender<Result<(), DaemonError>>,
    effects: &TerminalEffectSlot,
) {
    let deadline = tokio::time::Instant::now() + TERMINAL_CLOSURE_CORRELATION_WINDOW;
    loop {
        let event = match tokio::time::timeout_at(deadline, client.read_next_event()).await {
            Ok(Ok(event)) => Ok(event),
            Ok(Err(error)) if !is_attachment_stream_closed_without_event(&error) => Err(error),
            Ok(Err(_)) | Err(_) => {
                pending.push_back(Err(terminal_command_outcome_unavailable()));
                break;
            }
        };
        if queue_local_attachment_event(
            event,
            pending,
            route,
            local_takeover_pending,
            awaiting_snapshot_ack,
            last_state,
            effects,
        ) {
            break;
        }
    }
    // The event side now owns the only user-visible terminal outcome, avoiding
    // a race between a raw write failure and a queued typed lifecycle event.
    let _ = response.send(Ok(()));
}

#[cfg(any(unix, windows))]
fn terminal_command_outcome_unavailable() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::DaemonStopped,
        "terminal attachment closed while a command was in flight",
    )
}

#[cfg(any(unix, windows))]
fn queue_local_attachment_event(
    event: Result<LocalAttachmentEvent, DaemonError>,
    pending: &mut std::collections::VecDeque<Result<TerminalViewEvent, DaemonError>>,
    route: TerminalViewRoute,
    local_takeover_pending: &mut bool,
    awaiting_snapshot_ack: &mut bool,
    last_state: &mut TerminalViewTransportState,
    effects: &TerminalEffectSlot,
) -> bool {
    match event {
        Ok(LocalAttachmentEvent::Notification(notification)) => {
            effects.push(TerminalHostEffect::Notification(notification));
            false
        }
        Ok(LocalAttachmentEvent::ClipboardWrite(write)) => {
            effects.push(TerminalHostEffect::ClipboardWrite(write));
            false
        }
        Ok(LocalAttachmentEvent::Takeover(_)) if *local_takeover_pending => {
            *local_takeover_pending = false;
            if !*awaiting_snapshot_ack {
                *last_state = TerminalViewTransportState::Active;
                pending.push_back(Ok(TerminalViewEvent::TransportState(*last_state)));
            }
            false
        }
        Ok(LocalAttachmentEvent::Takeover(_)) => false,
        Ok(LocalAttachmentEvent::TransportState(state)) => {
            match terminal_transport_state_from_wire(state.state) {
                Ok(TerminalViewTransportState::Preparing) => false,
                Ok(TerminalViewTransportState::Reconnecting) => {
                    effects.clear();
                    if *last_state == TerminalViewTransportState::Reconnecting {
                        false
                    } else {
                        *last_state = TerminalViewTransportState::Reconnecting;
                        pending.push_back(Ok(TerminalViewEvent::TransportState(*last_state)));
                        false
                    }
                }
                Ok(state) if state == *last_state => false,
                Ok(state) => {
                    if state == TerminalViewTransportState::Active {
                        *awaiting_snapshot_ack = false;
                    }
                    *last_state = state;
                    pending.push_back(Ok(TerminalViewEvent::TransportState(state)));
                    false
                }
                Err(error) => {
                    effects.clear();
                    pending.push_back(Err(error));
                    true
                }
            }
        }
        Ok(event) => {
            let terminal = matches!(
                event,
                LocalAttachmentEvent::LeaseLost(_) | LocalAttachmentEvent::SessionEnded(_)
            );
            if terminal {
                effects.clear();
            }
            if local_event_requires_synchronizing(&event) {
                *awaiting_snapshot_ack = true;
                if *last_state != TerminalViewTransportState::Synchronizing {
                    *last_state = TerminalViewTransportState::Synchronizing;
                    pending.push_back(Ok(TerminalViewEvent::TransportState(*last_state)));
                }
            }
            match terminal_event_from_local(event, route) {
                Ok(Some(event)) => pending.push_back(Ok(event)),
                Ok(None) => {}
                Err(error) => {
                    pending.push_back(Err(error));
                    return true;
                }
            }
            terminal
        }
        Err(error) => {
            effects.clear();
            pending.push_back(Err(error));
            true
        }
    }
}

#[cfg(any(unix, windows))]
async fn handle_terminal_driver_command(
    command: Option<PendingTerminalCommand>,
    client: &mut SessionClient,
) -> TerminalDriverCommandResult {
    let Some(PendingTerminalCommand {
        command,
        deadline,
        response,
    }) = command
    else {
        let _ = client.detach().await;
        return TerminalDriverCommandResult::Stop;
    };
    if deadline <= tokio::time::Instant::now() || response.is_closed() {
        let _ = response.send(Err(crate::protocol::control_timeout()));
        return TerminalDriverCommandResult::Continue;
    }
    if let TerminalDriverCommand::UploadInput { origin, .. } = &command {
        let current = client.upload_origin();
        if !current.is_ok_and(|current| current == *origin) {
            let _ = response.send(Err(DaemonError::new(
                DomainErrorKind::LeaseLost,
                "upload belongs to a retired attachment",
            )));
            return TerminalDriverCommandResult::Continue;
        }
    }
    // A frontend cache invalidation can ask again while the previous window
    // response is still in flight. That local rejection writes no frame, so
    // it must not end an otherwise healthy attachment.
    if let TerminalDriverCommand::RequestHistoryWindow { query } = &command
        && let Err(error) = client.check_history_window_request(*query)
    {
        let _ = response.send(Err(error));
        return TerminalDriverCommandResult::Continue;
    }
    let success = match command {
        TerminalDriverCommand::SnapshotApplied { .. } => {
            TerminalDriverCommandResult::SnapshotApplied
        }
        TerminalDriverCommand::Detach => TerminalDriverCommandResult::Stop,
        _ => TerminalDriverCommandResult::Continue,
    };
    let result = tokio::time::timeout_at(deadline, async {
        match command {
            TerminalDriverCommand::UploadOrigin { reply } => {
                let _ = reply.send(client.upload_origin());
                Ok(())
            }
            TerminalDriverCommand::UploadInput { bytes, .. } => client.write_input(bytes).await,
            TerminalDriverCommand::SnapshotApplied { revision } => {
                client.snapshot_applied(revision).await
            }
            TerminalDriverCommand::Input { bytes } => client.write_input(bytes).await,
            TerminalDriverCommand::Resize { size } => client.resize(size).await,
            TerminalDriverCommand::Colors { profile } => client.update_colors(profile).await,
            TerminalDriverCommand::RequestSync { known_revision } => {
                client.request_sync(known_revision).await
            }
            TerminalDriverCommand::RequestHistoryWindow { query } => {
                client.request_history_window(query).await
            }
            TerminalDriverCommand::Detach => client.detach().await,
        }
    })
    .await
    .unwrap_or_else(|_| {
        client.invalidate_transport();
        Err(crate::protocol::control_timeout())
    });
    if result
        .as_ref()
        .err()
        .is_some_and(is_attachment_command_stream_closed)
    {
        if matches!(&success, TerminalDriverCommandResult::Stop) {
            let _ = response.send(Ok(()));
            return TerminalDriverCommandResult::Stop;
        }
        return TerminalDriverCommandResult::CommandStreamClosed { response };
    }
    let failed = result.is_err();
    let _ = response.send(result);
    if failed {
        TerminalDriverCommandResult::Stop
    } else {
        success
    }
}

#[cfg(any(unix, windows))]
enum TerminalDriverCommandResult {
    Continue,
    SnapshotApplied,
    CommandStreamClosed {
        response: tokio::sync::oneshot::Sender<Result<(), DaemonError>>,
    },
    Stop,
}

#[cfg(any(unix, windows))]
fn local_event_requires_synchronizing(event: &LocalAttachmentEvent) -> bool {
    matches!(
        event,
        LocalAttachmentEvent::Snapshot(_)
            | LocalAttachmentEvent::ResumeDelta(_)
            | LocalAttachmentEvent::SyncRequired(_)
    )
}

#[cfg(any(unix, windows))]
fn terminal_event_from_local(
    event: LocalAttachmentEvent,
    route: TerminalViewRoute,
) -> Result<Option<TerminalViewEvent>, DaemonError> {
    match event {
        LocalAttachmentEvent::Snapshot(snapshot) => Ok(Some(TerminalViewEvent::Snapshot(snapshot))),
        LocalAttachmentEvent::Delta(delta) => Ok(Some(TerminalViewEvent::Delta(delta))),
        LocalAttachmentEvent::ResumeDelta(delta) => Ok(Some(TerminalViewEvent::ResumeDelta(delta))),
        LocalAttachmentEvent::HistoryWindow(result) => {
            Ok(Some(TerminalViewEvent::HistoryWindow(result)))
        }
        LocalAttachmentEvent::ConnectionStatus(status) => {
            if route != TerminalViewRoute::Remote {
                return Err(terminal_protocol_error(
                    "local terminal received remote connection status",
                ));
            }
            let path = match voytty_proto::v2::TerminalConnectionPath::try_from(status.path)
                .map_err(|_| terminal_protocol_error("unknown terminal connection path"))?
            {
                voytty_proto::v2::TerminalConnectionPath::Unknown => {
                    TerminalViewConnectionPath::Unknown
                }
                voytty_proto::v2::TerminalConnectionPath::Direct => {
                    TerminalViewConnectionPath::Direct
                }
                voytty_proto::v2::TerminalConnectionPath::Relay => {
                    TerminalViewConnectionPath::Relay
                }
                voytty_proto::v2::TerminalConnectionPath::Unspecified => {
                    return Err(terminal_protocol_error(
                        "terminal connection path was unspecified",
                    ));
                }
            };
            Ok(Some(TerminalViewEvent::ConnectionStatus(
                TerminalViewConnectionStatus {
                    path,
                    rtt_ms: status.rtt_ms,
                },
            )))
        }
        LocalAttachmentEvent::SyncRequired(required) => Ok(Some(TerminalViewEvent::SyncRequired {
            latest_revision: Revision::new(required.latest_revision),
        })),
        LocalAttachmentEvent::LeaseLost(lost) => Ok(Some(TerminalViewEvent::LeaseLost {
            generation: lost.generation,
        })),
        LocalAttachmentEvent::SessionEnded(ended) => {
            let reason = match voytty_proto::v2::TerminalSessionEndReason::try_from(ended.reason)
                .map_err(|_| terminal_protocol_error("unknown terminal session end reason"))?
            {
                voytty_proto::v2::TerminalSessionEndReason::NaturalExit => {
                    TerminalViewEndReason::NaturalExit
                }
                voytty_proto::v2::TerminalSessionEndReason::ExplicitClose => {
                    TerminalViewEndReason::ExplicitClose
                }
                voytty_proto::v2::TerminalSessionEndReason::DaemonStop => {
                    TerminalViewEndReason::DaemonStop
                }
                voytty_proto::v2::TerminalSessionEndReason::DriverFailure => {
                    TerminalViewEndReason::DriverFailure
                }
                voytty_proto::v2::TerminalSessionEndReason::Unspecified => {
                    return Err(terminal_protocol_error(
                        "terminal session end reason was unspecified",
                    ));
                }
            };
            Ok(Some(TerminalViewEvent::SessionEnded(TerminalViewEnded {
                reason,
                exit_code: ended.exit_code,
                signal: ended.signal,
            })))
        }
        LocalAttachmentEvent::TransportState(_)
        | LocalAttachmentEvent::Takeover(_)
        | LocalAttachmentEvent::ClipboardWrite(_)
        | LocalAttachmentEvent::Notification(_) => Ok(None),
    }
}

#[cfg(any(unix, windows))]
fn terminal_transport_state_from_wire(
    value: i32,
) -> Result<TerminalViewTransportState, DaemonError> {
    match voytty_proto::v2::TerminalTransportState::try_from(value)
        .map_err(|_| terminal_protocol_error("unknown terminal transport state"))?
    {
        voytty_proto::v2::TerminalTransportState::Preparing => {
            Ok(TerminalViewTransportState::Preparing)
        }
        voytty_proto::v2::TerminalTransportState::Synchronizing => {
            Ok(TerminalViewTransportState::Synchronizing)
        }
        voytty_proto::v2::TerminalTransportState::Active => Ok(TerminalViewTransportState::Active),
        voytty_proto::v2::TerminalTransportState::Reconnecting => {
            Ok(TerminalViewTransportState::Reconnecting)
        }
        voytty_proto::v2::TerminalTransportState::Unspecified => Err(terminal_protocol_error(
            "terminal transport state was unspecified",
        )),
    }
}

#[cfg(any(unix, windows))]
fn terminal_protocol_error(detail: &'static str) -> DaemonError {
    DaemonError::new(DomainErrorKind::MalformedFrame, detail)
}

#[cfg(test)]
mod tests;
