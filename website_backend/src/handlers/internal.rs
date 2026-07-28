//! Internal control-plane API consumed exclusively by the core proxy. Every call
//! must present the shared internal secret header.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::json;

use crate::db::connection::SharedState;
use crate::db::queries;

const INTERNAL_HEADER: &str = "x-internal-secret";

fn check_secret(state: &SharedState, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let provided = headers
        .get(INTERNAL_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if provided == state.config.internal_secret {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "bad internal secret".into()))
    }
}

fn err<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[derive(Deserialize)]
pub struct TunnelUpReq {
    pub tunnel_id: i64,
    pub node_id: String,
    /// The agent's source IP as seen by the data plane (the user's machine).
    #[serde(default)]
    pub agent_ip: Option<String>,
}

/// Agent connected; mark the tunnel online (recording the agent's IP).
pub async fn tunnel_up(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(p): Json<TunnelUpReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    check_secret(&state, &headers)?;
    queries::set_tunnel_online(&state.db.pg, p.tunnel_id, &p.node_id, p.agent_ip.as_deref())
        .await
        .map_err(err)?;
    tracing::info!(
        "tunnel {} online on {} (agent {:?})",
        p.tunnel_id,
        p.node_id,
        p.agent_ip
    );
    Ok(Json(json!({ "status": "ok" })))
}

#[derive(Deserialize)]
pub struct TunnelDownReq {
    pub tunnel_id: i64,
}

/// Agent disconnected; mark offline (ports stay reserved for reconnect, freed by
/// explicit stop or the reconciliation sweep).
pub async fn tunnel_down(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(p): Json<TunnelDownReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    check_secret(&state, &headers)?;
    queries::set_tunnel_offline(&state.db.pg, p.tunnel_id)
        .await
        .map_err(err)?;
    tracing::info!("tunnel {} offline", p.tunnel_id);
    Ok(Json(json!({ "status": "ok" })))
}

#[derive(Deserialize)]
pub struct BandwidthReq {
    pub tunnel_id: i64,
    pub owner_id: i32,
    pub bytes_in: i64,
    pub bytes_out: i64,
}

pub async fn bandwidth(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(p): Json<BandwidthReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    check_secret(&state, &headers)?;
    queries::append_bandwidth(
        &state.db.pg,
        p.tunnel_id,
        p.owner_id,
        p.bytes_in,
        p.bytes_out,
    )
    .await
    .map_err(err)?;
    Ok(Json(json!({ "status": "ok" })))
}

#[derive(Deserialize)]
pub struct ConnLogReq {
    pub tunnel_id: i64,
    pub owner_id: i32,
    pub route_id: i16,
    pub kind: String,
    pub peer_ip: String,
    #[serde(default)]
    pub country: Option<String>,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub duration_ms: i64,
    #[serde(default)]
    pub blocked: bool,
}

/// The core reports one closed public connection (or a geo-blocked attempt).
pub async fn conn_log(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(p): Json<ConnLogReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    check_secret(&state, &headers)?;
    queries::insert_conn_log(
        &state.db.pg,
        p.tunnel_id,
        p.owner_id,
        p.route_id,
        &p.kind,
        &p.peer_ip,
        p.country.as_deref(),
        p.bytes_in,
        p.bytes_out,
        p.duration_ms,
        p.blocked,
    )
    .await
    .map_err(err)?;
    Ok(Json(json!({ "status": "ok" })))
}

pub async fn policy(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    check_secret(&state, &headers)?;
    let ports = queries::port_blocks(&state.db.pg).await.map_err(err)?;
    let regions = queries::region_blocks(&state.db.pg).await.map_err(err)?;
    let per_tunnel = queries::all_tunnel_region_blocks(&state.db.pg)
        .await
        .map_err(err)?;
    // Stringify the i64 keys so the map round-trips cleanly as JSON.
    let per_tunnel: std::collections::HashMap<String, Vec<String>> = per_tunnel
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    Ok(Json(json!({
        "blocked_ports": ports,
        "blocked_regions": regions,
        "tunnel_region_blocks": per_tunnel,
    })))
}

#[derive(Deserialize)]
pub struct NodeRegisterReq {
    pub node_id: String,
    pub name: String,
    #[serde(default)]
    pub region: Option<String>,
    /// Wildcard apex this node serves (e.g. "natforge.com" or "bg.natforge.com").
    pub public_host: String,
    /// host:port agents connect to for the yamux control channel.
    pub control_endpoint: String,
    /// How the website reaches this node's internal data-plane API.
    pub internal_url: String,
    pub http_port: i32,
    pub https_port: i32,
    pub port_min: i32,
    pub port_max: i32,
    #[serde(default)]
    pub control_cert_fp: Option<String>,
}

/// A data-plane node announces itself on boot. Technical fields refresh every
/// time; the admin-controlled name/region/active are preserved after first sight.
pub async fn node_register(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(p): Json<NodeRegisterReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    check_secret(&state, &headers)?;
    queries::register_node(
        &state.db.pg,
        &p.node_id,
        &p.name,
        p.region.as_deref(),
        &p.public_host,
        &p.control_endpoint,
        &p.internal_url,
        p.http_port,
        p.https_port,
        p.port_min,
        p.port_max,
        p.control_cert_fp.as_deref(),
    )
    .await
    .map_err(err)?;
    tracing::info!(
        "node {} ({}) registered: {}",
        p.node_id,
        p.name,
        p.public_host
    );
    Ok(Json(json!({ "status": "ok" })))
}
