//! Database connections and shared application state.
//!
//! Durable state (users, tunnels, routes, bandwidth, blocks, port pool) lives in
//! PostgreSQL; ephemeral state (RFC 8628 device codes, with TTL) lives in Redis.
//! `AppState::connect` fails fast with a clear message if either store is
//! unreachable, and runs migrations + seeds this node's port pool at boot.

use std::sync::Arc;

use anyhow::Context;
use redis::aio::ConnectionManager;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use crate::config::Config;

#[derive(Clone)]
pub struct Db {
    pub pg: PgPool,
    pub redis: ConnectionManager,
}

pub struct AppState {
    pub config: Config,
    pub http: reqwest::Client,
    pub db: Db,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    pub async fn connect(config: Config) -> anyhow::Result<SharedState> {
        let pg = PgPoolOptions::new()
            .max_connections(10)
            .connect(&config.database_url)
            .await
            .with_context(|| {
                format!(
                    "failed to connect to PostgreSQL at {} — is `docker compose up -d` running?",
                    config.database_url
                )
            })?;

        sqlx::migrate!("./migrations")
            .run(&pg)
            .await
            .context("failed to run database migrations")?;

        seed_port_pool(&pg, &config).await?;

        let redis_client = redis::Client::open(config.redis_url.clone())
            .context("invalid REDIS_URL")?;
        let redis = ConnectionManager::new(redis_client)
            .await
            .with_context(|| {
                format!(
                    "failed to connect to Redis at {} — is `docker compose up -d` running?",
                    config.redis_url
                )
            })?;

        Ok(Arc::new(AppState {
            config,
            http: reqwest::Client::new(),
            db: Db { pg, redis },
        }))
    }
}

/// Seed this node's public TCP port pool. Idempotent (ON CONFLICT DO NOTHING),
/// so restarts and range extensions are safe and never disturb live allocations.
async fn seed_port_pool(pg: &PgPool, config: &Config) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO port_pool (node_id, port)
         SELECT $1, g FROM generate_series($2::int, $3::int) AS g
         ON CONFLICT (node_id, port) DO NOTHING",
    )
    .bind(&config.node_id)
    .bind(config.public_port_min as i32)
    .bind(config.public_port_max as i32)
    .execute(pg)
    .await
    .context("failed to seed port_pool")?;
    Ok(())
}
