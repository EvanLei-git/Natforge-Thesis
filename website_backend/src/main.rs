//! NatForge — Website Backend (control plane).
//!
//! User authentication (Argon2 + JWT), the RFC 8628 device flow, multi-route
//! tunnel reservation, multi-region node registry, the admin panel, the internal
//! API the core proxy reports to, and serving of the static frontend.
//! Durable state in PostgreSQL, ephemeral state in Redis.

pub mod config;
pub mod db;
pub mod geo;
pub mod handlers;
pub mod jwt;
pub mod models;
pub mod routes;

use std::time::Duration;

use axum::response::Redirect;
use axum::routing::get;
use axum::serve;
use axum::Router;
use axum::http::header::CACHE_CONTROL;
use axum::http::HeaderValue;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::db::connection::AppState;

/// Tunnels silent longer than this are reclaimed (ports freed, row deleted).
const RECONCILE_GRACE_SECS: i64 = 3600;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();

    let config = Config::from_env();
    if config.jwt_secret == "natforge-dev-secret-change-me" || config.internal_secret == "natforge-internal-dev-secret" {
        tracing::warn!("SECURITY: using built-in DEV secrets — set JWT_SECRET and INTERNAL_SECRET to strong random values before any non-local deployment (tokens are forgeable otherwise).");
    }
    tracing::info!("connecting to PostgreSQL + Redis…");
    let state = AppState::connect(config.clone()).await?;
    tracing::info!("database ready; migrations applied; awaiting node self-registration");

    // Periodic reconciliation: reclaim ports from abandoned tunnels.
    {
        let st = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            loop {
                ticker.tick().await;
                match crate::db::queries::reconcile_abandoned(&st.db.pg, RECONCILE_GRACE_SECS).await {
                    Ok(n) if n > 0 => tracing::info!("reconciliation reclaimed {n} abandoned tunnel(s)"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("reconciliation error: {e}"),
                }
            }
        });
    }

    let api = routes::api_router(state.clone());
    let serve_dir = ServeDir::new(&config.frontend_dir);
    let app = Router::new()
        .route("/", get(|| async { Redirect::to("/views/index.html") }))
        .route("/device", get(|| async { Redirect::to("/views/index.html") }))
        .merge(api)
        .fallback_service(serve_dir)
        // Always revalidate static assets so a redesign never gets stuck behind a
        // stale browser cache (no rebuild is needed for frontend changes either).
        .layer(SetResponseHeaderLayer::overriding(CACHE_CONTROL, HeaderValue::from_static("no-cache")))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], config.port));
    tracing::info!("NatForge website backend listening on http://{addr}");
    tracing::info!("serving frontend from '{}'", config.frontend_dir);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve(listener, app).await?;
    Ok(())
}
