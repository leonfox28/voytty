//! Same-account Windows named pipes and detached process startup.
use crate::user_state::{FileLock, PathError, UserPaths};
use interprocess::local_socket::tokio::{
    Listener as PipeListener, Stream as PipeStream, prelude::*,
};
use interprocess::local_socket::{
    GenericNamespaced, ListenerOptions, ToNsName, traits::StreamCommon,
};
use interprocess::os::windows::{
    local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor,
};
use std::os::windows::process::CommandExt;
use std::{
    fmt, io,
    path::{Path, PathBuf},
    pin::Pin,
    process::{Command, Stdio},
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

/// Lifetime lock for a single account's daemon.
pub struct DaemonLock(FileLock);
impl DaemonLock {
    /// Acquires the daemon lock without waiting.
    pub fn try_acquire(paths: &UserPaths) -> Result<Option<Self>, LocalPlatformError> {
        Ok(FileLock::try_acquire(paths.daemon_lock(), paths.uid())?.map(Self))
    }
    /// Exact lock path.
    pub fn path(&self) -> &Path {
        self.0.path()
    }
}
/// Identity of the daemon's private endpoint marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonSocketOwnership(Vec<u8>);
/// Platform lifecycle failure.
#[derive(Debug)]
pub enum LocalPlatformError {
    /// Unsafe managed filesystem path.
    Path(PathError),
    /// Endpoint marker failed validation.
    UnsafeSocket(PathBuf),
    /// A daemon is already running.
    AlreadyRunning,
    /// A peer failed account authentication.
    PeerUidMismatch {
        /// Expected logical account.
        expected: u32,
        /// Observed logical account.
        actual: u32,
    },
    /// Unsupported operation.
    UnsupportedPlatform,
    /// Native operation failed.
    Io(String),
}
impl fmt::Display for LocalPlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Windows local daemon: {self:?}")
    }
}
impl std::error::Error for LocalPlatformError {}
impl From<PathError> for LocalPlatformError {
    fn from(e: PathError) -> Self {
        Self::Path(e)
    }
}
fn failure(e: impl fmt::Display) -> LocalPlatformError {
    LocalPlatformError::Io(e.to_string())
}

fn pipe_name(path: &Path) -> io::Result<String> {
    use std::os::windows::ffi::OsStrExt;
    let mut bytes = crate::windows_security::sid()?.into_bytes();
    bytes.extend(path.as_os_str().encode_wide().flat_map(u16::to_le_bytes));
    let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
    Ok(format!(
        "voytty-{}",
        digest
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ))
}
/// A reserved listener path, adopted once the daemon enters its Tokio runtime.
pub struct Listener(PathBuf);
impl Listener {
    /// Named pipes are always asynchronous after adoption.
    pub fn set_nonblocking(&self, _: bool) -> io::Result<()> {
        Ok(())
    }
}
/// Same-account async named-pipe listener.
pub struct AsyncListener(PipeListener);
impl AsyncListener {
    /// Creates the pipe with a protected current-user-only DACL.
    pub fn from_std(listener: Listener) -> io::Result<Self> {
        Self::bind(listener.0)
    }
    /// Opens a private named pipe for an explicit endpoint path.
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let sid = crate::windows_security::sid()?;
        let sddl = widestring::U16CString::from_str(format!("D:P(A;;GA;;;{sid})"))
            .map_err(io::Error::other)?;
        let name = pipe_name(path.as_ref())?;
        let listener = ListenerOptions::new()
            .name(name.to_ns_name::<GenericNamespaced>()?)
            .security_descriptor(SecurityDescriptor::deserialize(&sddl)?)
            .create_tokio()?;
        Ok(Self(listener))
    }
    /// Accepts a local client; the stream is authenticated before dispatch.
    pub async fn accept(&self) -> io::Result<(Stream, ())> {
        Ok((Stream::from_pipe(self.0.accept().await?)?, ()))
    }
}
/// Bidirectional named-pipe stream used by every local protocol.
#[derive(Default)]
struct WriteProgress {
    sent: std::sync::atomic::AtomicU64,
    finished: std::sync::atomic::AtomicBool,
    error: std::sync::Mutex<Option<String>>,
    waiter: std::sync::Mutex<Option<std::task::Waker>>,
}
impl WriteProgress {
    fn wake(&self) {
        if let Some(waker) = self.waiter.lock().unwrap_or_else(|e| e.into_inner()).take() {
            waker.wake();
        }
    }
    fn poll(&self, cx: &mut Context<'_>, queued: u64, finish: bool) -> Poll<io::Result<()>> {
        use std::sync::atomic::Ordering::Acquire;
        *self.waiter.lock().unwrap_or_else(|e| e.into_inner()) = Some(cx.waker().clone());
        if let Some(error) = &*self.error.lock().unwrap_or_else(|e| e.into_inner()) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                error.clone(),
            )));
        }
        if self.sent.load(Acquire) >= queued && (!finish || self.finished.load(Acquire)) {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }
}

/// Authenticated local byte stream with independent read/write completion.
pub struct Stream {
    io: tokio::io::DuplexStream,
    peer_pid: u32,
    queued: u64,
    progress: Option<std::sync::Arc<WriteProgress>>,
    pumps: Vec<tokio::task::JoinHandle<io::Result<()>>>,
}
impl Stream {
    /// Connects and authenticates the server account before sending any request.
    pub async fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        let name = pipe_name(path.as_ref())?;
        Self::from_pipe(PipeStream::connect(name.to_ns_name::<GenericNamespaced>()?).await?)
    }
    fn from_pipe(pipe: PipeStream) -> io::Result<Self> {
        let peer_pid = pipe
            .peer_creds()?
            .pid()
            .ok_or_else(|| io::Error::other("pipe has no peer PID"))?;
        crate::windows_security::authorize_process(peer_pid)?;
        // Named pipes have no directional shutdown. A bounded record layer
        // carries FIN independently in each direction so detach/upload can
        // finish writing while still awaiting the host's final response.
        let (io, pump) = tokio::io::duplex(65_536);
        let (mut app_read, mut app_write) = tokio::io::split(pump);
        let (mut pipe_read, mut pipe_write) = tokio::io::split(pipe);
        let input = tokio::spawn(async move {
            loop {
                let length = pipe_read.read_u32().await? as usize;
                if length == 0 {
                    return app_write.shutdown().await;
                }
                if length > 65_536 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "oversized local pipe record",
                    ));
                }
                let mut bytes = vec![0; length];
                pipe_read.read_exact(&mut bytes).await?;
                app_write.write_all(&bytes).await?;
            }
        });
        let progress = std::sync::Arc::new(WriteProgress::default());
        let report = std::sync::Arc::clone(&progress);
        let output = tokio::spawn(async move {
            let result = async {
                let mut bytes = [0; 65_536];
                loop {
                    let length = app_read.read(&mut bytes).await?;
                    tokio::time::timeout(std::time::Duration::from_secs(5), async {
                        pipe_write.write_u32(length as u32).await?;
                        pipe_write.write_all(&bytes[..length]).await
                    })
                    .await
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::TimedOut, "local pipe writer stalled")
                    })??;
                    report
                        .sent
                        .fetch_add(length as u64, std::sync::atomic::Ordering::Release);
                    if length == 0 {
                        report
                            .finished
                            .store(true, std::sync::atomic::Ordering::Release);
                    }
                    report.wake();
                    if length == 0 {
                        return Ok::<_, io::Error>(());
                    }
                }
            }
            .await;
            if let Err(error) = &result {
                *report.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.to_string());
                report.wake();
            }
            result
        });
        Ok(Self {
            io,
            peer_pid,
            queued: 0,
            progress: Some(progress),
            pumps: vec![input, output],
        })
    }
    /// In-memory pair for shared byte-protocol tests; native pipe tests use bind/connect.
    #[cfg(feature = "test-support")]
    pub fn pair() -> io::Result<(Self, Self)> {
        let (a, b) = tokio::io::duplex(65_536);
        let wrap = |io| Self {
            io,
            peer_pid: std::process::id(),
            queued: 0,
            progress: None,
            pumps: Vec::new(),
        };
        Ok((wrap(a), wrap(b)))
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(input) = self.pumps.first() {
            input.abort();
        }
        // Detach the bounded writer so accepted bytes and FIN survive dropping
        // the public stream, just as bytes already accepted by a Unix socket do.
        // Each pending pipe write has a deadline; an abandoned peer cannot leak it.
    }
}
impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_read(cx, buf)
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.io).poll_write(cx, buf);
        if let Poll::Ready(Ok(length)) = result {
            this.queued = this.queued.saturating_add(length as u64);
        }
        result
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match &this.progress {
            Some(progress) => progress.poll(cx, this.queued, false),
            None => Pin::new(&mut this.io).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        std::task::ready!(Pin::new(&mut this.io).poll_shutdown(cx))?;
        match &this.progress {
            Some(progress) => progress.poll(cx, this.queued, true),
            None => Poll::Ready(Ok(())),
        }
    }
}
/// Verifies the peer's full Windows SID; the compatibility UID is never trusted.
pub fn authorize_stream_peer(stream: &Stream, _: u32) -> Result<u32, LocalPlatformError> {
    let pid = stream.peer_pid;
    crate::windows_security::authorize_process(pid).map_err(failure)?;
    Ok(0)
}
/// Inspects the private endpoint marker without connecting or creating state.
pub fn inspect_daemon_socket(paths: &UserPaths) -> Result<bool, LocalPlatformError> {
    match std::fs::symlink_metadata(paths.socket()) {
        Ok(_) => {
            crate::user_state::validate_regular_file(paths.socket(), paths.uid())?;
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(failure(e)),
    }
}
/// Reserves one endpoint under the held daemon lock.
pub fn bind_owned_daemon_socket(
    paths: &UserPaths,
    lock: &DaemonLock,
) -> Result<(Listener, DaemonSocketOwnership), LocalPlatformError> {
    use ring::rand::SecureRandom;
    use std::io::Write;
    if lock.path() != paths.daemon_lock() {
        return Err(LocalPlatformError::UnsafeSocket(paths.socket().into()));
    }
    paths.prepare_runtime_directory()?;
    let mut nonce = vec![0; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut nonce)
        .map_err(failure)?;
    crate::user_state::atomic_write(paths.socket(), paths.uid(), |f| f.write_all(&nonce))?;
    Ok((
        Listener(paths.socket().into()),
        DaemonSocketOwnership(nonce),
    ))
}
/// Rebinds only the marker owned by this daemon incarnation.
pub fn rebind_owned_daemon_socket(
    paths: &UserPaths,
    lock: &DaemonLock,
    owner: &DaemonSocketOwnership,
) -> Result<(Listener, DaemonSocketOwnership), LocalPlatformError> {
    remove_owned_daemon_socket(paths, lock, owner)?;
    bind_owned_daemon_socket(paths, lock)
}
/// Removes only the matching daemon incarnation's endpoint marker.
pub fn remove_owned_daemon_socket(
    paths: &UserPaths,
    lock: &DaemonLock,
    owner: &DaemonSocketOwnership,
) -> Result<(), LocalPlatformError> {
    if lock.path() != paths.daemon_lock()
        || !inspect_daemon_socket(paths)?
        || std::fs::read(paths.socket()).map_err(failure)? != owner.0
    {
        return Err(LocalPlatformError::UnsafeSocket(paths.socket().into()));
    }
    std::fs::remove_file(paths.socket()).map_err(failure)
}
/// Removes a stale, validated marker while the lifetime lock is held.
pub fn remove_own_socket(paths: &UserPaths, lock: &DaemonLock) -> Result<(), LocalPlatformError> {
    if lock.path() != paths.daemon_lock() {
        return Err(LocalPlatformError::UnsafeSocket(paths.socket().into()));
    }
    if inspect_daemon_socket(paths)? {
        std::fs::remove_file(paths.socket()).map_err(failure)?;
    }
    Ok(())
}
/// Starts a daemon detached from the caller's console, with null inherited stdio.
pub fn detached_command(
    executable: &Path,
    paths: &UserPaths,
    argument: &str,
) -> Result<Command, LocalPlatformError> {
    paths.prepare_state_directories()?;
    let mut command = Command::new(executable);
    command
        .arg(argument)
        .current_dir(paths.home())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x00000008 | 0x00000200);
    Ok(command)
}
/// Releases an inherited console, including when started by the login entry.
pub fn detach_current_process() -> Result<(), LocalPlatformError> {
    winsafe::FreeConsole().map_err(failure)
}

/// Private parent/child control pipe with bounded synchronous IO.
pub struct IsolatedStream {
    pipe: IsolatedPipe,
    read_timeout: std::cell::Cell<Option<std::time::Duration>>,
    write_timeout: std::cell::Cell<Option<std::time::Duration>>,
}
enum IsolatedPipe {
    Server(win_custody::PipeServer),
    Client(win_custody::PipeClient),
}
impl IsolatedStream {
    fn new(pipe: IsolatedPipe) -> Self {
        Self {
            pipe,
            read_timeout: Default::default(),
            write_timeout: Default::default(),
        }
    }
    /// Sets the next read deadline, matching the Unix handoff channel contract.
    pub fn set_read_timeout(&self, value: Option<std::time::Duration>) -> io::Result<()> {
        self.read_timeout.set(value);
        Ok(())
    }
    /// Sets the next write deadline.
    pub fn set_write_timeout(&self, value: Option<std::time::Duration>) -> io::Result<()> {
        self.write_timeout.set(value);
        Ok(())
    }
    /// Invalidates a server pipe after a partial control frame.
    pub fn shutdown(&self, _: std::net::Shutdown) -> io::Result<()> {
        match &self.pipe {
            IsolatedPipe::Server(pipe) => pipe.disconnect(),
            IsolatedPipe::Client(_) => Ok(()),
        }
    }
}
fn pipe_wait(timeout: Option<std::time::Duration>) -> win_custody::PipeWait<'static> {
    win_custody::PipeWait::within(timeout.unwrap_or(std::time::Duration::from_secs(86400)))
}
impl io::Read for IsolatedStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let wait = pipe_wait(self.read_timeout.get());
        match &self.pipe {
            IsolatedPipe::Server(pipe) => pipe.read(bytes, &wait),
            IsolatedPipe::Client(pipe) => pipe.read(bytes, &wait),
        }
        .map_err(io::Error::other)
    }
}
impl io::Write for IsolatedStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let wait = pipe_wait(self.write_timeout.get());
        match &self.pipe {
            IsolatedPipe::Server(pipe) => pipe.write_all(bytes, &wait),
            IsolatedPipe::Client(pipe) => pipe.write_all(bytes, &wait),
        }
        .map_err(io::Error::other)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
const HANDOFF_PIPE: &str = "VOYTTY_INTERNAL_HANDOFF_PIPE";
/// Spawns a detached helper with a unique SID-protected, child-PID-checked pipe.
pub fn spawn_isolated_command(
    executable: &Path,
    working_directory: &Path,
    argument: &str,
) -> Result<(std::process::Child, IsolatedStream), LocalPlatformError> {
    use ring::rand::SecureRandom;
    let mut nonce = [0; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut nonce)
        .map_err(failure)?;
    let name = format!(
        r"\\.\pipe\voytty-handoff-{}",
        nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let security = win_custody::SecurityDescriptor::from_sddl(&format!(
        "D:P(A;;GA;;;{})",
        crate::windows_security::sid().map_err(failure)?
    ))
    .map_err(failure)?;
    let pipe =
        win_custody::PipeServer::create(&name, win_custody::PipeDirection::Duplex, &security)
            .map_err(failure)?;
    let mut command = Command::new(executable);
    command
        .arg(argument)
        .current_dir(working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x00000008 | 0x00000200);
    let mut child = command.env(HANDOFF_PIPE, &name).spawn().map_err(failure)?;
    let result = pipe.connect(
        &win_custody::PipeWait::within(std::time::Duration::from_secs(5)).with_peer(&child),
    );
    if let Err(error) = result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(failure(error));
    }
    if pipe.client_process_id().map_err(failure)? != child.id() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(LocalPlatformError::UnsafeSocket(name.into()));
    }
    Ok((child, IsolatedStream::new(IsolatedPipe::Server(pipe))))
}
/// Opens the one-use handoff pipe supplied only to the detached helper.
pub fn take_isolated_channel(_: u32) -> Result<IsolatedStream, LocalPlatformError> {
    let name = std::env::var(HANDOFF_PIPE).map_err(failure)?;
    if !name.starts_with(r"\\.\pipe\voytty-handoff-") {
        return Err(LocalPlatformError::UnsafeSocket(name.into()));
    }
    let pipe = win_custody::PipeClient::open(
        &name,
        win_custody::PipeAccess::ReadWrite,
        std::time::Instant::now() + std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(10),
    )
    .map_err(failure)?;
    crate::windows_security::authorize_process(pipe.server_process_id().map_err(failure)?)
        .map_err(failure)?;
    Ok(IsolatedStream::new(IsolatedPipe::Client(pipe)))
}
