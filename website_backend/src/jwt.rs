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
    decode::<SessionClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map(|d| d.claims)
    .map_err(|e| e.to_string())
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
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or((StatusCode::UNAUTHORIZED, "missing authorization header"))?;
        let token = header
            .strip_prefix("Bearer ")
            .ok_or((StatusCode::UNAUTHORIZED, "malformed authorization header"))?;
        let claims = verify_session(&state.config.jwt_secret, token)
            .map_err(|_| (StatusCode::UNAUTHORIZED, "invalid or expired token"))?;
        Ok(AuthUser {
            user_id: claims.sub,
            email: claims.email,
            role: claims.role,
        })
    }
}
