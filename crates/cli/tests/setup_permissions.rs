//! First-setup validation and no-mutation failure acceptance tests.

#[path = "../../daemon/tests/support/state_fixture.rs"]
mod state_fixture;

use clap::Parser;
use voytty_cli::{Cli, CliError, InteractionMode, execute};
use voytty_core::DomainErrorKind;
use voytty_daemon::bootstrap::bootstrap;
use voytty_daemon::config::{TerminalIdentity, load_config, validate_setup_profile};
use voytty_daemon::lifecycle::DaemonLauncher;
use voytty_daemon::operations::LocalRuntime;

use state_fixture::TestState;

#[tokio::test]
async fn noninteractive_missing_values_fail_before_state_or_identity_creation() {
    let state = TestState::new();
    let runtime = LocalRuntime::for_test(
        state.paths.clone(),
        DaemonLauncher::for_test("/does/not/exist".into(), "--unused".to_owned()),
    );
    let command = Cli::try_parse_from(["voytty", "setup"]).expect("setup parses");
    let error = execute(command, &runtime, InteractionMode::NonInteractive)
        .await
        .expect_err("missing setup values fail");
    assert!(matches!(error, CliError::Usage(_)));
    assert!(!state.paths.state_root().exists());
    assert!(!state.paths.identity().exists());
    assert!(!state.paths.database().exists());

    let missing_relay = Cli::try_parse_from([
        "voytty",
        "setup",
        "--name",
        "host",
        "--profile",
        "self-hosted",
    ])
    .expect("partial setup parses");
    assert!(matches!(
        execute(missing_relay, &runtime, InteractionMode::NonInteractive).await,
        Err(CliError::Usage(_))
    ));
    assert!(!state.paths.state_root().exists());
}

#[tokio::test]
async fn show_is_read_only_and_cannot_be_combined_with_updates() {
    let state = TestState::new();
    let runtime = LocalRuntime::for_test(
        state.paths.clone(),
        DaemonLauncher::for_test("/does/not/exist".into(), "--must-not-run".into()),
    );
    let show = Cli::try_parse_from(["voytty", "setup", "--show"]).expect("show parses");
    assert!(!show.records_diagnostics());
    let output = execute(show, &runtime, InteractionMode::NonInteractive)
        .await
        .expect("unconfigured inspection")
        .into_text()
        .expect("text");
    assert!(output.contains("Run `voytty setup` first"));
    assert!(!state.paths.state_root().exists());
    assert!(!state.paths.runtime_dir().exists());
    for (flag, value) in [
        ("--name", "host"),
        ("--profile", "official-n0"),
        ("--relay-url", "https://relay.example.com"),
        ("--terminal-identity", "ghostty"),
        ("--udp-port", "42424"),
    ] {
        assert!(Cli::try_parse_from(["voytty", "setup", "--show", flag, value]).is_err());
    }
}

#[tokio::test]
async fn existing_setup_updates_only_explicit_preferences_and_rejects_conflicts_atomically() {
    let state = TestState::new();
    let runtime = LocalRuntime::for_test(
        state.paths.clone(),
        DaemonLauncher::for_test("/does/not/exist".into(), "--must-not-run".into()),
    );
    let requested = validate_setup_profile(
        "configured",
        "self-hosted",
        Some("https://relay.example.com"),
    )
    .expect("profile");
    bootstrap(&state.paths, &requested).expect("bootstrap");
    let identity = std::fs::read(state.paths.identity()).expect("identity");
    let database = std::fs::read(state.paths.database()).expect("database");
    let original = std::fs::read(state.paths.config()).expect("config");
    let execute_args = |args: Vec<&str>| {
        let cli = Cli::try_parse_from(args).expect("parse setup");
        execute(cli, &runtime, InteractionMode::NonInteractive)
    };
    let show = execute_args(vec!["voytty", "setup", "--show"])
        .await
        .expect("show")
        .into_text()
        .expect("text");
    assert_eq!(
        execute_args(vec!["voytty", "setup"])
            .await
            .expect("repeat setup")
            .into_text()
            .expect("text"),
        show
    );
    assert!(show.contains("Infrastructure:    self-hosted"));
    assert!(!show.contains("relay.example.com"));
    assert_eq!(
        std::fs::read(state.paths.config()).expect("config"),
        original
    );

    execute_args(vec![
        "voytty",
        "setup",
        "--terminal-identity",
        "ghostty",
        "--udp-port",
        "42424",
    ])
    .await
    .expect("combined update");
    let configured = load_config(&state.paths).expect("configured");
    assert_eq!(configured.terminal_identity, TerminalIdentity::Ghostty);
    assert_eq!(configured.udp_port, 42424);
    assert_eq!(configured.infrastructure, requested.infrastructure);
    execute_args(vec!["voytty", "setup", "--udp-port", "0"])
        .await
        .expect("restore automatic");
    let configured = load_config(&state.paths).expect("configured");
    assert_eq!(configured.terminal_identity, TerminalIdentity::Ghostty);
    assert_eq!(configured.udp_port, 0);
    let before_conflicts = std::fs::read(state.paths.config()).expect("config");
    for args in [
        vec![
            "voytty",
            "setup",
            "--name",
            "different",
            "--udp-port",
            "42424",
        ],
        vec![
            "voytty",
            "setup",
            "--profile",
            "official-n0",
            "--terminal-identity",
            "voytty",
        ],
        vec![
            "voytty",
            "setup",
            "--relay-url",
            "https://different.example.com",
            "--udp-port",
            "42424",
        ],
    ] {
        let error = execute_args(args)
            .await
            .expect_err("immutable setup conflict");
        assert!(
            matches!(error, CliError::Daemon(ref error) if error.kind() == DomainErrorKind::AlreadyConfiguredConflict)
        );
        assert_eq!(
            std::fs::read(state.paths.config()).expect("config"),
            before_conflicts
        );
    }
    assert_eq!(
        std::fs::read(state.paths.identity()).expect("identity"),
        identity
    );
    assert_eq!(
        std::fs::read(state.paths.database()).expect("database"),
        database
    );
    assert!(!state.paths.socket().exists());
}

#[tokio::test]
async fn concurrent_setup_updates_preserve_other_preferences() {
    let state = TestState::new();
    let runtime = LocalRuntime::for_test(
        state.paths.clone(),
        DaemonLauncher::for_test("/does/not/exist".into(), "--must-not-run".into()),
    );
    let requested = validate_setup_profile("concurrent", "official-n0", None).expect("profile");
    bootstrap(&state.paths, &requested).expect("bootstrap");
    let (identity, port) = tokio::join!(
        execute(
            Cli::try_parse_from(["voytty", "setup", "--terminal-identity", "ghostty"])
                .expect("parse"),
            &runtime,
            InteractionMode::NonInteractive
        ),
        execute(
            Cli::try_parse_from(["voytty", "setup", "--udp-port", "42424"]).expect("parse"),
            &runtime,
            InteractionMode::NonInteractive
        ),
    );
    identity.expect("identity update");
    port.expect("port update");
    let saved = load_config(&state.paths).expect("config");
    assert_eq!(saved.terminal_identity, TerminalIdentity::Ghostty);
    assert_eq!(saved.udp_port, 42424);
    assert!(!state.paths.socket().exists());
}

#[cfg(unix)]
#[tokio::test]
async fn existing_setup_updates_do_not_contact_an_unresponsive_daemon_socket() {
    let state = TestState::new();
    let requested = validate_setup_profile("busy-daemon", "official-n0", None).expect("profile");
    bootstrap(&state.paths, &requested).expect("bootstrap");
    state
        .paths
        .prepare_runtime_directory()
        .expect("runtime directory");
    let listener = std::os::unix::net::UnixListener::bind(state.paths.socket()).expect("socket");
    listener.set_nonblocking(true).expect("nonblocking");
    let runtime = LocalRuntime::for_test(
        state.paths.clone(),
        DaemonLauncher::for_test("/does/not/exist".into(), "--must-not-run".into()),
    );
    execute(
        Cli::try_parse_from([
            "voytty",
            "setup",
            "--terminal-identity",
            "ghostty",
            "--udp-port",
            "42424",
        ])
        .expect("parse"),
        &runtime,
        InteractionMode::NonInteractive,
    )
    .await
    .expect("preferences do not depend on daemon responsiveness");
    let saved = load_config(&state.paths).expect("config");
    assert_eq!(saved.terminal_identity, TerminalIdentity::Ghostty);
    assert_eq!(saved.udp_port, 42424);
    assert_eq!(
        listener.accept().expect_err("no IPC connection").kind(),
        std::io::ErrorKind::WouldBlock
    );
}
