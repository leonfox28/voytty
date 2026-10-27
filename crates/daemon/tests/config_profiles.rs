//! Configuration profile acceptance tests.

#[path = "support/state_fixture.rs"]
mod state_fixture;

use voytty_daemon::config::{
    ConfigV1, InfrastructureConfig, TerminalIdentity, ValidatedInfrastructure, load_config,
    validate_setup_input, write_config,
};
use voytty_daemon::transport::InfrastructureProfile;

use state_fixture::TestState;

#[test]
fn official_and_self_hosted_profiles_are_mutually_exclusive_and_validated() {
    let official = ConfigV1 {
        schema_version: 1,
        terminal_identity: Default::default(),
        device_name: "work-mac".to_owned(),
        infrastructure: InfrastructureConfig::OfficialN0,
    }
    .validate()
    .expect("official profile");
    assert_eq!(official.infrastructure.profile_name(), "official-n0");

    let self_hosted = ConfigV1 {
        schema_version: 1,
        terminal_identity: Default::default(),
        device_name: "relay-host".to_owned(),
        infrastructure: InfrastructureConfig::SelfHosted {
            relay_url: "https://relay.example.com".to_owned(),
        },
    }
    .validate()
    .expect("self-hosted profile");
    let summary = InfrastructureProfile::from_validated(&self_hosted.infrastructure).summary();
    assert_eq!(summary.relays.len(), 1);
    assert_eq!(summary.relays[0].url.as_str(), "https://relay.example.com/");
    assert!(!summary.relays[0].quic_address_discovery);

    assert!(
        ConfigV1 {
            schema_version: 1,
            terminal_identity: Default::default(),
            device_name: "bad".to_owned(),
            infrastructure: InfrastructureConfig::SelfHosted {
                relay_url: "http://relay.example.com".to_owned(),
            },
        }
        .validate()
        .is_err()
    );
    assert!(toml::from_str::<ConfigV1>(
        "schema_version=1\ndevice_name='mixed'\n[infrastructure]\nprofile='official-n0'\nrelay_url='https://relay.example.com'\n"
    )
    .is_err());
}

#[test]
fn config_round_trip_uses_one_parser_and_preserves_validated_values() {
    let state = TestState::new();
    state.paths.prepare_state_directories().expect("state dirs");
    let requested = validate_setup_input("work-linux", ValidatedInfrastructure::OfficialN0)
        .expect("setup input");
    write_config(&state.paths, &requested).expect("config writes");
    assert_eq!(load_config(&state.paths).expect("config loads"), requested);
}

#[test]
fn terminal_identity_defaults_for_existing_configs_and_rejects_unknown_values() {
    let legacy = "schema_version=1\ndevice_name='host'\n[infrastructure]\nprofile='official-n0'\n";
    let config = toml::from_str::<ConfigV1>(legacy)
        .expect("legacy config")
        .validate()
        .expect("valid");
    assert_eq!(config.terminal_identity, TerminalIdentity::Voytty);
    for identity in TerminalIdentity::VALUES {
        let text = format!("terminal_identity='{identity}'\n{legacy}");
        let raw = toml::from_str::<ConfigV1>(&text).expect("known identity");
        let configured = raw.validate().expect("valid identity");
        assert_eq!(configured.terminal_identity.as_str(), identity);
        let encoded = toml::to_string(&ConfigV1::from_validated(&configured)).expect("encode");
        assert_eq!(
            toml::from_str::<ConfigV1>(&encoded)
                .expect("decode")
                .validate()
                .expect("validate"),
            configured
        );
    }
    for identity in ["", "kitty", "Ghostty", "ghostty;extra"] {
        assert!(
            toml::from_str::<ConfigV1>(&format!("terminal_identity='{identity}'\n{legacy}"))
                .is_err()
        );
    }
}
