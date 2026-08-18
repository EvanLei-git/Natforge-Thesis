//! Runtime configuration for the Core Proxy data-plane node.

use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    /// Internal signalling API (talks to natforge-backend).
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
    /// URL of the natforge-backend control plane.
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
    /// dashboard (`natforge-backend`). Subdomains remain tunnel routes.
    pub dashboard_addr: String,
    pub cf_api_token: String,
    pub cf_zone_id: String,
    /// Optional PEM cert + key for terminating public HTTPS on http-mode user
    /// subdomains with a `*.<public_host>` wildcard certificate. Unset = disabled.
    pub wildcard_cert_path: Option<String>,
    pub wildcard_key_path: Option<String>,
    /// Automatic per-domain HTTPS (ACME/Let's Encrypt HTTP-01) for custom domains.
    pub acme_enabled: bool,
    pub acme_email: String,
    /// Directory for issued custom-domain certs (per-domain subdirectories).
    pub acme_dir: String,
    /// Use the Let's Encrypt staging environment.
    pub acme_staging: bool,
    /// Override the ACME directory URL (e.g. a local pebble server for tests).
    pub acme_directory_url: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        let control_port = env_u16("CORE_CONTROL_PORT", 4000);
        let internal_api_port = env_u16("CORE_INTERNAL_PORT", 3001);
        let public_host = match env::var("PUBLIC_HOST") {
            Ok(v) => v,
            Err(_) => "natforge.com".to_string(),
        };
        let node_id = match env::var("NODE_ID") {
            Ok(v) => v,
            Err(_) => "edge-1".to_string(),
        };
        let node_name = match env::var("NODE_NAME") {
            Ok(v) => v,
            Err(_) => "Local".to_string(),
        };
        let node_region = match env::var("NODE_REGION") {
            Ok(s) => {
                if !s.trim().is_empty() {
                    Some(s)
                } else {
                    None
                }
            }
            Err(_) => None,
        };
        let control_endpoint = match env::var("CONTROL_ENDPOINT") {
            Ok(v) => v,
            Err(_) => format!("{public_host}:{control_port}"),
        };
        let internal_url = match env::var("INTERNAL_URL") {
            Ok(v) => v,
            Err(_) => format!("http://127.0.0.1:{internal_api_port}"),
        };
        let website_url = match env::var("WEBSITE_URL") {
            Ok(v) => v,
            Err(_) => "http://127.0.0.1:3000".to_string(),
        };
        let redis_url = match env::var("REDIS_URL") {
            Ok(v) => v,
            Err(_) => "redis://127.0.0.1:6379".to_string(),
        };
        let jwt_secret = match env::var("JWT_SECRET") {
            Ok(v) => v,
            Err(_) => "natforge-dev-secret-change-me".to_string(),
        };
        let internal_secret = match env::var("INTERNAL_SECRET") {
            Ok(v) => v,
            Err(_) => "natforge-internal-dev-secret".to_string(),
        };
        let max_header_bytes = match env::var("MAX_HEADER_BYTES") {
            Ok(v) => match v.parse() {
                Ok(n) => n,
                Err(_) => 16384,
            },
            Err(_) => 16384,
        };
        let geoip_db = match env::var("GEOIP_DB") {
            Ok(v) => v,
            Err(_) => String::new(),
        };
        let dashboard_addr = match env::var("DASHBOARD_ADDR") {
            Ok(v) => v,
            Err(_) => "127.0.0.1:3000".to_string(),
        };
        let cf_api_token = match env::var("CF_API_TOKEN") {
            Ok(v) => v,
            Err(_) => "mock_token".to_string(),
        };
        let cf_zone_id = match env::var("CF_ZONE_ID") {
            Ok(v) => v,
            Err(_) => "mock_zone".to_string(),
        };
        let wildcard_cert_path = match env::var("WILDCARD_CERT_PATH") {
            Ok(s) => {
                if !s.trim().is_empty() {
                    Some(s)
                } else {
                    None
                }
            }
            Err(_) => None,
        };
        let wildcard_key_path = match env::var("WILDCARD_KEY_PATH") {
            Ok(s) => {
                if !s.trim().is_empty() {
                    Some(s)
                } else {
                    None
                }
            }
            Err(_) => None,
        };
        let acme_email = match env::var("ACME_EMAIL") {
            Ok(v) => v,
            Err(_) => String::new(),
        };
        let acme_dir = match env::var("ACME_DIR") {
            Ok(v) => v,
            Err(_) => "/etc/natforge/acme".to_string(),
        };
        let acme_directory_url = match env::var("ACME_DIRECTORY_URL") {
            Ok(s) => {
                if !s.trim().is_empty() {
                    Some(s)
                } else {
                    None
                }
            }
            Err(_) => None,
        };
        Config {
            internal_api_port,
            control_port,
            http_port: env_u16("HTTP_PORT", 8080),
            https_port: env_u16("HTTPS_PORT", 8443),
            node_id,
            node_name,
            node_region,
            control_endpoint,
            internal_url,
            public_port_min: env_u16("PUBLIC_PORT_MIN", 20000) as i32,
            public_port_max: env_u16("PUBLIC_PORT_MAX", 20100) as i32,
            public_host,
            website_url,
            redis_url,
            jwt_secret,
            internal_secret,
            max_header_bytes,
            geoip_db,
            dashboard_addr,
            cf_api_token,
            cf_zone_id,
            wildcard_cert_path,
            wildcard_key_path,
            acme_enabled: env_bool("ACME_ENABLED", false),
            acme_email,
            acme_dir,
            acme_staging: env_bool("ACME_STAGING", false),
            acme_directory_url,
        }
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    match env::var(key) {
        Ok(v) => v == "1" || v.eq_ignore_ascii_case("true"),
        Err(_) => default,
    }
}

fn env_u16(key: &str, default: u16) -> u16 {
    let parsed = match env::var(key) {
        Ok(v) => match v.parse() {
            Ok(n) => Some(n),
            Err(_) => None,
        },
        Err(_) => None,
    };
    match parsed {
        Some(n) => n,
        None => default,
    }
}
