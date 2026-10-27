//! One-shot lifecycle owner, detached from the terminal it may terminate.
//!
//! Stop, restart, and update all stop the running daemon, which ends its
//! Sessions and possibly the terminal that asked. The frontend keeps
//! confirmation and progress; after an explicit handoff this child owns the
//! stop, any release activation, and any replacement start.
//!
//! Only this module decodes the private parent/child control protocol. Candidate
//! preparation and activation remain in their existing distribution/runtime owners.

use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Child;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use voytty_core::DomainErrorKind;
use voytty_diagnostics::{
    Event as DiagnosticEvent, Kind, Level, Operation, Outcome, Recorder, Stage,
};
use voytty_platform::local_unix::{spawn_isolated_command, take_isolated_channel};
use voytty_platform::user_state::UserPaths;

use crate::distribution::{PreparedRelease, ReleaseSelection};
use crate::error::DaemonError;
use crate::operations::{LifecycleStage, LocalRuntime, UpdateResult};
use crate::service::{DaemonReadiness, SessionImpact};

/// Hidden entry used only by the one-shot lifecycle launcher.
pub const INTERNAL_HANDOFF_ARGUMENT: &str = "--internal-handoff";
const MAX_CONTROL_BYTES: usize = 64 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const READY_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) trait HandoffInteraction {
    fn progress(&mut self, stage: LifecycleStage);
    fn confirm(&mut self, impact: &SessionImpact) -> Result<(), DaemonError>;
    /// Transfers ownership; only an update hands over a verified release.
    fn handoff(&mut self, release: Option<&PreparedRelease>) -> Result<(), DaemonError>;
    fn committed(&mut self);
}

/// Lifecycle command the frontend asks the detached owner to run.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Command {
    Stop,
    Restart,
    Update { version: Option<String> },
}

impl Command {
    const fn noun(&self) -> &'static str {
        match self {
            Self::Stop => "stop",
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

struct Channel {
    stream: UnixStream,
    usable: bool,
}

impl Channel {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            usable: true,
        }
    }

    fn send(&mut self, value: &impl Serialize) -> Result<(), DaemonError> {
        let result = self.write(value);
        if result.is_err() {
            self.usable = false;
            // A partial frame must never be followed by another frame.
            let _ = self.stream.shutdown(Shutdown::Both);
        }
        result
    }

    fn write(&mut self, value: &impl Serialize) -> Result<(), DaemonError> {
        if !self.usable {
            return Err(channel_error());
        }
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
        let deadline = Instant::now() + WRITE_TIMEOUT;
        let mut remaining = bytes.as_slice();
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

    fn receive<T: DeserializeOwned>(&mut self) -> Result<T, DaemonError> {
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
pub(crate) fn stop(
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
    )? {
        Completed::Stopped(impact) => Ok(impact),
        _ => Err(channel_error()),
    }
}

/// Restarts through the detached owner and returns the new daemon's readiness.
pub(crate) fn restart(
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
    )? {
        Completed::Restarted(readiness) => Ok(readiness),
        _ => Err(channel_error()),
    }
}

/// Prepares, activates, and starts one release through the detached owner.
pub(crate) fn update(
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
    match run_frontend(executable, paths, command, approved, confirm, progress)? {
        Completed::Updated(result) => Ok(result),
        _ => Err(channel_error()),
    }
}

fn run_frontend(
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
    let (child, stream) = spawn_isolated_command(
        executable,
        paths.home(),
        INTERNAL_HANDOFF_ARGUMENT,
    )
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
    let mut channel = Channel::new(stream);
    let result = (|| {
        channel
            .stream
            .set_read_timeout(Some(READY_TIMEOUT))
            .map_err(|_| channel_error())?;
        if !matches!(channel.receive()?, Event::Ready) {
            return Err(channel_error());
        }
        child.ready = true;
        channel
            .stream
            .set_read_timeout(None)
            .map_err(|_| channel_error())?;
        channel.send(&Request::Begin { command, approved })?;
        loop {
            match channel.receive()? {
                Event::Progress(stage) => progress(stage),
                Event::Confirm(impact) => {
                    if let Err(error) = confirm(&impact) {
                        let _ = channel.send(&Request::Cancel);
                        return Err(error);
                    }
                    channel.send(&Request::Approve)?;
                }
                Event::Handoff if !child.continued => {
                    progress(LifecycleStage::Continuing);
                    child.continued = true;
                    channel.send(&Request::Continue)?;
                }
                Event::Accepted if child.continued => {}
                Event::Complete(result) => return result.map_err(Failure::into_error),
                _ => return Err(channel_error()),
            }
        }
    })();
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
        channel: Channel::new(stream),
        paths: runtime.paths(),
        version: None,
        log_enabled: false,
        diagnostics: Recorder::default(),
        operation: Operation::default(),
        accepted: false,
        committed: false,
    };
    worker.channel.send(&Event::Ready)?;
    let Request::Begin { command, approved } = worker.channel.receive()? else {
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
        Command::Update { version } => worker
            .update(runtime, version, approved, prepare)
            .await
            .map(Completed::Updated),
    };
    let _ = worker
        .channel
        .send(&Event::Complete(result.clone().map_err(Into::into)));
    result.map(|_| ())
}

struct Worker<'a> {
    channel: Channel,
    paths: &'a UserPaths,
    version: Option<String>,
    log_enabled: bool,
    diagnostics: Recorder,
    operation: Operation,
    accepted: bool,
    committed: bool,
}

impl Worker<'_> {
    fn log(&self, record: DiagnosticEvent, durable: bool) -> Result<(), DaemonError> {
        if !self.log_enabled {
            return Ok(());
        }
        let mut record = record
            .operation(&self.operation)
            .committed(self.committed)
            .accepted(self.accepted);
        if let Some(version) = &self.version {
            record = record.release_version(version);
        }
        let accepted = self.diagnostics.record(record);
        if durable && (!accepted || !self.diagnostics.flush(Duration::from_secs(2))) {
            return Err(DaemonError::new(
                DomainErrorKind::PathUnsafe,
                "Unable to record the update outcome.",
            ));
        }
        Ok(())
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
        self.log_enabled = crate::diagnostics::configured(self.paths);
        self.diagnostics = crate::diagnostics::recorder(self.paths);
        let result = async {
            self.progress(LifecycleStage::Preparing);
            let prepared = prepare(version).await?;
            self.progress(LifecycleStage::Verified {
                version: prepared.version().to_owned(),
            });
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
        let _ = self.log(record, true);
        result
    }
}

impl HandoffInteraction for Worker<'_> {
    fn progress(&mut self, stage: LifecycleStage) {
        let code = match &stage {
            LifecycleStage::Preparing => Stage::Preparing,
            LifecycleStage::Verified { .. } => Stage::Verified,
            LifecycleStage::Continuing => Stage::Continuing,
            LifecycleStage::Stopping => Stage::Stopping,
            LifecycleStage::Activating => Stage::Activating,
            LifecycleStage::Starting => Stage::Starting,
        };
        let _ = self.log(DiagnosticEvent::new(Kind::UpdateStage).stage(code), false);
        let _ = self.channel.send(&Event::Progress(stage));
    }

    fn confirm(&mut self, impact: &SessionImpact) -> Result<(), DaemonError> {
        self.channel.send(&Event::Confirm(impact.clone()))?;
        match self.channel.receive()? {
            Request::Approve => Ok(()),
            _ => Err(cancelled()),
        }
    }

    fn committed(&mut self) {
        self.committed = true;
        let _ = self.log(
            DiagnosticEvent::new(Kind::UpdateStage).stage(Stage::Committed),
            true,
        );
    }

    fn handoff(&mut self, release: Option<&PreparedRelease>) -> Result<(), DaemonError> {
        if let Some(prepared) = release {
            self.version = Some(prepared.version().to_owned());
            self.log_enabled = match fs::symlink_metadata(self.paths.state_root()) {
                Ok(_) => crate::diagnostics::configured(self.paths),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(_) => {
                    return Err(DaemonError::new(
                        DomainErrorKind::PathUnsafe,
                        "Unable to inspect update state.",
                    ));
                }
            };
            if self.log_enabled {
                voytty_platform::diagnostics::Store::new(self.paths.clone())
                    .preflight()
                    .map_err(|_| {
                        DaemonError::new(
                            DomainErrorKind::PathUnsafe,
                            "Unable to open the update outcome log.",
                        )
                    })?;
            }
        }
        self.log(
            DiagnosticEvent::new(Kind::UpdateStage).stage(Stage::AwaitingHandoff),
            true,
        )?;
        self.channel.send(&Event::Handoff)?;
        if !matches!(self.channel.receive()?, Request::Continue) {
            return Err(cancelled());
        }
        self.accepted = true;
        self.log(
            DiagnosticEvent::new(Kind::UpdateStage).stage(Stage::Accepted),
            true,
        )?;
        // The received Continue transfers ownership even if its ACK is lost.
        let _ = self.channel.send(&Event::Accepted);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
