//! Runtime configuration for the website / control-plane backend.

use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    /// Public apex domain used to render tunnel hostnames (e.g. "natforge.com").
    pub domain: String,
    /// Internal URL of the core proxy data plane.
    pub core_url: String,
    /// HMAC secret for session + tunnel JWTs (shared with the core proxy).
    pub jwt_secret: String,
    /// Shared secret required on internal API calls from the core proxy.
    pub internal_secret: String,
    /// Directory containing the static frontend to serve.
    pub frontend_dir: String,
    /// PostgreSQL connection string.
    pub database_url: String,
    /// Redis connection string.
    pub redis_url: String,
    /// Path to a MaxMind GeoLite2-Country.mmdb (empty = login geo-blocking off).
    pub geoip_db: String,
    /// Account that is granted the `admin` role on registration. When set, ONLY
    /// this email ever auto-becomes admin (no first-come race). Empty = fall back
    /// to the "first registered account is admin" bootstrap (dev convenience).
    pub admin_email: String,
}

impl Config {
    pub fn from_env() -> Self {
        Config {
            port: env_u16("PORT", 3000),
            domain: env::var("NATFORGE_DOMAIN").unwrap_or_else(|_| "natforge.com".to_string()),
            core_url: env::var("CORE_URL").unwrap_or_else(|_| "http://127.0.0.1:3001".to_string()),
            jwt_secret: env::var("JWT_SECRET")
                .unwrap_or_else(|_| "natforge-dev-secret-change-me".to_string()),
            internal_secret: env::var("INTERNAL_SECRET")
                .unwrap_or_else(|_| "natforge-internal-dev-secret".to_string()),
            frontend_dir: env::var("FRONTEND_DIR").unwrap_or_else(|_| "frontend".to_string()),
            database_url: env::var("DATABASE_URL").unwrap_or_else(|_| {
                "postgres://natforge:natforge@127.0.0.1:5432/natforge".to_string()
            }),
            redis_url: env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),
            geoip_db: env::var("GEOIP_DB").unwrap_or_default(),
            admin_email: env::var("ADMIN_EMAIL").unwrap_or_default().trim().to_lowercase(),
        }
    }
}

fn env_u16(key: &str, default: u16) -> u16 {
    env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
