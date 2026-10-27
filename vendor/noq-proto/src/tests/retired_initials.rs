use std::sync::Arc;

use bytes::Bytes;

use super::token::FakeTimeSource;
use super::util::{Inbound, Pair, client_config, server_config};
use crate::{DatagramEvent, Duration, FourTuple, VarInt};

fn closed_connection() -> (Pair, Inbound, Arc<FakeTimeSource>) {
    let clock = Arc::new(FakeTimeSource::new());
    let mut config = server_config();
    config.time_source(clock.clone());
    let mut pair = Pair::new(Default::default(), config);
    let client = pair.begin_connect(client_config());
    pair.drive_client();
    let (received_at, initial) = pair.server.inbound.pop_first().expect("client Initial");
    pair.server.inbound.push(received_at, initial.clone());
    pair.drive();
    let server = pair.server.assert_accept();
    pair.finish_connect(client, server);
    let now = pair.time;
    pair.server_conn_mut(server)
        .close(now, VarInt(0), Bytes::new());
    pair.drive();
    assert_eq!(
        pair.server.known_connections(),
        0,
        "original connection drained"
    );
    assert_eq!(pair.client.known_connections(), 0);
    pair.server.connections.clear();
    pair.client.connections.clear();
    (pair, initial, clock)
}

fn deliver(pair: &mut Pair, initial: &Inbound) -> Option<DatagramEvent> {
    let mut response = Vec::new();
    let result = pair.server.endpoint.handle(
        pair.time,
        FourTuple {
            remote: initial.remote,
            local_ip: initial.dst_ip,
        },
        initial.ecn,
        initial.packet.clone(),
        &mut response,
    );
    assert!(response.is_empty(), "duplicate filtering sends no response");
    result
}

#[test]
fn voytty_delayed_initial_after_drain_does_not_create_another_incoming() {
    let (mut pair, initial, _) = closed_connection();
    pair.time += Duration::from_millis(275);
    for _ in 0..32 {
        assert!(
            deliver(&mut pair, &initial).is_none(),
            "late Initial must not consume another incoming slot"
        );
    }
    assert_eq!(pair.server.known_connections(), 0);
    // The same peer can immediately establish a fresh connection.
    pair.connect();
}

#[test]
fn voytty_delayed_initial_on_another_path_after_drain_is_still_a_duplicate() {
    let (mut pair, mut initial, _) = closed_connection();
    pair.time += Duration::from_millis(275);
    // Iroh exposes Direct as the actual IP socket address and Relay as a
    // synthetic IPv6 address. One Initial can therefore arrive on both tuples.
    initial.remote = "[fd15:70a:510b:1::1]:12345".parse().unwrap();
    initial.dst_ip = Some("::1".parse().unwrap());
    for _ in 0..32 {
        assert!(
            deliver(&mut pair, &initial).is_none(),
            "changing the transport path must not resurrect a drained handshake"
        );
    }
    assert_eq!(pair.server.known_connections(), 0);
    pair.connect();
}

#[test]
fn voytty_retired_initial_filter_preserves_other_peers_and_expires() {
    let (mut pair, initial, clock) = closed_connection();
    // Another peer starts its own handshake. Replaying the original bytes at
    // another address is instead a second path, covered by the regression above.
    pair.begin_connect(client_config());
    pair.drive_client();
    let (_, mut other_peer) = pair.server.inbound.pop_first().expect("fresh peer Initial");
    other_peer.remote.set_port(other_peer.remote.port() + 1);
    let Some(DatagramEvent::NewConnection(incoming)) = deliver(&mut pair, &other_peer) else {
        panic!("another peer must not inherit the first peer's retirement");
    };
    pair.server.endpoint.ignore(incoming);
    clock.advance(Duration::from_secs(60));
    pair.time += Duration::from_secs(60);
    let Some(DatagramEvent::NewConnection(incoming)) = deliver(&mut pair, &initial) else {
        panic!("expired retirement must restore normal Initial admission");
    };
    pair.server.endpoint.ignore(incoming);
}
