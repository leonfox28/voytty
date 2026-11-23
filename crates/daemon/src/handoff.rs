//! One-shot lifecycle owner, detached from the terminal it may terminate.
//!
//! Stop, restart, update, reset and uninstall can stop the running daemon, which ends its
//! Sessions and possibly the terminal that asked. The frontend keeps
//! confirmation and progress; after an explicit handoff this child owns the
//! stop, any release activation, and any replacement start.
//!
//! Only this module decodes the private parent/child control protocol. Candidate
//! preparation and activation remain in their existing distribution/runtime owners.

use std::fs;
use std::future::Future;
#[cfg(any(windows, test))]
use std::io::{Read, Write};
#[cfg(any(windows, test))]
use std::net::Shutdown;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::pin::Pin;
use std::process::Child;
use std::time::Duration;
#[cfg(any(windows, test))]
use std::time::Instant;
#[cfg(unix)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(windows)]
use voytty_platform::local::IsolatedStream as UnixStream;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use voytty_core::{DeviceId, DomainErrorKind};
use voytty_diagnostics::{
    Event as DiagnosticEvent, Kind, Level, Operation, Outcome, Recorder, Stage,
};
use voytty_platform::local::{spawn_isolated_command, take_isolated_channel};
use voytty_platform::user_state::UserPaths;

use crate::distribution::{PreparedRelease, ReleaseSelection};
use crate::error::DaemonError;
use crate::lifecycle::run_blocking;
use crate::operations::{IdentityResetResult, LifecycleStage, LocalRuntime, UpdateResult};
use crate::service::{DaemonReadiness, SessionImpact};

/// Hidden entry used only by the one-shot lifecycle launcher.
pub const INTERNAL_HANDOFF_ARGUMENT: &str = "--internal-handoff";
const MAX_CONTROL_BYTES: usize = 64 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const READY_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) type HandoffFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub(crate) trait HandoffInteraction: Send {
    fn progress(&mut self, stage: LifecycleStage) -> HandoffFuture<'_, ()>;
    fn confirm(&mut self, impact: &SessionImpact) -> HandoffFuture<'_, Result<(), DaemonError>>;
    /// Transfers ownership; only an update hands over a verified release.
    fn handoff(
        &mut self,
        release: Option<&PreparedRelease>,
    ) -> HandoffFuture<'_, Result<(), DaemonError>>;
    fn committed(&mut self) -> HandoffFuture<'_, ()>;
}

#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum Cleanup {
    Reset,
    Uninstall,
}

/// Lifecycle command the frontend asks the detached owner to run.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Command {
    Stop,
    Restart,
    Cleanup {
        operation: Cleanup,
        expected_identity: Option<[u8; DeviceId::LENGTH]>,
    },
    Update {
        version: Option<String>,
    },
}

impl Command {
    const fn noun(&self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Cleanup {
                operation: Cleanup::Reset,
                ..
            } => "reset",
            Self::Cleanup {
                operation: Cleanup::Uninstall,
                ..
            } => "uninstall",
            Self::Restart => "restart",
            Self::Update { .. } => "update",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Request {
    Begin { command: Command, approved: bool },
    Approve,
    Cancel,
    Continue,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Event {
    Ready,
    Progress(LifecycleStage),
    Confirm(SessionImpact),
    Handoff,
    Accepted,
    Complete(Result<Completed, Failure>),
}

/// Result of each command; its frontend accepts only the matching variant.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Completed {
    Stopped(Option<SessionImpact>),
    Restarted(DaemonReadiness),
    Updated(UpdateResult),
    Cleaned {
        removed: bool,
        previous_identity: Option<[u8; DeviceId::LENGTH]>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Failure {
    code: String,
    detail: String,
}

impl From<DaemonError> for Failure {
    fn from(error: DaemonError) -> Self {
        Self {
            code: error.kind().code().into(),
            detail: error.detail().into(),
        }
    }
}

impl Failure {
    fn into_error(self) -> DaemonError {
        DaemonError::new(
            DomainErrorKind::from_code(&self.code).unwrap_or(DomainErrorKind::MalformedFrame),
            self.detail,
        )
    }
}

fn channel_error() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::OperationOutcomeUnknown,
        "Lifecycle control connection closed or failed. If the operation was accepted, it may still be running; check voytty --version, voytty status and voytty logs before retrying.",
    )
}

fn cancelled() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::Cancelled,
        "Cancelled before interruption approval or execution handoff.",
    )
}

fn encode_control_frame(value: &impl Serialize) -> Result<Vec<u8>, DaemonError> {
    let body = serde_json::to_vec(value).map_err(|_| channel_error())?;
    if body.len() > MAX_CONTROL_BYTES {
        return Err(channel_error());
    }
    let mut bytes = Vec::with_capacity(4 + body.len());
    bytes.extend_from_slice(
        &u32::try_from(body.len())
            .map_err(|_| channel_error())?
            .to_be_bytes(),
    );
    bytes.extend_from_slice(&body);
    Ok(bytes)
}

#[cfg(any(windows, test))]
struct Channel {
    stream: UnixStream,
    usable: bool,
}

#[cfg(any(windows, test))]
impl Channel {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            usable: true,
        }
    }

    #[cfg(all(test, unix))]
    fn send(&mut self, value: &impl Serialize) -> Result<(), DaemonError> {
        let bytes = encode_control_frame(value)?;
        self.send_encoded(&bytes)
    }

    fn send_encoded(&mut self, bytes: &[u8]) -> Result<(), DaemonError> {
        let result = self.write_encoded(bytes);
        if result.is_err() {
            self.usable = false;
            // A partial frame must never be followed by another frame.
            let _ = self.stream.shutdown(Shutdown::Both);
        }
        result
    }

    fn write_encoded(&mut self, bytes: &[u8]) -> Result<(), DaemonError> {
        if !self.usable {
            return Err(channel_error());
        }
        let deadline = Instant::now() + WRITE_TIMEOUT;
        let mut remaining = bytes;
        while !remaining.is_empty() {
            let timeout = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(channel_error)?;
            self.stream
                .set_write_timeout(Some(timeout))
                .map_err(|_| channel_error())?;
            match self.stream.write(remaining) {
                Ok(0) => return Err(channel_error()),
                Ok(count) => remaining = &remaining[count..],
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(channel_error()),
            }
        }
        Ok(())
    }

    fn receive_bytes(&mut self) -> Result<Vec<u8>, DaemonError> {
        if !self.usable {
            return Err(channel_error());
        }
        let mut length = [0; 4];
        self.stream
            .read_exact(&mut length)
            .map_err(|_| channel_error())?;
        let length = u32::from_be_bytes(length) as usize;
        if length == 0 || length > MAX_CONTROL_BYTES {
            return Err(channel_error());
        }
        let mut bytes = vec![0; length];
        self.stream
            .read_exact(&mut bytes)
            .map_err(|_| channel_error())?;
        Ok(bytes)
    }

    #[cfg(all(test, unix))]
    fn receive<T: DeserializeOwned>(&mut self) -> Result<T, DaemonError> {
        let bytes = self.receive_bytes()?;
        serde_json::from_slice(&bytes).map_err(|_| channel_error())
    }
}

#[cfg(unix)]
struct AsyncChannel {
    stream: tokio::net::UnixStream,
    read_usable: bool,
    write_usable: bool,
}

#[cfg(unix)]
impl AsyncChannel {
    fn from_std(stream: UnixStream) -> Result<Self, DaemonError> {
        stream.set_nonblocking(true).map_err(|_| channel_error())?;
        let stream = tokio::net::UnixStream::from_std(stream).map_err(|_| channel_error())?;
        Ok(Self {
            stream,
            read_usable: true,
            write_usable: true,
        })
    }

    async fn send(&mut self, value: &impl Serialize) -> Result<(), DaemonError> {
        if !self.write_usable {
            return Err(channel_error());
        }
        let bytes = encode_control_frame(value)?;
        // Mark unusable across the await so a cancelled partial write cannot be reused.
        self.write_usable = false;
        let result = tokio::time::timeout(WRITE_TIMEOUT, self.stream.write_all(&bytes)).await;
        match result {
            Ok(Ok(())) => {
                self.write_usable = true;
                Ok(())
            }
            _ => {
                // A partial frame must never be followed by another frame.
                let _ = self.stream.shutdown().await;
                Err(channel_error())
            }
        }
    }

    async fn receive<T: DeserializeOwned>(&mut self) -> Result<T, DaemonError> {
        if !self.read_usable {
            return Err(channel_error());
        }
        // Mark unusable across the await so a cancelled partial read cannot be reused.
        self.read_usable = false;
        let mut length = [0; 4];
        self.stream
            .read_exact(&mut length)
            .await
            .map_err(|_| channel_error())?;
        let length = u32::from_be_bytes(length) as usize;
        if length == 0 || length > MAX_CONTROL_BYTES {
            return Err(channel_error());
        }
        let mut bytes = vec![0; length];
        self.stream
            .read_exact(&mut bytes)
            .await
            .map_err(|_| channel_error())?;
        let decoded = serde_json::from_slice(&bytes).map_err(|_| channel_error())?;
        self.read_usable = true;
        Ok(decoded)
    }

    async fn receive_with_timeout<T: DeserializeOwned>(
        &mut self,
        limit: Duration,
    ) -> Result<T, DaemonError> {
        tokio::time::timeout(limit, self.receive())
            .await
            .map_err(|_| channel_error())?
    }
}

#[cfg(windows)]
struct AsyncChannel {
    inner: Option<Channel>,
}

#[cfg(windows)]
impl AsyncChannel {
    fn from_std(stream: UnixStream) -> Result<Self, DaemonError> {
        Ok(Self {
            inner: Some(Channel::new(stream)),
        })
    }

    async fn send(&mut self, value: &impl Serialize) -> Result<(), DaemonError> {
        let bytes = encode_control_frame(value)?;
        let mut channel = self.inner.take().ok_or_else(channel_error)?;
        let (channel, result) = tokio::task::spawn_blocking(move || {
            let result = channel.send_encoded(&bytes);
            (channel, result)
        })
        .await
        .map_err(|_| channel_error())?;
        if result.is_ok() {
            self.inner = Some(channel);
        }
        result
    }

    async fn receive<T: DeserializeOwned>(&mut self) -> Result<T, DaemonError> {
        self.receive_inner(None).await
    }

    async fn receive_with_timeout<T: DeserializeOwned>(
        &mut self,
        limit: Duration,
    ) -> Result<T, DaemonError> {
        self.receive_inner(Some(limit)).await
    }

    async fn receive_inner<T: DeserializeOwned>(
        &mut self,
        limit: Option<Duration>,
    ) -> Result<T, DaemonError> {
        let mut channel = self.inner.take().ok_or_else(channel_error)?;
        let (channel, result) = tokio::task::spawn_blocking(move || {
            let result = channel
                .stream
                .set_read_timeout(limit)
                .map_err(|_| channel_error())
                .and_then(|()| channel.receive_bytes());
            (channel, result)
        })
        .await
        .map_err(|_| channel_error())?;
        let bytes = result?;
        self.inner = Some(channel);
        serde_json::from_slice(&bytes).map_err(|_| channel_error())
    }
}

// A child that never became ready cannot mutate and may be terminated.
// Once ready, let channel EOF/cancellation unwind its owned staging normally;
// after Continue, the child must also finish any accepted mutation independently.
struct OwnerChild {
    child: Option<Child>,
    continued: bool,
    ready: bool,
}

impl Drop for OwnerChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if !self.ready {
                let _ = child.kill();
            }
            let _ = std::thread::Builder::new()
                .name("voytty-handoff-reap".into())
                .spawn(move || {
                    let _ = child.wait();
                });
        }
    }
}

/// Stops through the detached owner; `None` means it was already stopped.
pub(crate) async fn stop(
    executable: &Path,
    paths: &UserPaths,
    approved: bool,
    confirm: impl FnMut(&SessionImpact) -> Result<(), DaemonError>,
    progress: impl FnMut(LifecycleStage),
) -> Result<Option<SessionImpact>, DaemonError> {
    match run_frontend(
        executable,
        paths,
        Command::Stop,
        approved,
        confirm,
        progress,
    )
    .await?
    {
        Completed::Stopped(impact) => Ok(impact),
        _ => Err(channel_error()),
    }
}

/// Restarts through the detached owner and returns the new daemon's readiness.
pub(crate) async fn restart(
    executable: &Path,
    paths: &UserPaths,
    approved: bool,
    confirm: impl FnMut(&SessionImpact) -> Result<(), DaemonError>,
    progress: impl FnMut(LifecycleStage),
) -> Result<DaemonReadiness, DaemonError> {
    match run_frontend(
        executable,
        paths,
        Command::Restart,
        approved,
        confirm,
        progress,
    )
    .await?
    {
        Completed::Restarted(readiness) => Ok(readiness),
        _ => Err(channel_error()),
    }
}

pub(crate) async fn cleanup(
    executable: &Path,
    paths: &UserPaths,
    operation: Cleanup,
    expected_identity: Option<DeviceId>,
    approved: bool,
) -> Result<IdentityResetResult, DaemonError> {
    let command = Command::Cleanup {
        operation,
        expected_identity: expected_identity.map(DeviceId::to_bytes),
    };
    match run_frontend(
        executable,
        paths,
        command,
        approved,
        crate::operations::confirmation_required,
        |_| {},
    )
    .await?
    {
        Completed::Cleaned {
            removed,
            previous_identity,
        } => Ok(IdentityResetResult {
            removed,
            previous_device_id: previous_identity.map(DeviceId::from_array),
        }),
        _ => Err(channel_error()),
    }
}

/// Prepares, activates, and starts one release through the detached owner.
pub(crate) async fn update(
    executable: &Path,
    paths: &UserPaths,
    version: Option<&str>,
    approved: bool,
    confirm: impl FnMut(&SessionImpact) -> Result<(), DaemonError>,
    progress: impl FnMut(LifecycleStage),
) -> Result<UpdateResult, DaemonError> {
    let command = Command::Update {
        version: version.map(str::to_owned),
    };
    match run_frontend(executable, paths, command, approved, confirm, progress).await? {
        Completed::Updated(result) => Ok(result),
        _ => Err(channel_error()),
    }
}

async fn run_frontend(
    executable: &Path,
    paths: &UserPaths,
    command: Command,
    approved: bool,
    mut confirm: impl FnMut(&SessionImpact) -> Result<(), DaemonError>,
    mut progress: impl FnMut(LifecycleStage),
) -> Result<Completed, DaemonError> {
    let noun = command.noun();
    let refused = if matches!(command, Command::Update { .. }) {
        DomainErrorKind::UpdateRejected
    } else {
        DomainErrorKind::Cancelled
    };
    let spawn_executable = executable.to_path_buf();
    let spawn_home = paths.home().to_path_buf();
    let (child, stream) = tokio::task::spawn_blocking(move || {
        spawn_isolated_command(&spawn_executable, &spawn_home, INTERNAL_HANDOFF_ARGUMENT)
    })
    .await
    .map_err(|_| {
        DaemonError::new(
            refused,
            format!(
                "Unable to start the independent {noun} process; nothing was stopped or installed."
            ),
        )
    })?
    .map_err(|_| {
        DaemonError::new(
            refused,
            format!(
                "Unable to start the independent {noun} process; nothing was stopped or installed."
            ),
        )
    })?;
    let mut child = OwnerChild {
        child: Some(child),
        continued: false,
        ready: false,
    };
    let mut channel = AsyncChannel::from_std(stream)?;
    let result = async {
        if !matches!(
            channel.receive_with_timeout(READY_TIMEOUT).await?,
            Event::Ready
        ) {
            return Err(channel_error());
        }
        child.ready = true;
        channel.send(&Request::Begin { command, approved }).await?;
        loop {
            match channel.receive().await? {
                Event::Progress(stage) => progress(stage),
                Event::Confirm(impact) => {
                    if let Err(error) = confirm(&impact) {
                        let _ = channel.send(&Request::Cancel).await;
                        return Err(error);
                    }
                    channel.send(&Request::Approve).await?;
                }
                Event::Handoff if !child.continued => {
                    progress(LifecycleStage::Continuing);
                    child.continued = true;
                    channel.send(&Request::Continue).await?;
                }
                Event::Accepted if child.continued => {}
                Event::Complete(result) => return result.map_err(Failure::into_error),
                _ => return Err(channel_error()),
            }
        }
    }
    .await;
    result.map_err(|error: DaemonError| {
        if !child.continued && error.kind() == DomainErrorKind::OperationOutcomeUnknown {
            DaemonError::new(
                DomainErrorKind::Cancelled,
                format!(
                    "The {noun} control connection failed before execution handoff; nothing was stopped or installed."
                ),
            )
        } else {
            error
        }
    })
}

/// Runs the hidden one-shot lifecycle owner. It never obtains terminal input directly.
pub fn run_internal_handoff() -> Result<(), DaemonError> {
    run_owner(&LocalRuntime::current()?)
}

/// Serves the hidden entry for a task-private runtime in multi-process tests.
#[doc(hidden)]
pub fn run_internal_handoff_for_test(runtime: &LocalRuntime) -> Result<(), DaemonError> {
    run_owner(runtime)
}

fn run_owner(runtime: &LocalRuntime) -> Result<(), DaemonError> {
    let paths = runtime.paths();
    let stream = take_isolated_channel(paths.uid()).map_err(|_| channel_error())?;
    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| channel_error())?;
    let uid = paths.uid();
    tokio.block_on(run_worker(runtime, stream, |version| async move {
        let executable = std::env::current_exe().map_err(|_| channel_error())?;
        crate::distribution::prepare_update(
            ReleaseSelection::parse(version.as_deref())?,
            executable,
            uid,
        )
        .await
    }))
}

async fn run_worker<F, Fut>(
    runtime: &LocalRuntime,
    stream: UnixStream,
    prepare: F,
) -> Result<(), DaemonError>
where
    F: FnOnce(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<PreparedRelease, DaemonError>>,
{
    let mut worker = Worker {
        channel: AsyncChannel::from_std(stream)?,
        paths: runtime.paths(),
        version: None,
        log_enabled: false,
        diagnostics: Recorder::default(),
        operation: Operation::default(),
        accepted: false,
        committed: false,
    };
    worker.channel.send(&Event::Ready).await?;
    let Request::Begin { command, approved } = worker.channel.receive().await? else {
        return Err(cancelled());
    };
    let result = match command {
        Command::Stop => runtime
            .apply_stop(approved, &mut worker)
            .await
            .map(Completed::Stopped),
        Command::Restart => runtime
            .apply_restart(approved, &mut worker)
            .await
            .map(Completed::Restarted),
        Command::Cleanup {
            operation,
            expected_identity,
        } => runtime
            .apply_cleanup(
                operation,
                expected_identity.map(DeviceId::from_array),
                approved,
                Some(&mut worker),
            )
            .await
            .map(|result| Completed::Cleaned {
                removed: result.removed,
                previous_identity: result.previous_device_id.map(DeviceId::to_bytes),
            }),
        Command::Update { version } => worker
            .update(runtime, version, approved, prepare)
            .await
            .map(Completed::Updated),
    };
    let _ = worker
        .channel
        .send(&Event::Complete(result.clone().map_err(Into::into)))
        .await;
    result.map(|_| ())
}

struct Worker<'a> {
    channel: AsyncChannel,
    paths: &'a UserPaths,
    version: Option<String>,
    log_enabled: bool,
    diagnostics: Recorder,
    operation: Operation,
    accepted: bool,
    committed: bool,
}

impl Worker<'_> {
    fn log(
        &self,
        record: DiagnosticEvent,
        durable: bool,
    ) -> impl Future<Output = Result<(), DaemonError>> + Send + 'static {
        let flush = if !self.log_enabled {
            Ok(None)
        } else {
            let mut record = record
                .operation(&self.operation)
                .committed(self.committed)
                .accepted(self.accepted);
            if let Some(version) = &self.version {
                record = record.release_version(version);
            }
            let accepted = self.diagnostics.record(record);
            if durable {
                if accepted {
                    Ok(Some(self.diagnostics.clone()))
                } else {
                    Err(DaemonError::new(
                        DomainErrorKind::PathUnsafe,
                        "Unable to record the update outcome.",
                    ))
                }
            } else {
                Ok(None)
            }
        };
        async move {
            if let Some(diagnostics) = flush? {
                let flushed =
                    tokio::task::spawn_blocking(move || diagnostics.flush(Duration::from_secs(2)))
                        .await
                        .unwrap_or(false);
                if !flushed {
                    return Err(DaemonError::new(
                        DomainErrorKind::PathUnsafe,
                        "Unable to record the update outcome.",
                    ));
                }
            }
            Ok(())
        }
    }

    /// Only updates record their stages and outcome in the daemon log.
    async fn update<F, Fut>(
        &mut self,
        runtime: &LocalRuntime,
        version: Option<String>,
        approved: bool,
        prepare: F,
    ) -> Result<UpdateResult, DaemonError>
    where
        F: FnOnce(Option<String>) -> Fut,
        Fut: std::future::Future<Output = Result<PreparedRelease, DaemonError>>,
    {
        let log_paths = self.paths.clone();
        let (log_enabled, diagnostics) = run_blocking(move || {
            let enabled = crate::diagnostics::configured(&log_paths);
            let recorder = crate::diagnostics::recorder(&log_paths);
            Ok((enabled, recorder))
        })
        .await?;
        self.log_enabled = log_enabled;
        self.diagnostics = diagnostics;
        let result = async {
            self.progress(LifecycleStage::Preparing).await;
            let prepared = prepare(version).await?;
            self.progress(LifecycleStage::Verified {
                version: prepared.version().to_owned(),
            })
            .await;
            runtime
                .apply_prepared_update(prepared, approved, &mut *self)
                .await
        }
        .await;
        let record = match &result {
            Ok(result) => DiagnosticEvent::new(Kind::UpdateCompleted)
                .outcome(Outcome::Success)
                .release_version(&result.installed_version)
                .daemon_started(result.daemon_started),
            Err(error) => DiagnosticEvent::new(Kind::UpdateCompleted)
                .level(if error.kind() == DomainErrorKind::Cancelled {
                    Level::Info
                } else {
                    Level::Warn
                })
                .error(error.kind())
                .outcome(if self.committed {
                    Outcome::PartialCompletion
                } else if error.kind() == DomainErrorKind::Cancelled {
                    Outcome::Cancelled
                } else {
                    Outcome::Failed
                }),
        }
        .committed(self.committed)
        .operation(&self.operation);
        let _ = self.log(record, true).await;
        result
    }
}

impl HandoffInteraction for Worker<'_> {
    fn progress(&mut self, stage: LifecycleStage) -> HandoffFuture<'_, ()> {
        Box::pin(async move {
            let code = match &stage {
                LifecycleStage::Preparing => Stage::Preparing,
                LifecycleStage::Verified { .. } => Stage::Verified,
                LifecycleStage::Continuing => Stage::Continuing,
                LifecycleStage::Stopping => Stage::Stopping,
                LifecycleStage::Activating => Stage::Activating,
                LifecycleStage::Starting => Stage::Starting,
            };
            let _ = self
                .log(DiagnosticEvent::new(Kind::UpdateStage).stage(code), false)
                .await;
            let _ = self.channel.send(&Event::Progress(stage)).await;
        })
    }

    fn confirm(&mut self, impact: &SessionImpact) -> HandoffFuture<'_, Result<(), DaemonError>> {
        let impact = impact.clone();
        Box::pin(async move {
            self.channel.send(&Event::Confirm(impact)).await?;
            match self.channel.receive().await? {
                Request::Approve => Ok(()),
                _ => Err(cancelled()),
            }
        })
    }

    fn committed(&mut self) -> HandoffFuture<'_, ()> {
        Box::pin(async move {
            self.committed = true;
            let _ = self
                .log(
                    DiagnosticEvent::new(Kind::UpdateStage).stage(Stage::Committed),
                    true,
                )
                .await;
        })
    }

    fn handoff(
        &mut self,
        release: Option<&PreparedRelease>,
    ) -> HandoffFuture<'_, Result<(), DaemonError>> {
        let version = release.map(|prepared| prepared.version().to_owned());
        Box::pin(async move {
            if let Some(version) = version {
                self.version = Some(version);
                let paths = self.paths.clone();
                self.log_enabled = run_blocking(move || {
                    let enabled = match fs::symlink_metadata(paths.state_root()) {
                        Ok(_) => crate::diagnostics::configured(&paths),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                        Err(_) => {
                            return Err(DaemonError::new(
                                DomainErrorKind::PathUnsafe,
                                "Unable to inspect update state.",
                            ));
                        }
                    };
                    if enabled {
                        voytty_platform::diagnostics::Store::new(paths)
                            .preflight()
                            .map_err(|_| {
                                DaemonError::new(
                                    DomainErrorKind::PathUnsafe,
                                    "Unable to open the update outcome log.",
                                )
                            })?;
                    }
                    Ok(enabled)
                })
                .await?;
            }
            self.log(
                DiagnosticEvent::new(Kind::UpdateStage).stage(Stage::AwaitingHandoff),
                true,
            )
            .await?;
            self.channel.send(&Event::Handoff).await?;
            if !matches!(self.channel.receive().await?, Request::Continue) {
                return Err(cancelled());
            }
            self.accepted = true;
            self.log(
                DiagnosticEvent::new(Kind::UpdateStage).stage(Stage::Accepted),
                true,
            )
            .await?;
            // The received Continue transfers ownership even if its ACK is lost.
            let _ = self.channel.send(&Event::Accepted).await;
            Ok(())
        })
    }
}

#[cfg(all(test, unix))]
mod tests;
