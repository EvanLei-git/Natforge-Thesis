//! Administrator panel: region blocking, global port bans, network overview.
//! Every handler requires the `admin` role.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::connection::SharedState;
use crate::db::queries;
use crate::jwt::AuthUser;
use crate::models::TunnelView;

fn require_admin(user: &AuthUser) -> Result<(), (StatusCode, String)> {
    if user.role == "admin" {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "administrator role required".into()))
    }
}

use crate::handlers::err;

pub async fn get_region_blocks(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    require_admin(&user)?;
    Ok(Json(
        queries::region_blocks(&state.db.pg).await.map_err(err)?,
    ))
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
        return Err((
            StatusCode::BAD_REQUEST,
            "country_code must be 2 letters".into(),
        ));
    }
    queries::add_region_block(&state.db.pg, &code)
        .await
        .map_err(err)?;
    Ok(Json(json!({ "status": "banned" })))
}

pub async fn remove_region_block(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(country_code): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    require_admin(&user)?;
    queries::remove_region_block(&state.db.pg, &country_code.to_uppercase())
        .await
        .map_err(err)?;
    Ok(Json(json!({ "status": "unbanned" })))
}

#[derive(Serialize)]
pub struct NetworkStats {
    pub total_users: i64,
    pub active_tunnels: i64,
    pub total_bytes_relayed: i64,
    pub blocked_regions: i64,
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
        total_bytes_relayed: s.total_bytes_relayed,
        blocked_regions: s.blocked_regions,
    }))
}

pub async fn all_tunnels(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<Vec<TunnelView>>, (StatusCode, String)> {
    require_admin(&user)?;
    Ok(Json(queries::all_tunnels(&state.db.pg).await.map_err(err)?))
}

/// Per-user overview (id, email, role, #tunnels, total traffic, last seen).
pub async fn list_users(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<Vec<crate::models::UserOverview>>, (StatusCode, String)> {
    require_admin(&user)?;
    Ok(Json(
        queries::users_overview(&state.db.pg).await.map_err(err)?,
    ))
}

/// DELETE /api/admin/users/{id} - remove a user and (by FK cascade) their tunnels,
/// freeing the ports. Live sessions are dropped first (best-effort).
pub async fn delete_user(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(user_id): Path<i32>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    require_admin(&user)?;
    if user_id == user.user_id {
        return Err((
            StatusCode::BAD_REQUEST,
            "you cannot delete your own admin account".into(),
        ));
    }
    for (_, sub, node_id) in queries::owner_tunnel_targets(&state.db.pg, user_id)
        .await
        .map_err(err)?
    {
        crate::handlers::tunnels::signal_node_stop(&state, &sub, &node_id).await;
    }
    let n = queries::delete_user(&state.db.pg, user_id)
        .await
        .map_err(err)?;
    if n == 0 {
        return Err((StatusCode::NOT_FOUND, "no such user".into()));
    }
    Ok(Json(
        json!({ "status": "user_deleted", "user_id": user_id }),
    ))
}

#[derive(Deserialize)]
pub struct BanReq {
    pub banned: bool,
}

/// PATCH /api/admin/users/{id} - ban or unban a user. Banning drops their live
/// tunnels (and marks them stopped); banned users cannot log in or reserve.
pub async fn set_user_ban(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(user_id): Path<i32>,
    Json(req): Json<BanReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    require_admin(&user)?;
    if user_id == user.user_id {
        return Err((
            StatusCode::BAD_REQUEST,
            "you cannot ban your own admin account".into(),
        ));
    }
    queries::set_user_banned(&state.db.pg, user_id, req.banned)
        .await
        .map_err(err)?;
    if req.banned {
        for (tid, sub, node_id) in queries::owner_tunnel_targets(&state.db.pg, user_id)
            .await
            .map_err(err)?
        {
            crate::handlers::tunnels::signal_node_stop(&state, &sub, &node_id).await;
            let _ = queries::stop_tunnel_keep(&state.db.pg, tid).await;
        }
    }
    Ok(Json(
        json!({ "status": if req.banned { "banned" } else { "unbanned" }, "user_id": user_id }),
    ))
}

// --------------------------------------------------------------------------
// Nodes / regions (each is a data-plane VM that self-registers on boot).
// --------------------------------------------------------------------------

/// All nodes (active and inactive) for the admin Regions panel.
pub async fn list_nodes(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<Vec<crate::models::Node>>, (StatusCode, String)> {
    require_admin(&user)?;
    Ok(Json(
        queries::list_nodes(&state.db.pg, false)
            .await
            .map_err(err)?,
    ))
}

#[derive(Deserialize)]
pub struct UpdateNodeReq {
    pub name: String,
    #[serde(default)]
    pub region: Option<String>,
    pub active: bool,
}

/// Admin renames a node, sets its human region label, and enables/disables it.
pub async fn update_node(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(node_id): Path<String>,
    Json(p): Json<UpdateNodeReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    require_admin(&user)?;
    let region_deref = p.region.as_deref();
    let region_trimmed = region_deref.map(str::trim);
    let region = match region_trimmed {
        Some(s) => {
            if !s.is_empty() {
                Some(s)
            } else {
                None
            }
        }
        None => None,
    };
    queries::update_node(&state.db.pg, &node_id, p.name.trim(), region, p.active)
        .await
        .map_err(err)?;
    Ok(Json(json!({ "status": "updated", "node_id": node_id })))
}

/// Admin removes a node (also frees its port pool). Tunnels keep their stored host.
pub async fn delete_node(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(node_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    require_admin(&user)?;
    queries::delete_node(&state.db.pg, &node_id)
        .await
        .map_err(err)?;
    Ok(Json(json!({ "status": "deleted", "node_id": node_id })))
}
