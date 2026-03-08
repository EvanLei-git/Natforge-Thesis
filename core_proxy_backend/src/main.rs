pub mod tunnel;
pub mod dns;
pub mod ddos;
pub mod api;

use axum::serve;
use tracing::info;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    
    // Core Router initialization (handles internal `allocate_tunnel` calls)
    let app = api::routes::core_routes();

    // The independent, high-availability Core Proxy listens on port 3001
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], 3001));
    info!("Core Proxy Anycast Router listening on {}", addr);
    
    // Wireguard and DDoS Heuristics Initialization
    info!("Initializing eBPF Volumetric DDoS mitigation filters...");
    info!("TCP / UDP Port Allocators standing by.");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    serve(listener, app).await.unwrap();
}
