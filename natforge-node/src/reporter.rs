//! Outbound reporting to the website control plane (HTTP, internal-secret guarded)
//! plus a best-effort Redis liveness mirror. All calls are best-effort: failures
//! are logged and never interrupt a live tunnel.

use std::sync::Arc;

use redis::AsyncCommands;
use serde_json::json;
use tracing::debug;

use crate::state::{CoreState, RouteHandle};

const INTERNAL_HEADER: &str = "x-internal-secret";
const LIVE_TTL: i64 = 30;

async fn post(state: &Arc<CoreState>, path: &str, body: serde_json::Value) {
    let _ = post_ok(state, path, body).await;
}

/// Like `post`, but returns whether the control plane accepted the call (2xx).
async fn post_ok(state: &Arc<CoreState>, path: &str, body: serde_json::Value) -> bool {
    let url = format!("{}{}", state.config.website_url, path);
    match state
        .http
        .post(&url)
        .header(INTERNAL_HEADER, &state.config.internal_secret)
        .json(&body)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => true,
        Ok(r) => {
            debug!("internal report to {path} returned {}", r.status());
            false
        }
        Err(e) => {
            debug!("internal report to {path} failed: {e}");
            false
        }
    }
}

/// Announce this node to the control plane on boot (and re-announce periodically).
/// The website upserts the node row and seeds its TCP port pool. Returns whether
/// the control plane accepted the registration.
pub async fn node_register(state: &Arc<CoreState>) -> bool {
    let cfg = &state.config;
    post_ok(
        state,
        "/api/internal/node_register",
        json!({
            "node_id": cfg.node_id,
            "name": cfg.node_name,
            "region": cfg.node_region,
            "public_host": cfg.public_host,
            "control_endpoint": cfg.control_endpoint,
            "internal_url": cfg.internal_url,
            "http_port": cfg.http_port as i32,
            "https_port": cfg.https_port as i32,
            "port_min": cfg.public_port_min,
            "port_max": cfg.public_port_max,
            "control_cert_fp": state.control_cert_fp,
        }),
    )
    .await
}

pub async fn tunnel_up(state: &Arc<CoreState>, tunnel_id: i64, _subdomain: &str, agent_ip: &str) {
    post(
        state,
        "/api/internal/tunnel_up",
        json!({ "tunnel_id": tunnel_id, "node_id": state.config.node_id, "agent_ip": agent_ip }),
    )
    .await;
}

pub async fn tunnel_down(state: &Arc<CoreState>, tunnel_id: i64, subdomain: &str) {
    post(
        state,
        "/api/internal/tunnel_down",
        json!({ "tunnel_id": tunnel_id }),
    )
    .await;
    redis_mirror_down(state, subdomain).await;
}

pub async fn report_bandwidth(
    state: &Arc<CoreState>,
    tunnel_id: i64,
    owner_id: i32,
    bytes_in: u64,
    bytes_out: u64,
) {
    post(
        state,
        "/api/internal/bandwidth",
        json!({
            "tunnel_id": tunnel_id,
            "owner_id": owner_id,
            // Saturating cast: counters never realistically reach i64::MAX, but avoid UB-free wrap.
            "bytes_in": bytes_in.min(i64::MAX as u64) as i64,
            "bytes_out": bytes_out.min(i64::MAX as u64) as i64,
        }),
    )
    .await;
}

/// Report one finished public connection (or a geo-blocked attempt) for the
/// per-tunnel connection log. Best-effort; never blocks the data path meaningfully.
#[allow(clippy::too_many_arguments)]
pub async fn report_conn_log(
    state: &Arc<CoreState>,
    handle: &RouteHandle,
    peer_ip: &str,
    country: Option<&str>,
    bytes_in: u64,
    bytes_out: u64,
    duration_ms: u128,
    blocked: bool,
) {
    post(
        state,
        "/api/internal/conn_log",
        json!({
            "tunnel_id": handle.tunnel_id,
            "owner_id": handle.owner_id,
            "route_id": handle.route_id,
            "kind": handle.mode.as_str(),
            "peer_ip": peer_ip,
            "country": country,
            "bytes_in": bytes_in.min(i64::MAX as u64) as i64,
            "bytes_out": bytes_out.min(i64::MAX as u64) as i64,
            "duration_ms": duration_ms.min(i64::MAX as u128) as i64,
            "blocked": blocked,
        }),
    )
    .await;
}

/// Refresh policy from the website: admin-wide blocked countries, and the
/// per-tunnel country block lists set by tunnel owners.
pub async fn refresh_policy(state: &Arc<CoreState>) {
    let url = format!("{}/api/internal/policy", state.config.website_url);
    let resp = state
        .http
        .get(&url)
        .header(INTERNAL_HEADER, &state.config.internal_secret)
        .send()
        .await;
    if let Ok(r) = resp
        && let Ok(v) = r.json::<serde_json::Value>().await
    {
        let blocked_regions_val = v.get("blocked_regions");
        let blocked_regions_arr = match blocked_regions_val {
            Some(p) => p.as_array(),
            None => None,
        };
        if let Some(regions) = blocked_regions_arr {
            let mut list: Vec<String> = Vec::new();
            for c in regions {
                if let Some(s) = c.as_str() {
                    list.push(s.to_uppercase());
                }
            }
            *state.blocked_regions.write().await = list;
        }
        // tunnel_region_blocks: { "<tunnel_id>": ["US","DE"], ... }
        let tunnel_blocks_val = v.get("tunnel_region_blocks");
        let tunnel_blocks_obj = match tunnel_blocks_val {
            Some(m) => m.as_object(),
            None => None,
        };
        if let Some(map) = tunnel_blocks_obj {
            let mut out = std::collections::HashMap::new();
            for (k, val) in map {
                if let (Ok(tid), Some(arr)) = (k.parse::<i64>(), val.as_array()) {
                    let mut codes: Vec<String> = Vec::new();
                    for c in arr {
                        if let Some(s) = c.as_str() {
                            codes.push(s.to_uppercase());
                        }
                    }
                    if !codes.is_empty() {
                        out.insert(tid, codes);
                    }
                }
            }
            *state.tunnel_region_blocks.write().await = out;
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
    let _: Result<(), _> = conn
        .set_ex(&live, format!("{node}:{tunnel_id}"), LIVE_TTL as u64)
        .await;
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
