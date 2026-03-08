use tracing::info;
use boringtun::noise::{Tunn, TunnResult};

/// Represents an active WireGuard Peer mapped to a single End-User
pub struct PeerTunnel {
    pub end_user_id: String,
    pub allocated_tcp_port: u16,
    pub allocated_udp_port: u16,
    pub bandwidth_used_bytes: u64,
}

impl PeerTunnel {
    pub fn new(id: String, tcp: u16, udp: u16) -> Self {
        info!("Allocating high-speed WireGuard tunnel for Peer: {} on TCP: {} / UDP: {}", id, tcp, udp);
        Self {
            end_user_id: id,
            allocated_tcp_port: tcp,
            allocated_udp_port: udp,
            bandwidth_used_bytes: 0,
        }
    }

    /// Increments the byte length stream array for billing analysis
    pub fn track_bandwidth(&mut self, bytes_appended: u64) {
        self.bandwidth_used_bytes += bytes_appended;
    }
}
