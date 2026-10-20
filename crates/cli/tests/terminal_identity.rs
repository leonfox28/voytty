//! CLI configuration through the host's real login-shell and notification path.

#![cfg(unix)]

#[path = "../../daemon/tests/support/state_fixture.rs"]
mod state_fixture;

use std::time::{Duration, Instant};

use clap::Parser;
use zterm_cli::{Cli, InteractionMode, execute};
use zterm_core::{DomainErrorKind, SessionName, SessionSelector};
use zterm_daemon::bootstrap::bootstrap;
use zterm_daemon::config::{TerminalIdentity, load_config, validate_setup_profile};
use zterm_daemon::lifecycle::{DaemonLauncher, run_local_only_daemon_for_test};
use zterm_daemon::local_ipc::{LocalAttachmentClient, LocalAttachmentEvent, LocalClient};
use zterm_daemon::operations::LocalRuntime;

use state_fixture::TestState;

const DEADLINE: Duration = Duration::from_secs(10);

fn runtime(state: &TestState) -> LocalRuntime {
    LocalRuntime::for_test(
        state.paths.clone(),
        DaemonLauncher::for_test("/does/not/exist".into(), "--must-not-run".into()),
    )
}

async fn command(runtime: &LocalRuntime, value: Option<&str>) -> String {
    let mut args = vec!["zterm", "config", "terminal-identity"];
    args.extend(value);
    let cli = Cli::try_parse_from(args).expect("parse config command");
    assert_eq!(cli.records_diagnostics(), value.is_some());
    execute(cli, runtime, InteractionMode::NonInteractive)
        .await
        .expect("configuration command")
        .into_text()
        .expect("text output")
}

#[tokio::test]
async fn configuration_is_validated_persisted_and_never_starts_a_daemon() {
    let state = TestState::new();
    let runtime = runtime(&state);
    for args in [
        vec!["zterm", "config", "terminal-identity"],
        vec!["zterm", "config", "terminal-identity", "ghostty"],
    ] {
        let error = execute(
            Cli::try_parse_from(args).expect("parse"),
            &runtime,
            InteractionMode::NonInteractive,
        )
        .await
        .expect_err("requires setup");
        assert!(error.to_string().contains("run `zterm setup`"));
    }
    assert!(!state.paths.state_root().exists());
    assert!(!state.paths.runtime_dir().exists());
    for invalid in ["", "kitty", "Ghostty", "ghostty\n"] {
        assert!(Cli::try_parse_from(["zterm", "config", "terminal-identity", invalid]).is_err());
    }
    let requested = validate_setup_profile(
        "identity-host",
        "self-hosted",
        Some("https://relay.example.com"),
    )
    .expect("setup input");
    let setup = bootstrap(&state.paths, &requested).expect("setup");
    let key_before = std::fs::read(state.paths.identity()).expect("identity");
    let database_before = std::fs::read(state.paths.database()).expect("database");
    let config_before = std::fs::read(state.paths.config()).expect("config");
    assert_eq!(command(&runtime, None).await, "Terminal identity: zterm\n");
    assert_eq!(
        std::fs::read(state.paths.config()).expect("config"),
        config_before
    );
    let output = command(&runtime, Some("ghostty")).await;
    assert!(output.contains("Terminal identity: ghostty"));
    assert!(output.contains("new sessions on this host"));
    let configured = load_config(&state.paths).expect("persisted config");
    assert_eq!(configured.terminal_identity, TerminalIdentity::Ghostty);
    assert_eq!(configured.device_name, requested.device_name);
    assert_eq!(configured.infrastructure, requested.infrastructure);
    assert_eq!(
        std::fs::read(state.paths.identity()).expect("identity"),
        key_before
    );
    assert_eq!(
        std::fs::read(state.paths.database()).expect("database"),
        database_before
    );
    assert!(!state.paths.socket().exists());
    let repeated = bootstrap(&state.paths, &requested).expect("repeat setup preserves preference");
    assert_eq!(repeated.device_id, setup.device_id);
    assert_eq!(repeated.config, configured);
    assert_eq!(
        command(&runtime, None).await,
        "Terminal identity: ghostty\n"
    );
    command(&runtime, Some("zterm")).await;
    assert_eq!(
        load_config(&state.paths).expect("restored config"),
        requested
    );
    let before = std::fs::read(state.paths.config()).expect("config before invalid request");
    assert_eq!(
        runtime
            .terminal_identity(Some("invalid"))
            .await
            .expect_err("runtime validates too")
            .kind(),
        DomainErrorKind::ConfigProfile
    );
    assert_eq!(
        std::fs::read(state.paths.config()).expect("config after invalid request"),
        before
    );
}

#[tokio::test]
async fn running_host_applies_identity_to_new_shells_and_delivers_their_notifications() {
    let state = TestState::new();
    let requested =
        validate_setup_profile("live-identity-host", "official-n0", None).expect("input");
    bootstrap(&state.paths, &requested).expect("setup");
    let runtime = runtime(&state);
    command(&runtime, Some("ghostty")).await;
    let paths = state.paths.clone();
    let daemon = std::thread::spawn(move || run_local_only_daemon_for_test(&paths));
    let client = LocalClient::new(state.paths.socket());
    let deadline = Instant::now() + DEADLINE;
    let started = loop {
        match client.readiness().await {
            Ok(ready) => break ready,
            Err(error)
                if error.kind() == DomainErrorKind::DaemonStopped && Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => panic!("daemon readiness: {error}"),
        }
    };
    command(&runtime, Some("zterm")).await;
    let mut native = LocalAttachmentClient::connect_main(state.paths.socket(), None)
        .await
        .expect("native session");
    synchronize(&mut native).await;
    assert_identity_notification(&mut native, "zterm").await;
    command(&runtime, Some("ghostty")).await;
    let repeated = runtime
        .setup(&requested)
        .await
        .expect("live setup preserves preference");
    assert_eq!(repeated.config.terminal_identity, TerminalIdentity::Ghostty);
    assert_identity_notification(&mut native, "zterm").await;

    let created = client
        .create_session(&SessionName::new("compatible").expect("name"), None, None)
        .await
        .expect("new session");
    let mut compatible = LocalAttachmentClient::connect_session(
        state.paths.socket(),
        SessionSelector::Id(created.session_id),
        false,
        None,
    )
    .await
    .expect("compatible session");
    synchronize(&mut compatible).await;
    assert_identity_notification(&mut compatible, "ghostty").await;
    command(&runtime, Some("zterm")).await;
    assert_identity_notification(&mut compatible, "ghostty").await;
    let restored = client
        .create_session(&SessionName::new("restored").expect("name"), None, None)
        .await
        .expect("restored session");
    let mut restored = LocalAttachmentClient::connect_session(
        state.paths.socket(),
        SessionSelector::Id(restored.session_id),
        false,
        None,
    )
    .await
    .expect("restored attachment");
    synchronize(&mut restored).await;
    assert_identity_notification(&mut restored, "zterm").await;
    assert_eq!(client.readiness().await.expect("same daemon"), started);
    assert_eq!(
        client
            .list_sessions()
            .await
            .expect("all sessions survived")
            .len(),
        3
    );
    client.stop(true).await.expect("stop disposable daemon");
    drop((native, compatible, restored));
    daemon
        .join()
        .expect("daemon thread")
        .expect("daemon shutdown");
}

async fn synchronize(client: &mut LocalAttachmentClient) {
    let mut revision = client.take_initial_snapshot().expect("snapshot").revision;
    let deadline = Instant::now() + DEADLINE;
    loop {
        assert!(
            Instant::now() < deadline,
            "snapshot synchronization deadline"
        );
        client
            .snapshot_applied(revision)
            .await
            .expect("acknowledge");
        match client.read_event(Duration::from_millis(100)).await {
            Ok(LocalAttachmentEvent::SyncRequired(_)) => {
                let LocalAttachmentEvent::Snapshot(snapshot) = client
                    .read_event(deadline.saturating_duration_since(Instant::now()))
                    .await
                    .expect("replacement")
                else {
                    panic!("expected replacement snapshot");
                };
                revision = snapshot.revision;
            }
            Ok(LocalAttachmentEvent::Delta(_)) => return,
            Err(error) if error.kind() == DomainErrorKind::DeadlineExceeded => return,
            event => panic!("unexpected synchronization event: {event:?}"),
        }
    }
}

async fn assert_identity_notification(client: &mut LocalAttachmentClient, expected: &str) {
    client.write_input(b"printf '\\033]9;%s|%s|%s|%s\\007' \"$TERM_PROGRAM\" \"$TERM\" \"$COLORTERM\" \"$TERM_PROGRAM_VERSION\"\n".to_vec()).await.expect("probe input");
    let deadline = Instant::now() + DEADLINE;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "notification deadline");
        match client
            .read_event(remaining)
            .await
            .expect("notification event")
        {
            LocalAttachmentEvent::Notification(notification) => {
                assert_eq!(
                    notification.body(),
                    format!(
                        "{expected}|xterm-256color|truecolor|{}",
                        env!("CARGO_PKG_VERSION")
                    )
                );
                return;
            }
            LocalAttachmentEvent::Delta(_) => {}
            event => panic!("unexpected environment probe event: {event:?}"),
        }
    }
}
