//! Real local socket evidence; Relay and address lookup services are disabled.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

use iroh::{Endpoint, RelayMode, SecretKey, endpoint::presets};
use voytty_daemon::transport::InfrastructureProfile;

fn builder(port: u16) -> iroh::endpoint::Builder {
    InfrastructureProfile::voytty()
        .endpoint_builder_with_udp_port(SecretKey::from_bytes(&[42; 32]), port)
        .relay_mode(RelayMode::Disabled)
        .clear_address_lookup()
}

#[tokio::test]
async fn automatic_allocation_and_fixed_dual_stack_rebinding() {
    let automatic = builder(0).bind().await.expect("automatic bind");
    let sockets = automatic.bound_sockets();
    assert!(sockets.iter().any(SocketAddr::is_ipv4));
    assert!(sockets.iter().any(SocketAddr::is_ipv6));
    assert!(sockets.iter().all(|address| address.port() != 0));
    let port = sockets
        .iter()
        .find(|addr| addr.is_ipv4())
        .expect("IPv4 socket")
        .port();
    automatic.close().await;
    // Iroh releases the OS sockets only after the last Endpoint handle drops.
    drop(automatic);

    for _ in 0..2 {
        let endpoint = builder(port).bind().await.expect("fixed bind and rebind");
        assert_eq!(
            endpoint.bound_sockets(),
            vec![
                SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)),
                SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)),
            ]
        );
        endpoint.close().await;
    }
}

#[tokio::test]
async fn occupied_ipv4_port_fails_instead_of_falling_back() {
    let occupied = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("reserve UDP port");
    let port = occupied.local_addr().expect("reserved address").port();
    let result = builder(port).bind().await;
    assert!(result.is_err(), "fixed IPv4 port must be required");
    drop(occupied);
    let endpoint = builder(port).bind().await.expect("released port binds");
    assert!(
        endpoint
            .bound_sockets()
            .iter()
            .all(|addr| addr.port() == port)
    );
    endpoint.close().await;
}

#[tokio::test]
async fn unavailable_ipv6_binding_keeps_ipv4_without_a_random_ipv6_port() {
    // Iroh binds a v6-only socket on every OS, avoiding platform-specific
    // dual-stack defaults in std::net::UdpSocket.
    let occupied = Endpoint::builder(presets::Minimal)
        .clear_ip_transports()
        .bind_addr((Ipv6Addr::UNSPECIFIED, 0))
        .expect("IPv6 address")
        .bind()
        .await
        .expect("reserve IPv6 UDP port");
    let port = occupied.bound_sockets()[0].port();
    let endpoint = builder(port).bind().await.expect("IPv4 remains available");
    assert_eq!(
        endpoint.bound_sockets(),
        vec![SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))]
    );
    endpoint.close().await;
    occupied.close().await;
}
