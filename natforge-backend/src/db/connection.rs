//! Database connections and shared application state.
//!
//! Durable state (users, tunnels, routes, bandwidth, blocks, port pool) lives in
//! PostgreSQL; ephemeral state (RFC 8628 device codes, with TTL) lives in Redis.
//! `AppState::connect` fails fast with a clear message if either store is
//! unreachable, and runs migrations at boot. Each data-plane node seeds its own
//! TCP port pool when it self-registers (see `queries::register_node`).

use std::sync::Arc;

use anyhow::Context;
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

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
    pub geo: crate::geo::GeoDb,
    pub metrics: crate::metrics::Metrics,
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
                    "failed to connect to PostgreSQL at {} - is `docker compose up -d` running?",
                    config.database_url
                )
            })?;

        sqlx::migrate!("./migrations")
            .run(&pg)
            .await
            .context("failed to run database migrations")?;

        let redis_client =
            redis::Client::open(config.redis_url.clone()).context("invalid REDIS_URL")?;
        let redis = ConnectionManager::new(redis_client)
            .await
            .with_context(|| {
                format!(
                    "failed to connect to Redis at {} - is `docker compose up -d` running?",
                    config.redis_url
                )
            })?;

        let geo = crate::geo::GeoDb::open(&config.geoip_db);
        Ok(Arc::new(AppState {
            config,
            http: reqwest::Client::new(),
            db: Db { pg, redis },
            geo,
            metrics: crate::metrics::Metrics::new(),
        }))
    }
}

// --------------------------------------------------------------------------
// Redis-backed ephemeral state: the RFC 8628 device-authorization codes, which
// are naturally short-lived and a perfect fit for Redis key TTLs.
// --------------------------------------------------------------------------

pub const DEVCODE_TTL_SECS: i64 = 3600; // 1 hour

fn devcode_user_key(user_code: &str) -> String {
    format!("nf:devcode:{user_code}")
}
fn devcode_index_key(device_code: &str) -> String {
    format!("nf:devcode:dc:{device_code}")
}

#[derive(Debug)]
pub struct DeviceRecord {
    pub device_code: String,
    pub approved_user: Option<i32>,
    /// Set only for a *device enrollment* (the `devices.id` created on approval).
    pub device_id: Option<i64>,
}

/// Fixed-window rate limit: increment `key`, giving it a `window_secs` TTL on the first
/// hit of each window. Returns true while at or under `limit`, false once over. Used to
/// bound online guessing of the short device pairing code.
pub async fn rate_limit_hit(
    redis: &ConnectionManager,
    key: &str,
    limit: i64,
    window_secs: i64,
) -> anyhow::Result<bool> {
    let mut conn = redis.clone();
    let n: i64 = conn.incr(key, 1i64).await?;
    if n == 1 {
        let _: () = conn.expire(key, window_secs).await?;
    }
    Ok(n <= limit)
}

/// Store a fresh device/user code pair (both expire together after the TTL).
pub async fn devcode_create(
    redis: &ConnectionManager,
    user_code: &str,
    device_code: &str,
) -> anyhow::Result<()> {
    let mut conn = redis.clone();
    let uk = devcode_user_key(user_code);
    let _: () = conn
        .hset_multiple(
            &uk,
            &[
                ("device_code", device_code),
                ("approved_user", ""),
                ("device_id", ""),
            ],
        )
        .await
        .context("redis devcode hset")?;
    let _: () = conn.expire(&uk, DEVCODE_TTL_SECS).await?;
    let _: () = conn
        .set_ex(
            devcode_index_key(device_code),
            user_code,
            DEVCODE_TTL_SECS as u64,
        )
        .await?;
    Ok(())
}

/// Mark a user code approved by `uid`. Returns false if the code does not exist
/// (expired or never issued).
pub async fn devcode_approve(
    redis: &ConnectionManager,
    user_code: &str,
    uid: i32,
) -> anyhow::Result<bool> {
    let mut conn = redis.clone();
    let uk = devcode_user_key(user_code);
    let exists: bool = conn.exists(&uk).await?;
    if !exists {
        return Ok(false);
    }
    let _: () = conn.hset(&uk, "approved_user", uid.to_string()).await?;
    Ok(true)
}

/// Approve a user code as a *device enrollment*: record the approving user AND the
/// `devices.id` created for it, so the enroll poll can mint a device token. Returns
/// false if the code does not exist (expired or never issued).
pub async fn devcode_approve_device(
    redis: &ConnectionManager,
    user_code: &str,
    uid: i32,
    device_id: i64,
) -> anyhow::Result<bool> {
    let mut conn = redis.clone();
    let uk = devcode_user_key(user_code);
    let exists: bool = conn.exists(&uk).await?;
    if !exists {
        return Ok(false);
    }
    let _: () = conn
        .hset_multiple(
            &uk,
            &[
                ("approved_user", uid.to_string()),
                ("device_id", device_id.to_string()),
            ],
        )
        .await?;
    Ok(true)
}

/// Resolve a device code to its approval state. `None` => unknown/expired.
pub async fn devcode_status(
    redis: &ConnectionManager,
    device_code: &str,
) -> anyhow::Result<Option<DeviceRecord>> {
    let mut conn = redis.clone();
    let user_code: Option<String> = conn.get(devcode_index_key(device_code)).await?;
    let Some(user_code) = user_code else {
        return Ok(None);
    };
    let uk = devcode_user_key(&user_code);
    let fields: Option<(String, String, Option<String>)> = conn
        .hget(&uk, &["device_code", "approved_user", "device_id"])
        .await?;
    let Some((stored_device, approved, device_id_s)) = fields else {
        return Ok(None);
    };
    if stored_device != device_code {
        return Ok(None);
    }
    let approved_user = if approved.is_empty() {
        None
    } else {
        approved.parse::<i32>().ok()
    };
    let device_id_nonempty = match device_id_s {
        Some(s) => {
            if !s.is_empty() {
                Some(s)
            } else {
                None
            }
        }
        None => None,
    };
    let device_id = match device_id_nonempty {
        Some(s) => s.parse::<i64>().ok(),
        None => None,
    };
    Ok(Some(DeviceRecord {
        device_code: stored_device,
        approved_user,
        device_id,
    }))
}

/// Consume a device code once its session token has been issued, making it
/// **single-use** (RFC 8628 §3.5). The `DEL` on the index key is the atomic
/// single-winner: Redis serialises it, so of any concurrent polls exactly one
/// gets `removed == 1` and may issue the token; the rest see the code as gone.
/// The user-keyed hash is dropped too (idempotent).
pub async fn devcode_consume(redis: &ConnectionManager, device_code: &str) -> anyhow::Result<bool> {
    let mut conn = redis.clone();
    let ik = devcode_index_key(device_code);
    let user_code: Option<String> = conn.get(&ik).await?;
    let removed: i64 = conn.del(&ik).await?;
    if let Some(uc) = user_code {
        let _: () = conn.del(devcode_user_key(&uc)).await?;
    }
    Ok(removed == 1)
}
