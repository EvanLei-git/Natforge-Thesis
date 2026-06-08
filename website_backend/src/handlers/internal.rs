//! Internal control-plane API consumed exclusively by the core proxy. Every call
//! must present the shared internal secret header.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::db::connection::SharedState;
use crate::db::queries;

const INTERNAL_HEADER: &str = "x-internal-secret";

fn check_secret(state: &SharedState, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let provided = headers.get(INTERNAL_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
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
}

/// Agent connected; mark the tunnel online.
pub async fn tunnel_up(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(p): Json<TunnelUpReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    check_secret(&state, &headers)?;
    queries::set_tunnel_online(&state.db.pg, p.tunnel_id, &p.node_id).await.map_err(err)?;
    tracing::info!("tunnel {} online on {}", p.tunnel_id, p.node_id);
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
    queries::set_tunnel_offline(&state.db.pg, p.tunnel_id).await.map_err(err)?;
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
    queries::append_bandwidth(&state.db.pg, p.tunnel_id, p.owner_id, p.bytes_in, p.bytes_out)
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
    Ok(Json(json!({ "blocked_ports": ports, "blocked_regions": regions })))
}
