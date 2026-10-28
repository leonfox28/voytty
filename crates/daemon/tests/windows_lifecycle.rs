//! Detached Windows daemon and real PowerShell session acceptance, in private state.
#[cfg(windows)]
mod windows {
    use futures_util::FutureExt;
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};
    use voytty_core::terminal::{TerminalSize, TerminalSurfaceRow};
    use voytty_core::{SessionName, SessionSelector};
    use voytty_daemon::bootstrap::bootstrap;
    use voytty_daemon::config::{ValidatedInfrastructure, validate_setup_input};
    use voytty_daemon::lifecycle::{DaemonLauncher, run_local_only_daemon_for_test};
    use voytty_daemon::local_ipc::{LocalAttachmentClient, LocalAttachmentEvent, LocalClient};
    use voytty_platform::user_state::{ExistingLockState, UserPaths, inspect_existing_lock};
    const PREFIX: &str = "--windows-daemon-fixture=";
    const HANDOFF: &str = "--windows-handoff-fixture";
    fn paths(root: &Path) -> UserPaths {
        UserPaths::for_test(0, root.to_owned(), root.join("state"), root.join("run"))
    }
    fn text(rows: &[TerminalSurfaceRow]) -> String {
        rows.iter()
            .flat_map(|row| row.cells.iter().map(|cell| cell.contents.as_str()))
            .collect()
    }
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::args().nth(1).as_deref() == Some(HANDOFF) {
            let mut channel = voytty_platform::local::take_isolated_channel(0)?;
            channel.set_read_timeout(Some(Duration::from_secs(5)))?;
            channel.set_write_timeout(Some(Duration::from_secs(5)))?;
            let mut message = [0; 4];
            channel.read_exact(&mut message)?;
            assert_eq!(&message, b"ping");
            channel.write_all(b"pong")?;
            return Ok(());
        }
        if let Some(root) = std::env::args()
            .nth(1)
            .and_then(|arg| arg.strip_prefix(PREFIX).map(PathBuf::from))
        {
            run_local_only_daemon_for_test(&paths(&root))?;
            return Ok(());
        }
        println!("Windows lifecycle: isolated child handoff");
        let temporary = tempfile::Builder::new()
            .prefix("voytty Windows ")
            .tempdir()?;
        let paths = paths(temporary.path());
        let (mut helper, mut channel) = voytty_platform::local::spawn_isolated_command(
            &std::env::current_exe()?,
            temporary.path(),
            HANDOFF,
        )?;
        channel.set_read_timeout(Some(Duration::from_secs(5)))?;
        channel.set_write_timeout(Some(Duration::from_secs(5)))?;
        let exchange = (|| -> std::io::Result<[u8; 4]> {
            channel.write_all(b"ping")?;
            let mut reply = [0; 4];
            channel.read_exact(&mut reply)?;
            Ok(reply)
        })();
        if exchange.is_err() {
            let _ = helper.kill();
        }
        let status = helper.wait()?;
        assert_eq!(&exchange?, b"pong");
        assert!(status.success(), "isolated handoff child succeeded");
        println!("Windows lifecycle: bootstrap and detached daemon");
        let setup = validate_setup_input("windows-fixture", ValidatedInfrastructure::OfficialN0)?;
        let first = bootstrap(&paths, &setup)?;
        let second = bootstrap(&paths, &setup)?;
        assert_eq!(first.device_id, second.device_id, "setup is idempotent");
        let launcher = DaemonLauncher::for_test(
            std::env::current_exe()?,
            format!("{PREFIX}{}", temporary.path().display()),
        );
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(async {
            let client = LocalClient::new(paths.socket());
            let result = std::panic::AssertUnwindSafe(exercise(&paths, &launcher, &client))
                .catch_unwind()
                .await;
            let stopped = client.stop(true).await;
            if stopped.is_ok() {
                wait_stopped(&paths).await?;
            }
            match result {
                Ok(Ok(())) => stopped.map_err(Into::into),
                Ok(Err(error)) => Err(error),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        })?;
        println!("WINDOWS_DAEMON_LIFECYCLE=PASS");
        Ok(())
    }
    async fn exercise(
        paths: &UserPaths,
        launcher: &DaemonLauncher,
        client: &LocalClient,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (a, b) = tokio::join!(launcher.ensure(paths), launcher.ensure(paths));
        assert_eq!(
            a?.started_at_unix, b?.started_at_unix,
            "one detached instance"
        );
        assert_eq!(
            inspect_existing_lock(paths.daemon_lock(), 0)?,
            ExistingLockState::Locked
        );
        println!("Windows lifecycle: create and attach PowerShell session");
        let created = client
            .create_session(
                &SessionName::new("native")?,
                Some(paths.home()),
                Some(TerminalSize::new(24, 80)),
            )
            .await?;
        let mut view = LocalAttachmentClient::connect_session(
            paths.socket(),
            SessionSelector::Id(created.session_id),
            false,
            None,
        )
        .await?;
        let initial = view
            .take_initial_snapshot()
            .ok_or("missing initial snapshot")?;
        view.snapshot_applied(initial.revision).await?;
        view.resize(TerminalSize::new(32, 100)).await?;
        // Construct the marker at runtime so input echo cannot satisfy output acceptance.
        view.write_input(b"[Console]::WriteLine(('NATIVE-' + 'POWERSHELL-OK'))\r".to_vec())
            .await?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let event = view
                .read_event(deadline.saturating_duration_since(Instant::now()))
                .await?;
            let contents = match event {
                LocalAttachmentEvent::Snapshot(snapshot) => {
                    view.snapshot_applied(snapshot.revision).await?;
                    text(&snapshot.surface.rows)
                }
                LocalAttachmentEvent::Delta(delta) => delta
                    .row_patches
                    .iter()
                    .map(|patch| text(std::slice::from_ref(&patch.replacement)))
                    .collect::<String>(),
                LocalAttachmentEvent::SyncRequired(_) => continue,
                LocalAttachmentEvent::LeaseLost(_) | LocalAttachmentEvent::SessionEnded(_) => {
                    return Err("session ended before output".into());
                }
                _ => String::new(),
            };
            if contents.contains("NATIVE-POWERSHELL-OK") {
                break;
            }
            if Instant::now() >= deadline {
                return Err("PowerShell output deadline".into());
            }
        }
        view.detach().await?;
        println!("Windows lifecycle: reattach retained session");
        assert_eq!(
            client.list_sessions().await?.len(),
            1,
            "detach preserves the shell"
        );
        let mut attached = LocalAttachmentClient::connect_session(
            paths.socket(),
            SessionSelector::Id(created.session_id),
            false,
            None,
        )
        .await?;
        let restored = attached
            .take_initial_snapshot()
            .ok_or("reattach snapshot")?;
        assert!(text(&restored.surface.rows).contains("NATIVE-POWERSHELL-OK"));
        attached.snapshot_applied(restored.revision).await?;
        attached.detach().await?;
        client.close_session(created.session_id).await?;
        assert!(client.list_sessions().await?.is_empty());
        client.stop(true).await?;
        wait_stopped(paths).await?;
        println!("Windows lifecycle: restart empty daemon");
        launcher.ensure(paths).await?;
        assert!(
            client.list_sessions().await?.is_empty(),
            "daemon restart starts with no sessions"
        );
        Ok(())
    }

    async fn wait_stopped(paths: &UserPaths) -> Result<(), Box<dyn std::error::Error>> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while inspect_existing_lock(paths.daemon_lock(), 0)? == ExistingLockState::Locked {
            if Instant::now() >= deadline {
                return Err("daemon failed to release its lock".into());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    }
}
fn main() {
    #[cfg(windows)]
    if let Err(error) = windows::run() {
        eprintln!("Windows daemon acceptance failed: {error}");
        std::process::exit(1);
    }
}
