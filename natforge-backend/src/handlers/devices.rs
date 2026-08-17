//! Device enrollment and management.
//!
//! Enrollment is agent-first and reuses the RFC 8628 device-code machinery: the
//! agent runs `natforge enroll`, gets a `user_code`, the user approves + names it
//! in the dashboard, and the agent's poll returns a long-lived **device token**.
//! Device management (list / rename / delete) is session-authed.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::connection::{self, SharedState};
use crate::db::queries;
use crate::jwt::{AuthUser, issue_device_token};
use crate::models::Device;

use crate::handlers::db_err;

/// A fresh random string from `charset` (own RNG, so callers never share a borrow).
fn random_from(charset: &[u8], n: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| charset[rng.gen_range(0..charset.len())] as char)
        .collect()
}

const HEX: &[u8] = b"0123456789abcdef";
const UC: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

// ---------------------------------------------------------------------------
// Enrollment (agent-first): the agent prints a short code, the user enters it here.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct EnrollStartRes {
    pub device_code: String,
    pub user_code: String,
    pub interval: u32,
    pub expires_in: u32,
}

/// `natforge enroll` starts here: issue a device/user code pair (unauthenticated).
pub async fn enroll_start(
    State(state): State<SharedState>,
) -> Result<Json<EnrollStartRes>, (StatusCode, String)> {
    let device_code = random_from(HEX, 40);
    let user_code = random_from(UC, 8);
    connection::devcode_create(&state.db.redis, &user_code, &device_code)
        .await
        .map_err(db_err)?;
    tracing::info!("device enrollment started: {user_code}");
    Ok(Json(EnrollStartRes {
        device_code,
        user_code,
        interval: 3,
        expires_in: connection::DEVCODE_TTL_SECS as u32,
    }))
}

#[derive(Deserialize)]
pub struct EnrollApproveReq {
    pub user_code: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Bind the code to this existing (caller-owned) device instead of creating a new
    /// one. Used to connect a device that was created earlier without a code.
    #[serde(default)]
    pub device_id: Option<i64>,
}

/// Bind the short code the agent printed to a device the caller owns (creating the
/// device if no `device_id` is given). Session-authed, so the device is owned by the
/// approving user; rate limited so the short code cannot be guessed online.
pub async fn enroll_approve(
    State(state): State<SharedState>,
    user: AuthUser,
    headers: axum::http::HeaderMap,
    Json(payload): Json<EnrollApproveReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ip = crate::geo::client_ip(&headers)
        .map(|i| i.to_string())
        .unwrap_or_else(|| "unknown".into());
    let ok_ip =
        connection::rate_limit_hit(&state.db.redis, &format!("nf:enrollrl:ip:{ip}"), 10, 60)
            .await
            .unwrap_or(true);
    let ok_all = connection::rate_limit_hit(&state.db.redis, "nf:enrollrl:all", 120, 60)
        .await
        .unwrap_or(true);
    if !ok_ip || !ok_all {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "too many attempts; wait a minute and try again".into(),
        ));
    }
    let code = payload.user_code.trim().to_uppercase().replace('-', "");
    // Link an existing owned device, or create a fresh one to bind the code to.
    let (device_id, created, name) = match payload.device_id {
        Some(did) => {
            let dev = queries::device_by_id(&state.db.pg, did)
                .await
                .map_err(db_err)?
                .filter(|d| d.owner_id == user.user_id)
                .ok_or((StatusCode::NOT_FOUND, "device not found".to_string()))?;
            (did, false, dev.name)
        }
        None => {
            let name = payload
                .name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("device")
                .to_string();
            let dev = queries::create_device(&state.db.pg, user.user_id, &name)
                .await
                .map_err(db_err)?;
            (dev.id, true, name)
        }
    };
    let ok = connection::devcode_approve_device(&state.db.redis, &code, user.user_id, device_id)
        .await
        .map_err(db_err)?;
    if !ok {
        // Only roll back a device we just created for this attempt.
        if created {
            let _ = queries::delete_device(&state.db.pg, device_id, user.user_id).await;
        }
        return Err((StatusCode::NOT_FOUND, "invalid or expired code".into()));
    }
    tracing::info!(
        "device '{name}' (#{device_id}) linked by user {}",
        user.user_id
    );
    Ok(Json(
        json!({ "status": "approved", "device_id": device_id, "name": name }),
    ))
}

#[derive(Deserialize)]
pub struct EnrollTokenReq {
    pub device_code: String,
}

/// The agent polls here; once approved it consumes the code (single-use) and gets
/// a long-lived device token bound to a fresh nonce (revocable by deleting the row).
pub async fn enroll_token(
    State(state): State<SharedState>,
    Json(payload): Json<EnrollTokenReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let rec = connection::devcode_status(&state.db.redis, &payload.device_code)
        .await
        .map_err(db_err)?;
    let Some(rec) = rec else {
        return Ok(Json(json!({ "status": "expired_token" })));
    };
    let (Some(owner), Some(device_id)) = (rec.approved_user, rec.device_id) else {
        return Ok(Json(json!({ "status": "authorization_pending" })));
    };
    // Single-use: the first approved poll wins; replays see it gone.
    let won = connection::devcode_consume(&state.db.redis, &payload.device_code)
        .await
        .map_err(db_err)?;
    if !won {
        return Ok(Json(json!({ "status": "expired_token" })));
    }
    let nonce = random_from(HEX, 32);
    queries::set_device_token(&state.db.pg, device_id, &nonce)
        .await
        .map_err(db_err)?;
    let token = issue_device_token(&state.config.jwt_secret, owner, device_id, &nonce);
    let name = queries::device_by_id(&state.db.pg, device_id)
        .await
        .map_err(db_err)?
        .map(|d| d.name)
        .unwrap_or_default();
    Ok(Json(json!({
        "status": "approved",
        "device_token": token,
        "device_id": device_id,
        "name": name,
    })))
}

// ---------------------------------------------------------------------------
// Create a device from the dashboard by name (pending). It is connected later,
// agent-first, by entering the code the agent prints (see enroll_approve).
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateDeviceReq {
    #[serde(default)]
    pub name: Option<String>,
}

/// POST /api/devices - create a device by name (pending, no agent yet).
pub async fn create_device(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(payload): Json<CreateDeviceReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let name = payload
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("device")
        .to_string();
    let device = queries::create_device(&state.db.pg, user.user_id, &name)
        .await
        .map_err(db_err)?;
    tracing::info!(
        "device '{}' (#{}) created by user {}",
        name,
        device.id,
        user.user_id
    );
    Ok(Json(json!({ "device_id": device.id, "name": name })))
}

// ---------------------------------------------------------------------------
// Device management (session-authed)
// ---------------------------------------------------------------------------

pub async fn list_devices(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<Vec<Device>>, (StatusCode, String)> {
    let devices = queries::list_devices(&state.db.pg, user.user_id)
        .await
        .map_err(db_err)?;
    Ok(Json(devices))
}

#[derive(Deserialize)]
pub struct RenameReq {
    pub name: String,
}

pub async fn rename_device(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(payload): Json<RenameReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let name = payload.name.trim();
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name required".into()));
    }
    let ok = queries::rename_device(&state.db.pg, id, user.user_id, name)
        .await
        .map_err(db_err)?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, "device not found".into()))
    }
}

pub async fn delete_device(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> Result<StatusCode, (StatusCode, String)> {
    let ok = queries::delete_device(&state.db.pg, id, user.user_id)
        .await
        .map_err(db_err)?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, "device not found".into()))
    }
}

/// De-attach the current machine from a device: revokes the running agent's token and
/// marks it offline, but keeps the device and its service hosts so the user can connect
/// a different machine (agent-first, via the enrollment code) without losing anything.
pub async fn disconnect_device(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> Result<StatusCode, (StatusCode, String)> {
    let ok = queries::disconnect_device(&state.db.pg, id, user.user_id)
        .await
        .map_err(db_err)?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, "device not found".into()))
    }
}
