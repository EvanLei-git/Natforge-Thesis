//! WireGuard peer abstraction (simulated).
//!
//! The production design (see thesis §3.3 / §4.2) wraps the multiplexed tunnel in
//! a `boringtun` userspace WireGuard session for end-to-end confidentiality. For
//! the locally-testable prototype the cryptographic handshake is simulated and the
//! struct is retained as the canonical place where per-peer bandwidth accounting
//! lives, mirroring the data the live relay tasks accumulate in `TunnelStats`.

use tracing::info;

/// Represents an active WireGuard peer mapped to a single end-user tunnel.
pub struct PeerTunnel {
    pub end_user_id: String,
    pub allocated_tcp_port: u16,
    pub allocated_udp_port: u16,
    pub bandwidth_used_bytes: u64,
}

impl PeerTunnel {
    pub fn new(id: String, tcp: u16, udp: u16) -> Self {
        info!(
            "Allocating simulated WireGuard tunnel for peer {id} on TCP {tcp} / UDP {udp}"
        );
        Self {
            end_user_id: id,
            allocated_tcp_port: tcp,
            allocated_udp_port: udp,
            bandwidth_used_bytes: 0,
        }
    }

    /// Increment the byte counter for billing/economics analysis.
    pub fn track_bandwidth(&mut self, bytes_appended: u64) {
        self.bandwidth_used_bytes += bytes_appended;
    }
}
