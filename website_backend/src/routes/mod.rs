//! REST route table for the website backend. Maps every documented endpoint
//! (see `frontend/DOCUMENTATION.md`) onto its handler. Path params use axum 0.8
//! `{name}` syntax.

use axum::routing::{delete, get, post, put};
use axum::Router;

use crate::db::connection::SharedState;
use crate::handlers::{admin, auth, internal, iphost, tunnels};

pub fn api_router(state: SharedState) -> Router {
    Router::new()
        // --- Authentication ---
        .route("/api/auth/register", post(auth::register_user))
        .route("/api/auth/login", post(auth::login_user))
        .route("/api/auth/device/start", post(auth::device_start))
        .route("/api/auth/device/token", post(auth::device_token))
        .route("/api/auth/device", post(auth::device_approve))
        // --- Service-host tunnels ---
        .route("/api/tunnels", get(tunnels::get_tunnels))
        .route("/api/tunnels/request", post(tunnels::request_tunnel))
        .route("/api/tunnels/{tunnel_id}", delete(tunnels::stop_tunnel))
        // --- IP host / edge node ---
        .route("/api/ip_host/status", get(iphost::get_status).post(iphost::set_relay_status))
        .route("/api/user/preferences", put(iphost::update_preferences))
        // --- Admin ---
        .route(
            "/api/admin/region_blocks",
            get(admin::get_region_blocks).post(admin::add_region_block),
        )
        .route(
            "/api/admin/region_blocks/{country_code}",
            delete(admin::remove_region_block),
        )
        .route(
            "/api/admin/port_blocks",
            get(admin::get_port_blocks).post(admin::add_port_block),
        )
        .route("/api/admin/port_blocks/{port}", delete(admin::remove_port_block))
        .route("/api/admin/stats", get(admin::network_stats))
        .route("/api/admin/tunnels", get(admin::all_tunnels))
        // --- Internal (core proxy only) ---
        .route("/api/internal/tunnel_up", post(internal::tunnel_up))
        .route("/api/internal/tunnel_down", post(internal::tunnel_down))
        .route("/api/internal/bandwidth", post(internal::bandwidth))
        .route("/api/internal/policy", get(internal::policy))
        .with_state(state)
}
