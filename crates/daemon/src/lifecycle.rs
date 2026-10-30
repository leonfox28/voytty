//! Per-user daemon singleflight launch, detached entry, and graceful cleanup.

use std::fmt;
#[cfg(any(unix, windows))]
use std::path::Path;
use std::time::Duration;
#[cfg(any(unix, windows))]
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use voytty_core::DomainErrorKind;
use voytty_platform::account::EffectiveAccount;
#[cfg(any(unix, windows))]
use voytty_platform::user_state::FileLock;
use voytty_platform::user_state::UserPaths;

use crate::client::LocalClient;
use crate::error::DaemonError;
use crate::service::DaemonReadiness;
#[cfg(any(unix, windows))]
use crate::service::DaemonService;

/// Hidden product argument used only to enter the detached daemon child.
pub const INTERNAL_DAEMON_ARGUMENT: &str = "--internal-daemon";

#[cfg(any(unix, windows))]
const START_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(any(unix, windows))]
const LOCK_POLL: Duration = Duration::from_millis(20);
const READINESS_PROBE_TIMEOUT: Duration = Duration::from_millis(250);
#[cfg(any(unix, windows))]
const RECOVERY_REBIND_BACKOFF: Duration = Duration::from_millis(20);

#[cfg(any(unix, windows))]
#[derive(Clone, Copy)]
enum DaemonNetworkMode {
    Production,
    LocalOnlyTest,
}

/// Executable and one hidden argument used by explicit lifecycle operations.
#[derive(Clone)]
pub struct DaemonLauncher {
    executable: std::path::PathBuf,
    internal_argument: String,
}

impl fmt::Debug for DaemonLauncher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DaemonLauncher")
            .field("executable", &"[REDACTED]")
            .field("internal_argument", &"[REDACTED]")
            .field("internal_argument_len", &self.internal_argument.len())
            .finish_non_exhaustive()
    }
}

impl DaemonLauncher {
    /// Resolves the current voytty executable and product hidden argument.
    pub fn current() -> Result<Self, DaemonError> {
        let executable = std::env::current_exe().map_err(|error| {
            DaemonError::new(
                DomainErrorKind::DaemonStartTimeout,
                format!("unable to locate current voytty executable: {error}"),
            )
        })?;
        Ok(Self {
            executable,
            internal_argument: INTERNAL_DAEMON_ARGUMENT.to_owned(),
        })
    }

    /// Creates an explicit launcher for task-private multi-process tests.
    #[doc(hidden)]
    #[must_use]
    pub fn for_test(executable: std::path::PathBuf, internal_argument: String) -> Self {
        Self {
            executable,
            internal_argument,
        }
    }

    /// Exact executable owned by this invocation's lifecycle operations.
    #[must_use]
    pub(crate) fn executable(&self) -> &std::path::Path {
        &self.executable
    }

    /// Ensures one daemon using this launch target.
    #[cfg(any(unix, windows))]
    pub async fn ensure(&self, paths: &UserPaths) -> Result<DaemonReadiness, DaemonError> {
        ensure_daemon_with(paths, &self.executable, &self.internal_argument).await
    }

    /// Returns the current platform limitation on unsupported targets.
    #[cfg(not(any(unix, windows)))]
    pub async fn ensure(&self, _paths: &UserPaths) -> Result<DaemonReadiness, DaemonError> {
        let _ = (&self.executable, &self.internal_argument);
        Err(unsupported())
    }
}

/// Resolves product state from the effective account database.
pub fn production_user_paths() -> Result<UserPaths, DaemonError> {
    let account = EffectiveAccount::current().map_err(|error| {
        DaemonError::new(DomainErrorKind::UnsupportedPlatform, error.to_string())
    })?;
    Ok(UserPaths::for_account(&account))
}

/// Returns readiness, starting at most one detached daemon when stopped.
#[cfg(any(unix, windows))]
pub async fn ensure_current_daemon(paths: &UserPaths) -> Result<DaemonReadiness, DaemonError> {
    DaemonLauncher::current()?.ensure(paths).await
}

/// Singleflight launcher with an explicit executable/argument for isolated harnesses.
#[cfg(any(unix, windows))]
#[doc(hidden)]
pub async fn ensure_daemon_with(
    paths: &UserPaths,
    executable: &Path,
    internal_argument: &str,
) -> Result<DaemonReadiness, DaemonError> {
    if let Some(readiness) = probe_readiness(paths).await? {
        return Ok(readiness);
    }
    paths
        .prepare_state_directories()
        .map_err(|error| DaemonError::new(DomainErrorKind::PathUnsafe, error.to_string()))?;
    let started = Instant::now();
    let _lifecycle = acquire_lifecycle_lock(paths, started).await?;
    if let Some(readiness) = probe_readiness(paths).await? {
        return Ok(readiness);
    }

    let mut command =
        voytty_platform::local::detached_command(executable, paths, internal_argument)
            .map_err(platform_error)?;
    let mut child = command.spawn().map_err(|error| {
        DaemonError::new(
            DomainErrorKind::DaemonStartTimeout,
            format!("unable to spawn detached daemon: {error}"),
        )
    })?;

    loop {
        // The spawned daemon binds its socket before it can answer. On a
        // loaded host one unanswered probe is not failure; START_TIMEOUT is.
        match probe_readiness(paths).await {
            Ok(Some(readiness)) => return Ok(readiness),
            Ok(None) => {}
            Err(error) if error.kind() == DomainErrorKind::DaemonStartTimeout => {}
            Err(error) => return Err(error),
        }
        if let Some(status) = child.try_wait().map_err(|error| {
            DaemonError::new(
                DomainErrorKind::DaemonStartTimeout,
                format!("unable to inspect daemon child: {error}"),
            )
        })? {
            return Err(DaemonError::new(
                DomainErrorKind::DaemonStartTimeout,
                format!("daemon child exited before readiness with {status}"),
            ));
        }
        if started.elapsed() >= START_TIMEOUT {
            return Err(DaemonError::new(
                DomainErrorKind::DaemonStartTimeout,
                "detached daemon did not become ready within 5 seconds",
            ));
        }
        tokio::time::sleep(LOCK_POLL).await;
    }
}

#[cfg(not(any(unix, windows)))]
/// Returns the current platform limitation on unsupported targets.
pub async fn ensure_current_daemon(_paths: &UserPaths) -> Result<DaemonReadiness, DaemonError> {
    Err(unsupported())
}

/// Product hidden entry: detach before runtime initialization, then serve.
pub fn run_internal_daemon() -> Result<(), DaemonError> {
    #[cfg(any(unix, windows))]
    voytty_platform::local::detach_current_process().map_err(platform_error)?;
    run_internal_daemon_foreground()
}

/// Service-manager entry: retains the original PID and never calls setsid or forks.
pub fn run_internal_daemon_foreground() -> Result<(), DaemonError> {
    #[cfg(any(unix, windows))]
    {
        let paths = production_user_paths()?;
        let _diagnostics = crate::diagnostics::install(&paths);
        std::panic::set_hook(Box::new(|_| {
            voytty_diagnostics::record(
                voytty_diagnostics::Event::new(voytty_diagnostics::Kind::ProcessPanicked)
                    .level(voytty_diagnostics::Level::Error),
            );
            let _ = voytty_diagnostics::flush(Duration::from_millis(250));
        }));
        voytty_diagnostics::record(voytty_diagnostics::Event::new(
            voytty_diagnostics::Kind::ProcessStarted,
        ));
        let result = run_daemon(&paths);
        if let Err(error) = &result {
            voytty_diagnostics::record(
                voytty_diagnostics::Event::new(voytty_diagnostics::Kind::ProcessFailed)
                    .level(voytty_diagnostics::Level::Error)
                    .error(error.kind()),
            );
        } else {
            voytty_diagnostics::record(voytty_diagnostics::Event::new(
                voytty_diagnostics::Kind::ProcessStopped,
            ));
        }
        result
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(unsupported())
    }
}

/// Runs one already-detached product daemon against explicit paths.
#[cfg(any(unix, windows))]
#[doc(hidden)]
pub fn run_daemon(paths: &UserPaths) -> Result<(), DaemonError> {
    run_daemon_with_network_mode(paths, DaemonNetworkMode::Production, None)
}

/// Runs an already-detached test daemon with only same-UID local IPC.
///
/// This explicit harness entry never prepares or binds an Iroh Endpoint. Product
/// startup always enters through [`run_daemon`] and retains the network owner.
#[cfg(any(unix, windows))]
#[doc(hidden)]
pub fn run_local_only_daemon_for_test(paths: &UserPaths) -> Result<(), DaemonError> {
    run_daemon_with_network_mode(paths, DaemonNetworkMode::LocalOnlyTest, None)
}

/// Runs an already-detached same-UID daemon with a deterministic test Session owner.
///
/// The injected service changes only PTY spawning. This entry retains the
/// production local IPC, Session registry, terminal driver, and cleanup paths,
/// and never prepares or binds an Iroh Endpoint.
#[cfg(any(unix, windows))]
#[doc(hidden)]
pub fn run_local_only_daemon_with_sessions_for_test(
    paths: &UserPaths,
    sessions: crate::session::SessionService,
) -> Result<(), DaemonError> {
    run_daemon_with_network_mode(paths, DaemonNetworkMode::LocalOnlyTest, Some(sessions))
}

#[cfg(any(unix, windows))]
fn run_daemon_with_network_mode(
    paths: &UserPaths,
    network_mode: DaemonNetworkMode,
    test_sessions: Option<crate::session::SessionService>,
) -> Result<(), DaemonError> {
    use std::sync::Arc;

    use crate::authorization::AuthorizationRegistry;
    use crate::bootstrap::validate_committed_setup_with_store;
    use crate::connection_broker::ConnectionIdentity;
    use crate::device_directory::DeviceDirectory;
    use crate::identity::DeviceIdentity;
    use crate::network::NetworkStartup;
    use crate::pairing::PairingManager;
    use crate::pairing_service::PairingService;
    use crate::remote_session::RemoteSessionService;
    use crate::service::{BrokerRemoteDeviceAccess, DeviceManagement};
    use crate::session_wire::RemoteSessionServiceHandler;
    use crate::store::{StateStore, StoreActor};
    use crate::transport::InfrastructureProfile;

    paths
        .prepare_state_directories()
        .map_err(|error| DaemonError::new(DomainErrorKind::PathUnsafe, error.to_string()))?;
    let daemon_lock = acquire_daemon_lock(paths)?;
    let store = StateStore::open(paths)?;
    let setup = validate_committed_setup_with_store(paths, &store)?;
    let network_inputs = match network_mode {
        DaemonNetworkMode::Production => {
            Some((store.list_authorizations()?, DeviceIdentity::load(paths)?))
        }
        DaemonNetworkMode::LocalOnlyTest => None,
    };
    let (listener, socket_ownership) =
        voytty_platform::local::bind_owned_daemon_socket(paths, &daemon_lock)
            .map_err(platform_error)?;
    let (service, network, _store_actor) = match network_inputs {
        Some((authorizations, identity)) => {
            let store_actor = StoreActor::start(store)?;
            let store_handle = store_actor.handle();
            let authorization = AuthorizationRegistry::new();
            authorization.preload(authorizations)?;
            let directory = DeviceDirectory::new(store_handle.clone());
            let connection_identity =
                ConnectionIdentity::product(setup.device_id, setup.config.device_name.clone())?
                    .with_capabilities(voytty_core::Capabilities::from_bits_retain(
                        voytty_core::Capabilities::LOCAL_LIFECYCLE
                            | voytty_core::Capabilities::SESSION_SERVICE
                            | voytty_core::Capabilities::TERMINAL_SERVICE
                            | voytty_core::Capabilities::FILE_UPLOAD_SERVICE,
                    ));
            let profile = InfrastructureProfile::from_validated(&setup.config.infrastructure);
            let limits = voytty_core::TransportLimits::default();
            let (network_startup, network_handle) = NetworkStartup::prepare(
                identity,
                profile,
                connection_identity.clone(),
                store_handle.clone(),
                authorization.clone(),
                limits,
            )?;
            let network_startup = network_startup.with_udp_port(setup.config.udp_port);
            let pairing = PairingService::new(
                PairingManager::new(setup.device_id, limits).map_err(DaemonError::from)?,
                store_handle.clone(),
                authorization.clone(),
                directory.clone(),
                network_handle.broker(),
                network_handle.observe(),
                connection_identity,
                limits,
            )?;
            let own_device_id = setup.device_id;
            let remote_sessions = RemoteSessionService::production(
                own_device_id,
                directory.clone(),
                network_handle.broker(),
            );
            let sessions =
                crate::session::SessionService::from_config(own_device_id, paths.clone());
            let service = DaemonService::with_network(setup, network_handle.observe(), sessions)
                .with_remote_sessions(remote_sessions);
            let session_handler = RemoteSessionServiceHandler::new(
                service.sessions().clone(),
                own_device_id,
                authorization.clone(),
            );
            let network_startup = network_startup
                .with_pair_handler(pairing.clone())
                .with_service_handler(session_handler)?;
            let remote_access = Arc::new(BrokerRemoteDeviceAccess::new(
                network_handle.broker(),
                service.sessions().clone(),
            ));
            let devices =
                DeviceManagement::new(store_handle, directory, authorization, remote_access);
            (
                Arc::new(
                    service
                        .with_device_management(devices)
                        .with_pairing(pairing),
                ),
                Some((network_startup, network_handle)),
                Some(store_actor),
            )
        }
        None => {
            let store_actor = StoreActor::start(store)?;
            let directory = DeviceDirectory::new(store_actor.handle());
            let own_device_id = setup.device_id;
            let remote_sessions = RemoteSessionService::local_only(own_device_id, directory);
            let service = match test_sessions {
                Some(sessions) => DaemonService::with_sessions(
                    setup,
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_secs()),
                    sessions,
                ),
                None => DaemonService::with_sessions(
                    setup,
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_secs()),
                    crate::session::SessionService::from_config(own_device_id, paths.clone()),
                ),
            };
            (
                Arc::new(service.with_remote_sessions(remote_sessions)),
                None,
                Some(store_actor),
            )
        }
    };
    let _ = crate::diagnostics::register_daemon(paths, &daemon_lock);
    tracing::info!(
        component = "daemon",
        operation = "ready",
        version = voytty_core::BuildIdentity::current().version,
        pid = std::process::id(),
        "Local daemon ready"
    );

    run_owned_daemon_listener(
        paths,
        &daemon_lock,
        listener,
        socket_ownership,
        service,
        crate::local_ipc::LocalIpcLimits::default(),
        Duration::from_secs(5),
        network,
    )
}

#[cfg(any(unix, windows))]
fn acquire_daemon_lock(
    paths: &UserPaths,
) -> Result<voytty_platform::local::DaemonLock, DaemonError> {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            return Err(DaemonError::new(
                DomainErrorKind::DaemonAlreadyRunning,
                "daemon.lock stayed busy throughout the startup budget",
            ));
        }
        if let Some(lock) =
            voytty_platform::local::DaemonLock::try_acquire(paths).map_err(platform_error)?
        {
            return Ok(lock);
        }
        // Read-only status/diagnostic probes also acquire this lock briefly.
        // A single busy observation does not prove another daemon exists.
        // Retry within the existing startup budget, without bypassing ownership.
        std::thread::sleep(LOCK_POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(any(unix, windows))]
#[allow(clippy::too_many_arguments)]
fn run_owned_daemon_listener(
    paths: &UserPaths,
    daemon_lock: &voytty_platform::local::DaemonLock,
    mut listener: voytty_platform::local::Listener,
    mut socket_ownership: voytty_platform::local::DaemonSocketOwnership,
    service: std::sync::Arc<DaemonService>,
    mut limits: crate::local_ipc::LocalIpcLimits,
    cleanup_timeout: Duration,
    network: Option<(
        crate::network::NetworkStartup,
        crate::network::NetworkHandle,
    )>,
) -> Result<(), DaemonError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            DaemonError::new(
                DomainErrorKind::DaemonStartTimeout,
                format!("unable to build daemon runtime: {error}"),
            )
        })?;
    let mut network = spawn_inside_runtime(&runtime, || {
        network.map(|(startup, handle)| startup.spawn(handle))
    });
    loop {
        let server_result = runtime.block_on(crate::local_ipc::serve_local_with_limits(
            listener,
            paths.uid(),
            std::sync::Arc::clone(&service),
            limits,
        ));
        let cleanup_deadline = Instant::now() + cleanup_timeout;
        let session_cleanup = service
            .sessions()
            .shutdown_until(cleanup_deadline)
            .map(|_| ());
        match (server_result, session_cleanup) {
            (Ok(()), Ok(())) => {
                tracing::info!(
                    component = "daemon",
                    operation = "stopping",
                    pid = std::process::id(),
                    "Local daemon stopping"
                );
                let pairing_cleanup = match service.pairing() {
                    Some(pairing) => runtime.block_on(pairing.shutdown_until(cleanup_deadline)),
                    None => Ok(()),
                };
                let network_cleanup = match network.as_mut() {
                    Some(network) => runtime.block_on(network.shutdown_until(cleanup_deadline)),
                    None => Ok(()),
                };
                let socket_cleanup = voytty_platform::local::remove_owned_daemon_socket(
                    paths,
                    daemon_lock,
                    &socket_ownership,
                )
                .map_err(platform_error);
                pairing_cleanup?;
                network_cleanup?;
                return socket_cleanup;
            }
            (Err(server_error), Ok(())) => {
                // A fatal listener is allowed to terminate only after all
                // child/actor ownership is truthfully released.
                if let Some(pairing) = service.pairing()
                    && let Err(error) = runtime.block_on(pairing.shutdown_until(cleanup_deadline))
                {
                    tracing::warn!(
                        component = "daemon",
                        operation = "cleanup_failed",
                        error_kind = error.kind().code(),
                        "pairing cleanup failed after fatal local listener exit"
                    );
                }
                if let Some(network) = network.as_mut()
                    && let Err(error) = runtime.block_on(network.shutdown_until(cleanup_deadline))
                {
                    tracing::warn!(
                        component = "daemon",
                        operation = "cleanup_failed",
                        error_kind = error.kind().code(),
                        "network cleanup failed after fatal local listener exit"
                    );
                }
                voytty_platform::local::remove_owned_daemon_socket(
                    paths,
                    daemon_lock,
                    &socket_ownership,
                )
                .map_err(platform_error)?;
                return Err(server_error);
            }
            (server_result, Err(cleanup_error)) => {
                // Retain the daemon lock, store/service, and process. The held
                // lock proves that the stale same-UID socket was created by
                // this daemon's listener; `bind_daemon_socket` validates that
                // path again before replacing it.
                match server_result {
                    Ok(()) => tracing::warn!(
                        component = "daemon",
                        operation = "cleanup_failed",
                        error_kind = cleanup_error.kind().code(),
                        "listener stopped before owned sessions were released; rebinding"
                    ),
                    Err(server_error) => tracing::warn!(
                        component = "daemon",
                        operation = "cleanup_failed",
                        error_kind = server_error.kind().code(),
                        cleanup_kind = cleanup_error.kind().code(),
                        "fatal listener exit retained owned sessions; rebinding"
                    ),
                }
                limits = limits.without_accept_failure_injection();
                let rebound = loop {
                    match voytty_platform::local::rebind_owned_daemon_socket(
                        paths,
                        daemon_lock,
                        &socket_ownership,
                    ) {
                        Ok(rebound) => break rebound,
                        Err(error) => {
                            tracing::warn!(
                                component = "daemon",
                                operation = "cleanup_failed",
                                error_kind = platform_error_kind(&error).code(),
                                "unable to rebind owned daemon socket; retrying"
                            );
                            std::thread::sleep(RECOVERY_REBIND_BACKOFF);
                        }
                    }
                };
                (listener, socket_ownership) = rebound;
                let _ = crate::diagnostics::register_daemon(paths, daemon_lock);
                tracing::info!(
                    component = "daemon",
                    operation = "listener_recovered",
                    "Local daemon listener recovered"
                );
            }
        }
    }
}

#[cfg(any(unix, windows))]
fn spawn_inside_runtime<T>(runtime: &tokio::runtime::Runtime, spawn: impl FnOnce() -> T) -> T {
    let _runtime_guard = runtime.enter();
    spawn()
}

/// Runs the exact owned-listener recovery loop with deterministic limits.
#[cfg(any(unix, windows))]
#[doc(hidden)]
pub fn run_owned_daemon_listener_for_test(
    paths: &UserPaths,
    daemon_lock: voytty_platform::local::DaemonLock,
    listener: voytty_platform::local::Listener,
    socket_ownership: voytty_platform::local::DaemonSocketOwnership,
    service: std::sync::Arc<DaemonService>,
    limits: crate::local_ipc::LocalIpcLimits,
    cleanup_timeout: Duration,
) -> Result<(), DaemonError> {
    run_owned_daemon_listener(
        paths,
        &daemon_lock,
        listener,
        socket_ownership,
        service,
        limits,
        cleanup_timeout,
        None,
    )
}

pub(crate) async fn probe_readiness(
    paths: &UserPaths,
) -> Result<Option<DaemonReadiness>, DaemonError> {
    let client = LocalClient::new(paths.socket());
    match tokio::time::timeout(READINESS_PROBE_TIMEOUT, client.readiness()).await {
        Ok(Ok(readiness)) => Ok(Some(readiness)),
        Ok(Err(error)) if error.kind() == DomainErrorKind::DaemonStopped => Ok(None),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(DaemonError::new(
            DomainErrorKind::DaemonStartTimeout,
            "existing local socket did not answer readiness",
        )),
    }
}

#[cfg(any(unix, windows))]
pub(crate) async fn acquire_lifecycle_lock(
    paths: &UserPaths,
    started: Instant,
) -> Result<FileLock, DaemonError> {
    loop {
        if let Some(lock) = FileLock::try_acquire(paths.lifecycle_lock(), paths.uid())
            .map_err(|error| DaemonError::new(DomainErrorKind::PathUnsafe, error.to_string()))?
        {
            return Ok(lock);
        }
        if started.elapsed() >= START_TIMEOUT {
            return Err(DaemonError::new(
                DomainErrorKind::DaemonStartTimeout,
                "timed out waiting for daemon launch singleflight",
            ));
        }
        tokio::time::sleep(LOCK_POLL).await;
    }
}

#[cfg(any(unix, windows))]
fn platform_error(error: voytty_platform::local::LocalPlatformError) -> DaemonError {
    let kind = platform_error_kind(&error);
    DaemonError::new(kind, error.to_string())
}

#[cfg(any(unix, windows))]
fn platform_error_kind(error: &voytty_platform::local::LocalPlatformError) -> DomainErrorKind {
    use voytty_platform::local::LocalPlatformError;
    match error {
        LocalPlatformError::Path(_) | LocalPlatformError::UnsafeSocket(_) => {
            DomainErrorKind::PathUnsafe
        }
        LocalPlatformError::AlreadyRunning => DomainErrorKind::DaemonAlreadyRunning,
        LocalPlatformError::PeerUidMismatch { .. } => DomainErrorKind::PeerUidMismatch,
        LocalPlatformError::UnsupportedPlatform => DomainErrorKind::UnsupportedPlatform,
        LocalPlatformError::Io(_) => DomainErrorKind::DaemonStopped,
    }
}

#[cfg(not(any(unix, windows)))]
fn unsupported() -> DaemonError {
    DaemonError::new(
        DomainErrorKind::UnsupportedPlatform,
        "local daemon lifecycle requires Unix or Windows",
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::spawn_inside_runtime;

    #[test]
    fn daemon_owned_tasks_spawn_inside_their_runtime() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread daemon runtime");
        let task = spawn_inside_runtime(&runtime, || tokio::spawn(async { 17_u8 }));

        assert_eq!(runtime.block_on(task).expect("spawned task joins"), 17);
    }
}

#[cfg(all(test, unix))]
#[path = "lifecycle_recovery_tests.rs"]
mod recovery_tests;
