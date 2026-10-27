//! The mobile endpoint profile recovers the first stale query after host restart.
#[path = "../../../tests/support/endpoint_restart.rs"]
mod support;

#[tokio::test]
async fn controller_profile_first_query_recovers_after_endpoint_restart() {
    support::first_query_recovers(|secret| {
        voytty_client::iroh_controller::endpoint_builder(secret, iroh::dns::DnsResolver::new())
    })
    .await;
}
