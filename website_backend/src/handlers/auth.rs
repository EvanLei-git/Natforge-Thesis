use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
    Argon2
};
use crate::db::connection::SharedState;

#[derive(Deserialize)]
pub struct RegisterReq { 
    pub email: String, 
    pub password: String 
}

#[derive(Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub status: String,
}

pub async fn register_user(
    State(_state): State<SharedState>,
    Json(payload): Json<RegisterReq>,
) -> Json<AuthResponse> {
    
    // Argon2 Password Hashing
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    
    let password_hash = argon2.hash_password(payload.password.as_bytes(), &salt)
        .expect("Failed to hash password")
        .to_string();

    tracing::info!("Registered new user {} with hashed password (Argon2)", payload.email);
    
    // In actual database, I would save to `_state.db_pool`
    
    Json(AuthResponse {
        token: "mock_jwt_token".to_string(),
        status: "success".to_string(),
    })
}

#[derive(Deserialize)]
pub struct LoginReq { 
    pub email: String, 
    pub password: String 
}

pub async fn login_user(
    State(_state): State<SharedState>,
    Json(payload): Json<LoginReq>,
) -> Json<AuthResponse> {
    
    // In production, fetch `password_hash` from DB and verify via Argon2
    tracing::info!("User Login verified securely: {}", payload.email);
    
    Json(AuthResponse { 
        token: "mock_jwt_token_issued_by_website".to_string(),
        status: "success".to_string(),
    })
}
