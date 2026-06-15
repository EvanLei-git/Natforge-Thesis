//! Userspace connection-rate abuse guard.
//!
//! This is an honest *userspace* heuristic — not a kernel eBPF/XDP drop path. A
//! per-IP connection-rate counter over a sliding one-second window blackholes a
//! source exceeding `MAX_CONN_PER_SEC` for `BLACKLIST_TTL`, shedding load before it
//! reaches the multiplexer. The blacklist is **time-bounded** (entries expire) so a
//! transient flood — or a spoofed source address — cannot permanently ban an
//! innocent IP or grow memory without limit. (A production deployment could push an
//! equivalent rule into the kernel via eBPF/XDP; that is noted as future work and
//! not claimed here.)

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::warn;

const MAX_CONN_PER_SEC: u32 = 200;
const WINDOW: Duration = Duration::from_secs(1);
const BLACKLIST_TTL: Duration = Duration::from_secs(600); // 10 min, then forgiven

struct Window {
    started: Instant,
    count: u32,
}

#[derive(Default)]
pub struct DdosProtector {
    windows: Mutex<HashMap<String, Window>>,
    /// ip -> time it was blacklisted; entries older than BLACKLIST_TTL are evicted.
    blacklist: Mutex<HashMap<String, Instant>>,
}

impl DdosProtector {
    /// Returns `true` if the connection from `source_ip` should be allowed.
    pub async fn analyze_connection(&self, source_ip: &str) -> bool {
        let now = Instant::now();
        {
            let mut bl = self.blacklist.lock().await;
            // Evict expired bans (bounds memory; forgives transient/spoofed floods).
            bl.retain(|_, t| now.duration_since(*t) < BLACKLIST_TTL);
            if bl.contains_key(source_ip) {
                return false;
            }
        }

        let mut windows = self.windows.lock().await;
        let w = windows.entry(source_ip.to_string()).or_insert(Window { started: now, count: 0 });
        if now.duration_since(w.started) > WINDOW {
            w.started = now;
            w.count = 0;
        }
        w.count += 1;
        if w.count > MAX_CONN_PER_SEC {
            warn!("connection-rate guard tripped: {source_ip} exceeded {MAX_CONN_PER_SEC} conn/s — blackholing in userspace (expires in {}s)", BLACKLIST_TTL.as_secs());
            drop(windows);
            self.blacklist.lock().await.insert(source_ip.to_string(), now);
            return false;
        }
        true
    }
}
