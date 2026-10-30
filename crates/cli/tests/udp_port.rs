//! Host-only UDP configuration and production daemon startup with isolated state.

#![cfg(any(unix, windows))]

#[path = "../../daemon/tests/support/state_fixture.rs"]
mod state_fixture;

use std::net::{Ipv4Addr, UdpSocket};
use std::time::Duration;

use clap::Parser;
use voytty_cli::{Cli, InteractionMode, execute};
use voytty_core::DomainErrorKind;
use voytty_daemon::bootstrap::bootstrap;
use voytty_daemon::config::{TerminalIdentity, load_config, validate_setup_profile};
use voytty_daemon::lifecycle::{DaemonLauncher, run_daemon};
use voytty_daemon::local_ipc::LocalClient;
use voytty_daemon::network::{NetworkDiagnostic, NetworkObservation};
use voytty_daemon::operations::LocalRuntime;

use state_fixture::TestState;

fn runtime(state: &TestState) -> LocalRuntime {
    LocalRuntime::for_test(
        state.paths.clone(),
        DaemonLauncher::for_test("/does/not/exist".into(), "--must-not-run".into()),
    )
}

async fn command(runtime: &LocalRuntime, value: Option<&str>) -> String {
    let mut args = vec!["voytty", "setup"];
    if let Some(value) = value {
        args.extend(["--udp-port", value]);
    } else {
        args.push("--show");
    }
    let cli = Cli::try_parse_from(args).expect("parse setup command");
    assert_eq!(cli.records_diagnostics(), value.is_some());
    execute(cli, runtime, InteractionMode::NonInteractive)
        .await
        .expect("setup preferences")
        .into_text()
        .expect("text output")
}

#[tokio::test]
async fn config_is_validated_persisted_and_preserved_by_setup_without_starting() {
    let state = TestState::new();
    let runtime = runtime(&state);
    assert!(
        command(&runtime, None)
            .await
            .contains("Run `voytty setup` first")
    );
    let error = execute(
        Cli::try_parse_from(["voytty", "setup", "--udp-port", "42424"]).expect("parse"),
        &runtime,
        InteractionMode::NonInteractive,
    )
    .await
    .expect_err("first setup requires a name");
    assert!(error.to_string().contains("--name"));
    assert!(!state.paths.state_root().exists());
    assert!(!state.paths.runtime_dir().exists());
    let mut requested =
        validate_setup_profile("udp-host", "self-hosted", Some("https://relay.example.com"))
            .expect("setup input");
    requested.terminal_identity = TerminalIdentity::Ghostty;
    let setup = bootstrap(&state.paths, &requested).expect("setup");
    let identity = std::fs::read(state.paths.identity()).expect("identity");
    let database = std::fs::read(state.paths.database()).expect("database");
    let original = std::fs::read(state.paths.config()).expect("config");
    assert!(
        command(&runtime, None)
            .await
            .contains("Saved UDP port:    0 (automatic)\n")
    );
    assert_eq!(
        std::fs::read(state.paths.config()).expect("config bytes"),
        original
    );
    for invalid in ["", "-1", "65536", "1.5", "auto", "42424\n"] {
        assert!(Cli::try_parse_from(["voytty", "setup", "--udp-port", invalid]).is_err());
    }
    assert_eq!(
        std::fs::read(state.paths.config()).expect("config bytes"),
        original
    );
    let output = command(&runtime, Some("42424")).await;
    assert!(output.contains("Saved UDP port:    42424\n"));
    assert!(output.contains("next daemon start"));
    assert!(output.contains("restarting ends existing sessions"));
    let configured = load_config(&state.paths).expect("saved config");
    assert_eq!(configured.udp_port, 42424);
    assert_eq!(configured.terminal_identity, requested.terminal_identity);
    assert_eq!(configured.infrastructure, requested.infrastructure);
    assert_eq!(configured.device_name, requested.device_name);
    assert_eq!(
        std::fs::read(state.paths.identity()).expect("identity bytes"),
        identity
    );
    assert_eq!(
        std::fs::read(state.paths.database()).expect("database bytes"),
        database
    );
    assert!(!state.paths.socket().exists());
    let repeated = bootstrap(&state.paths, &requested).expect("repeat setup");
    assert_eq!(repeated.device_id, setup.device_id);
    assert_eq!(repeated.config, configured);
    assert!(
        command(&runtime, None)
            .await
            .contains("Saved UDP port:    42424\n")
    );
    command(&runtime, Some("65535")).await;
    assert_eq!(
        load_config(&state.paths).expect("saved config").udp_port,
        65535
    );
    command(&runtime, Some("0")).await;
    assert_eq!(load_config(&state.paths).expect("saved config"), requested);
}

#[tokio::test]
async fn occupied_port_recovers_and_saved_changes_wait_for_daemon_restart() {
    let state = TestState::new();
    let requested =
        validate_setup_profile("udp-runtime", "official-n0", None).expect("setup input");
    let setup = bootstrap(&state.paths, &requested).expect("setup");
    let runtime = runtime(&state);
    let occupied = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("reserve UDP port");
    let port = occupied.local_addr().expect("reserved address").port();
    command(&runtime, Some(&port.to_string())).await;
    let paths = state.paths.clone();
    let daemon = std::thread::spawn(move || run_daemon(&paths));
    let client = LocalClient::new(state.paths.socket());
    wait_for_network(&client, "occupied port", |network| {
        !network.endpoint_bound
            && network.bind_attempts >= 2
            && network.diagnostic == Some(NetworkDiagnostic::EndpointBindFailed)
    })
    .await;
    let ready = client
        .readiness()
        .await
        .expect("local readiness despite bind failure");
    assert_eq!(
        client.status().await.expect("daemon status").device_id,
        setup.device_id
    );
    drop(occupied);
    wait_for_network(&client, "released port", |network| network.endpoint_bound).await;
    assert!(UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).is_err());

    let next = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("reserve next UDP port");
    let next_port = next.local_addr().expect("reserved address").port();
    command(&runtime, Some(&next_port.to_string())).await;
    let repeated = runtime
        .setup(&requested, Default::default())
        .await
        .expect("live setup preserves port");
    assert_eq!(repeated.config().udp_port, next_port);
    assert_eq!(client.readiness().await.expect("daemon readiness"), ready);
    assert!(
        client
            .status()
            .await
            .expect("daemon status")
            .network
            .endpoint_bound
    );
    assert!(UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).is_err());
    client.stop(true).await.expect("stop first daemon");
    daemon.join().expect("daemon thread").expect("shutdown");
    let old_port = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).expect("old port released");

    // A fresh production startup reloads the persisted value. Holding its port
    // first proves that startup did not fall back to automatic allocation.
    let paths = state.paths.clone();
    let daemon = std::thread::spawn(move || run_daemon(&paths));
    wait_for_network(&client, "occupied port", |network| {
        network.diagnostic == Some(NetworkDiagnostic::EndpointBindFailed)
    })
    .await;
    drop(next);
    wait_for_network(&client, "released port", |network| network.endpoint_bound).await;
    assert!(UdpSocket::bind((Ipv4Addr::UNSPECIFIED, next_port)).is_err());
    assert_eq!(
        client.status().await.expect("daemon status").device_id,
        setup.device_id
    );
    assert!(
        command(&runtime, None)
            .await
            .contains(&format!("Saved UDP port:    {next_port}\n"))
    );
    client.stop(true).await.expect("stop restarted daemon");
    daemon.join().expect("daemon thread").expect("shutdown");
    drop(old_port);
}

async fn wait_for_network(
    client: &LocalClient,
    phase: &str,
    ready: impl Fn(&NetworkObservation) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match client.status().await {
                Ok(status) if ready(&status.network) => return,
                // The socket can exist before the new daemon starts serving.
                // A pending probe is not readiness; wait for the asserted state
                // within this same deadline, even if one request times out.
                Err(error) => assert!(
                    matches!(
                        error.kind(),
                        DomainErrorKind::DaemonStopped | DomainErrorKind::DeadlineExceeded
                    ),
                    "{phase}: {error}"
                ),
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{phase}: network observation deadline"));
}
