//! NatForge — Core Proxy Backend (data plane).
//!
//! Owns the high-throughput relay: the agent control plane (yamux over TCP), the
//! shared HTTP :80 and HTTPS :443 subdomain routers, and dedicated TCP ports per
//! raw route. Also exposes a small internal API and periodically refreshes policy.

pub mod api;
pub mod config;
pub mod ddos;
pub mod dns;
pub mod jwt;
pub mod reporter;
pub mod state;
pub mod tunnel;

use std::time::Duration;

use axum::serve;
use tracing::{error, info};

use crate::config::Config;
use crate::state::CoreState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();

    let config = Config::from_env();
    info!("NatForge Core Proxy starting (node '{}')", config.node_id);
    info!(
        "public host '{}', shared http :{} / https :{}, control :{}",
        config.public_host, config.http_port, config.https_port, config.control_port
    );

    let state = CoreState::connect(config.clone()).await?;

    // Initial policy pull + periodic refresh.
    reporter::refresh_policy(&state).await;
    {
        let st = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(30));
            loop {
                ticker.tick().await;
                reporter::refresh_policy(&st).await;
            }
        });
    }

    // Agent control plane.
    {
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = tunnel::run_control_plane(st).await {
                error!("control plane stopped: {e}");
            }
        });
    }
    // Shared HTTP subdomain router.
    {
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = tunnel::shared::run_http(st).await {
                error!("http router stopped: {e}");
            }
        });
    }
    // Shared HTTPS (SNI passthrough) router.
    {
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = tunnel::shared::run_https(st).await {
                error!("https router stopped: {e}");
            }
        });
    }

    info!("initialising simulated eBPF volumetric DDoS heuristics");

    let app = api::routes::core_routes(state.clone());
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], config.internal_api_port));
    info!("core internal API listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve(listener, app).await?;
    Ok(())
}
