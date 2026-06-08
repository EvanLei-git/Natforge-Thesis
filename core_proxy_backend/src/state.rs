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

use crate::config::Config;
use crate::ddos::filter::DdosProtector;
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
    pub http: reqwest::Client,
}

impl CoreState {
    pub async fn connect(config: Config) -> anyhow::Result<Arc<Self>> {
        let client = redis::Client::open(config.redis_url.clone())?;
        let redis = redis::aio::ConnectionManager::new(client).await?;
        Ok(Arc::new(CoreState {
            config,
            tunnels: RwLock::new(HashMap::new()),
            http_routes: RwLock::new(HashMap::new()),
            https_routes: RwLock::new(HashMap::new()),
            port_routes: RwLock::new(HashMap::new()),
            redis,
            ddos: DdosProtector::default(),
            blocked_ports: RwLock::new(vec![25, 465, 587]),
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
}
