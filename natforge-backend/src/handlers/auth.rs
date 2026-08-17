//! Authentication: registration, login, and the RFC 8628 device-authorization
//! grant (device codes stored in Redis with a TTL).

use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::http::{HeaderMap, StatusCode};
use axum::{Json, extract::State};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::connection::SharedState;
use crate::db::{connection, queries};
use crate::jwt::{AuthUser, issue_session};

/// Reject the request if the caller's country is on the admin region-block list.
/// No GeoIP data (or a direct/local connection) means "unknown" → allowed.
async fn geo_gate(state: &SharedState, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let Some(country) = state.geo.country_from_headers(headers) else {
        return Ok(());
    };
    let blocked = queries::region_blocks(&state.db.pg)
        .await
        .map_err(db_err)?
        .iter()
        .any(|c| c.eq_ignore_ascii_case(&country));
    if blocked {
        tracing::warn!("rejected auth from geo-blocked country {country}");
        return Err((
            StatusCode::FORBIDDEN,
            format!("access from your region ({country}) is blocked"),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct RegisterReq {
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub role: String,
    pub status: String,
}

pub(crate) fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("hash password")
        .to_string()
}

pub(crate) fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

use crate::handlers::db_err;

pub async fn register_user(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(payload): Json<RegisterReq>,
) -> Result<Json<AuthResponse>, (StatusCode, String)> {
    geo_gate(&state, &headers).await?;
    let email = payload.email.trim().to_lowercase();
    if email.is_empty() || payload.password.len() < 8 {
        return Err((
            StatusCode::BAD_REQUEST,
            "email required and password must be at least 8 characters".into(),
        ));
    }
    if queries::user_by_email(&state.db.pg, &email)
        .await
        .map_err(db_err)?
        .is_some()
    {
        return Err((StatusCode::CONFLICT, "email already registered".into()));
    }
    let user = queries::create_user(&state.db.pg, &email, &hash_password(&payload.password))
        .await
        .map_err(db_err)?;
    tracing::info!(
        "registered {} (id {}, role {})",
        user.email,
        user.id,
        user.role
    );
    let token = issue_session(&state.config.jwt_secret, user.id, &user.email, &user.role);
    Ok(Json(AuthResponse {
        token,
        role: user.role,
        status: "registered".into(),
    }))
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub email: String,
    pub password: String,
}

pub async fn login_user(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(payload): Json<LoginReq>,
) -> Result<Json<AuthResponse>, (StatusCode, String)> {
    geo_gate(&state, &headers).await?;
    let email = payload.email.trim().to_lowercase();
    let user = queries::user_by_email(&state.db.pg, &email)
        .await
        .map_err(db_err)?;
    let user = user
        .filter(|u| verify_password(&payload.password, &u.password_hash))
        .ok_or((StatusCode::UNAUTHORIZED, "invalid credentials".to_string()))?;
    if user.banned {
        return Err((StatusCode::FORBIDDEN, "this account is banned".into()));
    }
    state.metrics.signins.with_label_values(&["password"]).inc();
    let token = issue_session(&state.config.jwt_secret, user.id, &user.email, &user.role);
    Ok(Json(AuthResponse {
        token,
        role: user.role,
        status: "ok".into(),
    }))
}

// ---------------------------------------------------------------------------
// RFC 8628 device authorization grant
// ---------------------------------------------------------------------------

fn random_code(len: usize, charset: &[u8]) -> String {
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| charset[rng.gen_range(0..charset.len())] as char)
        .collect()
}

#[derive(Serialize)]
pub struct DeviceStartRes {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u32,
    pub expires_in: u32,
}

pub async fn device_start(
    State(state): State<SharedState>,
) -> Result<Json<DeviceStartRes>, (StatusCode, String)> {
    const HEX: &[u8] = b"0123456789abcdef";
    const UC: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let device_code = random_code(40, HEX);
    let user_code = format!("{}-{}", random_code(4, UC), random_code(4, UC));
    connection::devcode_create(&state.db.redis, &user_code, &device_code)
        .await
        .map_err(db_err)?;
    tracing::info!("device authorization started: {user_code}");
    Ok(Json(DeviceStartRes {
        device_code,
        user_code,
        verification_uri: format!("https://{}/device", state.config.domain),
        interval: 3,
        expires_in: connection::DEVCODE_TTL_SECS as u32,
    }))
}

#[derive(Deserialize)]
pub struct DeviceTokenReq {
    pub device_code: String,
}

pub async fn device_token(
    State(state): State<SharedState>,
    Json(payload): Json<DeviceTokenReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let rec = connection::devcode_status(&state.db.redis, &payload.device_code)
        .await
        .map_err(db_err)?;
    match rec {
        None => Ok(Json(json!({ "status": "expired_token" }))),
        Some(r) => match r.approved_user {
            None => Ok(Json(json!({ "status": "authorization_pending" }))),
            Some(uid) => {
                // Single-use: the first approved poll consumes the code; any
                // later poll (or replay) sees it gone and gets `expired_token`.
                let won = connection::devcode_consume(&state.db.redis, &payload.device_code)
                    .await
                    .map_err(db_err)?;
                if !won {
                    return Ok(Json(json!({ "status": "expired_token" })));
                }
                let user = queries::user_by_id(&state.db.pg, uid)
                    .await
                    .map_err(db_err)?;
                match user {
                    Some(u) if u.banned => Ok(Json(json!({ "status": "banned" }))),
                    Some(u) => {
                        state
                            .metrics
                            .signins
                            .with_label_values(&["device_code"])
                            .inc();
                        let token =
                            issue_session(&state.config.jwt_secret, u.id, &u.email, &u.role);
                        Ok(Json(
                            json!({ "status": "approved", "token": token, "role": u.role }),
                        ))
                    }
                    None => Ok(Json(json!({ "status": "expired_token" }))),
                }
            }
        },
    }
}

#[derive(Deserialize)]
pub struct DeviceApproveReq {
    pub user_code: String,
}

pub async fn device_approve(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(payload): Json<DeviceApproveReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let code = payload.user_code.trim().to_uppercase();
    let ok = connection::devcode_approve(&state.db.redis, &code, user.user_id)
        .await
        .map_err(db_err)?;
    if ok {
        tracing::info!("device code {code} approved by user {}", user.user_id);
        Ok(Json(json!({ "status": "approved" })))
    } else {
        Err((StatusCode::NOT_FOUND, "invalid or expired code".into()))
    }
}
