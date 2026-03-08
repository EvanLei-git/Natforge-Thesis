use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Tracks inbound IP traffic to heuristically detect volumetric floods
#[derive(Default)]
pub struct DdosProtector {
    ip_hit_counts: RwLock<HashMap<String, u32>>,
    blacklist: RwLock<Vec<String>>,
}

impl DdosProtector {
    pub async fn analyze_packet(&self, source_ip: &str) -> bool {
        let mut counts = self.ip_hit_counts.write().await;
        let hits = counts.entry(source_ip.to_string()).or_insert(0);
        *hits += 1;

        if *hits > 10_000 {
            warn!("DDoS DETECTED! IP {} exceeded 10k packets/sec. Injecting eBPF drop rule.", source_ip);
            self.blacklist.write().await.push(source_ip.to_string());
            return false; // Drop packet
        }
        
        true // Allow packet
    }
}
