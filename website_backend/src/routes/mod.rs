//! REST route table for the website backend. Maps every documented endpoint
//! (see `frontend/DOCUMENTATION.md`) onto its handler. Path params use axum 0.8
//! `{name}` syntax.

use axum::routing::{delete, get, patch, post, put};
use axum::Router;

use crate::db::connection::SharedState;
use crate::handlers::{admin, auth, internal, tunnels, user};

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
        .route(
            "/api/tunnels/{tunnel_id}",
            delete(tunnels::delete_tunnel).patch(tunnels::edit_tunnel),
        )
        .route("/api/tunnels/{tunnel_id}/stop", post(tunnels::stop_tunnel))
        .route("/api/tunnels/{tunnel_id}/bandwidth", get(tunnels::tunnel_bandwidth))
        .route("/api/tunnels/{tunnel_id}/logs", get(tunnels::tunnel_logs))
        .route(
            "/api/tunnels/{tunnel_id}/region_blocks",
            get(tunnels::get_tunnel_region_blocks).put(tunnels::set_tunnel_region_blocks),
        )
        .route("/api/regions", get(tunnels::list_regions))
        // --- User self-service ---
        .route("/api/user/profile", get(user::get_profile).put(user::update_profile))
        .route("/api/user/password", put(user::change_password))
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
        .route("/api/admin/users", get(admin::list_users))
        .route(
            "/api/admin/users/{user_id}",
            patch(admin::set_user_ban).delete(admin::delete_user),
        )
        .route("/api/admin/nodes", get(admin::list_nodes))
        .route(
            "/api/admin/nodes/{node_id}",
            patch(admin::update_node).delete(admin::delete_node),
        )
        // --- Internal (core proxy only) ---
        .route("/api/internal/tunnel_up", post(internal::tunnel_up))
        .route("/api/internal/tunnel_down", post(internal::tunnel_down))
        .route("/api/internal/bandwidth", post(internal::bandwidth))
        .route("/api/internal/conn_log", post(internal::conn_log))
        .route("/api/internal/policy", get(internal::policy))
        .route("/api/internal/node_register", post(internal::node_register))
        .with_state(state)
}
