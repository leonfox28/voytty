//! Voytty-owned pseudo-terminal lifecycle boundary.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, TryLockError};

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty};

#[cfg(unix)]
mod unix_io;

#[derive(Default)]
struct InputStop {
    stopped: AtomicBool,
    #[cfg(unix)]
    wake: Option<mio::Waker>,
    #[cfg(windows)]
    master: NativeMaster,
}

impl InputStop {
    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        #[cfg(windows)]
        if let Some(master) = self.master.lock().unwrap_or_else(|e| e.into_inner()).take() {
            // ClosePseudoConsole releases the input pipe and attached process
            // tree. Older Windows versions wait for output to drain, so the
            // reader must keep running while this separate closer owns it.
            std::thread::spawn(move || drop(master));
        }
        #[cfg(unix)]
        if let Some(wake) = &self.wake {
            let _ = wake.wake();
        }
    }
}

#[cfg(all(unix, not(any(target_os = "android", target_os = "redox"))))]
const HOSTED_TERM: &str = "xterm-256color";
#[cfg(all(unix, not(any(target_os = "android", target_os = "redox"))))]
const HOSTED_COLORTERM: &str = "truecolor";

#[cfg(windows)]
type NativeMaster = Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>;
#[cfg(not(windows))]
type NativeMaster = Box<dyn MasterPty + Send>;

fn resize_master(master: &NativeMaster, size: PtySize) -> Result<(), PtyError> {
    #[cfg(windows)]
    let guard = master.lock().unwrap_or_else(|error| error.into_inner());
    #[cfg(windows)]
    let master = guard
        .as_ref()
        .ok_or_else(|| operation_error(PtyOperation::Resize, "PTY is closed"))?;
    master
        .resize(to_portable_size(size))
        .map_err(|error| operation_error(PtyOperation::Resize, error))
}

/// Identity advertised to child programs; terminal capabilities stay unchanged.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalIdentity {
    /// Voytty's native identity.
    #[default]
    Voytty,
    /// Compatibility with programs that only recognize Ghostty's OSC 9 support.
    Ghostty,
}

impl TerminalIdentity {
    /// Accepted configuration and command-line values.
    pub const VALUES: [&'static str; 2] = ["voytty", "ghostty"];

    /// Value exported as `TERM_PROGRAM` in a new login shell.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Voytty => "voytty",
            Self::Ghostty => "ghostty",
        }
    }
}

impl std::str::FromStr for TerminalIdentity {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "voytty" => Ok(Self::Voytty),
            "ghostty" => Ok(Self::Ghostty),
            _ => Err("terminal identity must be voytty or ghostty"),
        }
    }
}

/// Daemon variables that describe this host login rather than whichever
/// terminal, multiplexer, or connection started the daemon; `LC_*` is also
/// kept. Starter markers such as `TMUX` would make every hosted shell look
/// nested, so they are dropped and the login profile rebuilds the rest.
#[cfg(all(unix, not(any(target_os = "android", target_os = "redox"))))]
const INHERITED_LOGIN_VARIABLES: &[&str] = &[
    "PATH",
    "LANG",
    "LANGUAGE",
    "TZ",
    "TMPDIR",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_CONFIG_DIRS",
    "XDG_DATA_HOME",
    "XDG_DATA_DIRS",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
    "SSH_AUTH_SOCK",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "__CF_USER_TEXT_ENCODING",
];

#[cfg(all(unix, not(any(target_os = "android", target_os = "redox"))))]
use crate::account::{AccountError, EffectiveAccount};

/// Visible dimensions supplied when a pseudo-terminal is opened or resized.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PtySize {
    /// Number of terminal rows.
    pub rows: u16,
    /// Number of terminal columns.
    pub columns: u16,
    /// Width of one terminal cell in pixels, or zero when unknown.
    pub pixel_width: u16,
    /// Height of one terminal cell in pixels, or zero when unknown.
    pub pixel_height: u16,
}

impl PtySize {
    /// Creates a character-sized terminal with unknown pixel dimensions.
    #[must_use]
    pub const fn new(rows: u16, columns: u16) -> Self {
        Self {
            rows,
            columns,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

impl Default for PtySize {
    fn default() -> Self {
        Self::new(24, 80)
    }
}

/// Explicit process description for low-level platform tests and fixtures.
///
/// Product session creation uses [`PtyHost::spawn_current_account_login_shell`]
/// instead of exposing arbitrary commands to users.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplicitPtyCommand {
    program: PathBuf,
    arguments: Vec<OsString>,
    working_directory: PathBuf,
}

impl ExplicitPtyCommand {
    /// Creates an explicit command. Both paths must be absolute.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>, working_directory: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            working_directory: working_directory.into(),
        }
    }

    /// Appends one argument without invoking a shell.
    #[must_use]
    pub fn arg(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }
}

/// Path role associated with a validation error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyPathKind {
    /// An explicit fixture executable.
    Program,
    /// The effective account's configured login shell.
    LoginShell,
    /// The effective account's home directory.
    HomeDirectory,
    /// A requested session working directory.
    WorkingDirectory,
}

/// Reason a PTY path was rejected before opening the PTY.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyPathIssue {
    /// The path was not absolute.
    NotAbsolute,
    /// The path does not exist.
    NotFound,
    /// An executable path is not a regular file.
    NotFile,
    /// A directory path is not a directory.
    NotDirectory,
    /// The effective account cannot execute the file.
    NotExecutable,
    /// The effective account cannot search or enter the directory.
    Inaccessible,
}

/// PTY operation associated with an upstream operating-system failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyOperation {
    /// Opening a native PTY pair.
    Open,
    /// Cloning the PTY reader.
    TakeReader,
    /// Taking the PTY writer.
    TakeWriter,
    /// Spawning the root child.
    Spawn,
    /// Writing terminal input.
    WriteInput,
    /// Resizing the PTY.
    Resize,
    /// Observing or waiting for root-child exit.
    Wait,
    /// Explicitly terminating the root child.
    Close,
}

/// Typed failure returned by the voytty PTY boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PtyError {
    /// Rows and columns must both be non-zero.
    InvalidSize(PtySize),
    /// A path was rejected before any PTY was opened.
    InvalidPath {
        /// Role of the rejected path.
        kind: PtyPathKind,
        /// Rejected path.
        path: PathBuf,
        /// Validation failure.
        issue: PtyPathIssue,
    },
    /// The effective account record could not be queried.
    AccountLookup {
        /// Effective numeric user identifier, when available.
        uid: u32,
        /// Stable diagnostic detail from the account API.
        detail: String,
    },
    /// No account record exists for the effective user identifier.
    AccountNotFound {
        /// Effective numeric user identifier.
        uid: u32,
    },
    /// The requested account-backed operation is not implemented here.
    UnsupportedPlatform {
        /// Unsupported operation.
        operation: &'static str,
    },
    /// The only reader has already been transferred to its consumer.
    ReaderAlreadyTaken,
    /// A native PTY operation failed. Foreign implementation errors stay private.
    Operation {
        /// Failed operation.
        operation: PtyOperation,
        /// Diagnostic detail mapped at the crate boundary.
        detail: String,
    },
}

impl fmt::Display for PtyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSize(size) => write!(
                formatter,
                "invalid PTY size {}x{}: rows and columns must be non-zero",
                size.rows, size.columns
            ),
            Self::InvalidPath { kind, path, issue } => write!(
                formatter,
                "invalid {kind:?} path {}: {issue:?}",
                path.display()
            ),
            Self::AccountLookup { uid, detail } => {
                write!(formatter, "failed to read account {uid}: {detail}")
            }
            Self::AccountNotFound { uid } => write!(formatter, "account {uid} was not found"),
            Self::UnsupportedPlatform { operation } => {
                write!(formatter, "{operation} is unsupported on this platform")
            }
            Self::ReaderAlreadyTaken => write!(formatter, "the PTY reader was already taken"),
            Self::Operation { operation, detail } => {
                write!(formatter, "PTY {operation:?} failed: {detail}")
            }
        }
    }
}

impl std::error::Error for PtyError {}

/// Voytty-owned result of a root child terminating.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PtyExitStatus {
    exit_code: u32,
    signal: Option<String>,
}

impl PtyExitStatus {
    /// Returns the portable exit code projection.
    #[must_use]
    pub const fn exit_code(&self) -> u32 {
        self.exit_code
    }

    /// Returns the signal description when termination was signal-driven.
    #[must_use]
    pub fn signal(&self) -> Option<&str> {
        self.signal.as_deref()
    }

    /// Returns whether the child exited successfully without a signal.
    #[must_use]
    pub fn success(&self) -> bool {
        self.signal.is_none() && self.exit_code == 0
    }
}

/// Nonblocking root-child state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PtyChildState {
    /// The root child is still running.
    Running,
    /// The root child has terminated.
    Exited(PtyExitStatus),
}

/// The one blocking reader for a PTY session.
pub struct PtyReader {
    inner: Box<dyn Read + Send>,
}

impl Read for PtyReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.inner.read(buffer) {
            Err(error) if is_closed_pty_error(&error) => Ok(0),
            result => result,
        }
    }
}

fn is_closed_pty_error(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(nix::libc::EIO)
    }
    #[cfg(windows)]
    {
        matches!(error.raw_os_error(), Some(109 | 232 | 233))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = error;
        false
    }
}

/// Factory for native PTY sessions.
#[derive(Clone, Copy, Debug, Default)]
pub struct PtyHost;

impl PtyHost {
    /// Creates a native PTY host.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Spawns an explicit command for a platform fixture or integration test.
    pub fn spawn(
        &self,
        command: ExplicitPtyCommand,
        size: PtySize,
    ) -> Result<PtySession, PtyError> {
        validate_size(size)?;
        validate_executable(&command.program, PtyPathKind::Program)?;
        validate_directory(&command.working_directory, PtyPathKind::WorkingDirectory)?;

        let mut builder = CommandBuilder::new(&command.program);
        builder.args(command.arguments);
        builder.cwd(&command.working_directory);
        self.spawn_builder(builder, size)
    }

    /// Spawns the effective account's interactive shell (login shell on Unix).
    ///
    /// If `working_directory` is absent, the account home is used. The account
    /// database, not daemon environment variables or cwd, supplies all defaults.
    /// Only host-login variables pass through from the daemon environment.
    pub fn spawn_current_account_login_shell(
        &self,
        size: PtySize,
        working_directory: Option<&Path>,
    ) -> Result<PtySession, PtyError> {
        self.spawn_login_shell_with_identity(size, working_directory, TerminalIdentity::default())
    }

    /// Spawns an account login shell with an explicit child-program identity.
    pub fn spawn_login_shell_with_identity(
        &self,
        size: PtySize,
        working_directory: Option<&Path>,
        identity: TerminalIdentity,
    ) -> Result<PtySession, PtyError> {
        validate_size(size)?;

        #[cfg(all(unix, not(any(target_os = "android", target_os = "redox"))))]
        {
            let account = EffectiveAccount::current().map_err(account_error)?;
            let builder =
                login_shell_builder(&account, working_directory, std::env::vars_os(), identity)?;
            self.spawn_builder(builder, size)
        }

        #[cfg(windows)]
        {
            let account = crate::account::EffectiveAccount::current()
                .map_err(|error| operation_error(PtyOperation::Spawn, error))?;
            let directory = working_directory.unwrap_or(account.home());
            validate_directory(directory, PtyPathKind::WorkingDirectory)?;
            let mut builder = CommandBuilder::new(account.shell());
            builder.arg("-NoLogo");
            builder.cwd(directory);
            builder.env("TERM", "xterm-256color");
            builder.env("COLORTERM", "truecolor");
            builder.env("TERM_PROGRAM", identity.as_str());
            builder.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
            self.spawn_builder(builder, size)
        }
        #[cfg(not(any(
            windows,
            all(unix, not(any(target_os = "android", target_os = "redox")))
        )))]
        {
            let _ = (working_directory, identity);
            Err(PtyError::UnsupportedPlatform {
                operation: "current-account login shell",
            })
        }
    }

    fn spawn_builder(
        &self,
        builder: CommandBuilder,
        size: PtySize,
    ) -> Result<PtySession, PtyError> {
        let pair = portable_pty::native_pty_system()
            .openpty(to_portable_size(size))
            .map_err(|error| operation_error(PtyOperation::Open, error))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| operation_error(PtyOperation::TakeReader, error))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| operation_error(PtyOperation::TakeWriter, error))?;
        #[cfg(not(any(unix, windows)))]
        let input_stopped = Arc::new(InputStop::default());
        #[cfg(unix)]
        let (reader, writer, input_stopped) = unix_io::interruptible(&*pair.master, reader, writer)
            .map_err(|error| operation_error(PtyOperation::Open, error))?;
        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|error| operation_error(PtyOperation::Spawn, error))?;

        #[cfg(windows)]
        let master = Arc::new(Mutex::new(Some(pair.master)));
        #[cfg(not(windows))]
        let master = pair.master;
        #[cfg(windows)]
        let input_stopped = Arc::new(InputStop {
            stopped: AtomicBool::new(false),
            master: Arc::clone(&master),
        });
        Ok(PtySession {
            master,
            reader: Some(PtyReader { inner: reader }),
            writer,
            child,
            finished: None,
            input_stopped,
        })
    }
}

/// Owner of a root child and its native PTY handles.
///
/// Dropping this value does not invoke the child killer. Session owners must use
/// [`Self::close_explicitly`] when termination is intended.
pub struct PtySession {
    master: NativeMaster,
    reader: Option<PtyReader>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    finished: Option<PtyExitStatus>,
    input_stopped: Arc<InputStop>,
}

/// PTY input and resize capability separated from root-child control.
///
/// The retained terminal driver uses this split so a blocked PTY write cannot
/// prevent the session owner from terminating and reaping the root child.
#[doc(hidden)]
pub struct PtyIo {
    master: NativeMaster,
    writer: Box<dyn Write + Send>,
}

impl PtyIo {
    /// Writes and flushes ordered bytes to the PTY.
    pub fn write_input(&mut self, bytes: &[u8]) -> Result<(), PtyError> {
        self.writer
            .write_all(bytes)
            .and_then(|()| self.writer.flush())
            .map_err(|error| operation_error(PtyOperation::WriteInput, error))
    }

    /// Resizes the native PTY after validating non-zero dimensions.
    pub fn resize(&self, size: PtySize) -> Result<(), PtyError> {
        validate_size(size)?;
        resize_master(&self.master, size)
    }
}

/// Root-child observation and termination capability separated from PTY I/O.
///
/// This remains an owner-only platform primitive. Attachments and transports
/// never receive a `PtyChild`.
#[doc(hidden)]
pub struct PtyChild {
    child: Box<dyn Child + Send + Sync>,
    finished: Option<PtyExitStatus>,
    input_stopped: Arc<InputStop>,
}

/// Cloneable, non-waiting child interruption capability.
///
/// This is deliberately separate from [`PtyChild`]: unwind/drop paths can
/// request termination without waiting for the owner which remains responsible
/// for the truthful kill/wait/reap sequence.
#[doc(hidden)]
#[derive(Clone)]
pub struct PtyChildInterrupt {
    killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
    input_stopped: Arc<InputStop>,
}

impl PtyChildInterrupt {
    /// Requests child termination without waiting or contending indefinitely.
    pub fn interrupt(&self) -> Result<(), PtyError> {
        self.input_stopped.stop();
        let mut killer = match self.killer.try_lock() {
            Ok(killer) => killer,
            Err(TryLockError::Poisoned(poisoned)) => {
                self.killer.clear_poison();
                poisoned.into_inner()
            }
            // Another caller already owns the same interruption capability.
            Err(TryLockError::WouldBlock) => return Ok(()),
        };
        killer
            .kill()
            .map_err(|error| operation_error(PtyOperation::Close, error))
    }
}

impl PtyChild {
    /// Returns the root child's process identifier when available.
    #[must_use]
    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Observes root-child exit without blocking.
    pub fn try_wait(&mut self) -> Result<PtyChildState, PtyError> {
        if let Some(status) = &self.finished {
            return Ok(PtyChildState::Exited(status.clone()));
        }

        match self
            .child
            .try_wait()
            .map_err(|error| operation_error(PtyOperation::Wait, error))?
        {
            Some(status) => {
                let status = map_exit_status(&status);
                self.finished = Some(status.clone());
                #[cfg(windows)]
                self.input_stopped.stop();
                Ok(PtyChildState::Exited(status))
            }
            None => Ok(PtyChildState::Running),
        }
    }

    /// Waits until the root child terminates and returns its cached status.
    pub fn wait(&mut self) -> Result<PtyExitStatus, PtyError> {
        if let Some(status) = &self.finished {
            return Ok(status.clone());
        }

        let status = self
            .child
            .wait()
            .map_err(|error| operation_error(PtyOperation::Wait, error))?;
        let status = map_exit_status(&status);
        self.finished = Some(status.clone());
        #[cfg(windows)]
        self.input_stopped.stop();
        Ok(status)
    }

    /// Explicitly terminates and reaps the root child.
    pub fn close_explicitly(&mut self) -> Result<PtyExitStatus, PtyError> {
        self.input_stopped.stop();
        if let PtyChildState::Exited(status) = self.try_wait()? {
            return Ok(status);
        }

        if let Err(error) = self.child.kill() {
            if let PtyChildState::Exited(status) = self.try_wait()? {
                return Ok(status);
            }
            return Err(operation_error(PtyOperation::Close, error));
        }
        self.wait()
    }
}

/// Driver-facing ownership split for one already-spawned PTY session.
#[doc(hidden)]
pub struct PtyDriverParts {
    /// Sole blocking output reader.
    pub reader: PtyReader,
    /// Ordered PTY input and resize capability.
    pub io: PtyIo,
    /// Independent child observation and termination capability.
    pub child: PtyChild,
    /// Non-waiting interruption fallback for unwind/drop paths.
    pub interrupt: PtyChildInterrupt,
}

impl PtySession {
    /// Consumes the session into driver-owned reader, I/O, and child controls.
    ///
    /// The split is deliberately available only after spawn: all path and size
    /// validation remains owned by `PtyHost`, while the daemon can isolate a
    /// potentially blocked writer from child interruption.
    #[doc(hidden)]
    pub fn into_driver_parts(mut self) -> Result<PtyDriverParts, PtyError> {
        let reader = self.reader.take().ok_or(PtyError::ReaderAlreadyTaken)?;
        let interrupt = PtyChildInterrupt {
            killer: Arc::new(Mutex::new(self.child.clone_killer())),
            input_stopped: Arc::clone(&self.input_stopped),
        };
        Ok(PtyDriverParts {
            reader,
            io: PtyIo {
                master: self.master,
                writer: self.writer,
            },
            child: PtyChild {
                child: self.child,
                finished: self.finished,
                input_stopped: self.input_stopped,
            },
            interrupt,
        })
    }

    /// Transfers the blocking output reader exactly once.
    pub fn take_reader(&mut self) -> Result<PtyReader, PtyError> {
        self.reader.take().ok_or(PtyError::ReaderAlreadyTaken)
    }

    /// Writes and flushes ordered input bytes to the PTY.
    pub fn write_input(&mut self, bytes: &[u8]) -> Result<(), PtyError> {
        self.writer
            .write_all(bytes)
            .and_then(|()| self.writer.flush())
            .map_err(|error| operation_error(PtyOperation::WriteInput, error))
    }

    /// Resizes the PTY after validating non-zero character dimensions.
    pub fn resize(&self, size: PtySize) -> Result<(), PtyError> {
        validate_size(size)?;
        resize_master(&self.master, size)
    }

    /// Returns the root child's process identifier when the platform exposes it.
    #[must_use]
    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Observes root-child exit without blocking.
    pub fn try_wait(&mut self) -> Result<PtyChildState, PtyError> {
        if let Some(status) = &self.finished {
            return Ok(PtyChildState::Exited(status.clone()));
        }

        match self
            .child
            .try_wait()
            .map_err(|error| operation_error(PtyOperation::Wait, error))?
        {
            Some(status) => {
                let status = map_exit_status(&status);
                self.finished = Some(status.clone());
                #[cfg(windows)]
                self.input_stopped.stop();
                Ok(PtyChildState::Exited(status))
            }
            None => Ok(PtyChildState::Running),
        }
    }

    /// Waits until the root child terminates and returns its cached status.
    pub fn wait(&mut self) -> Result<PtyExitStatus, PtyError> {
        if let Some(status) = &self.finished {
            return Ok(status.clone());
        }

        let status = self
            .child
            .wait()
            .map_err(|error| operation_error(PtyOperation::Wait, error))?;
        let status = map_exit_status(&status);
        self.finished = Some(status.clone());
        #[cfg(windows)]
        self.input_stopped.stop();
        Ok(status)
    }

    /// Explicitly terminates a running root child through portable-pty, then waits.
    ///
    /// Voytty adds no signal escalation or process-group policy here.
    pub fn close_explicitly(&mut self) -> Result<PtyExitStatus, PtyError> {
        self.input_stopped.stop();
        if let PtyChildState::Exited(status) = self.try_wait()? {
            return Ok(status);
        }

        if let Err(error) = self.child.kill() {
            if let PtyChildState::Exited(status) = self.try_wait()? {
                return Ok(status);
            }
            return Err(operation_error(PtyOperation::Close, error));
        }
        self.wait()
    }
}

fn validate_size(size: PtySize) -> Result<(), PtyError> {
    if size.rows == 0
        || size.columns == 0
        || (cfg!(windows) && (size.rows > i16::MAX as u16 || size.columns > i16::MAX as u16))
    {
        Err(PtyError::InvalidSize(size))
    } else {
        Ok(())
    }
}

fn to_portable_size(size: PtySize) -> portable_pty::PtySize {
    portable_pty::PtySize {
        rows: size.rows,
        cols: size.columns,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    }
}

fn validate_executable(path: &Path, kind: PtyPathKind) -> Result<(), PtyError> {
    validate_absolute(path, kind)?;
    let metadata = path_metadata(path, kind)?;
    if !metadata.is_file() {
        return Err(invalid_path(kind, path, PtyPathIssue::NotFile));
    }
    validate_effective_access(path, kind, true)
}

fn validate_directory(path: &Path, kind: PtyPathKind) -> Result<(), PtyError> {
    validate_absolute(path, kind)?;
    let metadata = path_metadata(path, kind)?;
    if !metadata.is_dir() {
        return Err(invalid_path(kind, path, PtyPathIssue::NotDirectory));
    }
    validate_effective_access(path, kind, false)
}

fn validate_absolute(path: &Path, kind: PtyPathKind) -> Result<(), PtyError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(invalid_path(kind, path, PtyPathIssue::NotAbsolute))
    }
}

fn path_metadata(path: &Path, kind: PtyPathKind) -> Result<std::fs::Metadata, PtyError> {
    std::fs::metadata(path).map_err(|error| {
        let issue = if error.kind() == io::ErrorKind::NotFound {
            PtyPathIssue::NotFound
        } else {
            PtyPathIssue::Inaccessible
        };
        invalid_path(kind, path, issue)
    })
}

#[cfg(all(unix, not(any(target_os = "android", target_os = "redox"))))]
fn validate_effective_access(
    path: &Path,
    kind: PtyPathKind,
    executable: bool,
) -> Result<(), PtyError> {
    use nix::fcntl::AtFlags;
    use nix::unistd::{AccessFlags, faccessat};

    faccessat(None, path, AccessFlags::X_OK, AtFlags::AT_EACCESS).map_err(|_| {
        invalid_path(
            kind,
            path,
            if executable {
                PtyPathIssue::NotExecutable
            } else {
                PtyPathIssue::Inaccessible
            },
        )
    })
}

#[cfg(not(all(unix, not(any(target_os = "android", target_os = "redox")))))]
fn validate_effective_access(
    _path: &Path,
    _kind: PtyPathKind,
    _executable: bool,
) -> Result<(), PtyError> {
    Ok(())
}

fn invalid_path(kind: PtyPathKind, path: &Path, issue: PtyPathIssue) -> PtyError {
    PtyError::InvalidPath {
        kind,
        path: path.to_path_buf(),
        issue,
    }
}

fn operation_error(operation: PtyOperation, error: impl fmt::Display) -> PtyError {
    PtyError::Operation {
        operation,
        detail: error.to_string(),
    }
}

fn map_exit_status(status: &portable_pty::ExitStatus) -> PtyExitStatus {
    PtyExitStatus {
        exit_code: status.exit_code(),
        signal: status.signal().map(str::to_owned),
    }
}

#[cfg(all(unix, not(any(target_os = "android", target_os = "redox"))))]
fn account_error(error: AccountError) -> PtyError {
    match error {
        AccountError::Lookup { uid, detail } => PtyError::AccountLookup { uid, detail },
        AccountError::NotFound { uid } => PtyError::AccountNotFound { uid },
        AccountError::UnsupportedPlatform => PtyError::UnsupportedPlatform {
            operation: "current-account login shell",
        },
    }
}

#[cfg(all(unix, not(any(target_os = "android", target_os = "redox"))))]
fn login_shell_builder(
    account: &EffectiveAccount,
    requested_cwd: Option<&Path>,
    daemon_environment: impl IntoIterator<Item = (OsString, OsString)>,
    identity: TerminalIdentity,
) -> Result<CommandBuilder, PtyError> {
    validate_directory(account.home(), PtyPathKind::HomeDirectory)?;
    validate_executable(account.shell(), PtyPathKind::LoginShell)?;
    let cwd = requested_cwd.unwrap_or_else(|| account.home());
    validate_directory(cwd, PtyPathKind::WorkingDirectory)?;

    let mut builder = CommandBuilder::new_default_prog();
    // The builder starts from the whole daemon environment.
    builder.env_clear();
    for (key, value) in daemon_environment {
        if is_inherited_login_variable(&key) {
            builder.env(key, value);
        }
    }
    builder.env("HOME", account.home());
    builder.env("SHELL", account.shell());
    builder.env("USER", account.name());
    builder.env("LOGNAME", account.name());
    builder.env("TERM", HOSTED_TERM);
    builder.env("COLORTERM", HOSTED_COLORTERM);
    builder.env("TERM_PROGRAM", identity.as_str());
    builder.env("TERM_PROGRAM_VERSION", crate::build_identity().version);
    builder.cwd(cwd);
    Ok(builder)
}

#[cfg(all(unix, not(any(target_os = "android", target_os = "redox"))))]
fn is_inherited_login_variable(key: &std::ffi::OsStr) -> bool {
    key.to_str()
        .is_some_and(|key| key.starts_with("LC_") || INHERITED_LOGIN_VARIABLES.contains(&key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    struct ClosedUnixPty;

    #[cfg(unix)]
    impl Read for ClosedUnixPty {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from_raw_os_error(nix::libc::EIO))
        }
    }

    #[test]
    fn zero_character_dimension_is_rejected() {
        let size = PtySize::new(0, 80);
        assert_eq!(validate_size(size), Err(PtyError::InvalidSize(size)));
    }

    #[cfg(unix)]
    #[test]
    fn unix_eio_after_slave_close_is_normalized_to_eof() {
        let mut reader = PtyReader {
            inner: Box::new(ClosedUnixPty),
        };
        assert_eq!(reader.read(&mut [0; 1]).expect("normalized EOF"), 0);
    }

    #[cfg(all(unix, not(any(target_os = "android", target_os = "redox"))))]
    #[test]
    fn login_builder_uses_effective_account_defaults() -> Result<(), PtyError> {
        let account = EffectiveAccount::current().map_err(account_error)?;
        let builder = login_shell_builder(
            &account,
            None,
            std::iter::empty(),
            TerminalIdentity::default(),
        )?;

        assert!(builder.is_default_prog());
        assert_eq!(builder.get_env("HOME"), Some(account.home().as_os_str()));
        assert_eq!(builder.get_env("SHELL"), Some(account.shell().as_os_str()));
        assert_eq!(
            builder.get_env("USER"),
            Some(std::ffi::OsStr::new(account.name()))
        );
        assert_eq!(
            builder.get_env("LOGNAME"),
            Some(std::ffi::OsStr::new(account.name()))
        );
        assert_eq!(
            builder.get_env("TERM"),
            Some(std::ffi::OsStr::new(HOSTED_TERM))
        );
        assert_eq!(
            builder.get_env("COLORTERM"),
            Some(std::ffi::OsStr::new(HOSTED_COLORTERM))
        );
        assert_eq!(
            builder.get_env("TERM_PROGRAM"),
            Some(std::ffi::OsStr::new("voytty"))
        );
        assert_eq!(
            builder.get_env("TERM_PROGRAM_VERSION"),
            Some(std::ffi::OsStr::new(env!("CARGO_PKG_VERSION")))
        );
        assert_eq!(
            builder.get_cwd().map(OsString::as_os_str),
            Some(account.home().as_os_str())
        );
        Ok(())
    }

    #[cfg(all(unix, not(any(target_os = "android", target_os = "redox"))))]
    #[test]
    fn login_builder_drops_starter_terminal_environment() -> Result<(), PtyError> {
        let account = EffectiveAccount::current().map_err(account_error)?;
        let daemon_environment = [
            // Markers from the terminal, multiplexer, or connection that started the daemon.
            ("HERDR_ENV", "1"),
            ("HERDR_PANE_ID", "pane-1"),
            ("TMUX", "/tmp/tmux-501/default,1,0"),
            ("TMUX_PANE", "%1"),
            ("STY", "1.pts-0.host"),
            ("TERM_PROGRAM", "ghostty"),
            ("TERM_PROGRAM_VERSION", "1.2.3"),
            ("SSH_CONNECTION", "192.0.2.1 50000 192.0.2.2 22"),
            ("SHLVL", "3"),
            ("PWD", "/elsewhere"),
            ("LC", "not-a-locale-category"),
            // Account and terminal profile values are authoritative.
            ("HOME", "/elsewhere"),
            ("SHELL", "/bin/false"),
            ("USER", "starter"),
            ("LOGNAME", "starter"),
            ("TERM", "screen-256color"),
            ("COLORTERM", "24bit"),
            // Host login variables.
            ("PATH", "/usr/bin:/bin"),
            ("LANG", "zh_CN.UTF-8"),
            ("LC_CTYPE", "UTF-8"),
            ("TMPDIR", "/tmp/voytty-user/"),
            ("XDG_RUNTIME_DIR", "/run/user/501"),
            ("SSH_AUTH_SOCK", "/tmp/agent.sock"),
        ]
        .map(|(key, value)| (OsString::from(key), OsString::from(value)));
        let builder = login_shell_builder(
            &account,
            None,
            daemon_environment,
            TerminalIdentity::default(),
        )?;

        let environment = builder
            .iter_full_env_as_str()
            .collect::<std::collections::BTreeMap<_, _>>();
        // Names first: a regression must not print inherited test-process values.
        assert_eq!(
            environment.keys().copied().collect::<Vec<_>>(),
            [
                "COLORTERM",
                "HOME",
                "LANG",
                "LC_CTYPE",
                "LOGNAME",
                "PATH",
                "SHELL",
                "SSH_AUTH_SOCK",
                "TERM",
                "TERM_PROGRAM",
                "TERM_PROGRAM_VERSION",
                "TMPDIR",
                "USER",
                "XDG_RUNTIME_DIR",
            ]
        );
        let home = account.home().to_str().expect("UTF-8 test account home");
        let shell = account.shell().to_str().expect("UTF-8 test account shell");
        assert_eq!(
            environment,
            std::collections::BTreeMap::from([
                ("COLORTERM", HOSTED_COLORTERM),
                ("HOME", home),
                ("LANG", "zh_CN.UTF-8"),
                ("LC_CTYPE", "UTF-8"),
                ("LOGNAME", account.name()),
                ("PATH", "/usr/bin:/bin"),
                ("SHELL", shell),
                ("SSH_AUTH_SOCK", "/tmp/agent.sock"),
                ("TERM", HOSTED_TERM),
                ("TERM_PROGRAM", "voytty"),
                ("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION")),
                ("TMPDIR", "/tmp/voytty-user/"),
                ("USER", account.name()),
                ("XDG_RUNTIME_DIR", "/run/user/501"),
            ])
        );
        Ok(())
    }

    #[cfg(all(unix, not(any(target_os = "android", target_os = "redox"))))]
    #[test]
    fn compatibility_identity_only_changes_the_child_program_name() -> Result<(), PtyError> {
        let account = EffectiveAccount::current().map_err(account_error)?;
        let native =
            login_shell_builder(&account, None, std::iter::empty(), TerminalIdentity::Voytty)?;
        let compatible = login_shell_builder(
            &account,
            None,
            [(
                OsString::from("TERM_PROGRAM_VERSION"),
                OsString::from("9.9.9"),
            )],
            TerminalIdentity::Ghostty,
        )?;
        let mut expected = native
            .iter_full_env_as_str()
            .collect::<std::collections::BTreeMap<_, _>>();
        expected.insert("TERM_PROGRAM", "ghostty");
        assert_eq!(
            compatible
                .iter_full_env_as_str()
                .collect::<std::collections::BTreeMap<_, _>>(),
            expected
        );
        Ok(())
    }

    #[test]
    fn explicit_command_rejects_relative_paths_before_open() {
        let error = PtyHost::new()
            .spawn(
                ExplicitPtyCommand::new("fixture", "relative-cwd"),
                PtySize::default(),
            )
            .err();
        assert_eq!(
            error,
            Some(PtyError::InvalidPath {
                kind: PtyPathKind::Program,
                path: PathBuf::from("fixture"),
                issue: PtyPathIssue::NotAbsolute,
            })
        );
    }

    #[test]
    fn explicit_command_rejects_working_directory_before_open() -> Result<(), PtyError> {
        let executable = std::env::current_exe().map_err(|error| PtyError::Operation {
            operation: PtyOperation::Spawn,
            detail: error.to_string(),
        })?;
        let error = PtyHost::new()
            .spawn(
                ExplicitPtyCommand::new(executable, "relative-cwd"),
                PtySize::default(),
            )
            .err();
        assert_eq!(
            error,
            Some(PtyError::InvalidPath {
                kind: PtyPathKind::WorkingDirectory,
                path: PathBuf::from("relative-cwd"),
                issue: PtyPathIssue::NotAbsolute,
            })
        );
        Ok(())
    }

    #[cfg(all(unix, not(any(target_os = "android", target_os = "redox"))))]
    #[test]
    fn login_builder_rejects_invalid_shell_and_cwd_before_open() -> Result<(), PtyError> {
        let account = EffectiveAccount::current().map_err(account_error)?;
        let missing_shell =
            std::env::temp_dir().join(format!("voytty-missing-login-shell-{}", std::process::id()));
        let invalid_account = EffectiveAccount::for_test(
            account.uid(),
            account.gid(),
            account.name().to_owned(),
            account.home().to_path_buf(),
            missing_shell.clone(),
        );
        assert_eq!(
            login_shell_builder(
                &invalid_account,
                None,
                std::iter::empty(),
                TerminalIdentity::default()
            )
            .err(),
            Some(PtyError::InvalidPath {
                kind: PtyPathKind::LoginShell,
                path: missing_shell,
                issue: PtyPathIssue::NotFound,
            })
        );

        let missing_cwd =
            std::env::temp_dir().join(format!("voytty-missing-login-cwd-{}", std::process::id()));
        assert_eq!(
            login_shell_builder(
                &account,
                Some(&missing_cwd),
                std::iter::empty(),
                TerminalIdentity::default()
            )
            .err(),
            Some(PtyError::InvalidPath {
                kind: PtyPathKind::WorkingDirectory,
                path: missing_cwd,
                issue: PtyPathIssue::NotFound,
            })
        );
        Ok(())
    }
}
