//! Lightweight internal API for the Core Proxy, consumed by the website control
//! plane (never by end users): observe live tunnels and force one down when a user
//! clicks "Stop".

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use serde_json::json;

use crate::state::CoreState;

#[derive(Serialize)]
pub struct TunnelView {
    pub tunnel_id: i64,
    pub subdomain: String,
    pub owner_id: i32,
    pub public_ports: Vec<u16>,
    pub has_http: bool,
    pub has_https: bool,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "service": "core_proxy_backend" }))
}

async fn list_tunnels(State(state): State<Arc<CoreState>>) -> Json<Vec<TunnelView>> {
    let tunnels = state.tunnels.read().await;
    let out = tunnels
        .values()
        .map(|t| TunnelView {
            tunnel_id: t.tunnel_id,
            subdomain: t.subdomain.clone(),
            owner_id: t.owner_id,
            public_ports: t.public_ports.clone(),
            has_http: t.has_http,
            has_https: t.has_https,
            bytes_in: t.stats.bytes_in.load(Ordering::Relaxed),
            bytes_out: t.stats.bytes_out.load(Ordering::Relaxed),
        })
        .collect();
    Json(out)
}

/// Force a tunnel down: abort its yamux driver, which collapses the session and
/// triggers the normal teardown path in `handle_agent`.
async fn stop_tunnel(
    State(state): State<Arc<CoreState>>,
    Path(subdomain): Path<String>,
) -> Json<serde_json::Value> {
    if let Some(t) = state.tunnels.read().await.get(&subdomain) {
        t.driver_abort.abort();
        for jh in &t.listener_handles {
            jh.abort();
        }
        Json(json!({ "status": "stopping", "subdomain": subdomain }))
    } else {
        Json(json!({ "status": "not_found", "subdomain": subdomain }))
    }
}

pub fn core_routes(state: Arc<CoreState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/internal/tunnels", get(list_tunnels))
        .route("/internal/tunnels/{subdomain}/stop", post(stop_tunnel))
        .with_state(state)
}
