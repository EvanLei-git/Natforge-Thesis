pub mod models;
pub mod db;
pub mod routes;
pub mod handlers;

use std::sync::Arc;
use tokio::sync::RwLock;
use axum::{
    routing::{get, post, put, delete},
    Router,
    Json,
    extract::{State, Path},
};
use axum::serve;
use serde::{Deserialize, Serialize};

use db::connection::AppState;
use routes::auth_routes::initialize_routes;
use models::user::TunnelInfo;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    
    // In production, I would initialize `sqlx::PgPool` here.
    let shared_state = Arc::new(AppState::default());

    // Setup initial mock admin blocklists
    shared_state.region_blocks.write().await.insert("RU".to_string());
    shared_state.port_blocks.write().await.insert(25);
    shared_state.port_blocks.write().await.insert(465);

    let auth_router = initialize_routes(shared_state.clone());

    let app = Router::new()
        .merge(auth_router)
        // Service Host Flow
        .route("/api/tunnels", get(get_tunnels))
        .route("/api/tunnels/request", post(request_tunnel))
        .route("/api/tunnels/:subdomain", delete(stop_tunnel))
        // IP Host Flow
        .route("/api/ip_host/status", post(set_relay_status))
        .route("/api/user/preferences", put(update_preferences))
        // Admin Flow (Region Blocking)
        .route("/api/admin/region_blocks", get(get_region_blocks).post(add_region_block))
        .route("/api/admin/region_blocks/:country_code", delete(remove_region_block))
        .route("/api/admin/port_blocks", post(add_port_block))
        .route("/api/admin/port_blocks/:port", delete(remove_port_block))
        .route("/api/register", post(legacy_register_node)) // Legacy from previous step
        .with_state(shared_state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], 3000));
    tracing::info!("Website Backend (Auth/Billing) listening on {}", addr);
    
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    serve(listener, app).await.unwrap();
}

// ==========================================
// Handlers that haven't been modularized yet
// ==========================================

async fn get_tunnels(State(state): State<Arc<AppState>>) -> Json<Vec<TunnelInfo>> {
    let tunnels = state.active_tunnels.read().await;
    let list: Vec<TunnelInfo> = tunnels.values().cloned().collect();
    Json(list)
}

async fn request_tunnel(State(state): State<Arc<AppState>>) -> Json<TunnelInfo> {
    let subdomain = "duck-new".to_string(); // In reality, generate randomly
    let info = TunnelInfo {
        subdomain: subdomain.clone(),
        allocated_tcp_port: 25565,
        allocated_udp_port: 25565,
        status: "allocating".to_string(),
    };
    state.active_tunnels.write().await.insert(subdomain, info.clone());
    Json(info)
}

async fn stop_tunnel(State(state): State<Arc<AppState>>, Path(subdomain): Path<String>) -> Json<serde_json::Value> {
    state.active_tunnels.write().await.remove(&subdomain);
    Json(serde_json::json!({ "status": "tunnel_closed" }))
}

#[derive(Deserialize)]
struct RelayStatusReq { active: bool }
async fn set_relay_status(Json(payload): Json<RelayStatusReq>) -> Json<serde_json::Value> {
    tracing::info!("IP Host Relay Active: {}", payload.active);
    Json(serde_json::json!({ "status": "updated" }))
}

#[derive(Deserialize)]
struct PrefReq { max_bandwidth_mbps: u32, geo_pref_only: bool }
async fn update_preferences(Json(payload): Json<PrefReq>) -> Json<serde_json::Value> {
    tracing::info!("Updated BW: {} Mbps, strict Geo: {}", payload.max_bandwidth_mbps, payload.geo_pref_only);
    Json(serde_json::json!({ "status": "updated" }))
}

async fn get_region_blocks(State(state): State<Arc<AppState>>) -> Json<Vec<String>> {
    let blocks = state.region_blocks.read().await;
    let list: Vec<String> = blocks.iter().cloned().collect();
    Json(list)
}

#[derive(Deserialize)]
struct RegionBlockReq { country_code: String }
async fn add_region_block(State(state): State<Arc<AppState>>, Json(payload): Json<RegionBlockReq>) -> Json<serde_json::Value> {
    tracing::warn!("Global Region Ban Added: {}", payload.country_code);
    state.region_blocks.write().await.insert(payload.country_code);
    Json(serde_json::json!({ "status": "banned" }))
}

async fn remove_region_block(State(state): State<Arc<AppState>>, Path(country_code): Path<String>) -> Json<serde_json::Value> {
    state.region_blocks.write().await.remove(&country_code);
    Json(serde_json::json!({ "status": "unbanned" }))
}

#[derive(Deserialize)]
struct PortBlockReq { port: u16 }
async fn add_port_block(State(state): State<Arc<AppState>>, Json(payload): Json<PortBlockReq>) -> Json<serde_json::Value> {
    state.port_blocks.write().await.insert(payload.port);
    Json(serde_json::json!({ "status": "banned" }))
}

async fn remove_port_block(State(state): State<Arc<AppState>>, Path(port): Path<u16>) -> Json<serde_json::Value> {
    state.port_blocks.write().await.remove(&port);
    Json(serde_json::json!({ "status": "unbanned" }))
}

// Legacy register for proxy_node compile check
#[derive(Deserialize)]
struct RegisterRequest { role: String, subdomain_req: Option<String> }
#[derive(Serialize)]
struct RegisterResponse { status: String, allocated_subdomain: Option<String>, assigned_port: Option<u16> }

async fn legacy_register_node(Json(payload): Json<RegisterRequest>) -> Json<RegisterResponse> {
    let (subdomain, port) = if payload.role == "service_host" {
        (payload.subdomain_req.or(Some("duck-main".to_string())), Some(25565))
    } else {
        (None, None)
    };
    Json(RegisterResponse { status: "registered".to_string(), allocated_subdomain: subdomain, assigned_port: port })
}
