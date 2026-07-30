//! Prometheus metrics for the control plane.
//!
//! Exposed on a localhost-only listener (`127.0.0.1:9101/metrics`, wired in
//! `main.rs`) and scraped by Prometheus (see the `monitoring/` stack). Signins
//! are an in-process counter incremented at the login sites in `handlers::auth`;
//! the rest (active tunnels, users, TCP port-pool usage) are read from Postgres
//! at scrape time, which is the source of truth.

use axum::extract::State;
use axum::response::IntoResponse;
use prometheus::{Encoder, IntCounterVec, IntGauge, Opts, Registry, TextEncoder};

use crate::db::connection::SharedState;
use crate::db::queries;

pub struct Metrics {
    registry: Registry,
    pub signins: IntCounterVec,
    active_tunnels: IntGauge,
    total_users: IntGauge,
    tcp_ports_used: IntGauge,
    tcp_ports_total: IntGauge,
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Registry::new();
        let signins = IntCounterVec::new(
            Opts::new(
                "natforge_signins_total",
                "Successful signins since process start, by method",
            ),
            &["method"],
        )
        .expect("valid signins metric");
        let active_tunnels =
            IntGauge::new("natforge_active_tunnels", "Tunnels currently online").unwrap();
        let total_users = IntGauge::new("natforge_users_total", "Registered users").unwrap();
        let tcp_ports_used = IntGauge::new(
            "natforge_tcp_ports_used",
            "Dedicated TCP-pool ports currently allocated",
        )
        .unwrap();
        let tcp_ports_total =
            IntGauge::new("natforge_tcp_ports_total", "Dedicated TCP-pool capacity").unwrap();

        registry.register(Box::new(signins.clone())).unwrap();
        registry.register(Box::new(active_tunnels.clone())).unwrap();
        registry.register(Box::new(total_users.clone())).unwrap();
        registry.register(Box::new(tcp_ports_used.clone())).unwrap();
        registry
            .register(Box::new(tcp_ports_total.clone()))
            .unwrap();

        Self {
            registry,
            signins,
            active_tunnels,
            total_users,
            tcp_ports_used,
            tcp_ports_total,
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// `GET /metrics`: refresh the DB-derived gauges from Postgres, then encode the
/// registry in the Prometheus text exposition format.
pub async fn metrics_handler(State(state): State<SharedState>) -> impl IntoResponse {
    let m = &state.metrics;
    match queries::stats(&state.db.pg).await {
        Ok(s) => {
            m.active_tunnels.set(s.active_tunnels);
            m.total_users.set(s.total_users);
        }
        Err(e) => tracing::warn!("metrics: stats query failed: {e}"),
    }
    match queries::port_pool_usage(&state.db.pg).await {
        Ok((used, total)) => {
            m.tcp_ports_used.set(used);
            m.tcp_ports_total.set(total);
        }
        Err(e) => tracing::warn!("metrics: port_pool query failed: {e}"),
    }

    let mut buf = Vec::new();
    let encoder = TextEncoder::new();
    if let Err(e) = encoder.encode(&m.registry.gather(), &mut buf) {
        tracing::warn!("metrics: encode failed: {e}");
    }
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        buf,
    )
}
