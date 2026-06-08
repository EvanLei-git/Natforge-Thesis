//! IP-host (edge node) management.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::db::connection::SharedState;
use crate::db::queries;
use crate::jwt::AuthUser;
use crate::models::user::IpHostConfig;

fn err<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[derive(Deserialize)]
pub struct RelayStatusReq {
    pub active: bool,
}

pub async fn set_relay_status(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(payload): Json<RelayStatusReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    queries::ip_host_set_active(&state.db.pg, user.user_id, payload.active).await.map_err(err)?;
    Ok(Json(json!({ "status": "updated", "active": payload.active })))
}

#[derive(Deserialize)]
pub struct PrefReq {
    pub max_bandwidth_mbps: i32,
    pub geo_pref_only: bool,
}

pub async fn update_preferences(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(payload): Json<PrefReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    queries::ip_host_set_prefs(&state.db.pg, user.user_id, payload.max_bandwidth_mbps, payload.geo_pref_only)
        .await
        .map_err(err)?;
    Ok(Json(json!({ "status": "updated" })))
}

pub async fn get_status(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<IpHostConfig>, (StatusCode, String)> {
    let cfg = queries::ip_host_get(&state.db.pg, user.user_id).await.map_err(err)?;
    Ok(Json(cfg))
}
