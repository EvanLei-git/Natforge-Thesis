//! NatForge - Website Backend (control plane).
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
pub mod metrics;
pub mod models;
pub mod routes;

use std::time::Duration;

use axum::Router;
use axum::http::HeaderValue;
use axum::http::header::CACHE_CONTROL;
use axum::http::{StatusCode, Uri};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::serve;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::db::connection::AppState;

/// Tunnels silent longer than this are reclaimed (ports freed, row deleted).
const RECONCILE_GRACE_SECS: i64 = 3600;

/// A device is flipped offline once its agent has stopped polling for this long.
const DEVICE_OFFLINE_GRACE_SECS: i64 = 45;

/// A device service host that carries no traffic for this long has its dedicated public
/// ports reclaimed (the service-host row + its name are kept). 31 days.
const SERVICE_HOST_IDLE_SECS: i64 = 31 * 24 * 3600;

/// Connection-log rows kept per tunnel; older rows are pruned by the periodic sweep.
const CONN_LOG_KEEP_PER_TUNNEL: i64 = 2000;

/// Serve a clean, extensionless page URL from the views directory: "/" maps to the
/// landing page and "/<name>" to `views/<name>.html`. The name is restricted to
/// `[A-Za-z0-9_-]`, so a request can never traverse out of the views directory.
async fn serve_page(views_dir: &str, uri: &Uri) -> Response {
    let raw = uri.path().trim_matches('/');
    // The admin section lives under /admin/* but the views are flat files; map the
    // nested URLs onto them (the plain /admin, /users, /profile still work too).
    let name = match raw {
        "" => "landing",
        "admin/network" => "admin",
        "admin/users" => "users",
        "admin/tunnels" => "tunnels",
        "admin/profile" => "profile",
        other => other,
    };
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read_to_string(format!("{views_dir}/{name}.html")).await {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();

    let config = Config::from_env();
    if config.jwt_secret == "natforge-dev-secret-change-me"
        || config.internal_secret == "natforge-internal-dev-secret"
    {
        tracing::warn!(
            "SECURITY: using built-in DEV secrets - set JWT_SECRET and INTERNAL_SECRET to strong random values before any non-local deployment (tokens are forgeable otherwise)."
        );
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
                match crate::db::queries::reconcile_abandoned(&st.db.pg, RECONCILE_GRACE_SECS).await
                {
                    Ok(n) if n > 0 => {
                        tracing::info!("reconciliation reclaimed {n} abandoned tunnel(s)")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("reconciliation error: {e}"),
                }
                if let Err(e) = crate::db::queries::mark_stale_devices_offline(
                    &st.db.pg,
                    DEVICE_OFFLINE_GRACE_SECS,
                )
                .await
                {
                    tracing::warn!("device liveness sweep error: {e}");
                }
                match crate::db::queries::expire_idle_service_hosts(
                    &st.db.pg,
                    SERVICE_HOST_IDLE_SECS,
                )
                .await
                {
                    Ok(n) if n > 0 => {
                        tracing::info!("idle sweep reclaimed ports of {n} service host(s)")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("idle service-host sweep error: {e}"),
                }
                if let Err(e) =
                    crate::db::queries::prune_conn_logs(&st.db.pg, CONN_LOG_KEEP_PER_TUNNEL).await
                {
                    tracing::warn!("connection-log prune error: {e}");
                }
            }
        });
    }

    // Prometheus metrics on a localhost-only port, scraped by Prometheus (see monitoring/).
    {
        let st = state.clone();
        tokio::spawn(async move {
            let metrics_app = Router::new()
                .route("/metrics", get(crate::metrics::metrics_handler))
                .with_state(st);
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 9101));
            match tokio::net::TcpListener::bind(addr).await {
                Ok(listener) => {
                    tracing::info!("metrics endpoint on http://{addr}/metrics");
                    if let Err(e) = serve(listener, metrics_app).await {
                        tracing::warn!("metrics server error: {e}");
                    }
                }
                Err(e) => tracing::warn!("failed to bind metrics endpoint on {addr}: {e}"),
            }
        });
    }

    let api = routes::api_router(state.clone());
    let views_dir = format!("{}/views", config.frontend_dir);
    let app = Router::new()
        .merge(api)
        // Static assets and the API client sit at fixed, absolute paths.
        .nest_service(
            "/assets",
            ServeDir::new(format!("{}/assets", config.frontend_dir)),
        )
        .route_service(
            "/client.js",
            ServeFile::new(format!("{}/api/client.js", config.frontend_dir)),
        )
        // The device-flow entry point sends the user to the sign-in page.
        .route("/device", get(|| async { Redirect::to("/signin") }))
        // Clean, extensionless page URLs: "/" -> landing, "/<name>" -> views/<name>.html.
        // One handler covers every current and future page, with no per-page route.
        .fallback({
            let views = views_dir.clone();
            move |uri: Uri| {
                let views = views.clone();
                async move { serve_page(&views, &uri).await }
            }
        })
        // Always revalidate static assets so a redesign never gets stuck behind a
        // stale browser cache (no rebuild is needed for frontend changes either).
        .layer(SetResponseHeaderLayer::overriding(
            CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        ))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], config.port));
    tracing::info!("NatForge website backend listening on http://{addr}");
    tracing::info!("serving frontend from '{}'", config.frontend_dir);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve(listener, app).await?;
    Ok(())
}
