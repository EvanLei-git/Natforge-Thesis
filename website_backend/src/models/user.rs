use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct User {
    pub id: i32,
    pub email: String,
    pub password_hash: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TunnelInfo {
    pub subdomain: String,
    pub allocated_tcp_port: u16,
    pub allocated_udp_port: u16,
    pub status: String,
}
