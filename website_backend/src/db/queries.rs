//! PostgreSQL query layer (runtime sqlx - no compile-time `query!` macros, so the
//! build never needs a live database). All `route_id`/port values are stored as
//! `i16`/`i32` (Postgres has no `u16`) and cast to `u16` only in application code.

use anyhow::Context;
use rand::Rng;
use sqlx::{PgPool, Postgres, Transaction};

use crate::models::{Device, RouteRow, RouteView, TunnelRow, TunnelView, User};
use natforge_proto::RouteMode;

// --------------------------------------------------------------------------
// Users
// --------------------------------------------------------------------------

/// Create a user. New accounts always get the default `user` role; admin is granted
/// manually in the database (`UPDATE users SET role='admin' WHERE email=...`).
pub async fn create_user(pg: &PgPool, email: &str, password_hash: &str) -> anyhow::Result<User> {
    let user = sqlx::query_as::<_, User>(
        "INSERT INTO users (email, password_hash)
         VALUES ($1, $2)
         RETURNING id, email, name, password_hash, role, banned, max_tunnels, created_at",
    )
    .bind(email)
    .bind(password_hash)
    .fetch_one(pg)
    .await?;
    Ok(user)
}

pub async fn user_by_email(pg: &PgPool, email: &str) -> anyhow::Result<Option<User>> {
    Ok(sqlx::query_as::<_, User>(
        "SELECT id, email, name, password_hash, role, banned, max_tunnels, created_at FROM users WHERE email = $1",
    )
    .bind(email)
    .fetch_optional(pg)
    .await?)
}

pub async fn user_by_id(pg: &PgPool, id: i32) -> anyhow::Result<Option<User>> {
    Ok(sqlx::query_as::<_, User>(
        "SELECT id, email, name, password_hash, role, banned, max_tunnels, created_at FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pg)
    .await?)
}

/// The per-user cap on how many service hosts (tunnels) an account may own. Defaults to
/// 10 for a missing row (should not happen for an authenticated caller).
pub async fn user_max_tunnels(pg: &PgPool, id: i32) -> anyhow::Result<i32> {
    Ok(
        sqlx::query_scalar::<_, i32>("SELECT max_tunnels FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(pg)
            .await?
            .unwrap_or(10),
    )
}

// --------------------------------------------------------------------------
// Devices (persistent, enrolled agents that own services)
// --------------------------------------------------------------------------

const DEVICE_COLS: &str = "id, owner_id, name, token_fp, status, agent_ip, last_seen, created_at";

pub async fn create_device(pg: &PgPool, owner_id: i32, name: &str) -> anyhow::Result<Device> {
    Ok(sqlx::query_as::<_, Device>(&format!(
        "INSERT INTO devices (owner_id, name) VALUES ($1, $2) RETURNING {DEVICE_COLS}"
    ))
    .bind(owner_id)
    .bind(name)
    .fetch_one(pg)
    .await?)
}

pub async fn list_devices(pg: &PgPool, owner_id: i32) -> anyhow::Result<Vec<Device>> {
    Ok(sqlx::query_as::<_, Device>(&format!(
        "SELECT {DEVICE_COLS} FROM devices WHERE owner_id = $1 ORDER BY created_at"
    ))
    .bind(owner_id)
    .fetch_all(pg)
    .await?)
}

pub async fn device_by_id(pg: &PgPool, id: i64) -> anyhow::Result<Option<Device>> {
    Ok(
        sqlx::query_as::<_, Device>(&format!("SELECT {DEVICE_COLS} FROM devices WHERE id = $1"))
            .bind(id)
            .fetch_optional(pg)
            .await?,
    )
}

/// Bind a freshly-issued device token's nonce to the device and mark it online.
pub async fn set_device_token(pg: &PgPool, id: i64, nonce: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE devices SET token_fp = $1, status = 'online', last_seen = now() WHERE id = $2",
    )
    .bind(nonce)
    .bind(id)
    .execute(pg)
    .await?;
    Ok(())
}

pub async fn rename_device(
    pg: &PgPool,
    id: i64,
    owner_id: i32,
    name: &str,
) -> anyhow::Result<bool> {
    let r = sqlx::query("UPDATE devices SET name = $1 WHERE id = $2 AND owner_id = $3")
        .bind(name)
        .bind(id)
        .bind(owner_id)
        .execute(pg)
        .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn delete_device(pg: &PgPool, id: i64, owner_id: i32) -> anyhow::Result<bool> {
    let r = sqlx::query("DELETE FROM devices WHERE id = $1 AND owner_id = $2")
        .bind(id)
        .bind(owner_id)
        .execute(pg)
        .await?;
    Ok(r.rows_affected() > 0)
}

/// The service tunnels belonging to a device.
pub async fn device_service_tunnels(pg: &PgPool, device_id: i64) -> anyhow::Result<Vec<TunnelRow>> {
    Ok(sqlx::query_as::<_, TunnelRow>(
        "SELECT id, subdomain, name, owner_id, route_sig, status, public_host, node_id, agent_ip, created_at, last_seen
         FROM tunnels WHERE device_id = $1 ORDER BY id",
    )
    .bind(device_id)
    .fetch_all(pg)
    .await?)
}

/// Load a tunnel's routes (non-transactional; used to build a device's config).
pub async fn routes_for_tunnel(pg: &PgPool, tunnel_id: i64) -> anyhow::Result<Vec<RouteRow>> {
    Ok(sqlx::query_as::<_, RouteRow>(
        "SELECT id, tunnel_id, route_id, kind, local_port, public_port, label
         FROM routes WHERE tunnel_id = $1 ORDER BY route_id",
    )
    .bind(tunnel_id)
    .fetch_all(pg)
    .await?)
}

// --------------------------------------------------------------------------
// Tunnel reservation
// --------------------------------------------------------------------------

#[derive(Debug)]
pub struct ReservedTunnel {
    pub tunnel_id: i64,
    pub subdomain: String,
    pub node_id: String,
    pub public_host: String,
    pub routes: Vec<RouteRow>,
    pub custom_domain: Option<String>,
    pub reused: bool,
}

#[derive(Debug)]
pub enum ReserveError {
    LimitReached(i32),
    PortExhausted,
    BadSubdomain,
    SubdomainTaken(String),
    /// The route-set collides with another of the owner's tunnels *on the same device*
    /// (the per-device `(owner_id, device_id, route_sig)` uniqueness). Carries the
    /// colliding signature.
    RouteSetExists(String),
    Db(anyhow::Error),
}

/// Validate a user-chosen subdomain label: 3–30 chars, lowercase a–z/0–9/'-',
/// must start and end alphanumeric (a valid DNS label).
pub fn valid_subdomain(s: &str) -> bool {
    let n = s.len();
    if !(3..=30).contains(&n) {
        return false;
    }
    let b = s.as_bytes();
    let alnum = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    if !alnum(b[0]) || !alnum(b[n - 1]) {
        return false;
    }
    s.bytes().all(|c| alnum(c) || c == b'-')
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
        "SELECT id, tunnel_id, route_id, kind, local_port, public_port, label
         FROM routes WHERE tunnel_id = $1 ORDER BY route_id",
    )
    .bind(tunnel_id)
    .fetch_all(&mut **tx)
    .await?)
}

async fn load_custom_domain(
    tx: &mut Transaction<'_, Postgres>,
    tunnel_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, Option<String>>("SELECT custom_domain FROM tunnels WHERE id = $1")
        .bind(tunnel_id)
        .fetch_one(&mut **tx)
        .await
}

/// Set (or clear, with `None`) a tunnel's custom domain. A duplicate hostname trips
/// the `tunnels_custom_domain_uq` unique index, surfaced as a sqlx database error.
pub async fn set_custom_domain(
    pg: &PgPool,
    tunnel_id: i64,
    domain: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE tunnels SET custom_domain = $2 WHERE id = $1")
        .bind(tunnel_id)
        .bind(domain)
        .execute(pg)
        .await?;
    Ok(())
}

/// Move a tunnel to another node/region: free the ports it holds on its current
/// node, reallocate a port from the target node's pool for each tcp/udp route, and
/// point the tunnel at the target node + its wildcard host. All in one transaction,
/// so a full target pool rolls the whole move back. The subdomain is unchanged.
pub async fn migrate_tunnel(
    pg: &PgPool,
    tunnel_id: i64,
    target_node_id: &str,
    target_public_host: &str,
) -> Result<(), ReserveError> {
    let mut tx = pg.begin().await.map_err(|e| ReserveError::Db(e.into()))?;
    let routes = load_routes(&mut tx, tunnel_id)
        .await
        .map_err(ReserveError::Db)?;
    // Free the ports this tunnel holds on its current node.
    sqlx::query("UPDATE port_pool SET tunnel_id=NULL, route_id=NULL WHERE tunnel_id=$1")
        .bind(tunnel_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ReserveError::Db(e.into()))?;
    // Reallocate a dedicated port from the target node's pool per tcp/udp route.
    for r in &routes {
        if r.kind != "tcp" && r.kind != "udp" {
            continue;
        }
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
        .bind(target_node_id)
        .bind(tunnel_id)
        .bind(r.route_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ReserveError::Db(e.into()))?;
        let Some(port) = port else {
            return Err(ReserveError::PortExhausted); // rollback frees everything
        };
        sqlx::query("UPDATE routes SET public_port=$1 WHERE tunnel_id=$2 AND route_id=$3")
            .bind(port)
            .bind(tunnel_id)
            .bind(r.route_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| ReserveError::Db(e.into()))?;
    }
    sqlx::query("UPDATE tunnels SET node_id=$2, public_host=$3 WHERE id=$1")
        .bind(tunnel_id)
        .bind(target_node_id)
        .bind(target_public_host)
        .execute(&mut *tx)
        .await
        .map_err(|e| ReserveError::Db(e.into()))?;
    tx.commit().await.map_err(|e| ReserveError::Db(e.into()))?;
    Ok(())
}

/// Reconcile a tunnel's routes to exactly `requested`, in one transaction, without
/// tearing the tunnel down. Existing routes matched by `(kind, local_port)` keep
/// their `route_id`, dedicated port and label; routes no longer requested are dropped
/// and their pool port freed; newly requested routes get the next dense `route_id`
/// and (for tcp/udp) a freshly popped pool port. The tunnel's `route_sig` is updated
/// to the new set so idempotent reuse and the `(owner_id, route_sig)` uniqueness stay
/// consistent. The running agent picks the change up on its next reconnect (the caller
/// signals the node to drop the live session). Returns the tunnel's full route list.
pub async fn set_service_routes(
    pg: &PgPool,
    tunnel_id: i64,
    node_id: &str,
    requested: &[(RouteMode, u16, Option<String>)],
) -> Result<Vec<RouteRow>, ReserveError> {
    let mut tx = pg.begin().await.map_err(|e| ReserveError::Db(e.into()))?;
    let existing = load_routes(&mut tx, tunnel_id)
        .await
        .map_err(ReserveError::Db)?;

    // Keys present in the desired final state, as (kind, local_port).
    let want: Vec<(&'static str, i32)> = requested
        .iter()
        .map(|(m, p, _)| (m.as_str(), *p as i32))
        .collect();

    // 1) Drop existing routes that are no longer requested (free the pool port first).
    for r in &existing {
        let still = want.iter().any(|(k, p)| *k == r.kind && *p == r.local_port);
        if !still {
            sqlx::query(
                "UPDATE port_pool SET tunnel_id=NULL, route_id=NULL
                 WHERE tunnel_id=$1 AND route_id=$2",
            )
            .bind(tunnel_id)
            .bind(r.route_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| ReserveError::Db(e.into()))?;
            sqlx::query("DELETE FROM routes WHERE tunnel_id=$1 AND route_id=$2")
                .bind(tunnel_id)
                .bind(r.route_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| ReserveError::Db(e.into()))?;
        }
    }

    // 2) Add newly requested routes, keyed off the current max route_id so dense ids
    // never collide with a kept route.
    let mut next_route_id: i16 = existing.iter().map(|r| r.route_id).max().unwrap_or(0);
    for (mode, local_port, label) in requested {
        let already = existing
            .iter()
            .any(|r| r.kind == mode.as_str() && r.local_port == *local_port as i32);
        if already {
            continue;
        }
        next_route_id += 1;
        let label = label
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let public_port: Option<i32> = if mode.is_host_routed() {
            None
        } else {
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
            .bind(next_route_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| ReserveError::Db(e.into()))?;
            match port {
                Some(p) => Some(p),
                None => return Err(ReserveError::PortExhausted), // rollback frees everything
            }
        };
        sqlx::query(
            "INSERT INTO routes (tunnel_id, route_id, kind, local_port, public_port, label)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(tunnel_id)
        .bind(next_route_id)
        .bind(mode.as_str())
        .bind(*local_port as i32)
        .bind(public_port)
        .bind(&label)
        .execute(&mut *tx)
        .await
        .map_err(|e| ReserveError::Db(e.into()))?;
    }

    // 3) Keep the tunnel's route signature in step with its actual routes so reuse
    // and the per-device (owner_id, device_id, route_sig) uniqueness remain correct.
    let pairs: Vec<(RouteMode, u16)> = requested.iter().map(|(m, p, _)| (*m, *p)).collect();
    let sig = route_signature(&pairs);
    if let Err(e) = sqlx::query("UPDATE tunnels SET route_sig=$2 WHERE id=$1")
        .bind(tunnel_id)
        .bind(&sig)
        .execute(&mut *tx)
        .await
    {
        return match e {
            sqlx::Error::Database(db)
                if db.constraint() == Some("tunnels_owner_device_route_uq") =>
            {
                Err(ReserveError::RouteSetExists(sig))
            }
            other => Err(ReserveError::Db(other.into())),
        };
    }

    let out = load_routes(&mut tx, tunnel_id)
        .await
        .map_err(ReserveError::Db)?;
    tx.commit().await.map_err(|e| ReserveError::Db(e.into()))?;
    Ok(out)
}

/// The device a tunnel is a service of, if any.
pub async fn tunnel_device_id(pg: &PgPool, tunnel_id: i64) -> anyhow::Result<Option<i64>> {
    Ok(
        sqlx::query_scalar::<_, Option<i64>>("SELECT device_id FROM tunnels WHERE id = $1")
            .bind(tunnel_id)
            .fetch_optional(pg)
            .await?
            .flatten(),
    )
}

/// The (protocol, local_port) endpoints already claimed by a device's OTHER services
/// (every service except `exclude_tunnel_id`; pass a non-existent id such as 0 to mean
/// "all of them"). Used to keep each device's local endpoints unique across its
/// services so two services never fight over the same local socket. tcp:N and udp:N
/// are distinct sockets, so uniqueness is on the pair, not the port alone.
pub async fn device_routes_excluding(
    pg: &PgPool,
    device_id: i64,
    exclude_tunnel_id: i64,
) -> anyhow::Result<Vec<(String, i32)>> {
    Ok(sqlx::query_as::<_, (String, i32)>(
        "SELECT r.kind, r.local_port FROM routes r
         JOIN tunnels t ON t.id = r.tunnel_id
         WHERE t.device_id = $1 AND t.id <> $2",
    )
    .bind(device_id)
    .bind(exclude_tunnel_id)
    .fetch_all(pg)
    .await?)
}

async fn insert_tunnel(
    tx: &mut Transaction<'_, Postgres>,
    cand: &str,
    owner_id: i32,
    sig: &str,
    public_host: &str,
    node_id: &str,
    device_id: Option<i64>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO tunnels (subdomain, owner_id, route_sig, status, public_host, node_id, device_id)
         VALUES ($1, $2, $3, 'awaiting_agent', $4, $5, $6) RETURNING id",
    )
    .bind(cand)
    .bind(owner_id)
    .bind(sig)
    .bind(public_host)
    .bind(node_id)
    .bind(device_id)
    .fetch_one(&mut **tx)
    .await
}

async fn is_reserved_name(
    tx: &mut Transaction<'_, Postgres>,
    name: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT exists(SELECT 1 FROM reserved_subdomains WHERE name = $1)")
        .bind(name)
        .fetch_one(&mut **tx)
        .await
}

/// Atomically and idempotently reserve a tunnel for `owner_id` on `device_id` (None for
/// a device-less CLI service host). Idempotency + uniqueness are scoped to the device,
/// so the same route set on two DIFFERENT devices is allowed (each machine has its own
/// local ports), while a reconnecting agent on the same device (or CLI) still reuses its
/// tunnel. Otherwise a subdomain is allocated - the user's chosen one if
/// `custom_subdomain` is given (validated and must be free), else a random one - TCP
/// ports are popped from the pool, and rows are inserted, all in one transaction.
#[allow(clippy::too_many_arguments)] // each argument is a distinct reservation input
pub async fn reserve_tunnel(
    pg: &PgPool,
    owner_id: i32,
    node_id: &str,
    public_host: &str,
    requested: &[(RouteMode, u16, Option<String>)],
    custom_subdomain: Option<&str>,
    max_tunnels: i32,
    allow_reuse: bool,
    device_id: Option<i64>,
) -> Result<ReservedTunnel, ReserveError> {
    let pairs: Vec<(RouteMode, u16)> = requested.iter().map(|(m, p, _)| (*m, *p)).collect();
    let sig = route_signature(&pairs);
    let mut tx = pg.begin().await.map_err(|e| ReserveError::Db(e.into()))?;

    // A tunnel with the same (owner, device, route signature) already exists. When reuse
    // is allowed (a reconnecting agent presenting the same route set) we return it so it
    // keeps its subdomain and ports; when it is not (an explicit "create a new service
    // host" from the dashboard) the same set on the same device is a conflict. The device
    // scope is what lets two different devices expose the same local port.
    let existing = sqlx::query_as::<_, TunnelRow>(
        "SELECT id, subdomain, name, owner_id, route_sig, status, public_host, node_id, agent_ip, created_at, last_seen
         FROM tunnels
         WHERE owner_id = $1 AND route_sig = $2 AND COALESCE(device_id, 0) = COALESCE($3::bigint, 0)
         FOR UPDATE",
    )
    .bind(owner_id)
    .bind(&sig)
    .bind(device_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ReserveError::Db(e.into()))?;
    if let Some(existing) = existing {
        if !allow_reuse {
            return Err(ReserveError::RouteSetExists(existing.subdomain));
        }
        let routes = load_routes(&mut tx, existing.id)
            .await
            .map_err(ReserveError::Db)?;
        let custom_domain = load_custom_domain(&mut tx, existing.id)
            .await
            .map_err(|e| ReserveError::Db(e.into()))?;
        let ex_node = existing
            .node_id
            .clone()
            .unwrap_or_else(|| node_id.to_string());
        let ex_host = existing.public_host.clone();
        tx.commit().await.map_err(|e| ReserveError::Db(e.into()))?;
        return Ok(ReservedTunnel {
            tunnel_id: existing.id,
            subdomain: existing.subdomain,
            node_id: ex_node,
            public_host: ex_host,
            routes,
            custom_domain,
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

    // Allocate the subdomain: the user's chosen one (validated, must be free) or a
    // random one with retry. (Reconnects were already handled by the reuse check
    // above; a rare concurrent (owner, route_sig) race just surfaces as an error
    // and the caller retries.)
    let (tunnel_id, subdomain): (i64, String) = if let Some(custom) = custom_subdomain {
        let cand = custom.trim().to_lowercase();
        if !valid_subdomain(&cand) {
            return Err(ReserveError::BadSubdomain);
        }
        if is_reserved_name(&mut tx, &cand)
            .await
            .map_err(|e| ReserveError::Db(e.into()))?
        {
            return Err(ReserveError::SubdomainTaken(cand));
        }
        match insert_tunnel(
            &mut tx,
            &cand,
            owner_id,
            &sig,
            public_host,
            node_id,
            device_id,
        )
        .await
        {
            Ok(id) => (id, cand),
            Err(sqlx::Error::Database(db)) if db.constraint() == Some("tunnels_subdomain_uq") => {
                return Err(ReserveError::SubdomainTaken(cand));
            }
            Err(e) => return Err(ReserveError::Db(e.into())),
        }
    } else {
        let mut chosen: Option<(i64, String)> = None;
        for _ in 0..10 {
            let cand = random_subdomain();
            if is_reserved_name(&mut tx, &cand)
                .await
                .map_err(|e| ReserveError::Db(e.into()))?
            {
                continue;
            }
            match insert_tunnel(
                &mut tx,
                &cand,
                owner_id,
                &sig,
                public_host,
                node_id,
                device_id,
            )
            .await
            {
                Ok(id) => {
                    chosen = Some((id, cand));
                    break;
                }
                Err(sqlx::Error::Database(db))
                    if db.constraint() == Some("tunnels_subdomain_uq") =>
                {
                    continue;
                }
                Err(e) => return Err(ReserveError::Db(e.into())),
            }
        }
        chosen.ok_or(ReserveError::Db(anyhow::anyhow!(
            "could not allocate a unique subdomain after several attempts"
        )))?
    };

    // Insert routes; allocate a dedicated TCP port per tcp route.
    let mut out = Vec::with_capacity(requested.len());
    for (i, (mode, local_port, label)) in requested.iter().enumerate() {
        let route_id = (i as i16) + 1;
        let label = label
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let public_port: Option<i32> = if !mode.is_host_routed() {
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
            "INSERT INTO routes (tunnel_id, route_id, kind, local_port, public_port, label)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(tunnel_id)
        .bind(route_id)
        .bind(mode.as_str())
        .bind(*local_port as i32)
        .bind(public_port)
        .bind(&label)
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
            label,
        });
    }

    tx.commit().await.map_err(|e| ReserveError::Db(e.into()))?;
    Ok(ReservedTunnel {
        tunnel_id,
        subdomain,
        node_id: node_id.to_string(),
        public_host: public_host.to_string(),
        routes: out,
        custom_domain: None,
        reused: false,
    })
}

// --------------------------------------------------------------------------
// Tunnel views + lifecycle
// --------------------------------------------------------------------------

fn route_endpoint(kind: &str, subdomain: &str, domain: &str, public_port: Option<i32>) -> String {
    match kind {
        "http" => format!("http://{subdomain}.{domain}"),
        "https" => format!("https://{subdomain}.{domain}"),
        // tcp: the subdomain + dedicated port. The wildcard *.domain resolves the
        // subdomain to the NatForge server, which relays to the host's machine.
        _ => format!("{subdomain}.{domain}:{}", public_port.unwrap_or(0)),
    }
}

async fn build_views(pg: &PgPool, tunnels: Vec<TunnelRow>) -> anyhow::Result<Vec<TunnelView>> {
    let mut views = Vec::with_capacity(tunnels.len());
    for t in tunnels {
        let routes = sqlx::query_as::<_, RouteRow>(
            "SELECT id, tunnel_id, route_id, kind, local_port, public_port, label
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
        // Region label for the location UI: prefer the node's region, else its name.
        let region: Option<String> = match &t.node_id {
            Some(nid) => get_node(pg, nid).await?.map(|n| n.region.unwrap_or(n.name)),
            None => None,
        };
        let (custom_domain, device_id): (Option<String>, Option<i64>) =
            sqlx::query_as("SELECT custom_domain, device_id FROM tunnels WHERE id = $1")
                .bind(t.id)
                .fetch_one(pg)
                .await?;
        // Endpoints use the tunnel's own node public_host (region), not a global domain.
        let host = t.public_host.clone();
        let route_views = routes
            .into_iter()
            .map(|r| RouteView {
                route_id: r.route_id as u16,
                mode: r.kind.clone(),
                local_port: r.local_port,
                public_port: r.public_port,
                public_endpoint: route_endpoint(&r.kind, &t.subdomain, &host, r.public_port),
                label: r.label,
            })
            .collect();
        let owner = user_by_id(pg, t.owner_id).await?;
        let owner_email = owner.as_ref().map(|u| u.email.clone());
        let owner_name = owner.and_then(|u| u.name);
        views.push(TunnelView {
            tunnel_id: t.id,
            full_host: format!("{}.{}", t.subdomain, host),
            subdomain: t.subdomain,
            name: t.name,
            public_host: t.public_host,
            status: t.status,
            agent_ip: t.agent_ip,
            owner_id: t.owner_id,
            owner_email,
            owner_name,
            node_id: t.node_id,
            region,
            device_id,
            custom_domain,
            bytes_in,
            bytes_out,
            created_at: t.created_at,
            routes: route_views,
        });
    }
    Ok(views)
}

pub async fn tunnels_for_owner(pg: &PgPool, owner_id: i32) -> anyhow::Result<Vec<TunnelView>> {
    let tunnels = sqlx::query_as::<_, TunnelRow>(
        "SELECT id, subdomain, name, owner_id, route_sig, status, public_host, node_id, agent_ip, created_at, last_seen
         FROM tunnels WHERE owner_id = $1 ORDER BY created_at DESC",
    )
    .bind(owner_id)
    .fetch_all(pg)
    .await?;
    build_views(pg, tunnels).await
}

pub async fn all_tunnels(pg: &PgPool) -> anyhow::Result<Vec<TunnelView>> {
    let tunnels = sqlx::query_as::<_, TunnelRow>(
        "SELECT id, subdomain, name, owner_id, route_sig, status, public_host, node_id, agent_ip, created_at, last_seen
         FROM tunnels ORDER BY created_at DESC",
    )
    .fetch_all(pg)
    .await?;
    build_views(pg, tunnels).await
}

/// Returns (owner_id, subdomain, node_id) for a tunnel, to authorize the stop and
/// route the data-plane signal to the node hosting it.
pub async fn tunnel_owner_subdomain(
    pg: &PgPool,
    tunnel_id: i64,
) -> anyhow::Result<Option<(i32, String, Option<String>)>> {
    Ok(sqlx::query_as::<_, (i32, String, Option<String>)>(
        "SELECT owner_id, subdomain, node_id FROM tunnels WHERE id = $1",
    )
    .bind(tunnel_id)
    .fetch_optional(pg)
    .await?)
}

pub async fn set_tunnel_online(
    pg: &PgPool,
    tunnel_id: i64,
    node_id: &str,
    agent_ip: Option<&str>,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE tunnels SET status='online', node_id=$2, agent_ip=$3, last_seen=now() WHERE id=$1",
    )
    .bind(tunnel_id)
    .bind(node_id)
    .bind(agent_ip)
    .execute(pg)
    .await?;
    Ok(())
}

/// Aggregated per-user overview for the admin Users page.
pub async fn users_overview(pg: &PgPool) -> anyhow::Result<Vec<crate::models::UserOverview>> {
    Ok(sqlx::query_as::<_, crate::models::UserOverview>(
        "SELECT u.id, u.email, u.name, u.role, u.banned, u.created_at,
                (SELECT count(*) FROM tunnels t WHERE t.owner_id = u.id) AS tunnel_count,
                (SELECT max(last_seen) FROM tunnels t WHERE t.owner_id = u.id) AS last_seen,
                COALESCE((
                    SELECT sum(b.bytes_in + b.bytes_out) FROM (
                        SELECT DISTINCT ON (tunnel_id) tunnel_id, bytes_in, bytes_out
                        FROM bandwidth_logs WHERE owner_id = u.id
                        ORDER BY tunnel_id, recorded_at DESC
                    ) b
                ), 0)::bigint AS total_bytes
         FROM users u ORDER BY u.id",
    )
    .fetch_all(pg)
    .await?)
}

pub async fn set_tunnel_offline(pg: &PgPool, tunnel_id: i64) -> anyhow::Result<()> {
    sqlx::query("UPDATE tunnels SET status='offline', last_seen=now() WHERE id=$1")
        .bind(tunnel_id)
        .execute(pg)
        .await?;
    Ok(())
}

/// Delete a tunnel and release its ports back to the pool. Returns the number of
/// tunnel rows actually deleted (0 if it was already gone - the caller can then
/// report accurately rather than claiming a no-op succeeded).
pub async fn delete_tunnel(pg: &PgPool, tunnel_id: i64) -> anyhow::Result<u64> {
    let mut tx = pg.begin().await?;
    sqlx::query("UPDATE port_pool SET tunnel_id=NULL, route_id=NULL WHERE tunnel_id=$1")
        .bind(tunnel_id)
        .execute(&mut *tx)
        .await?;
    let deleted = sqlx::query("DELETE FROM tunnels WHERE id=$1")
        .bind(tunnel_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(deleted)
}

/// "Stop" a tunnel: mark it `stopped` but KEEP the row and its reserved ports, so
/// the user can restart it (reservation reuse gives back the same subdomain/ports).
/// Stopped tunnels are exempt from the reconciliation sweep. Returns rows affected.
pub async fn stop_tunnel_keep(pg: &PgPool, tunnel_id: i64) -> anyhow::Result<u64> {
    Ok(
        sqlx::query("UPDATE tunnels SET status='stopped', last_seen=now() WHERE id=$1")
            .bind(tunnel_id)
            .execute(pg)
            .await?
            .rows_affected(),
    )
}

/// Set (or clear, with `None`) a tunnel's display name.
pub async fn rename_tunnel(pg: &PgPool, tunnel_id: i64, name: Option<&str>) -> anyhow::Result<()> {
    sqlx::query("UPDATE tunnels SET name=$2 WHERE id=$1")
        .bind(tunnel_id)
        .bind(name)
        .execute(pg)
        .await?;
    Ok(())
}

/// Current lifecycle status of a tunnel ('awaiting_agent'|'online'|'offline'|'stopped').
pub async fn tunnel_status(pg: &PgPool, tunnel_id: i64) -> anyhow::Result<Option<String>> {
    Ok(
        sqlx::query_scalar::<_, String>("SELECT status FROM tunnels WHERE id = $1")
            .bind(tunnel_id)
            .fetch_optional(pg)
            .await?,
    )
}

/// True if `name` is on the reserved-subdomain block list (pool variant of the
/// reservation-time check, for the admin/owner edit path).
pub async fn is_reserved_subdomain(pg: &PgPool, name: &str) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT exists(SELECT 1 FROM reserved_subdomains WHERE name = $1)",
    )
    .bind(name)
    .fetch_one(pg)
    .await?)
}

/// True if `name` is already used by some *other* tunnel (excludes `self_id`).
pub async fn subdomain_in_use(pg: &PgPool, name: &str, self_id: i64) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT exists(SELECT 1 FROM tunnels WHERE subdomain = $1 AND id <> $2)",
    )
    .bind(name)
    .bind(self_id)
    .fetch_one(pg)
    .await?)
}

/// Change a tunnel's subdomain (the routing key); `public_host` is the node base
/// and is unchanged, so the full host becomes `{new}.{public_host}`. Relies on the
/// caller to have validated format / reserved / uniqueness first; the UNIQUE
/// constraint is the final backstop (surfaces as an error).
pub async fn set_tunnel_subdomain(
    pg: &PgPool,
    tunnel_id: i64,
    subdomain: &str,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE tunnels SET subdomain=$2 WHERE id=$1")
        .bind(tunnel_id)
        .bind(subdomain)
        .execute(pg)
        .await?;
    Ok(())
}

/// Set/clear a single route's free-text label (scoped to its tunnel).
pub async fn update_route_label(
    pg: &PgPool,
    tunnel_id: i64,
    route_id: i32,
    label: Option<&str>,
) -> anyhow::Result<u64> {
    let r = sqlx::query("UPDATE routes SET label=$3 WHERE tunnel_id=$1 AND route_id=$2")
        .bind(tunnel_id)
        .bind(route_id)
        .bind(label)
        .execute(pg)
        .await?;
    Ok(r.rows_affected())
}

/// (tunnel_id, subdomain, node_id) for every tunnel owned by `owner_id` - used to
/// signal each tunnel's node to drop the live session (e.g. on ban).
pub async fn owner_tunnel_targets(
    pg: &PgPool,
    owner_id: i32,
) -> anyhow::Result<Vec<(i64, String, Option<String>)>> {
    Ok(sqlx::query_as::<_, (i64, String, Option<String>)>(
        "SELECT id, subdomain, node_id FROM tunnels WHERE owner_id = $1",
    )
    .bind(owner_id)
    .fetch_all(pg)
    .await?)
}

// --------------------------------------------------------------------------
// User profile + moderation
// --------------------------------------------------------------------------

/// Update a user's display name and email. Email uniqueness is enforced by the DB
/// (`users_email_key`); the caller should pre-check to return a friendly 409.
pub async fn update_user_profile(
    pg: &PgPool,
    user_id: i32,
    name: Option<&str>,
    email: &str,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE users SET name=$2, email=$3 WHERE id=$1")
        .bind(user_id)
        .bind(name)
        .bind(email)
        .execute(pg)
        .await?;
    Ok(())
}

pub async fn update_user_password(pg: &PgPool, user_id: i32, new_hash: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE users SET password_hash=$2 WHERE id=$1")
        .bind(user_id)
        .bind(new_hash)
        .execute(pg)
        .await?;
    Ok(())
}

pub async fn set_user_banned(pg: &PgPool, user_id: i32, banned: bool) -> anyhow::Result<()> {
    sqlx::query("UPDATE users SET banned=$2 WHERE id=$1")
        .bind(user_id)
        .bind(banned)
        .execute(pg)
        .await?;
    Ok(())
}

/// Delete a user; FK cascades remove their tunnels/routes/logs and free their ports.
pub async fn delete_user(pg: &PgPool, user_id: i32) -> anyhow::Result<u64> {
    Ok(sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user_id)
        .execute(pg)
        .await?
        .rows_affected())
}

/// Cheap banned-state check for guarding actions on an existing session.
pub async fn is_user_banned(pg: &PgPool, user_id: i32) -> anyhow::Result<bool> {
    Ok(
        sqlx::query_scalar::<_, bool>("SELECT banned FROM users WHERE id=$1")
            .bind(user_id)
            .fetch_optional(pg)
            .await?
            .unwrap_or(false),
    )
}

pub async fn append_bandwidth(
    pg: &PgPool,
    tunnel_id: i64,
    owner_id: i32,
    bytes_in: i64,
    bytes_out: i64,
) -> anyhow::Result<()> {
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

/// Recent cumulative bandwidth snapshots for one tunnel (newest first, capped).
pub async fn bandwidth_series(
    pg: &PgPool,
    tunnel_id: i64,
    limit: i64,
) -> anyhow::Result<Vec<crate::models::BandwidthSample>> {
    Ok(sqlx::query_as::<_, crate::models::BandwidthSample>(
        "SELECT bytes_in, bytes_out, recorded_at FROM bandwidth_logs
         WHERE tunnel_id = $1 ORDER BY recorded_at DESC LIMIT $2",
    )
    .bind(tunnel_id)
    .bind(limit)
    .fetch_all(pg)
    .await?)
}

/// Append one connection-log row (a closed connection or a geo-blocked attempt).
#[allow(clippy::too_many_arguments)]
pub async fn insert_conn_log(
    pg: &PgPool,
    tunnel_id: i64,
    owner_id: i32,
    route_id: i16,
    kind: &str,
    peer_ip: &str,
    country: Option<&str>,
    bytes_in: i64,
    bytes_out: i64,
    duration_ms: i64,
    blocked: bool,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO connection_logs
            (tunnel_id, owner_id, route_id, kind, peer_ip, country, bytes_in, bytes_out, duration_ms, blocked)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(tunnel_id)
    .bind(owner_id)
    .bind(route_id)
    .bind(kind)
    .bind(peer_ip)
    .bind(country)
    .bind(bytes_in)
    .bind(bytes_out)
    .bind(duration_ms)
    .bind(blocked)
    .execute(pg)
    .await?;
    Ok(())
}

/// Recent connection-log rows for one tunnel (newest first, capped).
pub async fn recent_conn_logs(
    pg: &PgPool,
    tunnel_id: i64,
    limit: i64,
) -> anyhow::Result<Vec<crate::models::ConnLog>> {
    Ok(sqlx::query_as::<_, crate::models::ConnLog>(
        "SELECT id, route_id, kind, peer_ip, country, bytes_in, bytes_out, duration_ms, blocked, created_at
         FROM connection_logs WHERE tunnel_id = $1 ORDER BY id DESC LIMIT $2",
    )
    .bind(tunnel_id)
    .bind(limit)
    .fetch_all(pg)
    .await?)
}

/// Owner of a tunnel (for authorizing per-tunnel reads/writes). None if missing.
pub async fn tunnel_owner(pg: &PgPool, tunnel_id: i64) -> anyhow::Result<Option<i32>> {
    Ok(
        sqlx::query_scalar::<_, i32>("SELECT owner_id FROM tunnels WHERE id = $1")
            .bind(tunnel_id)
            .fetch_optional(pg)
            .await?,
    )
}

/// Reclaim ports + delete tunnels that have been silent past the grace period
/// (covers crashed agents/cores whose `tunnel_down` never arrived).
pub async fn reconcile_abandoned(pg: &PgPool, grace_secs: i64) -> anyhow::Result<u64> {
    let mut tx = pg.begin().await?;
    let stale: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM tunnels
         WHERE status NOT IN ('awaiting_agent', 'stopped')
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

// --------------------------------------------------------------------------
// Admin policy + stats
// --------------------------------------------------------------------------

pub async fn region_blocks(pg: &PgPool) -> anyhow::Result<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT country_code FROM region_blocks ORDER BY country_code")
            .fetch_all(pg)
            .await?,
    )
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

/// Countries a specific tunnel's owner has chosen to block (alpha-2, sorted).
pub async fn tunnel_region_blocks(pg: &PgPool, tunnel_id: i64) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT country_code FROM tunnel_region_blocks WHERE tunnel_id = $1 ORDER BY country_code",
    )
    .bind(tunnel_id)
    .fetch_all(pg)
    .await?)
}

/// Replace a tunnel's blocked-country list wholesale (PUT semantics).
pub async fn set_tunnel_region_blocks(
    pg: &PgPool,
    tunnel_id: i64,
    codes: &[String],
) -> anyhow::Result<()> {
    let mut tx = pg.begin().await?;
    sqlx::query("DELETE FROM tunnel_region_blocks WHERE tunnel_id = $1")
        .bind(tunnel_id)
        .execute(&mut *tx)
        .await?;
    for code in codes {
        sqlx::query(
            "INSERT INTO tunnel_region_blocks(tunnel_id, country_code) VALUES ($1,$2)
             ON CONFLICT DO NOTHING",
        )
        .bind(tunnel_id)
        .bind(code)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// All per-tunnel block lists as tunnel_id -> [codes], for the core policy pull.
pub async fn all_tunnel_region_blocks(
    pg: &PgPool,
) -> anyhow::Result<std::collections::HashMap<i64, Vec<String>>> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT tunnel_id, country_code FROM tunnel_region_blocks ORDER BY tunnel_id",
    )
    .fetch_all(pg)
    .await?;
    let mut map: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
    for (tid, cc) in rows {
        map.entry(tid).or_default().push(cc);
    }
    Ok(map)
}

pub struct Stats {
    pub total_users: i64,
    pub active_tunnels: i64,
    pub total_bytes_relayed: i64,
    pub blocked_regions: i64,
}

pub async fn stats(pg: &PgPool) -> anyhow::Result<Stats> {
    let total_users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(pg)
        .await?;
    let active_tunnels: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tunnels WHERE status='online'")
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
    let blocked_regions: i64 = sqlx::query_scalar("SELECT count(*) FROM region_blocks")
        .fetch_one(pg)
        .await?;
    Ok(Stats {
        total_users,
        active_tunnels,
        total_bytes_relayed: total_bytes.unwrap_or(0),
        blocked_regions,
    })
}

/// Dedicated TCP-pool ports currently allocated to a tunnel, and the pool's total
/// capacity. Lets the dashboard watch how close a node is to its port limit.
pub async fn port_pool_usage(pg: &PgPool) -> anyhow::Result<(i64, i64)> {
    let used: i64 =
        sqlx::query_scalar("SELECT count(*) FROM port_pool WHERE tunnel_id IS NOT NULL")
            .fetch_one(pg)
            .await?;
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM port_pool")
        .fetch_one(pg)
        .await?;
    Ok((used, total))
}

// --------------------------------------------------------------------------
// Nodes (data-plane VMs / regions)
// --------------------------------------------------------------------------

use crate::models::Node;

/// A node self-registers on boot. Technical fields are refreshed every time;
/// admin-controlled fields (name, region, active) are preserved after the first
/// registration. The node's TCP port range is seeded into the pool idempotently.
#[allow(clippy::too_many_arguments)]
pub async fn register_node(
    pg: &PgPool,
    node_id: &str,
    name: &str,
    region: Option<&str>,
    public_host: &str,
    control_endpoint: &str,
    internal_url: &str,
    http_port: i32,
    https_port: i32,
    port_min: i32,
    port_max: i32,
    control_cert_fp: Option<&str>,
) -> anyhow::Result<()> {
    let mut tx = pg.begin().await?;
    sqlx::query(
        "INSERT INTO nodes (node_id, name, region, public_host, control_endpoint, internal_url, http_port, https_port, control_cert_fp, last_seen)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())
         ON CONFLICT (node_id) DO UPDATE SET
             public_host = $4, control_endpoint = $5, internal_url = $6, http_port = $7, https_port = $8, control_cert_fp = $9, last_seen = now()",
    )
    .bind(node_id)
    .bind(name)
    .bind(region)
    .bind(public_host)
    .bind(control_endpoint)
    .bind(internal_url)
    .bind(http_port)
    .bind(https_port)
    .bind(control_cert_fp)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO port_pool (node_id, port)
         SELECT $1, g FROM generate_series($2::int, $3::int) AS g
         ON CONFLICT (node_id, port) DO NOTHING",
    )
    .bind(node_id)
    .bind(port_min)
    .bind(port_max)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

const NODE_COLS: &str = "node_id, name, region, public_host, control_endpoint, internal_url, http_port, https_port, active, control_cert_fp, last_seen, created_at";

pub async fn list_nodes(pg: &PgPool, active_only: bool) -> anyhow::Result<Vec<Node>> {
    let sql = if active_only {
        format!("SELECT {NODE_COLS} FROM nodes WHERE active ORDER BY name")
    } else {
        format!("SELECT {NODE_COLS} FROM nodes ORDER BY name")
    };
    Ok(sqlx::query_as::<_, Node>(&sql).fetch_all(pg).await?)
}

pub async fn get_node(pg: &PgPool, node_id: &str) -> anyhow::Result<Option<Node>> {
    Ok(
        sqlx::query_as::<_, Node>(&format!("SELECT {NODE_COLS} FROM nodes WHERE node_id = $1"))
            .bind(node_id)
            .fetch_optional(pg)
            .await?,
    )
}

/// Default node for a reservation when none is specified: first active node.
pub async fn default_node(pg: &PgPool) -> anyhow::Result<Option<Node>> {
    Ok(sqlx::query_as::<_, Node>(&format!(
        "SELECT {NODE_COLS} FROM nodes WHERE active ORDER BY created_at LIMIT 1"
    ))
    .fetch_optional(pg)
    .await?)
}

pub async fn update_node(
    pg: &PgPool,
    node_id: &str,
    name: &str,
    region: Option<&str>,
    active: bool,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE nodes SET name=$2, region=$3, active=$4 WHERE node_id=$1")
        .bind(node_id)
        .bind(name)
        .bind(region)
        .bind(active)
        .execute(pg)
        .await?;
    Ok(())
}

pub async fn delete_node(pg: &PgPool, node_id: &str) -> anyhow::Result<()> {
    let mut tx = pg.begin().await?;
    sqlx::query("DELETE FROM port_pool WHERE node_id=$1")
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM nodes WHERE node_id=$1")
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
