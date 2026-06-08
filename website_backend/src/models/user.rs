//! Database row structs (sqlx `FromRow`) and API view structs.

use serde::{Deserialize, Serialize};

/// `users` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: i32,
    pub email: String,
    pub password_hash: String,
    pub role: String,
    pub max_tunnels: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// `tunnels` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TunnelRow {
    pub id: i64,
    pub subdomain: String,
    pub owner_id: i32,
    pub route_sig: String,
    pub status: String,
    pub public_host: String,
    pub node_id: Option<String>,
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
}

/// `ip_hosts` row.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct IpHostConfig {
    pub user_id: i32,
    pub active: bool,
    pub max_bandwidth_mbps: i32,
    pub geo_pref_only: bool,
    pub bytes_relayed: i64,
}

// --------------------------------------------------------------------------
// API view structs (serialized to the dashboard).
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct RouteView {
    pub route_id: u16,
    pub mode: String,
    pub local_port: i32,
    pub public_endpoint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TunnelView {
    pub tunnel_id: i64,
    pub subdomain: String,
    pub full_host: String,
    pub public_host: String,
    pub status: String,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub routes: Vec<RouteView>,
}

/// A route as requested by the agent/dashboard when reserving a tunnel.
#[derive(Debug, Clone, Deserialize)]
pub struct RequestedRoute {
    pub mode: natforge_proto::RouteMode,
    pub local_port: u16,
}
