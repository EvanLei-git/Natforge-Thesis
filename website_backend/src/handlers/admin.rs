//! Administrator panel: region blocking, global port bans, network overview.
//! Every handler requires the `admin` role.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::connection::SharedState;
use crate::db::queries;
use crate::jwt::AuthUser;
use crate::models::user::TunnelView;

fn require_admin(user: &AuthUser) -> Result<(), (StatusCode, String)> {
    if user.role == "admin" {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "administrator role required".into()))
    }
}

fn err<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub async fn get_region_blocks(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    require_admin(&user)?;
    Ok(Json(queries::region_blocks(&state.db.pg).await.map_err(err)?))
}

#[derive(Deserialize)]
pub struct RegionBlockReq {
    pub country_code: String,
}

pub async fn add_region_block(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(payload): Json<RegionBlockReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    require_admin(&user)?;
    let code = payload.country_code.trim().to_uppercase();
    if code.len() != 2 {
        return Err((StatusCode::BAD_REQUEST, "country_code must be 2 letters".into()));
    }
    queries::add_region_block(&state.db.pg, &code).await.map_err(err)?;
    Ok(Json(json!({ "status": "banned" })))
}

pub async fn remove_region_block(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(country_code): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    require_admin(&user)?;
    queries::remove_region_block(&state.db.pg, &country_code.to_uppercase()).await.map_err(err)?;
    Ok(Json(json!({ "status": "unbanned" })))
}

pub async fn get_port_blocks(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<Vec<i32>>, (StatusCode, String)> {
    require_admin(&user)?;
    Ok(Json(queries::port_blocks(&state.db.pg).await.map_err(err)?))
}

#[derive(Deserialize)]
pub struct PortBlockReq {
    pub port: u16,
}

pub async fn add_port_block(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(payload): Json<PortBlockReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    require_admin(&user)?;
    queries::add_port_block(&state.db.pg, payload.port).await.map_err(err)?;
    Ok(Json(json!({ "status": "banned", "port": payload.port })))
}

pub async fn remove_port_block(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(port): Path<u16>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    require_admin(&user)?;
    queries::remove_port_block(&state.db.pg, port).await.map_err(err)?;
    Ok(Json(json!({ "status": "unbanned", "port": port })))
}

#[derive(Serialize)]
pub struct NetworkStats {
    pub total_users: i64,
    pub active_tunnels: i64,
    pub active_edge_nodes: i64,
    pub total_bytes_relayed: i64,
    pub blocked_regions: i64,
    pub blocked_ports: i64,
}

pub async fn network_stats(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<NetworkStats>, (StatusCode, String)> {
    require_admin(&user)?;
    let s = queries::stats(&state.db.pg).await.map_err(err)?;
    Ok(Json(NetworkStats {
        total_users: s.total_users,
        active_tunnels: s.active_tunnels,
        active_edge_nodes: s.active_edge_nodes,
        total_bytes_relayed: s.total_bytes_relayed,
        blocked_regions: s.blocked_regions,
        blocked_ports: s.blocked_ports,
    }))
}

pub async fn all_tunnels(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<Vec<TunnelView>>, (StatusCode, String)> {
    require_admin(&user)?;
    Ok(Json(queries::all_tunnels(&state.db.pg, &state.config.domain).await.map_err(err)?))
}
