//! NatForge - Core Proxy Backend (data plane).
//!
//! Owns the high-throughput relay: the agent control plane (yamux over TCP), the
//! shared HTTP :80 and HTTPS :443 subdomain routers, and dedicated TCP ports per
//! raw route. Also exposes a small internal API and periodically refreshes policy.

pub mod acme;
pub mod api;
pub mod config;
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
    // Install a process-default rustls crypto provider (ring) for libraries that
    // expect one, e.g. the ACME client's HTTPS calls to the CA.
    let _ = rustls::crypto::ring::default_provider().install_default();

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

    // Hot-reload the wildcard TLS certificate on renewal (certbot rewrites the PEM
    // roughly every 60 days) so a renewal never needs a restart or drops tunnels.
    if let (Some(cert), Some(key)) = (
        config.wildcard_cert_path.clone(),
        config.wildcard_key_path.clone(),
    ) {
        let st = state.clone();
        tokio::spawn(async move {
            let mtime = || std::fs::metadata(&cert).and_then(|m| m.modified()).ok();
            let mut last = mtime();
            let mut ticker = tokio::time::interval(Duration::from_secs(3600));
            loop {
                ticker.tick().await;
                let now = mtime();
                if now != last {
                    match crate::tls::load_wildcard_acceptor(&cert, &key) {
                        Ok(acceptor) => {
                            st.set_wildcard_acceptor(Some(acceptor)).await;
                            last = now;
                            info!("wildcard TLS certificate reloaded");
                        }
                        Err(e) => tracing::warn!("wildcard TLS reload failed: {e}"),
                    }
                }
            }
        });
    }

    // Hot-reload the GeoIP database if it is refreshed on disk (e.g. by a cron
    // running scripts/update-geoip.sh), so updates apply without a restart.
    if !config.geoip_db.trim().is_empty() {
        let st = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(3600));
            loop {
                ticker.tick().await;
                st.geo.reload_if_changed();
            }
        });
    }

    // Periodically renew ACME certs for custom domains (best-effort).
    if config.acme_enabled {
        let st = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(6 * 3600));
            loop {
                ticker.tick().await;
                st.acme.renew_due().await;
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

    let app = api::core_routes(state.clone());
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], config.internal_api_port));
    info!("core internal API listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve(listener, app).await?;
    Ok(())
}
