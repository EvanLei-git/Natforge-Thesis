use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::{HashMap, HashSet};
use crate::models::user::TunnelInfo;

// In a fully deployed environment, this wraps sqlx::PgPool and redis::Client.
// For the professional structure definition, I establish the struct that will be shared across handlers.
#[derive(Default)]
pub struct AppState {
    pub active_tunnels: RwLock<HashMap<String, TunnelInfo>>,
    pub region_blocks: RwLock<HashSet<String>>,
    pub port_blocks: RwLock<HashSet<u16>>,
    // Mocking Postgres connection pool
    // pub db_pool: sqlx::PgPool, 
}

pub type SharedState = Arc<AppState>;
