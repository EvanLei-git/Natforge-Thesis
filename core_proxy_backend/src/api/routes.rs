use axum::{routing::post, Router, Json};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct TunnelAllocationReq {
    pub end_user_id: String,
    pub target_subdomain: String,
}

#[derive(Serialize)]
pub struct TunnelAllocationRes {
    pub allocated_tcp_port: u16,
    pub allocated_udp_port: u16,
    pub wireguard_pubkey: String,
}

/// Endpoint called strictly by the Internal `website_backend` or authorized daemons 
/// to spin up a new high-speed proxy tunnel.
async fn allocate_tunnel(Json(payload): Json<TunnelAllocationReq>) -> Json<TunnelAllocationRes> {
    tracing::info!("Core Engine allocating Anycast Proxy for {} on {}", payload.end_user_id, payload.target_subdomain);
    
    // In production, this dynamically binds a TCP and UDP listener based on available system ports.
    Json(TunnelAllocationRes {
        allocated_tcp_port: 25565,
        allocated_udp_port: 25565,
        wireguard_pubkey: "mock_wg_pubkey_xyz123".to_string(),
    })
}

pub fn core_routes() -> Router {
    Router::new()
        .route("/internal/allocate_tunnel", post(allocate_tunnel))
}
