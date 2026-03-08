use axum::{Router, routing::post};
use crate::handlers::auth::{register_user, login_user};
use crate::db::connection::SharedState;

pub fn initialize_routes(state: SharedState) -> Router {
    Router::new()
        .route("/api/auth/register", post(register_user))
        .route("/api/auth/login", post(login_user))
        .with_state(state)
}
