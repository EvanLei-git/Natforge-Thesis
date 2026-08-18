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
}

impl Config {
    pub fn from_env() -> Self {
        let port = env_u16("PORT", 3000);
        let domain = match env::var("NATFORGE_DOMAIN") {
            Ok(v) => v,
            Err(_) => "natforge.com".to_string(),
        };
        let core_url = match env::var("CORE_URL") {
            Ok(v) => v,
            Err(_) => "http://127.0.0.1:3001".to_string(),
        };
        let jwt_secret = match env::var("JWT_SECRET") {
            Ok(v) => v,
            Err(_) => "natforge-dev-secret-change-me".to_string(),
        };
        let internal_secret = match env::var("INTERNAL_SECRET") {
            Ok(v) => v,
            Err(_) => "natforge-internal-dev-secret".to_string(),
        };
        let frontend_dir = match env::var("FRONTEND_DIR") {
            Ok(v) => v,
            Err(_) => "natforge-frontend".to_string(),
        };
        let database_url = match env::var("DATABASE_URL") {
            Ok(v) => v,
            Err(_) => "postgres://natforge:natforge@127.0.0.1:5432/natforge".to_string(),
        };
        let redis_url = match env::var("REDIS_URL") {
            Ok(v) => v,
            Err(_) => "redis://127.0.0.1:6379".to_string(),
        };
        let geoip_db = match env::var("GEOIP_DB") {
            Ok(v) => v,
            Err(_) => String::new(),
        };
        Config {
            port,
            domain,
            core_url,
            jwt_secret,
            internal_secret,
            frontend_dir,
            database_url,
            redis_url,
            geoip_db,
        }
    }
}

fn env_u16(key: &str, default: u16) -> u16 {
    let raw = env::var(key);
    let parsed = match raw {
        Ok(v) => v.parse().ok(),
        Err(_) => None,
    };
    match parsed {
        Some(n) => n,
        None => default,
    }
}
