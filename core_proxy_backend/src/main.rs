//! NatForge - Core Proxy Backend (data plane).
//!
//! Owns the high-throughput relay: the agent control plane (yamux over TCP), the
//! shared HTTP :80 and HTTPS :443 subdomain routers, and dedicated TCP ports per
//! raw route. Also exposes a small internal API and periodically refreshes policy.

pub mod api;
pub mod config;
pub mod ddos;
pub mod dns;
pub mod geo;
pub mod jwt;
pub mod reporter;
pub mod state;
pub mod tls;
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
    if config.jwt_secret == "natforge-dev-secret-change-me"
        || config.internal_secret == "natforge-internal-dev-secret"
    {
        tracing::warn!(
            "SECURITY: using built-in DEV secrets - set JWT_SECRET and INTERNAL_SECRET to strong random values before any non-local deployment (tunnel tokens are forgeable otherwise)."
        );
    }
    info!("NatForge Core Proxy starting (node '{}')", config.node_id);
    info!(
        "public host '{}', shared http :{} / https :{}, control :{}",
        config.public_host, config.http_port, config.https_port, config.control_port
    );

    let state = CoreState::connect(config.clone()).await?;

    // Announce this node to the control plane (upserts the node row + seeds its
    // TCP port pool) before serving traffic, retrying until the website accepts it
    // so the very first tunnel reservation can already see this region. Re-announced
    // on the policy tick so a later website restart relearns us.
    for attempt in 1..=30u32 {
        if reporter::node_register(&state).await {
            info!(
                "registered node '{}' with the control plane",
                config.node_id
            );
            break;
        }
        if attempt == 1 {
            info!("waiting for the control plane to accept node registration…");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // Initial policy pull + periodic refresh (also re-registers this node).
    reporter::refresh_policy(&state).await;
    {
        let st = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(30));
            loop {
                ticker.tick().await;
                reporter::node_register(&st).await;
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

    info!("userspace connection-rate DDoS filter active");

    let app = api::core_routes(state.clone());
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], config.internal_api_port));
    info!("core internal API listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve(listener, app).await?;
    Ok(())
}
