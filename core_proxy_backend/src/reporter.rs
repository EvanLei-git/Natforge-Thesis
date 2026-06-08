//! Outbound reporting to the website control plane (HTTP, internal-secret guarded)
//! plus a best-effort Redis liveness mirror. All calls are best-effort: failures
//! are logged and never interrupt a live tunnel.

use std::sync::Arc;

use redis::AsyncCommands;
use serde_json::json;
use tracing::debug;

use crate::state::CoreState;

const INTERNAL_HEADER: &str = "x-internal-secret";
const LIVE_TTL: i64 = 30;

async fn post(state: &Arc<CoreState>, path: &str, body: serde_json::Value) {
    let url = format!("{}{}", state.config.website_url, path);
    if let Err(e) = state
        .http
        .post(&url)
        .header(INTERNAL_HEADER, &state.config.internal_secret)
        .json(&body)
        .send()
        .await
    {
        debug!("internal report to {path} failed: {e}");
    }
}

pub async fn tunnel_up(state: &Arc<CoreState>, tunnel_id: i64, _subdomain: &str) {
    post(
        state,
        "/api/internal/tunnel_up",
        json!({ "tunnel_id": tunnel_id, "node_id": state.config.node_id }),
    )
    .await;
}

pub async fn tunnel_down(state: &Arc<CoreState>, tunnel_id: i64, subdomain: &str) {
    post(state, "/api/internal/tunnel_down", json!({ "tunnel_id": tunnel_id })).await;
    redis_mirror_down(state, subdomain).await;
}

pub async fn report_bandwidth(state: &Arc<CoreState>, tunnel_id: i64, owner_id: i32, bytes_in: u64, bytes_out: u64) {
    post(
        state,
        "/api/internal/bandwidth",
        json!({
            "tunnel_id": tunnel_id,
            "owner_id": owner_id,
            "bytes_in": bytes_in as i64,
            "bytes_out": bytes_out as i64,
        }),
    )
    .await;
}

/// Refresh the globally blocked-port list from the website (admin policy).
pub async fn refresh_policy(state: &Arc<CoreState>) {
    let url = format!("{}/api/internal/policy", state.config.website_url);
    let resp = state
        .http
        .get(&url)
        .header(INTERNAL_HEADER, &state.config.internal_secret)
        .send()
        .await;
    if let Ok(r) = resp {
        if let Ok(v) = r.json::<serde_json::Value>().await {
            if let Some(ports) = v.get("blocked_ports").and_then(|p| p.as_array()) {
                let list: Vec<u16> = ports.iter().filter_map(|p| p.as_u64().map(|n| n as u16)).collect();
                *state.blocked_ports.write().await = list;
            }
        }
    }
}

// ---- Redis liveness mirror (best-effort; idempotent SET..EX, refreshed each tick) ----

pub async fn redis_mirror_up(
    state: &Arc<CoreState>,
    tunnel_id: i64,
    subdomain: &str,
    has_http: bool,
    has_https: bool,
    ports: &[u16],
) {
    let mut conn = state.redis.clone();
    let node = &state.config.node_id;
    let live = format!("nf:tunnel:live:{subdomain}");
    let _: Result<(), _> = conn.set_ex(&live, format!("{node}:{tunnel_id}"), LIVE_TTL as u64).await;
    if has_http || has_https {
        let hk = format!("nf:route:host:{subdomain}");
        let _: Result<(), _> = conn.set_ex(&hk, node, LIVE_TTL as u64).await;
    }
    for p in ports {
        let pk = format!("nf:route:port:{p}");
        let _: Result<(), _> = conn.set_ex(&pk, subdomain, LIVE_TTL as u64).await;
    }
}

async fn redis_mirror_down(state: &Arc<CoreState>, subdomain: &str) {
    let mut conn = state.redis.clone();
    let _: Result<(), _> = conn.del(format!("nf:tunnel:live:{subdomain}")).await;
    let _: Result<(), _> = conn.del(format!("nf:route:host:{subdomain}")).await;
}
