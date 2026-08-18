//! Lightweight internal API for the Core Proxy, consumed by the website control
//! plane (never by end users). Guarded by the shared internal secret: observe live
//! tunnels and force one down when a user clicks "Stop".

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::Serialize;
use serde_json::json;

use crate::state::CoreState;

const INTERNAL_HEADER: &str = "x-internal-secret";

fn check_secret(state: &Arc<CoreState>, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let provided = match headers.get(INTERNAL_HEADER) {
        Some(v) => match v.to_str() {
            Ok(s) => s,
            Err(_) => "",
        },
        None => "",
    };
    if provided == state.config.internal_secret {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "bad internal secret".into()))
    }
}

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
    Json(json!({ "status": "ok", "service": "natforge-node" }))
}

async fn list_tunnels(
    State(state): State<Arc<CoreState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<TunnelView>>, (StatusCode, String)> {
    check_secret(&state, &headers)?;
    let tunnels = state.tunnels.read().await;
    let mut out = Vec::new();
    for t in tunnels.values() {
        out.push(TunnelView {
            tunnel_id: t.tunnel_id,
            subdomain: t.subdomain.clone(),
            owner_id: t.owner_id,
            public_ports: t.public_ports.clone(),
            has_http: t.has_http,
            has_https: t.has_https,
            bytes_in: t.stats.bytes_in.load(Ordering::Relaxed),
            bytes_out: t.stats.bytes_out.load(Ordering::Relaxed),
        });
    }
    Ok(Json(out))
}

/// Force a tunnel down: abort its yamux driver + listeners and synchronously
/// remove its route-registry entries (closing the window where new connections
/// could be routed to a dead session), then let `handle_agent` finish teardown.
async fn stop_tunnel(
    State(state): State<Arc<CoreState>>,
    headers: HeaderMap,
    Path(subdomain): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    check_secret(&state, &headers)?;
    let info = {
        let tunnels = state.tunnels.read().await;
        match tunnels.get(&subdomain) {
            Some(t) => {
                t.driver_abort.abort();
                for jh in &t.listener_handles {
                    jh.abort();
                }
                Some((
                    t.tunnel_id,
                    t.public_ports.clone(),
                    t.udp_ports.clone(),
                    t.custom_domain.clone(),
                ))
            }
            None => None,
        }
    };
    match info {
        Some((tunnel_id, ports, udp_ports, custom_domain)) => {
            crate::tunnel::teardown(
                &state,
                tunnel_id,
                &subdomain,
                &ports,
                &udp_ports,
                custom_domain.as_deref(),
            )
            .await;
            Ok(Json(
                json!({ "status": "stopping", "subdomain": subdomain }),
            ))
        }
        None => Ok(Json(
            json!({ "status": "not_found", "subdomain": subdomain }),
        )),
    }
}

pub fn core_routes(state: Arc<CoreState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/internal/tunnels", get(list_tunnels))
        .route("/internal/tunnels/{subdomain}/stop", post(stop_tunnel))
        .with_state(state)
}
