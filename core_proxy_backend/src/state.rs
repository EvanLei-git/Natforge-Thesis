//! Shared, in-memory state for the Core Proxy node.
//!
//! The in-process registries are the hot-path source of truth for routing (the
//! yamux `mpsc::Sender`s are not serializable, so they cannot live in Redis).
//! Redis holds a best-effort liveness mirror for future multi-node reads.
//!
//! Three registries:
//!   * `http_routes`  : subdomain  -> handle  (shared :80 Host routing)
//!   * `https_routes` : subdomain  -> handle  (shared :443 SNI routing)
//!   * `port_routes`  : public TCP port -> handle (dedicated raw-TCP ports)
//! Plus `tunnels` (subdomain -> ActiveTunnel) for lifecycle/teardown.

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, RwLock};
use yamux::{ConnectionError, Stream};

use tokio_rustls::TlsAcceptor;

use crate::config::Config;
use crate::ddos::DdosProtector;
use crate::geo::GeoDb;
use natforge_proto::RouteMode;

/// Request to the tunnel's yamux driver to open one outbound stream. `route_id`
/// is informational (the caller writes the preamble); the driver only needs `reply`.
pub struct OpenStream {
    pub route_id: u16,
    pub reply: oneshot::Sender<Result<Stream, ConnectionError>>,
}

/// Live byte counters for a tunnel (shared by all its routes' relay tasks).
#[derive(Default)]
pub struct TunnelStats {
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
}

/// A routable destination: which tunnel/route, and how to open a stream to it.
#[derive(Clone)]
pub struct RouteHandle {
    pub tunnel_id: i64,
    pub owner_id: i32,
    pub route_id: u16,
    pub mode: RouteMode,
    pub open_tx: mpsc::Sender<OpenStream>,
    pub stats: Arc<TunnelStats>,
}

/// An active, agent-backed tunnel and everything needed to tear it down cleanly.
pub struct ActiveTunnel {
    pub tunnel_id: i64,
    pub subdomain: String,
    pub owner_id: i32,
    /// TCP public ports bound for this tunnel (for registry cleanup).
    pub public_ports: Vec<u16>,
    /// Whether this tunnel registered http / https on its subdomain.
    pub has_http: bool,
    pub has_https: bool,
    pub stats: Arc<TunnelStats>,
    /// Public listener tasks (tcp routes) to abort on teardown.
    pub listener_handles: Vec<tokio::task::JoinHandle<()>>,
    /// Aborts the yamux driver — used to force a tunnel down from the API.
    pub driver_abort: tokio::task::AbortHandle,
}

pub struct CoreState {
    pub config: Config,
    pub tunnels: RwLock<HashMap<String, ActiveTunnel>>, // key = subdomain
    pub http_routes: RwLock<HashMap<String, RouteHandle>>, // key = subdomain
    pub https_routes: RwLock<HashMap<String, RouteHandle>>, // key = subdomain
    pub port_routes: RwLock<HashMap<u16, RouteHandle>>, // key = public port
    pub redis: redis::aio::ConnectionManager,
    pub ddos: DdosProtector,
    pub blocked_ports: RwLock<Vec<u16>>,
    /// Admin-wide blocked countries (ISO alpha-2), refreshed from website policy.
    pub blocked_regions: RwLock<Vec<String>>,
    /// Per-tunnel user-chosen blocked countries: tunnel_id -> [CC, ...].
    pub tunnel_region_blocks: RwLock<HashMap<i64, Vec<String>>>,
    /// IP → country resolver (optional GeoLite2 database).
    pub geo: GeoDb,
    /// TLS acceptor for the agent control connection (self-signed, boot-generated).
    pub tls: TlsAcceptor,
    /// SHA-256 fingerprint of our control certificate (agents pin this).
    pub control_cert_fp: String,
    pub http: reqwest::Client,
}

impl CoreState {
    pub async fn connect(config: Config) -> anyhow::Result<Arc<Self>> {
        let client = redis::Client::open(config.redis_url.clone())?;
        let redis = redis::aio::ConnectionManager::new(client).await?;
        let geo = GeoDb::open(&config.geoip_db);
        let control = crate::tls::generate()?;
        Ok(Arc::new(CoreState {
            config,
            tunnels: RwLock::new(HashMap::new()),
            http_routes: RwLock::new(HashMap::new()),
            https_routes: RwLock::new(HashMap::new()),
            port_routes: RwLock::new(HashMap::new()),
            redis,
            ddos: DdosProtector::default(),
            blocked_ports: RwLock::new(vec![25, 465, 587]),
            blocked_regions: RwLock::new(Vec::new()),
            tunnel_region_blocks: RwLock::new(HashMap::new()),
            geo,
            tls: control.acceptor,
            control_cert_fp: control.fingerprint,
            http: reqwest::Client::new(),
        }))
    }

    /// Look up the routing handle for an http/https subdomain.
    pub async fn host_route(&self, subdomain: &str, mode: RouteMode) -> Option<RouteHandle> {
        match mode {
            RouteMode::Http => self.http_routes.read().await.get(subdomain).cloned(),
            RouteMode::Https => self.https_routes.read().await.get(subdomain).cloned(),
            RouteMode::Tcp => None,
        }
    }

    /// Whether a connection from `country` to `tunnel_id` should be refused, by
    /// either the platform-wide block list or this tunnel's own block list. An
    /// unknown country (no GeoIP data) is always allowed — we never block blindly.
    pub async fn is_country_blocked(&self, tunnel_id: i64, country: Option<&str>) -> bool {
        let Some(cc) = country else { return false };
        if self.blocked_regions.read().await.iter().any(|c| c == cc) {
            return true;
        }
        self.tunnel_region_blocks
            .read()
            .await
            .get(&tunnel_id)
            .is_some_and(|list| list.iter().any(|c| c == cc))
    }
}
