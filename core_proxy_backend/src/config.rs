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
    /// Wildcard apex this node serves / advertises to end-users + DNS
    /// (e.g. "natforge.com" or "bg.natforge.com").
    pub public_host: String,
    /// This node's id (matches the website's port_pool seeding).
    pub node_id: String,
    /// Human-friendly node name shown in the admin panel + region dropdown.
    pub node_name: String,
    /// Optional human region label (e.g. "Germany", "Bulgaria").
    pub node_region: Option<String>,
    /// host:port the agent connects to for the yamux control plane. Defaults to
    /// `<public_host>:<control_port>`; override when behind NAT / a load balancer.
    pub control_endpoint: String,
    /// How the website reaches THIS node's internal API (website → core).
    pub internal_url: String,
    /// Inclusive public TCP port pool this node owns (seeded on registration).
    pub public_port_min: i32,
    pub public_port_max: i32,
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
    /// Path to a MaxMind GeoLite2-Country.mmdb (empty = geo-blocking disabled).
    pub geoip_db: String,
    /// Where the bare apex / www host on :80 is forwarded - the control-plane
    /// dashboard (`website_backend`). Subdomains remain tunnel routes.
    pub dashboard_addr: String,
    pub cf_api_token: String,
    pub cf_zone_id: String,
    /// Optional PEM cert + key for terminating public HTTPS on http-mode user
    /// subdomains with a `*.<public_host>` wildcard certificate. Unset = disabled.
    pub wildcard_cert_path: Option<String>,
    pub wildcard_key_path: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        let control_port = env_u16("CORE_CONTROL_PORT", 4000);
        let internal_api_port = env_u16("CORE_INTERNAL_PORT", 3001);
        let public_host = env::var("PUBLIC_HOST").unwrap_or_else(|_| "natforge.com".to_string());
        Config {
            internal_api_port,
            control_port,
            http_port: env_u16("HTTP_PORT", 8080),
            https_port: env_u16("HTTPS_PORT", 8443),
            node_id: env::var("NODE_ID").unwrap_or_else(|_| "edge-1".to_string()),
            node_name: env::var("NODE_NAME").unwrap_or_else(|_| "Local".to_string()),
            node_region: env::var("NODE_REGION")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            control_endpoint: env::var("CONTROL_ENDPOINT")
                .unwrap_or_else(|_| format!("{public_host}:{control_port}")),
            internal_url: env::var("INTERNAL_URL")
                .unwrap_or_else(|_| format!("http://127.0.0.1:{internal_api_port}")),
            public_port_min: env_u16("PUBLIC_PORT_MIN", 20000) as i32,
            public_port_max: env_u16("PUBLIC_PORT_MAX", 20100) as i32,
            public_host,
            website_url: env::var("WEBSITE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:3000".to_string()),
            redis_url: env::var("REDIS_URL")
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),
            jwt_secret: env::var("JWT_SECRET")
                .unwrap_or_else(|_| "natforge-dev-secret-change-me".to_string()),
            internal_secret: env::var("INTERNAL_SECRET")
                .unwrap_or_else(|_| "natforge-internal-dev-secret".to_string()),
            max_header_bytes: env::var("MAX_HEADER_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(16384),
            geoip_db: env::var("GEOIP_DB").unwrap_or_default(),
            dashboard_addr: env::var("DASHBOARD_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:3000".to_string()),
            cf_api_token: env::var("CF_API_TOKEN").unwrap_or_else(|_| "mock_token".to_string()),
            cf_zone_id: env::var("CF_ZONE_ID").unwrap_or_else(|_| "mock_zone".to_string()),
            wildcard_cert_path: env::var("WILDCARD_CERT_PATH")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            wildcard_key_path: env::var("WILDCARD_KEY_PATH")
                .ok()
                .filter(|s| !s.trim().is_empty()),
        }
    }
}

fn env_u16(key: &str, default: u16) -> u16 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
