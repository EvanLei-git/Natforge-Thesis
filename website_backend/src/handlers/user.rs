//! Authenticated user self-service: profile (name + email) and password change.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::connection::SharedState;
use crate::db::queries;
use crate::handlers::auth::{hash_password, verify_password};
use crate::jwt::AuthUser;

fn err<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[derive(Serialize)]
pub struct ProfileRes {
    pub id: i32,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
}

/// GET /api/user/profile — the caller's own account.
pub async fn get_profile(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<ProfileRes>, (StatusCode, String)> {
    let u = queries::user_by_id(&state.db.pg, user.user_id)
        .await
        .map_err(err)?
        .ok_or((StatusCode::NOT_FOUND, "no such user".to_string()))?;
    Ok(Json(ProfileRes { id: u.id, email: u.email, name: u.name, role: u.role }))
}

#[derive(Deserialize)]
pub struct UpdateProfileReq {
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// PUT /api/user/profile — change display name and/or email.
pub async fn update_profile(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(req): Json<UpdateProfileReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let email = req.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err((StatusCode::BAD_REQUEST, "a valid email is required".into()));
    }
    let name = req.name.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if name.map(|s| s.chars().count() > 60).unwrap_or(false) {
        return Err((StatusCode::BAD_REQUEST, "name too long (max 60 chars)".into()));
    }
    // Email uniqueness: allowed only if free or already ours.
    if let Some(existing) = queries::user_by_email(&state.db.pg, &email).await.map_err(err)? {
        if existing.id != user.user_id {
            return Err((StatusCode::CONFLICT, "that email is already in use".into()));
        }
    }
    queries::update_user_profile(&state.db.pg, user.user_id, name, &email)
        .await
        .map_err(err)?;
    Ok(Json(json!({ "status": "updated", "email": email, "name": name })))
}

#[derive(Deserialize)]
pub struct ChangePwReq {
    pub current_password: String,
    pub new_password: String,
}

/// PUT /api/user/password — change password (verifies the current one first).
pub async fn change_password(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(req): Json<ChangePwReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if req.new_password.len() < 8 {
        return Err((StatusCode::BAD_REQUEST, "new password must be at least 8 characters".into()));
    }
    let u = queries::user_by_id(&state.db.pg, user.user_id)
        .await
        .map_err(err)?
        .ok_or((StatusCode::NOT_FOUND, "no such user".to_string()))?;
    if !verify_password(&req.current_password, &u.password_hash) {
        return Err((StatusCode::UNAUTHORIZED, "current password is incorrect".into()));
    }
    queries::update_user_password(&state.db.pg, user.user_id, &hash_password(&req.new_password))
        .await
        .map_err(err)?;
    Ok(Json(json!({ "status": "password_changed" })))
}
