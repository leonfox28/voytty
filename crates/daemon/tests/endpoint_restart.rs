//! Daemon endpoint profiles keep restart recovery inside the original query budget.
#[path = "../../../tests/support/endpoint_restart.rs"]
mod support;

#[tokio::test]
async fn official_profile_first_query_recovers_after_endpoint_restart() {
    support::first_query_recovers(|secret| {
        zterm_daemon::transport::InfrastructureProfile::OfficialN0.endpoint_builder(secret)
    })
    .await;
}

#[tokio::test]
async fn self_hosted_profile_first_query_recovers_after_endpoint_restart() {
    let profile = zterm_daemon::transport::InfrastructureProfile::SelfHosted {
        relay_url: "https://127.0.0.1:9".parse().expect("inert relay"),
    };
    support::first_query_recovers(|secret| profile.endpoint_builder(secret)).await;
}
