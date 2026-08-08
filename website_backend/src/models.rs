//! Database row structs (sqlx `FromRow`) and API view structs.

use serde::{Deserialize, Serialize};

/// `users` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: i32,
    pub email: String,
    pub name: Option<String>,
    pub password_hash: String,
    pub role: String,
    pub banned: bool,
    pub max_tunnels: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// `devices` row. A device is a persistent, enrolled agent that owns services.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Device {
    pub id: i64,
    pub owner_id: i32,
    pub name: String,
    /// Nonce of the issued device token; matched on every device-token use so a
    /// deleted device revokes its token. Never exposed to the API.
    #[serde(skip_serializing)]
    pub token_fp: Option<String>,
    pub status: String,
    pub agent_ip: Option<String>,
    pub last_seen: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// `tunnels` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TunnelRow {
    pub id: i64,
    pub subdomain: String,
    pub name: Option<String>,
    pub owner_id: i32,
    pub route_sig: String,
    pub status: String,
    pub public_host: String,
    pub node_id: Option<String>,
    pub agent_ip: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_seen: Option<chrono::DateTime<chrono::Utc>>,
}

/// `routes` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RouteRow {
    pub id: i64,
    pub tunnel_id: i64,
    pub route_id: i16,
    pub kind: String,
    pub local_port: i32,
    pub public_port: Option<i32>,
    pub label: Option<String>,
    /// Optional SRV service label; when set, the data plane provisions a DNS SRV record.
    pub srv_service: Option<String>,
}

/// `nodes` row - a data-plane VM / region.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Node {
    pub node_id: String,
    pub name: String,
    pub region: Option<String>,
    pub public_host: String,
    pub control_endpoint: String,
    pub internal_url: String,
    pub http_port: i32,
    pub https_port: i32,
    pub active: bool,
    /// SHA-256 fingerprint of the node's self-signed control certificate.
    pub control_cert_fp: Option<String>,
    pub last_seen: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// One logged public connection to a tunnel (or a geo-blocked attempt).
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct ConnLog {
    pub id: i64,
    pub route_id: i16,
    pub kind: String,
    pub peer_ip: String,
    pub country: Option<String>,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub duration_ms: i64,
    pub blocked: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// A point on a tunnel's cumulative bandwidth curve (one reporter snapshot).
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct BandwidthSample {
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
}

/// Aggregated per-user row for the admin Users view.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct UserOverview {
    pub id: i32,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
    pub banned: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub tunnel_count: i64,
    pub total_bytes: i64,
    pub last_seen: Option<chrono::DateTime<chrono::Utc>>,
}

// --------------------------------------------------------------------------
// API view structs (serialized to the dashboard).
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct RouteView {
    pub route_id: u16,
    pub mode: String,
    pub local_port: i32,
    /// The dedicated public port for tcp/udp/both; None for host-routed http/https
    /// (which share the wildcard host on 80/443).
    pub public_port: Option<i32>,
    pub public_endpoint: String,
    pub label: Option<String>,
    /// Optional SRV service label the owner set on this route (e.g. "minecraft").
    pub srv_service: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TunnelView {
    pub tunnel_id: i64,
    pub subdomain: String,
    pub name: Option<String>,
    pub full_host: String,
    pub public_host: String,
    pub status: String,
    pub agent_ip: Option<String>,
    pub owner_id: i32,
    /// Owner identity for the admin tunnel list (email always present, name optional).
    pub owner_email: Option<String>,
    pub owner_name: Option<String>,
    /// The node hosting this tunnel + its human region label (for the location UI).
    pub node_id: Option<String>,
    pub region: Option<String>,
    /// The device this tunnel is a service of, if any (groups services under devices).
    pub device_id: Option<i64>,
    /// User-owned hostname fronting this tunnel, if set.
    pub custom_domain: Option<String>,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Last time this service host carried real traffic (in or out). Drives the client
    /// idle countdown; None until the first byte flows (client falls back to created_at).
    pub last_active_at: Option<chrono::DateTime<chrono::Utc>>,
    pub routes: Vec<RouteView>,
}

/// A route as requested by the agent/dashboard when reserving a tunnel. `label`
/// is optional free-text so users can name "GTA server", "web", etc.
#[derive(Debug, Clone, Deserialize)]
pub struct RequestedRoute {
    pub mode: natforge_proto::RouteMode,
    pub local_port: u16,
    #[serde(default)]
    pub label: Option<String>,
}
