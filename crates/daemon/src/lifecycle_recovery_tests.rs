//! Recovery exercises the actual listener loop with a controlled child owner.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zterm_core::terminal::TerminalSize;
use zterm_core::{
    AttachmentId, DeviceId, DomainErrorKind, OperationId, ResourceLimits, SessionName,
};
use zterm_platform::local_unix::{DaemonLock, bind_owned_daemon_socket};
use zterm_platform::pty::{ExplicitPtyCommand, PtyHost, PtySize};

use super::run_owned_daemon_listener_for_test;
use crate::bootstrap::bootstrap;
use crate::config::{ValidatedInfrastructure, validate_setup_input};
use crate::local_ipc::{LocalClient, LocalIpcLimits};
use crate::service::DaemonService;
use crate::state_fixture::TestState;

fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn shell_path() -> Result<std::path::PathBuf, String> {
    ["/bin/sh", "/usr/bin/sh"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .find(|path| path.is_file())
        .ok_or_else(|| "test shell unavailable".to_owned())
}

#[tokio::test]
async fn fatal_listener_exit_rebinds_actual_daemon_loop_until_owned_child_can_stop()
-> Result<(), String> {
    let state = TestState::new();
    let requested = validate_setup_input(
        "fatal-listener-recovery",
        ValidatedInfrastructure::OfficialN0,
    )
    .map_err(display)?;
    let setup = bootstrap(&state.paths, &requested).map_err(display)?;
    let lock = DaemonLock::try_acquire(&state.paths)
        .map_err(display)?
        .ok_or_else(|| "daemon lock unavailable".to_owned())?;
    let (listener, socket_ownership) =
        bind_owned_daemon_socket(&state.paths, &lock).map_err(display)?;
    let shell = shell_path()?;
    let cwd = state.paths.home().to_path_buf();
    let process_id = Arc::new(Mutex::new(None));
    let sessions = crate::session::SessionService::with_spawner(
        DeviceId::from_array([47; 32]),
        ResourceLimits::default(),
        {
            let process_id = Arc::clone(&process_id);
            move |size, requested| {
                let working_directory = requested.unwrap_or(&cwd).to_path_buf();
                let session = PtyHost::new()
                    .spawn(
                        ExplicitPtyCommand::new(&shell, &working_directory)
                            .arg("-c")
                            .arg("trap '' HUP; printf 'FATAL-OWNER-READY\\r\\n'; while :; do :; done"),
                        PtySize::new(size.rows, size.columns),
                    )
                    .map_err(|error| {
                        crate::error::DaemonError::new(
                            DomainErrorKind::InvalidWorkingDirectory,
                            error.to_string(),
                        )
                    })?;
                *process_id.lock().map_err(|_| {
                    crate::error::DaemonError::new(
                        DomainErrorKind::StoreUnavailable,
                        "fatal-listener fixture process lock poisoned",
                    )
                })? = session.process_id();
                Ok((session, working_directory))
            }
        },
    );
    let principal = sessions.local_principal(AttachmentId::from_array([47; 16]));
    let lease = sessions.issue_operation_lease(principal).map_err(display)?;
    let created = sessions
        .create(
            principal,
            OperationId { lease, sequence: 1 },
            SessionName::new("fatal-owner").map_err(display)?,
            None,
            Some(TerminalSize::new(24, 80)),
        )
        .map_err(display)?;
    // Hold the real child owner until the rebound listener has been observed.
    // A short shutdown budget alone does not keep the asynchronous reaper from
    // finishing before the status request is scheduled under load.
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let cleanup_sessions = sessions.clone();
    let child_owner = std::thread::spawn(move || {
        cleanup_sessions.with_child_cleanup_blocked_for_test(created.session_id, || {
            held_tx.send(()).expect("test awaits child ownership");
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
        })
    });
    tokio::time::timeout(Duration::from_secs(1), held_rx)
        .await
        .map_err(display)?
        .map_err(display)?;

    let service = Arc::new(DaemonService::with_sessions(setup, 47, sessions));
    let daemon_paths = state.paths.clone();
    let daemon_service = Arc::clone(&service);
    let server = std::thread::spawn(move || {
        run_owned_daemon_listener_for_test(
            &daemon_paths,
            lock,
            listener,
            socket_ownership,
            daemon_service,
            LocalIpcLimits::for_test(Duration::from_secs(2))
                .with_fatal_accept_failure_after_for_test(0),
            Duration::from_millis(20),
        )
    });

    let client = LocalClient::new(state.paths.socket());
    let recovery_deadline = Instant::now() + Duration::from_secs(1);
    let status = loop {
        match client.status().await {
            Ok(status) => break status,
            Err(error)
                if matches!(
                    error.kind(),
                    DomainErrorKind::DaemonStopped | DomainErrorKind::Cancelled
                ) =>
            {
                if Instant::now() >= recovery_deadline {
                    return Err("fatal listener never rebound its diagnostic socket".into());
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(error) => return Err(display(error)),
        }
    };
    assert_eq!(status.active_session_count, 1);
    assert_eq!(status.active_session_names, vec!["fatal-owner"]);
    assert!(
        !server.is_finished(),
        "daemon exited while child ownership remained"
    );
    let process_id = process_id
        .lock()
        .map_err(|_| "process lock poisoned".to_owned())?
        .ok_or_else(|| "fixture process ID missing".to_owned())?;
    assert!(
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(i32::try_from(process_id).map_err(display)?),
            None,
        )
        .is_ok(),
        "recovered status must not outlive the retained child"
    );

    drop(release_tx);
    child_owner
        .join()
        .map_err(|_| "child owner thread panicked".to_owned())?
        .map_err(display)?;
    let stopped = client.stop(true).await.map_err(display)?;
    assert!(stopped.stopping);
    tokio::task::spawn_blocking(move || server.join())
        .await
        .map_err(display)?
        .map_err(|_| "owned daemon listener thread panicked".to_owned())?
        .map_err(display)?;
    assert!(
        !state.paths.socket().exists(),
        "successful final stop owns socket removal"
    );
    Ok(())
}
