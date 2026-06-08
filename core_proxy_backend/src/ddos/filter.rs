//! Heuristic volumetric DDoS guard.
//!
//! Production edge nodes would push a drop rule into an eBPF/XDP program once an
//! IP crosses a packet-rate threshold (thesis §3 / [11]). In userspace we
//! approximate the same protective behaviour with a per-IP connection-rate
//! counter over a sliding one-second window: any source that opens more than
//! `MAX_CONN_PER_SEC` new connections in a window is blackholed for the rest of
//! the process lifetime, shedding the load before it reaches the multiplexer.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::warn;

const MAX_CONN_PER_SEC: u32 = 200;
const WINDOW: Duration = Duration::from_secs(1);

struct Window {
    started: Instant,
    count: u32,
}

#[derive(Default)]
pub struct DdosProtector {
    windows: Mutex<HashMap<String, Window>>,
    blacklist: Mutex<Vec<String>>,
}

impl DdosProtector {
    /// Returns `true` if the connection from `source_ip` should be allowed.
    pub async fn analyze_connection(&self, source_ip: &str) -> bool {
        {
            let bl = self.blacklist.lock().await;
            if bl.iter().any(|ip| ip == source_ip) {
                return false;
            }
        }

        let mut windows = self.windows.lock().await;
        let now = Instant::now();
        let w = windows.entry(source_ip.to_string()).or_insert(Window {
            started: now,
            count: 0,
        });

        if now.duration_since(w.started) > WINDOW {
            w.started = now;
            w.count = 0;
        }
        w.count += 1;

        if w.count > MAX_CONN_PER_SEC {
            warn!(
                "DDoS heuristic tripped: {source_ip} exceeded {MAX_CONN_PER_SEC} conn/s — installing simulated eBPF drop rule"
            );
            drop(windows);
            self.blacklist.lock().await.push(source_ip.to_string());
            return false;
        }
        true
    }
}
