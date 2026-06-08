//! Runtime configuration for the Core Proxy data-plane node.

use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    /// Internal signalling API (talks to website_backend).
    pub internal_api_port: u16,
    /// Agent control plane (yamux over TCP).
    pub control_port: u16,
    /// Shared HTTP listener (Host-header routing). Dev default 8080; prod 80.
    pub http_port: u16,
    /// Shared HTTPS listener (SNI passthrough routing). Dev default 8443; prod 443.
    pub https_port: u16,
    /// Hostname advertised to end-users / DNS.
    pub public_host: String,
    /// This node's id (matches the website's port_pool seeding).
    pub node_id: String,
    /// URL of the website_backend control plane.
    pub website_url: String,
    /// Redis URL (live routing mirror + rate limiting).
    pub redis_url: String,
    /// Shared HMAC secret for verifying tunnel tokens.
    pub jwt_secret: String,
    /// Shared secret for internal API calls.
    pub internal_secret: String,
    /// Max bytes to buffer while sniffing a Host header / TLS ClientHello.
    pub max_header_bytes: usize,
    pub cf_api_token: String,
    pub cf_zone_id: String,
}

impl Config {
    pub fn from_env() -> Self {
        Config {
            internal_api_port: env_u16("CORE_INTERNAL_PORT", 3001),
            control_port: env_u16("CORE_CONTROL_PORT", 4000),
            http_port: env_u16("HTTP_PORT", 8080),
            https_port: env_u16("HTTPS_PORT", 8443),
            public_host: env::var("PUBLIC_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
            node_id: env::var("NODE_ID").unwrap_or_else(|_| "edge-1".to_string()),
            website_url: env::var("WEBSITE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:3000".to_string()),
            redis_url: env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),
            jwt_secret: env::var("JWT_SECRET")
                .unwrap_or_else(|_| "natforge-dev-secret-change-me".to_string()),
            internal_secret: env::var("INTERNAL_SECRET")
                .unwrap_or_else(|_| "natforge-internal-dev-secret".to_string()),
            max_header_bytes: env::var("MAX_HEADER_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(16384),
            cf_api_token: env::var("CF_API_TOKEN").unwrap_or_else(|_| "mock_token".to_string()),
            cf_zone_id: env::var("CF_ZONE_ID").unwrap_or_else(|_| "mock_zone".to_string()),
        }
    }
}

fn env_u16(key: &str, default: u16) -> u16 {
    env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
