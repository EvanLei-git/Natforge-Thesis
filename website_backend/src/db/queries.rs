//! PostgreSQL query layer (runtime sqlx — no compile-time `query!` macros, so the
//! build never needs a live database). All `route_id`/port values are stored as
//! `i16`/`i32` (Postgres has no `u16`) and cast to `u16` only in application code.

use anyhow::Context;
use rand::Rng;
use sqlx::{PgPool, Postgres, Transaction};

use crate::models::user::{IpHostConfig, RouteRow, RouteView, TunnelRow, TunnelView, User};
use natforge_proto::RouteMode;

// --------------------------------------------------------------------------
// Users
// --------------------------------------------------------------------------

/// Create a user. The very first account becomes the administrator.
pub async fn create_user(pg: &PgPool, email: &str, password_hash: &str) -> anyhow::Result<User> {
    let user = sqlx::query_as::<_, User>(
        "INSERT INTO users (email, password_hash, role)
         VALUES ($1, $2, CASE WHEN (SELECT count(*) FROM users) = 0 THEN 'admin' ELSE 'user' END)
         RETURNING id, email, password_hash, role, max_tunnels, created_at",
    )
    .bind(email)
    .bind(password_hash)
    .fetch_one(pg)
    .await?;
    Ok(user)
}

pub async fn user_by_email(pg: &PgPool, email: &str) -> anyhow::Result<Option<User>> {
    Ok(sqlx::query_as::<_, User>(
        "SELECT id, email, password_hash, role, max_tunnels, created_at FROM users WHERE email = $1",
    )
    .bind(email)
    .fetch_optional(pg)
    .await?)
}

pub async fn user_by_id(pg: &PgPool, id: i32) -> anyhow::Result<Option<User>> {
    Ok(sqlx::query_as::<_, User>(
        "SELECT id, email, password_hash, role, max_tunnels, created_at FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pg)
    .await?)
}

// --------------------------------------------------------------------------
// Tunnel reservation
// --------------------------------------------------------------------------

#[derive(Debug)]
pub struct ReservedTunnel {
    pub tunnel_id: i64,
    pub subdomain: String,
    pub routes: Vec<RouteRow>,
    pub reused: bool,
}

#[derive(Debug)]
pub enum ReserveError {
    LimitReached(i32),
    PortExhausted,
    BlockedPort(u16),
    Db(anyhow::Error),
}

const WORDS: &[&str] = &[
    "duck", "fox", "wolf", "owl", "lynx", "hawk", "puma", "crow", "newt", "bear", "elk", "ibis",
];

fn random_subdomain() -> String {
    let mut rng = rand::thread_rng();
    let word = WORDS[rng.gen_range(0..WORDS.len())];
    let suffix: String = (0..4)
        .map(|_| b"abcdefghijklmnopqrstuvwxyz0123456789"[rng.gen_range(0..36)] as char)
        .collect();
    format!("{word}-{suffix}")
}

/// Canonical signature of a requested route set: sorted "mode:local_port" joined
/// by ",". Two requests with the same signature reuse the same tunnel.
pub fn route_signature(routes: &[(RouteMode, u16)]) -> String {
    let mut parts: Vec<String> = routes
        .iter()
        .map(|(m, p)| format!("{}:{}", m.as_str(), p))
        .collect();
    parts.sort();
    parts.join(",")
}

async fn load_routes(
    tx: &mut Transaction<'_, Postgres>,
    tunnel_id: i64,
) -> anyhow::Result<Vec<RouteRow>> {
    Ok(sqlx::query_as::<_, RouteRow>(
        "SELECT id, tunnel_id, route_id, kind, local_port, public_port
         FROM routes WHERE tunnel_id = $1 ORDER BY route_id",
    )
    .bind(tunnel_id)
    .fetch_all(&mut **tx)
    .await?)
}

/// Atomically and idempotently reserve a tunnel for `owner_id`. If a tunnel with
/// the same owner + route signature already exists it is reused (so reconnecting
/// agents keep their subdomain and ports). Otherwise a fresh subdomain is
/// allocated, TCP ports are popped from the pool, and rows are inserted — all in
/// one transaction, so any failure rolls back cleanly (no port leak).
pub async fn reserve_tunnel(
    pg: &PgPool,
    owner_id: i32,
    node_id: &str,
    public_host: &str,
    requested: &[(RouteMode, u16)],
    max_tunnels: i32,
) -> Result<ReservedTunnel, ReserveError> {
    let sig = route_signature(requested);
    let mut tx = pg.begin().await.map_err(|e| ReserveError::Db(e.into()))?;

    // Idempotent reuse.
    if let Some(existing) = sqlx::query_as::<_, TunnelRow>(
        "SELECT id, subdomain, owner_id, route_sig, status, public_host, node_id, created_at, last_seen
         FROM tunnels WHERE owner_id = $1 AND route_sig = $2",
    )
    .bind(owner_id)
    .bind(&sig)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ReserveError::Db(e.into()))?
    {
        let routes = load_routes(&mut tx, existing.id).await.map_err(ReserveError::Db)?;
        tx.commit().await.map_err(|e| ReserveError::Db(e.into()))?;
        return Ok(ReservedTunnel {
            tunnel_id: existing.id,
            subdomain: existing.subdomain,
            routes,
            reused: true,
        });
    }

    // Per-user tunnel cap.
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tunnels WHERE owner_id = $1")
        .bind(owner_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ReserveError::Db(e.into()))?;
    if count >= max_tunnels as i64 {
        return Err(ReserveError::LimitReached(max_tunnels));
    }

    // Insert the tunnel row, retrying subdomain collisions.
    let mut tunnel_id: Option<i64> = None;
    let mut subdomain = String::new();
    for _ in 0..8 {
        let cand = random_subdomain();
        let reserved: bool = sqlx::query_scalar(
            "SELECT exists(SELECT 1 FROM reserved_subdomains WHERE name = $1)",
        )
        .bind(&cand)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ReserveError::Db(e.into()))?;
        if reserved {
            continue;
        }
        let res = sqlx::query_scalar::<_, i64>(
            "INSERT INTO tunnels (subdomain, owner_id, route_sig, status, public_host, node_id)
             VALUES ($1, $2, $3, 'awaiting_agent', $4, $5) RETURNING id",
        )
        .bind(&cand)
        .bind(owner_id)
        .bind(&sig)
        .bind(public_host)
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await;
        match res {
            Ok(id) => {
                tunnel_id = Some(id);
                subdomain = cand;
                break;
            }
            Err(sqlx::Error::Database(db)) if db.constraint() == Some("tunnels_owner_route_uq") => {
                // Concurrent identical request won the race — reuse theirs.
                let existing = sqlx::query_as::<_, TunnelRow>(
                    "SELECT id, subdomain, owner_id, route_sig, status, public_host, node_id, created_at, last_seen
                     FROM tunnels WHERE owner_id = $1 AND route_sig = $2",
                )
                .bind(owner_id)
                .bind(&sig)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| ReserveError::Db(e.into()))?;
                let routes = load_routes(&mut tx, existing.id).await.map_err(ReserveError::Db)?;
                tx.commit().await.map_err(|e| ReserveError::Db(e.into()))?;
                return Ok(ReservedTunnel {
                    tunnel_id: existing.id,
                    subdomain: existing.subdomain,
                    routes,
                    reused: true,
                });
            }
            Err(sqlx::Error::Database(db)) if db.constraint() == Some("tunnels_subdomain_uq") => {
                continue; // subdomain collision; try another
            }
            Err(e) => return Err(ReserveError::Db(e.into())),
        }
    }
    let tunnel_id = tunnel_id.ok_or(ReserveError::Db(anyhow::anyhow!(
        "could not allocate a unique subdomain after several attempts"
    )))?;

    // Insert routes; allocate a dedicated TCP port per tcp route.
    let mut out = Vec::with_capacity(requested.len());
    for (i, (mode, local_port)) in requested.iter().enumerate() {
        let route_id = (i as i16) + 1;
        let public_port: Option<i32> = if *mode == RouteMode::Tcp {
            let port: Option<i32> = sqlx::query_scalar(
                "WITH picked AS (
                     SELECT port FROM port_pool
                     WHERE node_id = $1 AND tunnel_id IS NULL
                     ORDER BY port FOR UPDATE SKIP LOCKED LIMIT 1
                 )
                 UPDATE port_pool p SET tunnel_id = $2, route_id = $3
                 FROM picked WHERE p.node_id = $1 AND p.port = picked.port
                 RETURNING p.port",
            )
            .bind(node_id)
            .bind(tunnel_id)
            .bind(route_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| ReserveError::Db(e.into()))?;
            match port {
                Some(p) => Some(p),
                None => return Err(ReserveError::PortExhausted), // rollback frees everything
            }
        } else {
            None
        };

        sqlx::query(
            "INSERT INTO routes (tunnel_id, route_id, kind, local_port, public_port)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(tunnel_id)
        .bind(route_id)
        .bind(mode.as_str())
        .bind(*local_port as i32)
        .bind(public_port)
        .execute(&mut *tx)
        .await
        .map_err(|e| ReserveError::Db(e.into()))?;

        out.push(RouteRow {
            id: 0,
            tunnel_id,
            route_id,
            kind: mode.as_str().to_string(),
            local_port: *local_port as i32,
            public_port,
        });
    }

    tx.commit().await.map_err(|e| ReserveError::Db(e.into()))?;
    Ok(ReservedTunnel { tunnel_id, subdomain, routes: out, reused: false })
}

// --------------------------------------------------------------------------
// Tunnel views + lifecycle
// --------------------------------------------------------------------------

fn route_endpoint(kind: &str, subdomain: &str, domain: &str, public_host: &str, public_port: Option<i32>) -> String {
    match kind {
        "http" => format!("http://{subdomain}.{domain}"),
        "https" => format!("https://{subdomain}.{domain}"),
        _ => format!("{}:{}", public_host, public_port.unwrap_or(0)),
    }
}

async fn build_views(pg: &PgPool, tunnels: Vec<TunnelRow>, domain: &str) -> anyhow::Result<Vec<TunnelView>> {
    let mut views = Vec::with_capacity(tunnels.len());
    for t in tunnels {
        let routes = sqlx::query_as::<_, RouteRow>(
            "SELECT id, tunnel_id, route_id, kind, local_port, public_port
             FROM routes WHERE tunnel_id = $1 ORDER BY route_id",
        )
        .bind(t.id)
        .fetch_all(pg)
        .await?;
        let bw: Option<(i64, i64)> = sqlx::query_as(
            "SELECT bytes_in, bytes_out FROM bandwidth_logs
             WHERE tunnel_id = $1 ORDER BY recorded_at DESC LIMIT 1",
        )
        .bind(t.id)
        .fetch_optional(pg)
        .await?;
        let (bytes_in, bytes_out) = bw.unwrap_or((0, 0));
        let route_views = routes
            .into_iter()
            .map(|r| RouteView {
                route_id: r.route_id as u16,
                mode: r.kind.clone(),
                local_port: r.local_port,
                public_endpoint: route_endpoint(&r.kind, &t.subdomain, domain, &t.public_host, r.public_port),
            })
            .collect();
        views.push(TunnelView {
            tunnel_id: t.id,
            full_host: format!("{}.{}", t.subdomain, domain),
            subdomain: t.subdomain,
            public_host: t.public_host,
            status: t.status,
            bytes_in,
            bytes_out,
            created_at: t.created_at,
            routes: route_views,
        });
    }
    Ok(views)
}

pub async fn tunnels_for_owner(pg: &PgPool, owner_id: i32, domain: &str) -> anyhow::Result<Vec<TunnelView>> {
    let tunnels = sqlx::query_as::<_, TunnelRow>(
        "SELECT id, subdomain, owner_id, route_sig, status, public_host, node_id, created_at, last_seen
         FROM tunnels WHERE owner_id = $1 ORDER BY created_at DESC",
    )
    .bind(owner_id)
    .fetch_all(pg)
    .await?;
    build_views(pg, tunnels, domain).await
}

pub async fn all_tunnels(pg: &PgPool, domain: &str) -> anyhow::Result<Vec<TunnelView>> {
    let tunnels = sqlx::query_as::<_, TunnelRow>(
        "SELECT id, subdomain, owner_id, route_sig, status, public_host, node_id, created_at, last_seen
         FROM tunnels ORDER BY created_at DESC",
    )
    .fetch_all(pg)
    .await?;
    build_views(pg, tunnels, domain).await
}

/// Returns (owner_id, subdomain) for a tunnel, used to authorize stop + locate it.
pub async fn tunnel_owner_subdomain(pg: &PgPool, tunnel_id: i64) -> anyhow::Result<Option<(i32, String)>> {
    Ok(sqlx::query_as::<_, (i32, String)>(
        "SELECT owner_id, subdomain FROM tunnels WHERE id = $1",
    )
    .bind(tunnel_id)
    .fetch_optional(pg)
    .await?)
}

pub async fn set_tunnel_online(pg: &PgPool, tunnel_id: i64, node_id: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE tunnels SET status='online', node_id=$2, last_seen=now() WHERE id=$1")
        .bind(tunnel_id)
        .bind(node_id)
        .execute(pg)
        .await?;
    Ok(())
}

pub async fn set_tunnel_offline(pg: &PgPool, tunnel_id: i64) -> anyhow::Result<()> {
    sqlx::query("UPDATE tunnels SET status='offline', last_seen=now() WHERE id=$1")
        .bind(tunnel_id)
        .execute(pg)
        .await?;
    Ok(())
}

/// Delete a tunnel and release its ports back to the pool.
pub async fn delete_tunnel(pg: &PgPool, tunnel_id: i64) -> anyhow::Result<()> {
    let mut tx = pg.begin().await?;
    sqlx::query("UPDATE port_pool SET tunnel_id=NULL, route_id=NULL WHERE tunnel_id=$1")
        .bind(tunnel_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM tunnels WHERE id=$1")
        .bind(tunnel_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn append_bandwidth(pg: &PgPool, tunnel_id: i64, owner_id: i32, bytes_in: i64, bytes_out: i64) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO bandwidth_logs (tunnel_id, owner_id, bytes_in, bytes_out) VALUES ($1,$2,$3,$4)",
    )
    .bind(tunnel_id)
    .bind(owner_id)
    .bind(bytes_in)
    .bind(bytes_out)
    .execute(pg)
    .await?;
    sqlx::query("UPDATE tunnels SET last_seen=now() WHERE id=$1")
        .bind(tunnel_id)
        .execute(pg)
        .await?;
    Ok(())
}

/// Reclaim ports + delete tunnels that have been silent past the grace period
/// (covers crashed agents/cores whose `tunnel_down` never arrived).
pub async fn reconcile_abandoned(pg: &PgPool, grace_secs: i64) -> anyhow::Result<u64> {
    let mut tx = pg.begin().await?;
    let stale: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM tunnels
         WHERE status <> 'awaiting_agent'
           AND COALESCE(last_seen, created_at) < now() - make_interval(secs => $1)",
    )
    .bind(grace_secs as f64)
    .fetch_all(&mut *tx)
    .await?;
    if stale.is_empty() {
        return Ok(0);
    }
    sqlx::query("UPDATE port_pool SET tunnel_id=NULL, route_id=NULL WHERE tunnel_id = ANY($1)")
        .bind(&stale)
        .execute(&mut *tx)
        .await?;
    let deleted = sqlx::query("DELETE FROM tunnels WHERE id = ANY($1)")
        .bind(&stale)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(deleted)
}

pub async fn is_port_blocked(pg: &PgPool, port: u16) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar("SELECT exists(SELECT 1 FROM port_blocks WHERE port = $1)")
        .bind(port as i32)
        .fetch_one(pg)
        .await?)
}

// --------------------------------------------------------------------------
// IP host (edge node)
// --------------------------------------------------------------------------

pub async fn ip_host_get(pg: &PgPool, user_id: i32) -> anyhow::Result<IpHostConfig> {
    let row = sqlx::query_as::<_, IpHostConfig>(
        "SELECT user_id, active, max_bandwidth_mbps, geo_pref_only, bytes_relayed
         FROM ip_hosts WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pg)
    .await?;
    Ok(row.unwrap_or(IpHostConfig {
        user_id,
        active: false,
        max_bandwidth_mbps: 100,
        geo_pref_only: false,
        bytes_relayed: 0,
    }))
}

pub async fn ip_host_set_active(pg: &PgPool, user_id: i32, active: bool) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO ip_hosts (user_id, active) VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE SET active = $2, updated_at = now()",
    )
    .bind(user_id)
    .bind(active)
    .execute(pg)
    .await?;
    Ok(())
}

pub async fn ip_host_set_prefs(pg: &PgPool, user_id: i32, mbps: i32, geo: bool) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO ip_hosts (user_id, max_bandwidth_mbps, geo_pref_only) VALUES ($1, $2, $3)
         ON CONFLICT (user_id) DO UPDATE SET max_bandwidth_mbps = $2, geo_pref_only = $3, updated_at = now()",
    )
    .bind(user_id)
    .bind(mbps)
    .bind(geo)
    .execute(pg)
    .await?;
    Ok(())
}

// --------------------------------------------------------------------------
// Admin policy + stats
// --------------------------------------------------------------------------

pub async fn region_blocks(pg: &PgPool) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT country_code FROM region_blocks ORDER BY country_code")
        .fetch_all(pg)
        .await?)
}

pub async fn add_region_block(pg: &PgPool, code: &str) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO region_blocks(country_code) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(code)
        .execute(pg)
        .await?;
    Ok(())
}

pub async fn remove_region_block(pg: &PgPool, code: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM region_blocks WHERE country_code = $1")
        .bind(code)
        .execute(pg)
        .await?;
    Ok(())
}

pub async fn port_blocks(pg: &PgPool) -> anyhow::Result<Vec<i32>> {
    Ok(sqlx::query_scalar("SELECT port FROM port_blocks ORDER BY port")
        .fetch_all(pg)
        .await?)
}

pub async fn add_port_block(pg: &PgPool, port: u16) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO port_blocks(port) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(port as i32)
        .execute(pg)
        .await?;
    Ok(())
}

pub async fn remove_port_block(pg: &PgPool, port: u16) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM port_blocks WHERE port = $1")
        .bind(port as i32)
        .execute(pg)
        .await?;
    Ok(())
}

pub struct Stats {
    pub total_users: i64,
    pub active_tunnels: i64,
    pub active_edge_nodes: i64,
    pub total_bytes_relayed: i64,
    pub blocked_regions: i64,
    pub blocked_ports: i64,
}

pub async fn stats(pg: &PgPool) -> anyhow::Result<Stats> {
    let total_users: i64 = sqlx::query_scalar("SELECT count(*) FROM users").fetch_one(pg).await?;
    let active_tunnels: i64 = sqlx::query_scalar("SELECT count(*) FROM tunnels WHERE status='online'")
        .fetch_one(pg)
        .await?;
    let active_edge_nodes: i64 = sqlx::query_scalar("SELECT count(*) FROM ip_hosts WHERE active")
        .fetch_one(pg)
        .await?;
    // Latest snapshot per tunnel, summed.
    // sum(bigint) yields NUMERIC in Postgres; cast back to bigint for i64 decode.
    let total_bytes: Option<i64> = sqlx::query_scalar(
        "SELECT COALESCE(sum(b.bytes_in + b.bytes_out), 0)::bigint FROM (
            SELECT DISTINCT ON (tunnel_id) tunnel_id, bytes_in, bytes_out
            FROM bandwidth_logs ORDER BY tunnel_id, recorded_at DESC
         ) b",
    )
    .fetch_one(pg)
    .await
    .context("stats bandwidth")?;
    let blocked_regions: i64 = sqlx::query_scalar("SELECT count(*) FROM region_blocks").fetch_one(pg).await?;
    let blocked_ports: i64 = sqlx::query_scalar("SELECT count(*) FROM port_blocks").fetch_one(pg).await?;
    Ok(Stats {
        total_users,
        active_tunnels,
        active_edge_nodes,
        total_bytes_relayed: total_bytes.unwrap_or(0),
        blocked_regions,
        blocked_ports,
    })
}
