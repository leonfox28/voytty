//! Transport-independent live session registry and attachment state machine.

mod attachments;
mod service;

use attachments::{dispatch_command, final_terminal_update, reap_detached};
#[cfg(test)]
use std::collections::VecDeque;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
#[cfg(all(test, unix))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use voytty_core::terminal::TerminalColorProfile;

use iroh::SecretKey;
use tokio::sync::watch;
#[cfg(all(unix, test))]
use voytty_core::terminal::TerminalHistoryWindowAnchor;
#[cfg(any(unix, windows))]
use voytty_core::terminal::TerminalHistoryWindowQuery;
#[cfg(any(unix, windows))]
use voytty_core::terminal::TerminalHostEffect;
#[cfg(all(unix, test))]
use voytty_core::terminal::TerminalSurfaceHistoryWindowResult;
use voytty_core::terminal::{
    TerminalSize, TerminalSurfaceDelta, TerminalSurfaceDeltaResult, TerminalSurfaceSnapshot,
};
use voytty_core::{
    AttachmentId, AttachmentPrincipal, ControllerLease, DaemonIncarnation, DeviceId,
    DomainErrorKind, OperationId, OperationLease, ResourceLimits, ResumeViewId, Revision,
    SessionEndReason, SessionId, SessionName, SessionSelector,
};
use voytty_platform::account::EffectiveAccount;
use voytty_platform::pty::{
    PtyChildState, PtyError, PtyHost, PtyPathKind, PtySession, PtySize, TerminalIdentity,
};
use voytty_platform::user_state::UserPaths;
use voytty_terminal::{TerminalError, TerminalModel};

use crate::error::DaemonError;
use crate::terminal_driver::{
    TerminalAttachment, TerminalDriver, TerminalDriverConfig, TerminalDriverError,
    TerminalDriverInterrupt, TerminalDriverOwnership, spawn_background_reaper,
};
#[cfg(any(unix, windows))]
use crate::terminal_driver::{TerminalEffectBroker, TerminalHistoryRead};

const OPERATION_RESULTS_PER_EPOCH: usize = 128;
const MAX_ACTIVE_OPERATION_EPOCHS: usize = 64;
const MAX_REPLAY_PRINCIPALS: usize = 64;
const SESSION_COMMAND_CAPACITY: usize = 16;
const SESSION_MONITOR_INTERVAL: Duration = Duration::from_millis(10);
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_QUEUED: u8 = 0;
const COMMAND_STARTED: u8 = 1;
const COMMAND_EXPIRED: u8 = 2;

type SpawnSession =
    dyn Fn(TerminalSize, Option<&Path>) -> Result<(PtySession, PathBuf), DaemonError> + Send + Sync;
type MutationResult = Result<SessionSummary, DaemonError>;
type FinalAttachmentUpdate = Result<Option<AttachmentUpdate>, DaemonError>;
type FinalAttachmentUpdateSlot = Arc<Mutex<Option<FinalAttachmentUpdate>>>;

pub use voytty_client::model::SessionSummary;

/// Latest state transition of one attachment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttachmentLifecycle {
    /// A full snapshot is waiting for an exact acknowledgement.
    AwaitingSnapshot {
        /// Snapshot revision which must be applied atomically.
        revision: Revision,
    },
    /// This attachment owns input at the given lease generation.
    Active {
        /// Monotonic controller generation.
        generation: u64,
    },
    /// Snapshot synchronization completed and takeover may now commit.
    PreparedTakeover,
    /// Another attachment atomically took over this controller.
    LeaseLost {
        /// Generation now owned by the replacement controller.
        generation: u64,
    },
    /// The root shell and PTY have ended.
    SessionEnded(SessionEndReason),
}

/// Read-only upload authority; dropping it never detaches a terminal.
#[cfg(any(unix, windows))]
pub(crate) struct UploadAdmission {
    pub(crate) generation: u64,
    pub(crate) lifecycle: watch::Receiver<AttachmentLifecycle>,
    pub(crate) detached: Arc<AtomicBool>,
}

#[cfg(any(unix, windows))]
impl UploadAdmission {
    pub(crate) fn is_current(&self) -> bool {
        !self.detached.load(Ordering::Acquire)
            && match *self.lifecycle.borrow() {
                AttachmentLifecycle::Active { generation } => generation == self.generation,
                AttachmentLifecycle::AwaitingSnapshot { .. } => true,
                _ => false,
            }
    }
}

/// One latest terminal update for a synchronized attachment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttachmentUpdate {
    /// A full replacement snapshot is required.
    Snapshot(TerminalSurfaceSnapshot),
    /// One merged delta advances the current checkpoint.
    Delta(TerminalSurfaceDelta),
}

/// Result of preparing a new local or future remote attachment.
pub struct PreparedAttachment {
    /// Handle used by one duplex transport stream.
    pub attachment: Arc<SessionAttachment>,
    /// Initial full host-authoritative state.
    pub snapshot: TerminalSurfaceSnapshot,
    /// Initial merged resume update when an exact host checkpoint matched.
    #[cfg(any(unix, windows))]
    pub(crate) initial_delta: Option<TerminalSurfaceDelta>,
}

/// Exact optional resume identity supplied by an authenticated remote view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RemoteResumeRequest {
    pub(crate) view_id: ResumeViewId,
    pub(crate) known_revision: Option<Revision>,
}

/// Initial model geometry and observations installed before starting a PTY.
pub(crate) struct InitialTerminal {
    pub(crate) viewport: Option<TerminalSize>,
    pub(crate) colors: TerminalColorProfile,
}

/// Fully validated remote-only attachment preparation arguments.
#[cfg(any(unix, windows))]
pub(crate) struct RemoteAttachmentRequest {
    pub(crate) base_colors: TerminalColorProfile,
    pub(crate) selector: Option<SessionSelector>,
    pub(crate) create_main: bool,
    pub(crate) takeover: bool,
    pub(crate) initial_viewport: Option<TerminalSize>,
    pub(crate) resume: RemoteResumeRequest,
}

/// Per-session impact of detaching one remote principal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PrincipalDetachOutcome {
    /// Attachments removed from this session.
    pub attachments_removed: usize,
    /// Whether the removed principal owned the controller lease.
    pub controller_released: bool,
}

/// Aggregate impact of detaching one remote principal across every session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PrincipalDetachImpact {
    /// Sessions in which at least one attachment was removed.
    pub sessions_affected: usize,
    /// Total attachments removed across all sessions.
    pub attachments_removed: usize,
    /// Number of sessions whose controller lease was released.
    pub controllers_released: usize,
}

/// Transport-independent handle to one live attachment.
pub struct SessionAttachment {
    actor: Arc<SessionActor>,
    attachment_id: AttachmentId,
    detached: Arc<AtomicBool>,
    revisions: watch::Receiver<Revision>,
    lifecycle: watch::Receiver<AttachmentLifecycle>,
    #[cfg(any(unix, windows))]
    effect_broker: TerminalEffectBroker,
    #[cfg(any(unix, windows))]
    effect_wakeup: watch::Receiver<()>,
    #[cfg(any(unix, windows))]
    final_update: FinalAttachmentUpdateSlot,
}

impl SessionAttachment {
    /// Stable attachment identity.
    #[must_use]
    pub const fn attachment_id(&self) -> AttachmentId {
        self.attachment_id
    }

    /// Session identity shared by every attachment to the same PTY.
    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.actor.id
    }

    /// Subscribes to the latest terminal revision without a revision queue.
    pub fn revision_watch(&self) -> Result<watch::Receiver<Revision>, DaemonError> {
        Ok(self.revisions.clone())
    }

    /// Subscribes to lifecycle changes such as takeover or session end.
    pub fn lifecycle_watch(&self) -> Result<watch::Receiver<AttachmentLifecycle>, DaemonError> {
        Ok(self.lifecycle.clone())
    }

    /// Subscribes to payload-free transient host-effect wakeups.
    #[cfg(any(unix, windows))]
    pub(crate) fn effect_watch(&self) -> watch::Receiver<()> {
        self.effect_wakeup.clone()
    }

    /// Takes only a transient effect bound to this exact attachment.
    #[cfg(any(unix, windows))]
    pub(crate) fn take_host_effect(&self) -> Result<Option<TerminalHostEffect>, DaemonError> {
        self.effect_broker
            .take_for(self.attachment_id)
            .map_err(map_driver_error)
    }

    /// Confirms a full snapshot, or returns a newer replacement snapshot.
    pub fn snapshot_applied(
        &self,
        revision: Revision,
    ) -> Result<Option<TerminalSurfaceSnapshot>, DaemonError> {
        self.snapshot_applied_until(revision, default_deadline())
    }

    pub(crate) fn snapshot_applied_until(
        &self,
        revision: Revision,
        deadline: Instant,
    ) -> Result<Option<TerminalSurfaceSnapshot>, DaemonError> {
        self.actor
            .request(deadline, |meta, reply| SessionCommand::SnapshotApplied {
                meta,
                attachment_id: self.attachment_id,
                revision,
                reply,
            })
    }

    /// Produces one merged latest delta or a full resynchronization.
    pub fn next_update(&self) -> Result<Option<AttachmentUpdate>, DaemonError> {
        self.next_update_until(default_deadline())
    }

    pub(crate) fn next_update_until(
        &self,
        deadline: Instant,
    ) -> Result<Option<AttachmentUpdate>, DaemonError> {
        self.actor
            .request(deadline, |meta, reply| SessionCommand::NextUpdate {
                meta,
                attachment_id: self.attachment_id,
                reply,
            })
    }

    /// Produces the final drained terminal update before a terminal lifecycle event.
    #[cfg(any(unix, windows))]
    pub(crate) fn final_update_until(
        &self,
        deadline: Instant,
    ) -> Result<Option<AttachmentUpdate>, DaemonError> {
        if let Some(update) = lock(&self.final_update, "attachment final update")?.take() {
            return update;
        }
        self.actor
            .request(deadline, |meta, reply| SessionCommand::FinalUpdate {
                meta,
                attachment_id: self.attachment_id,
                reply,
            })
    }

    /// Discards a client baseline and returns a fresh full snapshot.
    pub fn sync_latest(
        &self,
        known_revision: Revision,
    ) -> Result<TerminalSurfaceSnapshot, DaemonError> {
        self.sync_latest_until(known_revision, default_deadline())
    }

    pub(crate) fn sync_latest_until(
        &self,
        known_revision: Revision,
        deadline: Instant,
    ) -> Result<TerminalSurfaceSnapshot, DaemonError> {
        self.actor
            .request(deadline, |meta, reply| SessionCommand::SyncLatest {
                meta,
                attachment_id: self.attachment_id,
                known_revision,
                reply,
            })
    }

    /// Waits for an eligible history read outside the Session actor.
    #[cfg(all(unix, test))]
    pub(crate) fn history_window_until(
        &self,
        query: TerminalHistoryWindowQuery,
        deadline: Instant,
    ) -> Result<TerminalSurfaceHistoryWindowResult, DaemonError> {
        let read = self.begin_history_window_until(query, deadline)?;
        let result = read.wait_until(deadline).map_err(map_driver_error);
        self.validate_history_controller_until(deadline)?;
        result
    }

    #[cfg(any(unix, windows))]
    pub(crate) fn begin_history_window_until(
        &self,
        query: TerminalHistoryWindowQuery,
        deadline: Instant,
    ) -> Result<TerminalHistoryRead, DaemonError> {
        self.actor
            .request(deadline, |meta, reply| SessionCommand::HistoryWindow {
                meta,
                attachment_id: self.attachment_id,
                query,
                reply,
            })
    }

    #[cfg(any(unix, windows))]
    pub(crate) fn validate_history_controller_until(
        &self,
        deadline: Instant,
    ) -> Result<(), DaemonError> {
        self.actor.request(deadline, |meta, reply| {
            SessionCommand::ValidateHistoryController {
                meta,
                attachment_id: self.attachment_id,
                reply,
            }
        })
    }

    /// Writes controller bytes only after snapshot synchronization.
    pub fn write_input(&self, bytes: &[u8]) -> Result<(), DaemonError> {
        self.write_input_until(bytes, default_deadline())
    }

    pub(crate) fn write_input_until(
        &self,
        bytes: &[u8],
        deadline: Instant,
    ) -> Result<(), DaemonError> {
        self.actor
            .request(deadline, |meta, reply| SessionCommand::WriteInput {
                meta,
                attachment_id: self.attachment_id,
                bytes: bytes.to_vec(),
                reply,
            })
    }

    /// Atomically resizes the native PTY and terminal model.
    pub fn resize(&self, size: TerminalSize) -> Result<Revision, DaemonError> {
        self.resize_until(size, default_deadline())
    }

    pub(crate) fn update_colors_until(
        &self,
        sequence: u64,
        base_colors: TerminalColorProfile,
        deadline: Instant,
    ) -> Result<(), DaemonError> {
        self.actor
            .request(deadline, |meta, reply| SessionCommand::UpdateColors {
                meta,
                attachment_id: self.attachment_id,
                sequence,
                base_colors,
                reply,
            })
    }

    pub(crate) fn resize_until(
        &self,
        size: TerminalSize,
        deadline: Instant,
    ) -> Result<Revision, DaemonError> {
        self.actor
            .request(deadline, |meta, reply| SessionCommand::Resize {
                meta,
                attachment_id: self.attachment_id,
                size,
                reply,
            })
    }

    /// Releases only this attachment; the session and PTY continue running.
    pub fn detach(&self) {
        self.detached.store(true, Ordering::Release);
    }

    /// Moves an active authenticated remote controller checkpoint into the
    /// SessionActor's sole resume cell and releases its controller lease.
    #[cfg(any(unix, windows))]
    pub(crate) fn detach_for_remote_resume_until(
        &self,
        deadline: Instant,
    ) -> Result<bool, DaemonError> {
        self.actor.request(deadline, |meta, reply| {
            SessionCommand::DetachForRemoteResume {
                meta,
                attachment_id: self.attachment_id,
                reply,
            }
        })
    }
}

impl Drop for SessionAttachment {
    fn drop(&mut self) {
        self.detach();
    }
}

/// The unique transport-independent service for all live sessions in one daemon.
#[derive(Clone)]
pub struct SessionService {
    inner: Arc<RegistryInner>,
    own_device_id: DeviceId,
    spawner: Arc<SpawnSession>,
    limits: ResourceLimits,
    replay: Arc<Mutex<ReplayRegistry>>,
    #[cfg(test)]
    panic_creation_after_spawn: Arc<AtomicBool>,
    #[cfg(test)]
    panic_next_creation_cleanup: Arc<AtomicBool>,
}

#[derive(Clone, Eq, PartialEq)]
enum OperationFingerprint {
    Create {
        base_colors: TerminalColorProfile,
        name: SessionName,
        working_directory: Option<PathBuf>,
        viewport: Option<TerminalSize>,
    },
    Rename {
        session_id: SessionId,
        new_name: SessionName,
    },
    Close {
        session_id: SessionId,
    },
    Takeover {
        session_id: SessionId,
    },
}

impl fmt::Debug for OperationFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create {
                base_colors: _,
                name,
                working_directory,
                viewport,
            } => formatter
                .debug_struct("Create")
                .field("name", name)
                .field("working_directory", &"[REDACTED]")
                .field("working_directory_present", &working_directory.is_some())
                .field("viewport", viewport)
                .finish(),
            Self::Rename {
                session_id,
                new_name,
            } => formatter
                .debug_struct("Rename")
                .field("session_id", session_id)
                .field("new_name", new_name)
                .finish(),
            Self::Close { session_id } => formatter
                .debug_struct("Close")
                .field("session_id", session_id)
                .finish(),
            Self::Takeover { session_id } => formatter
                .debug_struct("Takeover")
                .field("session_id", session_id)
                .finish(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReplayOperationKey {
    replay_key: ReplayKey,
    operation_id: OperationId,
}

#[derive(Clone)]
struct SessionEntry {
    actor: Arc<SessionActor>,
    name: SessionName,
    working_directory: PathBuf,
    ownership: OwnershipToken,
}

impl SessionEntry {
    fn summary(&self) -> Result<SessionSummary, DaemonError> {
        let runtime = self.actor.runtime_summary()?;
        Ok(SessionSummary {
            session_id: self.actor.id,
            name: self.name.clone(),
            revision: self.actor.latest_revision(),
            has_controller: runtime.has_controller,
            working_directory: self.working_directory.clone(),
            viewport: runtime.viewport,
        })
    }
}

/// Unforgeable in-process ownership identity shared by a creation name slot,
/// its session-count reservation, and the eventual provisional/live actor entry.
/// Cleanup always compares this identity before removing any reservation.
#[derive(Clone)]
struct OwnershipToken(Arc<()>);

impl OwnershipToken {
    fn new() -> Self {
        Self(Arc::new(()))
    }

    fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Default)]
struct RegistryInner {
    state: Mutex<RegistryState>,
    reservations: Mutex<ReservationState>,
    #[cfg(test)]
    candidate_ids: Mutex<VecDeque<SessionId>>,
    #[cfg(test)]
    fail_provisional_registration: AtomicBool,
}

impl RegistryInner {
    fn reserve_name(
        &self,
        name: &SessionName,
        wait_for_starting: bool,
    ) -> Result<NameReservation, DaemonError> {
        let mut state = lock(&self.state, "session registry")?;
        if !state.accepting {
            return Err(DaemonError::new(
                DomainErrorKind::Cancelled,
                "session registry is shutting down",
            ));
        }
        match state.by_name.get(name) {
            Some(NameSlot::Live { session_id, .. }) => {
                let entry = state
                    .by_id
                    .get(session_id)
                    .cloned()
                    .ok_or_else(session_not_found)?;
                Ok(NameReservation::Existing(entry))
            }
            Some(NameSlot::Starting { creation, .. }) if wait_for_starting => {
                Ok(NameReservation::Waiting(Arc::clone(creation)))
            }
            Some(NameSlot::Starting { .. }) => Err(session_already_exists(name)),
            None => {
                let creation = Arc::new(CreationCell::default());
                state.by_name.insert(
                    name.clone(),
                    NameSlot::Starting {
                        creation: Arc::clone(&creation),
                        session_id: None,
                    },
                );
                Ok(NameReservation::Owner(creation))
            }
        }
    }

    fn publish_name(
        &self,
        name: &SessionName,
        creation: &Arc<CreationCell>,
        session_id: SessionId,
        actor: &Arc<SessionActor>,
    ) -> Result<bool, DaemonError> {
        let mut state = lock(&self.state, "session registry")?;
        let owns_reservation = state.accepting
            && !creation.is_cancelled()
            && state.by_name.get(name).is_some_and(|slot| {
                matches!(
                    slot,
                    NameSlot::Starting {
                        creation: current,
                        session_id: Some(current_id),
                    } if Arc::ptr_eq(current, creation) && *current_id == session_id
                )
            });
        if !owns_reservation {
            return Ok(false);
        }
        let owns_provisional = state
            .provisional
            .get(&session_id)
            .is_some_and(|entry| Arc::ptr_eq(&entry.actor, actor));
        if !owns_provisional {
            return Ok(false);
        }
        let Some(entry) = state.provisional.remove(&session_id) else {
            return Ok(false);
        };
        state.by_name.insert(
            name.clone(),
            NameSlot::Live {
                session_id,
                ownership: entry.ownership.clone(),
            },
        );
        state.by_id.insert(session_id, entry);
        Ok(true)
    }

    fn register_provisional(
        &self,
        session_id: SessionId,
        entry: SessionEntry,
    ) -> Result<(), DaemonError> {
        #[cfg(test)]
        if self
            .fail_provisional_registration
            .swap(false, Ordering::AcqRel)
        {
            return Err(synchronization_error("injected provisional registration"));
        }
        let mut state = lock(&self.state, "session registry")?;
        if state.by_id.contains_key(&session_id)
            || state.provisional.contains_key(&session_id)
            || state
                .cleanup_only
                .iter()
                .any(|entry| entry.actor.id == session_id)
        {
            return Err(resource_error("duplicate provisional session identity"));
        }
        let owns_name = state.by_name.get(&entry.name).is_some_and(|slot| {
            matches!(
                slot,
                NameSlot::Starting {
                    creation,
                    session_id: Some(current_id),
                } if *current_id == session_id && creation.ownership.ptr_eq(&entry.ownership)
            )
        });
        if !owns_name {
            return Err(resource_error(
                "provisional actor does not own its starting name slot",
            ));
        }
        state.provisional.insert(session_id, entry);
        Ok(())
    }

    /// Cleanup fallback for an actor which started before ordinary provisional
    /// registration reported an error (for example, a poisoned normal lock).
    /// Poison is recovered here and the exact name/resource token is checked;
    /// an unrelated owner is never overwritten.
    fn retain_provisional_for_cleanup(&self, session_id: SessionId, entry: SessionEntry) {
        let mut state = cleanup_lock(&self.state);
        if state
            .provisional
            .get(&session_id)
            .is_some_and(|current| Arc::ptr_eq(&current.actor, &entry.actor))
            || state
                .by_id
                .get(&session_id)
                .is_some_and(|current| Arc::ptr_eq(&current.actor, &entry.actor))
            || state
                .cleanup_only
                .iter()
                .any(|current| Arc::ptr_eq(&current.actor, &entry.actor))
        {
            return;
        }
        let owns_name = state.by_name.get(&entry.name).is_some_and(|slot| {
            matches!(
                slot,
                NameSlot::Starting {
                    creation,
                    session_id: Some(current_id),
                } if *current_id == session_id && creation.ownership.ptr_eq(&entry.ownership)
            )
        });
        let reservations = cleanup_lock(&self.reservations);
        let owns_reservation = reservations
            .reservations
            .get(&session_id)
            .is_some_and(|ownership| ownership.ptr_eq(&entry.ownership));
        drop(reservations);
        if owns_name
            && owns_reservation
            && !state.by_id.contains_key(&session_id)
            && !state.provisional.contains_key(&session_id)
        {
            state.provisional.insert(session_id, entry);
        } else {
            // An impossible/corrupt ID conflict still must not hide a started
            // actor. This cleanup-only owner is included in shutdown and is
            // removed by actor identity, without touching unrelated tokens.
            state.cleanup_only.push(entry);
        }
    }

    fn rename(
        &self,
        session_id: SessionId,
        new_name: SessionName,
    ) -> Result<SessionEntry, DaemonError> {
        let mut state = lock(&self.state, "session registry")?;
        if !state.accepting {
            return Err(DaemonError::new(
                DomainErrorKind::Cancelled,
                "session registry is shutting down",
            ));
        }
        let current = state
            .by_id
            .get(&session_id)
            .cloned()
            .ok_or_else(session_not_found)?;
        if current.name.is_main() {
            return Err(reserved_main());
        }
        if state.by_name.contains_key(&new_name) {
            return Err(session_already_exists(&new_name));
        }
        let owns_old_name = state.by_name.get(&current.name).is_some_and(|slot| {
            matches!(
                slot,
                NameSlot::Live {
                    session_id: current_id,
                    ownership,
                } if *current_id == session_id && ownership.ptr_eq(&current.ownership)
            )
        });
        if !owns_old_name {
            return Err(synchronization_error("session name ownership"));
        }
        state.by_name.remove(&current.name);
        state.by_name.insert(
            new_name.clone(),
            NameSlot::Live {
                session_id,
                ownership: current.ownership.clone(),
            },
        );
        let updated = SessionEntry {
            name: new_name,
            ..current
        };
        state.by_id.insert(session_id, updated.clone());
        Ok(updated)
    }

    fn resolve(&self, selector: &SessionSelector) -> Result<Arc<SessionActor>, DaemonError> {
        let state = lock(&self.state, "session registry")?;
        let entry = match selector {
            SessionSelector::Id(session_id) => state.by_id.get(session_id),
            SessionSelector::Name(name) => state.by_name.get(name).and_then(|slot| match slot {
                NameSlot::Live { session_id, .. } => state.by_id.get(session_id),
                NameSlot::Starting { .. } => None,
            }),
        };
        entry
            .map(|entry| Arc::clone(&entry.actor))
            .ok_or_else(session_not_found)
    }

    fn entry(&self, session_id: SessionId) -> Result<SessionEntry, DaemonError> {
        lock(&self.state, "session registry")?
            .by_id
            .get(&session_id)
            .cloned()
            .ok_or_else(session_not_found)
    }

    fn live_entries(&self) -> Result<Vec<SessionEntry>, DaemonError> {
        Ok(lock(&self.state, "session registry")?
            .by_id
            .values()
            .cloned()
            .collect())
    }

    fn reserve_creation_session(
        &self,
        name: &SessionName,
        creation: &Arc<CreationCell>,
        limits: ResourceLimits,
    ) -> Result<SessionId, DaemonError> {
        // State-before-reservations is also used by completion. Holding the state
        // lock makes shutdown cancellation and identity/session-count reservation
        // one atomic ownership boundary. No path may acquire these locks in
        // the opposite order.
        let mut state = lock(&self.state, "session registry")?;
        let owns_reservation = state.accepting
            && !creation.is_cancelled()
            && state.by_name.get(name).is_some_and(|slot| {
                matches!(
                    slot,
                    NameSlot::Starting {
                        creation: current,
                        session_id: None,
                    } if Arc::ptr_eq(current, creation)
                )
            });
        if !owns_reservation {
            return Err(DaemonError::new(
                DomainErrorKind::Cancelled,
                "session creation reservation was cancelled",
            ));
        }
        let mut reservations = lock(&self.reservations, "session reservations")?;
        if reservations.reservations.len() >= limits.max_live_sessions {
            return Err(resource_error("live session limit reached"));
        }
        let session_id = (0..16)
            .map(|_| self.next_session_id_candidate())
            .find(|candidate| {
                !state.by_id.contains_key(candidate)
                    && !state.provisional.contains_key(candidate)
                    && !state
                        .cleanup_only
                        .iter()
                        .any(|entry| entry.actor.id == *candidate)
                    && !reservations.reservations.contains_key(candidate)
            })
            .ok_or_else(|| resource_error("unable to allocate a unique session identity"))?;
        let replaced = reservations
            .reservations
            .insert(session_id, creation.ownership.clone());
        debug_assert!(
            replaced.is_none(),
            "atomic identity reservation replaced an owner"
        );
        let Some(NameSlot::Starting {
            creation: current,
            session_id: current_id,
        }) = state.by_name.get_mut(name)
        else {
            unreachable!("verified starting name slot disappeared while locked")
        };
        debug_assert!(Arc::ptr_eq(current, creation));
        *current_id = Some(session_id);
        Ok(session_id)
    }

    fn next_session_id_candidate(&self) -> SessionId {
        #[cfg(test)]
        if let Some(candidate) = cleanup_lock(&self.candidate_ids).pop_front() {
            return candidate;
        }
        SessionId::from_array(random_16())
    }

    #[cfg(all(test, unix))]
    fn inject_session_id_candidates(&self, candidates: impl IntoIterator<Item = SessionId>) {
        cleanup_lock(&self.candidate_ids).extend(candidates);
    }

    #[cfg(all(test, unix))]
    fn fail_next_provisional_registration(&self) {
        self.fail_provisional_registration
            .store(true, Ordering::Release);
    }

    /// Releases a creation which never acquired a registry-visible actor.
    /// Both the name and session-count reservation are retained unless this exact creation
    /// token still owns them.
    fn release_creation(
        &self,
        name: &SessionName,
        creation: &Arc<CreationCell>,
        session_id: Option<SessionId>,
    ) {
        let mut state = cleanup_lock(&self.state);
        let owns_name = state.by_name.get(name).is_some_and(|slot| {
            matches!(
                slot,
                NameSlot::Starting {
                    creation: current,
                    session_id: current_id,
                } if Arc::ptr_eq(current, creation) && *current_id == session_id
            )
        });
        if let Some(session_id) = session_id {
            let mut reservations = cleanup_lock(&self.reservations);
            let owns_reservation = reservations
                .reservations
                .get(&session_id)
                .is_some_and(|ownership| ownership.ptr_eq(&creation.ownership));
            if owns_reservation {
                reservations.reservations.remove(&session_id);
            }
        }
        if owns_name {
            state.by_name.remove(name);
        }
    }

    fn reservation_count(&self) -> Result<usize, DaemonError> {
        Ok(cleanup_lock(&self.reservations).reservations.len())
    }

    fn complete(&self, session_id: SessionId, actor: &Arc<SessionActor>) {
        let mut state = cleanup_lock(&self.state);
        let live_matches = state
            .by_id
            .get(&session_id)
            .is_some_and(|entry| Arc::ptr_eq(&entry.actor, actor));
        let provisional_matches = state
            .provisional
            .get(&session_id)
            .is_some_and(|entry| Arc::ptr_eq(&entry.actor, actor));
        let cleanup_position = state
            .cleanup_only
            .iter()
            .position(|entry| Arc::ptr_eq(&entry.actor, actor));
        let entry = if live_matches {
            state.by_id.get(&session_id).cloned()
        } else if provisional_matches {
            state.provisional.get(&session_id).cloned()
        } else {
            cleanup_position.map(|position| state.cleanup_only[position].clone())
        };
        let Some(entry) = entry else {
            return;
        };
        let owns_name = state
            .by_name
            .get(&entry.name)
            .is_some_and(|slot| match slot {
                NameSlot::Live {
                    session_id: current,
                    ownership,
                } => *current == session_id && ownership.ptr_eq(&entry.ownership),
                NameSlot::Starting {
                    creation,
                    session_id: Some(current),
                } => *current == session_id && creation.ownership.ptr_eq(&entry.ownership),
                NameSlot::Starting {
                    session_id: None, ..
                } => false,
            });
        let mut reservations = cleanup_lock(&self.reservations);
        let owns_reservation = reservations
            .reservations
            .get(&session_id)
            .is_some_and(|ownership| ownership.ptr_eq(&entry.ownership));
        if live_matches {
            state.by_id.remove(&session_id);
        } else if provisional_matches {
            state.provisional.remove(&session_id);
        } else if let Some(position) = cleanup_position {
            state.cleanup_only.remove(position);
        }
        if owns_name {
            state.by_name.remove(&entry.name);
        }
        if owns_reservation {
            reservations.reservations.remove(&session_id);
        }
    }

    fn begin_shutdown(&self, approved: bool) -> Result<Vec<Arc<CreationCell>>, Vec<String>> {
        let mut state = cleanup_lock(&self.state);
        // Creation admission and the idle decision share this lock. Include
        // reservations and cleanup owners, not only published live Sessions.
        if !approved {
            let mut names = state
                .by_name
                .keys()
                .map(ToString::to_string)
                .collect::<BTreeSet<_>>();
            names.extend(
                state
                    .by_id
                    .values()
                    .chain(state.provisional.values())
                    .chain(state.cleanup_only.iter())
                    .map(|entry| entry.name.to_string()),
            );
            if !names.is_empty() || !cleanup_lock(&self.reservations).reservations.is_empty() {
                return Err(names.into_iter().collect());
            }
        }
        state.accepting = false;
        let creations = state
            .by_name
            .values()
            .filter_map(|slot| match slot {
                NameSlot::Starting { creation, .. } => Some(Arc::clone(creation)),
                NameSlot::Live { .. } => None,
            })
            .collect::<Vec<_>>();
        Ok(creations)
    }

    fn owned_entries(&self) -> Result<Vec<SessionEntry>, DaemonError> {
        let state = cleanup_lock(&self.state);
        let mut entries = state.by_id.values().cloned().collect::<Vec<_>>();
        entries.extend(state.provisional.values().cloned());
        entries.extend(state.cleanup_only.iter().cloned());
        Ok(entries)
    }

    fn resume_after_failed_shutdown(&self) -> Result<(), DaemonError> {
        cleanup_lock(&self.state).accepting = true;
        Ok(())
    }
}

pub(crate) enum ShutdownOutcome {
    Stopped(Vec<SessionSummary>),
    NeedsConfirmation(Vec<String>),
}

struct RegistryState {
    by_id: BTreeMap<SessionId, SessionEntry>,
    provisional: BTreeMap<SessionId, SessionEntry>,
    cleanup_only: Vec<SessionEntry>,
    by_name: BTreeMap<SessionName, NameSlot>,
    accepting: bool,
}

impl Default for RegistryState {
    fn default() -> Self {
        Self {
            by_id: BTreeMap::new(),
            provisional: BTreeMap::new(),
            cleanup_only: Vec::new(),
            by_name: BTreeMap::new(),
            accepting: true,
        }
    }
}

enum NameSlot {
    Starting {
        creation: Arc<CreationCell>,
        session_id: Option<SessionId>,
    },
    Live {
        session_id: SessionId,
        ownership: OwnershipToken,
    },
}

enum NameReservation {
    Owner(Arc<CreationCell>),
    Waiting(Arc<CreationCell>),
    Existing(SessionEntry),
}

struct CreationCell {
    result: Mutex<Option<Result<SessionEntry, DaemonError>>>,
    changed: Condvar,
    cancelled: AtomicBool,
    ownership: OwnershipToken,
}

impl Default for CreationCell {
    fn default() -> Self {
        Self {
            result: Mutex::new(None),
            changed: Condvar::new(),
            cancelled: AtomicBool::new(false),
            ownership: OwnershipToken::new(),
        }
    }
}

impl CreationCell {
    fn complete(&self, result: Result<SessionEntry, DaemonError>) {
        let mut current = cleanup_lock(&self.result);
        if current.is_none() {
            *current = Some(result);
            self.changed.notify_all();
        }
    }

    fn wait_until(
        &self,
        deadline: Instant,
    ) -> Result<Result<SessionEntry, DaemonError>, DaemonError> {
        let mut result = lock(&self.result, "session creation")?;
        loop {
            if let Some(result) = result.clone() {
                return Ok(result);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(deadline_error(
                    "session creation did not finish before its deadline",
                ));
            }
            let (next, timeout) = self
                .changed
                .wait_timeout(result, deadline.saturating_duration_since(now))
                .map_err(|_| synchronization_error("session creation"))?;
            result = next;
            if timeout.timed_out() && result.is_none() {
                return Err(deadline_error(
                    "session creation did not finish before its deadline",
                ));
            }
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.complete(Err(DaemonError::new(
            DomainErrorKind::Cancelled,
            "session creation was cancelled by daemon shutdown",
        )));
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

struct CreationOwner {
    registry: Arc<RegistryInner>,
    name: SessionName,
    creation: Arc<CreationCell>,
    session_id: Option<SessionId>,
    actor: Option<SessionEntry>,
    cleanup_deadline: Instant,
    finished: bool,
}

impl CreationOwner {
    fn new(
        registry: Arc<RegistryInner>,
        name: SessionName,
        creation: Arc<CreationCell>,
        cleanup_deadline: Instant,
    ) -> Self {
        Self {
            registry,
            name,
            creation,
            session_id: None,
            actor: None,
            cleanup_deadline,
            finished: false,
        }
    }

    fn set_session_id(&mut self, session_id: SessionId) {
        self.session_id = Some(session_id);
    }

    fn set_actor(&mut self, actor: Arc<SessionActor>, working_directory: PathBuf) {
        self.actor = Some(SessionEntry {
            actor,
            name: self.name.clone(),
            working_directory,
            ownership: self.creation.ownership.clone(),
        });
    }

    fn retain_provisional_for_cleanup(&self) {
        let (Some(session_id), Some(entry)) = (self.session_id, self.actor.as_ref()) else {
            return;
        };
        self.registry
            .retain_provisional_for_cleanup(session_id, entry.clone());
        entry.actor.mark_registry_owned();
    }

    fn finish(
        &mut self,
        mut result: Result<SessionEntry, DaemonError>,
    ) -> Result<SessionEntry, DaemonError> {
        if result.is_err()
            && let Err(cleanup) = self.cleanup_owned()
        {
            result = Err(cleanup);
        }
        self.creation.complete(result.clone());
        self.finished = true;
        result
    }

    fn cleanup_owned(&mut self) -> Result<(), DaemonError> {
        if let Some(entry) = &self.actor {
            // Registration failure must not turn this into an invisible child.
            // The cleanup registry holds the actor before this bounded wait;
            // eventual worker completion compare-removes it and the name slot.
            self.retain_provisional_for_cleanup();
            entry.actor.begin_end(SessionEndReason::DriverFailure);
            entry.actor.wait_finished_until(self.cleanup_deadline)?;
            entry.actor.join_finished()?;
        } else {
            self.registry
                .release_creation(&self.name, &self.creation, self.session_id.take());
        }
        Ok(())
    }
}

impl Drop for CreationOwner {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let _ = self.cleanup_owned();
        self.creation.complete(Err(outcome_unknown()));
    }
}

struct SpawnedPtyOwner(Option<PtySession>);

impl SpawnedPtyOwner {
    fn new(session: PtySession) -> Self {
        Self(Some(session))
    }

    fn take(&mut self) -> PtySession {
        self.0.take().expect("spawned PTY owner contains a session")
    }
}

impl Drop for SpawnedPtyOwner {
    fn drop(&mut self) {
        if let Some(mut session) = self.0.take() {
            let _ = session.close_explicitly();
        }
    }
}

#[derive(Default)]
struct ReservationState {
    reservations: BTreeMap<SessionId, OwnershipToken>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ReplayPrincipal {
    device_id: DeviceId,
    authorization_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ReplayKey {
    principal: ReplayPrincipal,
    lease_ordinal: u64,
}

impl ReplayKey {
    fn new(principal: AttachmentPrincipal, lease_ordinal: u64) -> Self {
        Self {
            principal: ReplayPrincipal::from_attachment(principal),
            lease_ordinal,
        }
    }
}

impl ReplayPrincipal {
    fn from_attachment(principal: AttachmentPrincipal) -> Self {
        match principal {
            AttachmentPrincipal::LocalSameUid { own_device_id, .. } => ReplayPrincipal {
                device_id: own_device_id,
                authorization_generation: 0,
            },
            AttachmentPrincipal::RemoteEndpoint {
                device_id,
                auth_generation,
            } => ReplayPrincipal {
                device_id,
                authorization_generation: auth_generation,
            },
        }
    }
}

struct ReplayRegistry {
    incarnation: DaemonIncarnation,
    epochs: BTreeMap<ReplayKey, ReplayEpochEntry>,
    retired_through: BTreeMap<ReplayPrincipal, u64>,
    issued_through: BTreeMap<ReplayPrincipal, u64>,
    access_clock: u64,
}

struct ReplayEpochEntry {
    epoch: Arc<ReplayEpoch>,
    last_access: u64,
}

impl ReplayRegistry {
    fn new() -> Self {
        Self {
            incarnation: DaemonIncarnation::from_array(random_16()),
            epochs: BTreeMap::new(),
            retired_through: BTreeMap::new(),
            issued_through: BTreeMap::new(),
            access_clock: 0,
        }
    }

    fn issue(
        &mut self,
        attachment_principal: AttachmentPrincipal,
    ) -> Result<OperationLease, DaemonError> {
        let principal = ReplayPrincipal::from_attachment(attachment_principal);
        if !self.issued_through.contains_key(&principal)
            && self.issued_through.len() >= MAX_REPLAY_PRINCIPALS
        {
            return Err(resource_error("too many operation replay principals"));
        }
        let ordinal = self
            .issued_through
            .get(&principal)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| resource_error("operation lease ordinal exhausted"))?;
        while self.epochs.len() >= MAX_ACTIVE_OPERATION_EPOCHS {
            self.retire_one()?;
        }
        let last_access = self.next_access();
        self.epochs.insert(
            ReplayKey {
                principal,
                lease_ordinal: ordinal,
            },
            ReplayEpochEntry {
                epoch: Arc::new(ReplayEpoch::default()),
                last_access,
            },
        );
        self.issued_through.insert(principal, ordinal);
        Ok(OperationLease {
            daemon_incarnation: self.incarnation,
            ordinal,
        })
    }

    fn register(
        &mut self,
        key: ReplayKey,
        operation_id: OperationId,
        fingerprint: OperationFingerprint,
    ) -> Result<ReplayRegistration, DaemonError> {
        // Incarnation is deliberately first: an old-daemon token must not
        // influence this daemon's ordinal floor or allocation state.
        if operation_id.lease.daemon_incarnation != self.incarnation {
            return Err(outcome_unknown());
        }
        if operation_id.sequence == 0 || operation_id.lease.ordinal != key.lease_ordinal {
            return Err(outcome_unknown());
        }
        if self
            .retired_through
            .get(&key.principal)
            .is_some_and(|floor| key.lease_ordinal <= *floor)
        {
            return Err(outcome_unknown());
        }
        if self
            .issued_through
            .get(&key.principal)
            .is_none_or(|issued| key.lease_ordinal > *issued)
        {
            return Err(outcome_unknown());
        }
        let access = self.next_access();
        if let Some(entry) = self.epochs.get_mut(&key) {
            entry.last_access = access;
            return entry.epoch.register(operation_id.sequence, fingerprint);
        }
        // Every executable epoch is daemon-issued and registry-resident. A
        // missing issued epoch was retired; it must never be recreated.
        Err(outcome_unknown())
    }

    fn next_access(&mut self) -> u64 {
        if self.access_clock == u64::MAX {
            let mut order = self
                .epochs
                .iter()
                .map(|(key, entry)| (*key, entry.last_access))
                .collect::<Vec<_>>();
            order.sort_by_key(|(_, access)| *access);
            for (index, (key, _)) in order.into_iter().enumerate() {
                if let Some(entry) = self.epochs.get_mut(&key) {
                    entry.last_access = u64::try_from(index).unwrap_or(u64::MAX - 1);
                }
            }
            self.access_clock = u64::try_from(self.epochs.len()).unwrap_or(u64::MAX - 1);
        }
        self.access_clock += 1;
        self.access_clock
    }

    fn retire_one(&mut self) -> Result<(), DaemonError> {
        let mut candidate = None;
        for (key, entry) in &self.epochs {
            let mut prefix_complete = true;
            for (active, active_entry) in &self.epochs {
                if active.principal == key.principal
                    && active.lease_ordinal <= key.lease_ordinal
                    && !active_entry.epoch.is_fully_complete()?
                {
                    prefix_complete = false;
                    break;
                }
            }
            if prefix_complete
                && candidate.is_none_or(|(_, last_access)| entry.last_access < last_access)
            {
                candidate = Some((*key, entry.last_access));
            }
        }
        let key = candidate.map(|(key, _)| key).ok_or_else(|| {
            resource_error("operation replay registry is full of in-flight epochs")
        })?;
        let retirement_floor = self
            .retired_through
            .get(&key.principal)
            .copied()
            .unwrap_or(0)
            .max(key.lease_ordinal);
        self.retired_through.insert(key.principal, retirement_floor);
        self.epochs.retain(|candidate, _| {
            candidate.principal != key.principal || candidate.lease_ordinal > retirement_floor
        });
        Ok(())
    }
}

#[derive(Default)]
struct ReplayEpoch {
    state: Mutex<ReplayEpochState>,
}

#[derive(Default)]
struct ReplayEpochState {
    low_water: u64,
    operations: BTreeMap<u64, Arc<OperationCell>>,
}

impl ReplayEpoch {
    fn register(
        &self,
        sequence: u64,
        fingerprint: OperationFingerprint,
    ) -> Result<ReplayRegistration, DaemonError> {
        let mut state = lock(&self.state, "operation epoch")?;
        if let Some(cell) = state.operations.get(&sequence) {
            if cell.fingerprint != fingerprint {
                return Err(outcome_unknown());
            }
            return Ok(ReplayRegistration::Join(Arc::clone(cell)));
        }
        if sequence == 0 || sequence < state.low_water {
            return Err(outcome_unknown());
        }
        while state.operations.len() >= OPERATION_RESULTS_PER_EPOCH {
            let Some((&oldest, cell)) = state.operations.first_key_value() else {
                break;
            };
            if !cell.is_complete()? {
                return Err(resource_error(
                    "operation replay window is full of in-flight operations",
                ));
            }
            state.operations.remove(&oldest);
            state.low_water = state.low_water.max(oldest.saturating_add(1));
        }
        let cell = Arc::new(OperationCell::new(fingerprint));
        state.operations.insert(sequence, Arc::clone(&cell));
        Ok(ReplayRegistration::Execute(cell))
    }

    fn is_fully_complete(&self) -> Result<bool, DaemonError> {
        let state = lock(&self.state, "operation epoch")?;
        for cell in state.operations.values() {
            if !cell.is_complete()? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

enum ReplayRegistration {
    Execute(Arc<OperationCell>),
    Join(Arc<OperationCell>),
}

struct OperationCell {
    fingerprint: OperationFingerprint,
    result: Mutex<Option<MutationResult>>,
    changed: Condvar,
}

impl OperationCell {
    fn new(fingerprint: OperationFingerprint) -> Self {
        Self {
            fingerprint,
            result: Mutex::new(None),
            changed: Condvar::new(),
        }
    }

    fn complete(&self, result: MutationResult) {
        let mut current = cleanup_lock(&self.result);
        if current.is_none() {
            *current = Some(result);
            self.changed.notify_all();
        }
    }

    fn wait(&self) -> MutationResult {
        let mut result = lock(&self.result, "operation result")?;
        loop {
            if let Some(result) = result.clone() {
                return result;
            }
            result = self
                .changed
                .wait(result)
                .map_err(|_| synchronization_error("operation result"))?;
        }
    }

    fn is_complete(&self) -> Result<bool, DaemonError> {
        Ok(lock(&self.result, "operation result")?.is_some())
    }
}

struct OperationCompletionGuard {
    cell: Arc<OperationCell>,
    completed: bool,
}

impl OperationCompletionGuard {
    fn new(cell: Arc<OperationCell>) -> Self {
        Self {
            cell,
            completed: false,
        }
    }

    fn complete(&mut self, result: MutationResult) {
        self.cell.complete(result);
        self.completed = true;
    }
}

impl Drop for OperationCompletionGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.cell.complete(Err(outcome_unknown()));
        }
    }
}

struct SessionActor {
    id: SessionId,
    commands: Mutex<Option<crossbeam_channel::Sender<SessionCommand>>>,
    cached: Mutex<ActorRuntimeSummary>,
    revision: watch::Receiver<Revision>,
    registry: Weak<RegistryInner>,
    limits: ResourceLimits,
    interrupt: TerminalDriverInterrupt,
    driver_ownership: TerminalDriverOwnership,
    end: Mutex<EndState>,
    end_changed: Condvar,
    registry_owned: AtomicBool,
    worker_done: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
    #[cfg(test)]
    panic_close: AtomicBool,
}

#[derive(Clone, Copy)]
struct ActorRuntimeSummary {
    has_controller: bool,
    viewport: TerminalSize,
    ended: bool,
}

#[derive(Default)]
struct EndState {
    requested: Option<SessionEndReason>,
    interrupt: InterruptState,
    result: Option<Result<(), DaemonError>>,
}

#[derive(Default)]
enum InterruptState {
    #[default]
    Idle,
    Pending,
    Ready(Result<(), DaemonError>),
}

enum CommandStart {
    Started,
    Expired,
    Ending,
}

struct SessionRuntime {
    driver: Option<TerminalDriver>,
    attachments: BTreeMap<AttachmentId, ActorAttachment>,
    resume: Option<RemoteResumeCheckpoint>,
    controller: Option<ControllerLease>,
    controller_operation: Option<ReplayOperationKey>,
    next_generation: u64,
    viewport: TerminalSize,
    last_revision: Revision,
}

struct ActorAttachment {
    base_colors: TerminalColorProfile,
    color_sequence: u64,
    pending_color_snapshot: Option<TerminalSurfaceSnapshot>,
    principal: AttachmentPrincipal,
    #[cfg(any(unix, windows))]
    resume_view_id: Option<ResumeViewId>,
    terminal: TerminalAttachment,
    ever_active: bool,
    prepared_snapshot_applied: bool,
    applied_snapshot_revision: Option<Revision>,
    sync: AttachmentSync,
    lifecycle: watch::Sender<AttachmentLifecycle>,
    detached: Arc<AtomicBool>,
    final_update: FinalAttachmentUpdateSlot,
}

struct RemoteResumeCheckpoint {
    principal: AttachmentPrincipal,
    session_id: SessionId,
    view_id: ResumeViewId,
    revision: Revision,
    terminal: TerminalAttachment,
}

#[derive(Clone, Copy)]
enum AttachmentSync {
    Awaiting {
        revision: Revision,
        target: SyncTarget,
    },
    Active {
        generation: u64,
    },
    PreparedTakeover,
}

#[derive(Clone, Copy)]
enum SyncTarget {
    Active { generation: u64 },
    PreparedTakeover,
}

#[derive(Clone)]
struct CommandMeta {
    deadline: Instant,
    gate: Arc<CommandGate>,
}

struct CommandGate {
    state: AtomicU8,
}

impl Default for CommandGate {
    fn default() -> Self {
        Self {
            state: AtomicU8::new(COMMAND_QUEUED),
        }
    }
}

impl CommandMeta {
    fn try_start(&self) -> bool {
        if Instant::now() >= self.deadline {
            let _ = self.gate.state.compare_exchange(
                COMMAND_QUEUED,
                COMMAND_EXPIRED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return false;
        }
        self.gate
            .state
            .compare_exchange(
                COMMAND_QUEUED,
                COMMAND_STARTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

/// One enqueued actor command whose result has not yet been awaited.
struct CommandWaiter<R> {
    response: Receiver<Result<R, DaemonError>>,
    gate: Arc<CommandGate>,
}

impl<R> CommandWaiter<R> {
    fn wait(self, deadline: Instant) -> Result<R, DaemonError> {
        wait_for_command_response(self.response, self.gate, deadline)
    }
}

enum SessionCommand {
    UpdateColors {
        meta: CommandMeta,
        attachment_id: AttachmentId,
        sequence: u64,
        base_colors: TerminalColorProfile,
        reply: SyncSender<Result<(), DaemonError>>,
    },
    PrepareAttach {
        base_colors: TerminalColorProfile,
        meta: CommandMeta,
        principal: AttachmentPrincipal,
        takeover: bool,
        resume: Option<RemoteResumeRequest>,
        reply: SyncSender<Result<PreparedAttachment, DaemonError>>,
    },
    #[cfg(any(unix, windows))]
    DetachForRemoteResume {
        meta: CommandMeta,
        attachment_id: AttachmentId,
        reply: SyncSender<Result<bool, DaemonError>>,
    },
    SnapshotApplied {
        meta: CommandMeta,
        attachment_id: AttachmentId,
        revision: Revision,
        reply: SyncSender<Result<Option<TerminalSurfaceSnapshot>, DaemonError>>,
    },
    NextUpdate {
        meta: CommandMeta,
        attachment_id: AttachmentId,
        reply: SyncSender<Result<Option<AttachmentUpdate>, DaemonError>>,
    },
    #[cfg(any(unix, windows))]
    FinalUpdate {
        meta: CommandMeta,
        attachment_id: AttachmentId,
        reply: SyncSender<Result<Option<AttachmentUpdate>, DaemonError>>,
    },
    SyncLatest {
        meta: CommandMeta,
        attachment_id: AttachmentId,
        known_revision: Revision,
        reply: SyncSender<Result<TerminalSurfaceSnapshot, DaemonError>>,
    },
    #[cfg(any(unix, windows))]
    HistoryWindow {
        meta: CommandMeta,
        attachment_id: AttachmentId,
        query: TerminalHistoryWindowQuery,
        reply: SyncSender<Result<TerminalHistoryRead, DaemonError>>,
    },
    #[cfg(any(unix, windows))]
    ValidateHistoryController {
        meta: CommandMeta,
        attachment_id: AttachmentId,
        reply: SyncSender<Result<(), DaemonError>>,
    },
    #[cfg(any(unix, windows))]
    AdmitUpload {
        meta: CommandMeta,
        principal: AttachmentPrincipal,
        attachment_id: AttachmentId,
        reply: SyncSender<Result<UploadAdmission, DaemonError>>,
    },
    WriteInput {
        meta: CommandMeta,
        attachment_id: AttachmentId,
        bytes: Vec<u8>,
        reply: SyncSender<Result<(), DaemonError>>,
    },
    Resize {
        meta: CommandMeta,
        attachment_id: AttachmentId,
        size: TerminalSize,
        reply: SyncSender<Result<Revision, DaemonError>>,
    },
    Takeover {
        meta: CommandMeta,
        principal: AttachmentPrincipal,
        attachment_id: AttachmentId,
        operation_key: ReplayOperationKey,
        continuation: bool,
        reply: SyncSender<Result<(), DaemonError>>,
    },
    DetachRemotePrincipal {
        meta: CommandMeta,
        device_id: DeviceId,
        #[cfg(test)]
        processed: Option<SyncSender<()>>,
        reply: SyncSender<Result<PrincipalDetachOutcome, DaemonError>>,
    },
    #[cfg(any(unix, windows))]
    CountRemoteAttachments {
        meta: CommandMeta,
        device_id: DeviceId,
        reply: SyncSender<Result<usize, DaemonError>>,
    },
    #[cfg(all(test, unix))]
    BlockPtyEffect {
        meta: CommandMeta,
        entered: SyncSender<()>,
        release: Receiver<()>,
        executions: Arc<AtomicUsize>,
        reply: SyncSender<Result<(), DaemonError>>,
    },
    #[cfg(all(test, unix))]
    PanicWorker,
    Wake,
}

impl SessionActor {
    fn start(
        id: SessionId,
        viewport: TerminalSize,
        driver: TerminalDriver,
        registry: Weak<RegistryInner>,
        limits: ResourceLimits,
    ) -> Result<Arc<Self>, DaemonError> {
        let revision = driver.revision_watch();
        let interrupt = driver.interrupt_handle();
        let driver_ownership = driver.ownership_handle();
        let (commands, receiver) = crossbeam_channel::bounded(SESSION_COMMAND_CAPACITY);
        let actor = Arc::new(Self {
            id,
            commands: Mutex::new(Some(commands)),
            cached: Mutex::new(ActorRuntimeSummary {
                has_controller: false,
                viewport,
                ended: false,
            }),
            revision,
            registry,
            limits,
            interrupt,
            driver_ownership,
            end: Mutex::new(EndState::default()),
            end_changed: Condvar::new(),
            registry_owned: AtomicBool::new(false),
            worker_done: AtomicBool::new(false),
            worker: Mutex::new(None),
            #[cfg(test)]
            panic_close: AtomicBool::new(false),
        });
        let startup = Arc::new(Mutex::new(Some(driver)));
        let thread_startup = Arc::clone(&startup);
        let thread_actor = Arc::clone(&actor);
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let spawned = thread::Builder::new()
            .name("voytty-session-actor".into())
            .spawn(move || {
                let _dispatch = tracing::dispatcher::set_default(&dispatch);
                let mut finalizer = ActorWorkerFinalizer::new(Arc::clone(&thread_actor));
                let driver = thread_startup
                    .lock()
                    .ok()
                    .and_then(|mut driver| driver.take());
                let Some(driver) = driver else {
                    drop(receiver);
                    finalizer.complete(Err(synchronization_error("session actor startup")));
                    return;
                };
                let mut runtime = SessionRuntime {
                    driver: Some(driver),
                    attachments: BTreeMap::new(),
                    resume: None,
                    controller: None,
                    controller_operation: None,
                    next_generation: 0,
                    viewport,
                    last_revision: Revision::ZERO,
                };
                let result = catch_unwind(AssertUnwindSafe(|| {
                    run_session_actor(&thread_actor, &mut runtime, &receiver)
                }))
                .unwrap_or_else(|_| Err(outcome_unknown()));
                drop(receiver);
                // If execution unwound before the ordinary explicit finalizer,
                // dropping the retained runtime hands its driver to the
                // nonblocking reaper. The actor finalizer below waits on the
                // driver's ownership signal before removing registry tokens.
                drop(runtime);
                finalizer.complete(result);
            });
        match spawned {
            Ok(worker) => {
                *actor
                    .worker
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
                Ok(actor)
            }
            Err(error) => {
                if let Some(driver) = lock(&startup, "session actor startup")?.take() {
                    let _ = driver.finalize_explicit();
                }
                Err(resource_error(format!(
                    "unable to start session actor thread: {error}"
                )))
            }
        }
    }

    fn command_sender(&self) -> Result<crossbeam_channel::Sender<SessionCommand>, DaemonError> {
        cleanup_lock(&self.commands)
            .clone()
            .ok_or_else(session_not_found)
    }

    fn wake(&self) {
        if let Ok(commands) = self.command_sender() {
            let _ = commands.try_send(SessionCommand::Wake);
        }
    }

    fn request<R>(
        &self,
        deadline: Instant,
        build: impl FnOnce(CommandMeta, SyncSender<Result<R, DaemonError>>) -> SessionCommand,
    ) -> Result<R, DaemonError> {
        self.enqueue_command(deadline, build)?.wait(deadline)
    }

    /// Enqueues one command without waiting for its result. The returned
    /// [`CommandWaiter`] must be awaited to observe the exact outcome. This
    /// split lets a multi-actor operation admit every command before waiting
    /// for any of them, so one blocked actor cannot delay another's effect.
    fn enqueue_command<R>(
        &self,
        deadline: Instant,
        build: impl FnOnce(CommandMeta, SyncSender<Result<R, DaemonError>>) -> SessionCommand,
    ) -> Result<CommandWaiter<R>, DaemonError> {
        let gate = Arc::new(CommandGate::default());
        let meta = CommandMeta {
            deadline,
            gate: Arc::clone(&gate),
        };
        let (reply, response) = mpsc::sync_channel(1);
        match self
            .command_sender()?
            .send_deadline(build(meta, reply), deadline)
        {
            Ok(()) => {}
            Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
                return Err(session_not_found());
            }
            Err(crossbeam_channel::SendTimeoutError::Timeout(_)) => {
                return Err(deadline_error(
                    "session command queue admission deadline elapsed",
                ));
            }
        }
        Ok(CommandWaiter {
            response,
            gate: Arc::clone(&gate),
        })
    }

    fn runtime_summary(&self) -> Result<ActorRuntimeSummary, DaemonError> {
        let summary = *lock(&self.cached, "session actor summary")?;
        if summary.ended {
            Err(session_not_found())
        } else {
            Ok(summary)
        }
    }

    #[cfg(all(test, unix))]
    fn block_pty_effect_for_test(
        &self,
        deadline: Instant,
        entered: SyncSender<()>,
        release: Receiver<()>,
        executions: Arc<AtomicUsize>,
    ) -> Result<(), DaemonError> {
        self.request(deadline, |meta, reply| SessionCommand::BlockPtyEffect {
            meta,
            entered,
            release,
            executions,
            reply,
        })
    }

    #[cfg(all(test, unix))]
    fn panic_worker_for_test(&self, deadline: Instant) -> Result<(), DaemonError> {
        self.command_sender()?
            .try_send(SessionCommand::PanicWorker)
            .map_err(|_| resource_error("unable to inject actor worker panic"))?;
        while !self.worker_done() {
            if Instant::now() >= deadline {
                return Err(deadline_error("actor worker panic did not finalize"));
            }
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }

    #[cfg(test)]
    fn panic_next_close_for_test(&self) {
        self.panic_close.store(true, Ordering::Release);
    }

    fn latest_revision(&self) -> Revision {
        *self.revision.borrow()
    }

    fn update_cached(&self, runtime: &SessionRuntime, ended: bool) {
        if let Ok(mut cached) = self.cached.lock() {
            *cached = ActorRuntimeSummary {
                has_controller: runtime.controller.is_some(),
                viewport: runtime.viewport,
                ended,
            };
        }
    }

    fn mark_registry_owned(self: &Arc<Self>) {
        self.registry_owned.store(true, Ordering::Release);
        if self.worker_done()
            && let Some(registry) = self.registry.upgrade()
        {
            registry.complete(self.id, self);
        }
    }

    fn begin_end(self: &Arc<Self>, reason: SessionEndReason) {
        let spawn_interrupt = {
            let Ok(mut end) = self.end.lock() else {
                return;
            };
            if end.result.is_some() {
                return;
            }
            if end.requested.is_none() {
                end.requested = Some(reason);
            }
            match end.interrupt {
                InterruptState::Idle | InterruptState::Ready(Err(_)) => {
                    end.interrupt = InterruptState::Pending;
                    true
                }
                InterruptState::Pending | InterruptState::Ready(Ok(())) => false,
            }
        };
        if spawn_interrupt {
            let actor = Arc::clone(self);
            let interrupt = self.interrupt.clone();
            let spawned = thread::Builder::new()
                .name("voytty-session-close".into())
                .spawn(move || {
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        #[cfg(test)]
                        if actor.panic_close.swap(false, Ordering::AcqRel) {
                            panic!("injected session close panic");
                        }
                        interrupt
                            .close_explicitly()
                            .map(|_| ())
                            .map_err(map_driver_error)
                    }))
                    .unwrap_or_else(|_| Err(outcome_unknown()));
                    actor.finish_interrupt(result);
                });
            if let Err(error) = spawned {
                self.finish_interrupt(Err(resource_error(format!(
                    "unable to start session close thread: {error}"
                ))));
            }
        }
        self.wake();
    }

    fn finish_interrupt(&self, result: Result<(), DaemonError>) {
        if let Ok(mut end) = self.end.lock() {
            end.interrupt = InterruptState::Ready(result);
        }
        self.end_changed.notify_all();
        self.wake();
    }

    fn ready_end_reason(&self) -> Option<SessionEndReason> {
        let end = self.end.lock().ok()?;
        match (&end.requested, &end.interrupt) {
            (Some(reason), InterruptState::Ready(Ok(()))) => Some(reason.clone()),
            _ => None,
        }
    }

    fn requested_end_reason(&self) -> Option<SessionEndReason> {
        self.end.lock().ok()?.requested.clone()
    }

    fn try_start_command(&self, meta: &CommandMeta) -> CommandStart {
        let Ok(end) = self.end.lock() else {
            return CommandStart::Ending;
        };
        if end.requested.is_some() || end.result.is_some() {
            return CommandStart::Ending;
        }
        if meta.try_start() {
            CommandStart::Started
        } else {
            CommandStart::Expired
        }
    }

    fn finish_end(&self, result: Result<(), DaemonError>) {
        if let Ok(mut end) = self.end.lock() {
            end.result = Some(result);
        }
        self.end_changed.notify_all();
    }

    fn wait_finished(&self) -> Result<(), DaemonError> {
        self.wait_finished_until(Instant::now() + DEFAULT_SHUTDOWN_TIMEOUT)
    }

    fn wait_finished_until(&self, deadline: Instant) -> Result<(), DaemonError> {
        let mut end = lock(&self.end, "session completion")?;
        loop {
            if let Some(result) = end.result.clone() {
                return result;
            }
            if let InterruptState::Ready(Err(error)) = &end.interrupt {
                return Err(error.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(deadline_error(
                    "session cleanup did not finish before deadline",
                ));
            }
            let (next, timeout) = self
                .end_changed
                .wait_timeout(end, deadline.saturating_duration_since(now))
                .map_err(|_| synchronization_error("session completion"))?;
            end = next;
            if timeout.timed_out() && end.result.is_none() {
                return Err(deadline_error(
                    "session cleanup did not finish before deadline",
                ));
            }
        }
    }

    fn worker_done(&self) -> bool {
        self.worker_done.load(Ordering::Acquire)
    }

    fn join_finished(&self) -> Result<(), DaemonError> {
        if !self.worker_done() {
            return Err(deadline_error("session actor has not finished"));
        }
        let worker = lock(&self.worker, "session actor thread")?.take();
        if let Some(worker) = worker {
            worker
                .join()
                .map_err(|_| synchronization_error("session actor thread"))?;
        }
        Ok(())
    }

    fn join_finished_cleanup(&self) -> Result<(), DaemonError> {
        if !self.worker_done() {
            return Err(deadline_error("session actor has not finished"));
        }
        let worker = cleanup_lock(&self.worker).take();
        if let Some(worker) = worker {
            worker
                .join()
                .map_err(|_| synchronization_error("session actor thread"))?;
        }
        Ok(())
    }
}

impl Drop for SessionActor {
    fn drop(&mut self) {
        self.interrupt.interrupt();
        let worker = match self.worker.get_mut() {
            Ok(worker) => worker.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(worker) = worker {
            // Taking the handle is exclusive with `join_finished`; handing it
            // off is safe even when Drop runs on the actor worker itself.
            spawn_background_reaper("voytty-session-reaper", move || {
                let _ = worker.join();
            });
        }
    }
}

struct ActorWorkerFinalizer {
    actor: Arc<SessionActor>,
    completed: bool,
}

impl ActorWorkerFinalizer {
    fn new(actor: Arc<SessionActor>) -> Self {
        Self {
            actor,
            completed: false,
        }
    }

    fn complete(&mut self, result: Result<(), DaemonError>) {
        // A bounded crossbeam queue retains unprocessed commands (and their
        // reply senders) until both ends are gone. Live attachments can keep
        // this actor alive long after the worker has stopped receiving.
        cleanup_lock(&self.actor.commands).take();
        self.actor.driver_ownership.wait_released();
        self.actor.worker_done.store(true, Ordering::Release);
        self.actor.finish_end(result);
        if self.actor.registry_owned.load(Ordering::Acquire)
            && let Some(registry) = self.actor.registry.upgrade()
        {
            registry.complete(self.actor.id, &self.actor);
        }
        self.completed = true;
    }
}

impl Drop for ActorWorkerFinalizer {
    fn drop(&mut self) {
        if !self.completed {
            self.complete(Err(outcome_unknown()));
        }
    }
}

fn wait_for_command_response<R>(
    response: Receiver<Result<R, DaemonError>>,
    gate: Arc<CommandGate>,
    deadline: Instant,
) -> Result<R, DaemonError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    match response.recv_timeout(remaining) {
        Ok(result) => result,
        Err(RecvTimeoutError::Disconnected) => Err(session_not_found()),
        Err(RecvTimeoutError::Timeout) => {
            if gate
                .state
                .compare_exchange(
                    COMMAND_QUEUED,
                    COMMAND_EXPIRED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return Err(deadline_error(
                    "session command expired before starting its side effect",
                ));
            }
            if gate.state.load(Ordering::Acquire) == COMMAND_STARTED {
                return response.recv().map_err(|_| session_not_found())?;
            }
            Err(deadline_error(
                "session command expired before starting its side effect",
            ))
        }
    }
}

fn run_session_actor(
    actor: &Arc<SessionActor>,
    runtime: &mut SessionRuntime,
    commands: &crossbeam_channel::Receiver<SessionCommand>,
) -> Result<(), DaemonError> {
    loop {
        reap_detached(actor, runtime)?;
        if let Some(reason) = actor.ready_end_reason() {
            return finish_runtime(actor, runtime, reason);
        }
        match poll_driver(runtime) {
            Ok(Some(reason)) => {
                let reason = actor.requested_end_reason().unwrap_or(reason);
                return finish_runtime(actor, runtime, reason);
            }
            Ok(None) => {}
            Err(_) => {
                if let Some(driver) = runtime.driver.as_ref()
                    && driver.close_explicitly().is_ok()
                {
                    let reason = actor
                        .requested_end_reason()
                        .unwrap_or(SessionEndReason::DriverFailure);
                    return finish_runtime(actor, runtime, reason);
                }
            }
        }

        match commands.recv_timeout(SESSION_MONITOR_INTERVAL) {
            Ok(SessionCommand::Wake) => {}
            Ok(command) => dispatch_command(actor, runtime, command),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                if let Some(driver) = runtime.driver.as_ref() {
                    let _ = driver.close_explicitly();
                }
                return finish_runtime(actor, runtime, SessionEndReason::DaemonStop);
            }
        }
    }
}

fn poll_driver(runtime: &SessionRuntime) -> Result<Option<SessionEndReason>, DaemonError> {
    let driver = runtime.driver.as_ref().ok_or_else(session_not_found)?;
    driver.check_health().map_err(map_driver_error)?;
    match driver.try_wait().map_err(map_driver_error)? {
        PtyChildState::Running => Ok(None),
        PtyChildState::Exited(status) => Ok(Some(SessionEndReason::NaturalExit {
            exit_code: status.exit_code(),
            signal: status.signal().map(ToOwned::to_owned),
        })),
    }
}

fn finish_runtime(
    actor: &Arc<SessionActor>,
    runtime: &mut SessionRuntime,
    reason: SessionEndReason,
) -> Result<(), DaemonError> {
    if let Some(driver) = runtime.driver.as_ref() {
        runtime.last_revision = driver.latest_revision();
    }
    runtime.resume = None;
    runtime.controller = None;
    runtime.controller_operation = None;
    if let Some(driver) = runtime.driver.as_ref() {
        driver.set_effect_target(None).map_err(map_driver_error)?;
    }
    let finalization = runtime
        .driver
        .take()
        .ok_or_else(session_not_found)
        .and_then(|driver| {
            driver
                .finalize_natural()
                .map(|_| ())
                .map_err(map_driver_error)
        });
    for attachment in runtime.attachments.values_mut() {
        let update = final_terminal_update(&mut attachment.terminal);
        if let Ok(mut final_update) = attachment.final_update.lock() {
            *final_update = Some(update);
        }
        attachment
            .lifecycle
            .send_replace(AttachmentLifecycle::SessionEnded(reason.clone()));
    }
    actor.update_cached(runtime, true);
    let (reason_code, exit_code, signal) = match &reason {
        SessionEndReason::NaturalExit { exit_code, signal } => {
            ("natural_exit", Some(*exit_code), signal.as_deref())
        }
        SessionEndReason::ExplicitClose => ("explicit_close", None, None),
        SessionEndReason::DaemonStop => ("daemon_stop", None, None),
        SessionEndReason::DriverFailure => ("driver_failure", None, None),
    };
    if let Err(error) = &finalization {
        tracing::warn!(component = "session", operation = "cleanup_failed", session_id = %actor.id, reason = error.kind().code(), "Session cleanup failed");
    }
    if reason == SessionEndReason::DriverFailure {
        tracing::warn!(component = "session", operation = "ended", session_id = %actor.id, reason = reason_code, "Session ended after driver failure");
    } else {
        tracing::info!(component = "session", operation = "ended", session_id = %actor.id, reason = reason_code, exit_code, signal, "Session ended");
    }
    finalization
}

fn random_16() -> [u8; 16] {
    let secret = SecretKey::generate().to_bytes();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&secret[..16]);
    bytes
}

fn validate_viewport(limits: ResourceLimits, size: TerminalSize) -> Result<(), DaemonError> {
    if size.rows == 0
        || size.columns == 0
        || size.rows > limits.max_viewport_rows
        || size.columns > limits.max_viewport_columns
    {
        return Err(resource_error(format!(
            "viewport {}x{} exceeds the {}x{} bound",
            size.columns, size.rows, limits.max_viewport_columns, limits.max_viewport_rows
        )));
    }
    Ok(())
}

fn default_deadline() -> Instant {
    Instant::now() + DEFAULT_COMMAND_TIMEOUT
}

fn ensure_before_deadline(deadline: Instant) -> Result<(), DaemonError> {
    if Instant::now() >= deadline {
        Err(deadline_error("operation deadline elapsed before commit"))
    } else {
        Ok(())
    }
}

fn map_pty_error(error: PtyError) -> DaemonError {
    match error {
        PtyError::InvalidPath {
            kind: PtyPathKind::WorkingDirectory,
            ..
        } => DaemonError::new(
            DomainErrorKind::InvalidWorkingDirectory,
            "working directory is invalid or inaccessible",
        ),
        error @ PtyError::UnsupportedPlatform { .. } => {
            DaemonError::new(DomainErrorKind::UnsupportedPlatform, error.to_string())
        }
        error => DaemonError::new(DomainErrorKind::StoreUnavailable, error.to_string()),
    }
}

fn map_terminal_error(error: TerminalError) -> DaemonError {
    DaemonError::new(DomainErrorKind::ResourceExhausted, error.to_string())
}

pub(crate) fn map_driver_error(error: TerminalDriverError) -> DaemonError {
    match error {
        TerminalDriverError::Pty(error) => map_pty_error(error),
        TerminalDriverError::Terminal(error) => map_terminal_error(error),
        TerminalDriverError::Deadline(detail) => deadline_error(detail),
        TerminalDriverError::HistoryReadPending => {
            resource_error("terminal history read already pending")
        }
        TerminalDriverError::Cancelled(detail) => {
            DaemonError::new(DomainErrorKind::Cancelled, detail)
        }
        error => DaemonError::new(DomainErrorKind::StoreUnavailable, error.to_string()),
    }
}

fn lock<'a, T>(mutex: &'a Mutex<T>, name: &'static str) -> Result<MutexGuard<'a, T>, DaemonError> {
    mutex.lock().map_err(|_| synchronization_error(name))
}

/// Ownership finalizers cannot abandon a child or reservation merely because
/// an earlier owner panicked while holding a mutex. Cleanup recovers the value,
/// clears poison for later diagnostics/retries, and still compare-checks the
/// exact ownership token before removing anything.
fn cleanup_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            mutex.clear_poison();
            poisoned.into_inner()
        }
    }
}

fn synchronization_error(name: &'static str) -> DaemonError {
    DaemonError::new(
        DomainErrorKind::StoreUnavailable,
        format!("{name} lock is poisoned"),
    )
}

fn reserved_main() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::ReservedSessionName,
        "main is reserved for the default create-if-missing session",
    )
}

fn session_already_exists(name: &SessionName) -> DaemonError {
    DaemonError::new(
        DomainErrorKind::SessionAlreadyExists,
        format!("session {name:?} already exists"),
    )
}

fn invalid_session(detail: impl Into<String>) -> DaemonError {
    DaemonError::new(DomainErrorKind::SessionNotFound, detail)
}

fn session_not_found() -> DaemonError {
    DaemonError::new(DomainErrorKind::SessionNotFound, "session is not live")
}

fn principal_mismatch() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::LeaseLost,
        "takeover attachment belongs to a different principal",
    )
}

fn not_synchronized(detail: impl Into<String>) -> DaemonError {
    DaemonError::new(DomainErrorKind::NotSynchronized, detail)
}

fn lease_lost() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::LeaseLost,
        "attachment does not own the current controller lease",
    )
}

fn resource_error(detail: impl Into<String>) -> DaemonError {
    DaemonError::new(DomainErrorKind::ResourceExhausted, detail)
}

fn deadline_error(detail: impl Into<String>) -> DaemonError {
    DaemonError::new(DomainErrorKind::DeadlineExceeded, detail)
}

fn outcome_unknown() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::OperationOutcomeUnknown,
        "operation result belongs to a retired epoch or evicted sequence",
    )
}

#[cfg(test)]
mod tests;
