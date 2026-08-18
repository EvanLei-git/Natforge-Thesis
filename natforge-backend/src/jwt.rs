//! JWT minting + verification and the `AuthUser` request extractor.
//!
//! Session tokens authorize the REST API; multi-route tunnel tokens (shape defined
//! in `natforge_proto::TunnelClaims`) authorize the data-plane handshake.

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use natforge_proto::{RouteClaim, TunnelClaims};

const SESSION_TTL_SECS: i64 = 60 * 60 * 24; // 24h
const TUNNEL_TTL_SECS: i64 = 60 * 60; // 1h (idempotent reservation keeps reconnects stable)
const DEVICE_TTL_SECS: i64 = 60 * 60 * 24 * 365; // 1y; revoked out-of-band via the device's nonce

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionClaims {
    pub sub: i32,
    pub email: String,
    pub role: String,
    pub exp: usize,
}

pub fn issue_session(secret: &str, user_id: i32, email: &str, role: &str) -> String {
    let claims = SessionClaims {
        sub: user_id,
        email: email.to_string(),
        role: role.to_string(),
        exp: (now() + SESSION_TTL_SECS) as usize,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("jwt encode")
}

pub fn issue_tunnel_token(
    secret: &str,
    user_id: i32,
    tunnel_id: i64,
    subdomain: &str,
    routes: Vec<RouteClaim>,
    custom_domain: Option<String>,
) -> String {
    let claims = TunnelClaims {
        v: 1,
        sub: user_id,
        tunnel_id,
        subdomain: subdomain.to_string(),
        purpose: "tunnel".to_string(),
        routes,
        custom_domain,
        exp: (now() + TUNNEL_TTL_SECS) as usize,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("jwt encode")
}

pub fn verify_session(secret: &str, token: &str) -> Result<SessionClaims, String> {
    let validation = Validation::new(Algorithm::HS256);
    let decoded = decode::<SessionClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    );
    match decoded {
        Ok(d) => Ok(d.claims),
        Err(e) => Err(e.to_string()),
    }
}

/// Pull the raw token out of an `Authorization: Bearer <token>` header.
fn bearer_token(parts: &Parts) -> Result<&str, (StatusCode, &'static str)> {
    let auth_value = parts.headers.get(axum::http::header::AUTHORIZATION);
    let auth_str = match auth_value {
        Some(v) => v.to_str().ok(),
        None => None,
    };
    let header = match auth_str {
        Some(h) => h,
        None => return Err((StatusCode::UNAUTHORIZED, "missing authorization header")),
    };
    match header.strip_prefix("Bearer ") {
        Some(rest) => Ok(rest),
        None => Err((StatusCode::UNAUTHORIZED, "malformed authorization header")),
    }
}

/// Authenticated user, extracted from the `Authorization: Bearer` header.
#[derive(Clone, Debug)]
pub struct AuthUser {
    pub user_id: i32,
    pub email: String,
    pub role: String,
}

impl FromRequestParts<crate::db::connection::SharedState> for AuthUser {
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &crate::db::connection::SharedState,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer_token(parts)?;
        let claims = match verify_session(&state.config.jwt_secret, token) {
            Ok(c) => c,
            Err(_) => return Err((StatusCode::UNAUTHORIZED, "invalid or expired token")),
        };
        Ok(AuthUser {
            user_id: claims.sub,
            email: claims.email,
            role: claims.role,
        })
    }
}

// ---------------------------------------------------------------------------
// Device tokens: long-lived credentials a persistent, enrolled agent uses to
// pull its config and reserve its services. Carries a `nonce` that must match
// the device's stored `token_fp`, so deleting the device revokes the token.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceClaims {
    pub sub: i32,        // owner user id
    pub device_id: i64,  // devices.id
    pub nonce: String,   // must equal devices.token_fp (revocation)
    pub purpose: String, // "device"
    pub exp: usize,
}

pub fn issue_device_token(secret: &str, owner_id: i32, device_id: i64, nonce: &str) -> String {
    let claims = DeviceClaims {
        sub: owner_id,
        device_id,
        nonce: nonce.to_string(),
        purpose: "device".to_string(),
        exp: (now() + DEVICE_TTL_SECS) as usize,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("jwt encode")
}

pub fn verify_device_token(secret: &str, token: &str) -> Result<DeviceClaims, String> {
    let validation = Validation::new(Algorithm::HS256);
    let decoded = decode::<DeviceClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    );
    let claims = match decoded {
        Ok(d) => d.claims,
        Err(e) => return Err(e.to_string()),
    };
    if claims.purpose != "device" {
        return Err("not a device token".to_string());
    }
    Ok(claims)
}

/// An enrolled device, extracted from a `Bearer` device token and verified against
/// the stored nonce so a deleted (or rotated) device's token is rejected at once.
#[derive(Clone, Debug)]
pub struct DeviceAuth {
    pub owner_id: i32,
    pub device_id: i64,
}

impl FromRequestParts<crate::db::connection::SharedState> for DeviceAuth {
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &crate::db::connection::SharedState,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer_token(parts)?;
        let claims = match verify_device_token(&state.config.jwt_secret, token) {
            Ok(c) => c,
            Err(_) => return Err((StatusCode::UNAUTHORIZED, "invalid device token")),
        };
        let device_row = crate::db::queries::device_by_id(&state.db.pg, claims.device_id).await;
        let device_opt = match device_row {
            Ok(d) => d,
            Err(_) => return Err((StatusCode::INTERNAL_SERVER_ERROR, "database error")),
        };
        let device = match device_opt {
            Some(d) => d,
            None => return Err((StatusCode::UNAUTHORIZED, "device not found")),
        };
        if device.token_fp.as_deref() != Some(claims.nonce.as_str()) {
            return Err((StatusCode::UNAUTHORIZED, "device token revoked"));
        }
        Ok(DeviceAuth {
            owner_id: device.owner_id,
            device_id: device.id,
        })
    }
}
