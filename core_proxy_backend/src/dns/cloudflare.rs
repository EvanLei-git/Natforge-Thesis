//! Cloudflare DNS provisioning (mock by default).
//!
//! When a tunnel comes up the core proxy provisions an SRV record
//! (`_minecraft._tcp.<subdomain>`) pointing at the allocated public port so that
//! clients resolve straight to the edge node, transparently bypassing the host's
//! CGNAT. With a real `CF_API_TOKEN` this would POST to the Cloudflare v4 API; the
//! prototype logs the intended record so the flow is observable end-to-end.

use tracing::info;

use crate::config::Config;

pub struct CloudflareManager {
    api_token: String,
    zone_id: String,
}

impl CloudflareManager {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            api_token: cfg.cf_api_token.clone(),
            zone_id: cfg.cf_zone_id.clone(),
        }
    }

    /// Provision an SRV record for a freshly allocated tunnel.
    pub async fn map_srv_record(
        &self,
        subdomain: &str,
        target_host: &str,
        port: u16,
    ) -> Result<(), String> {
        if self.api_token == "mock_token" {
            info!(
                "(Cloudflare mock) zone {}: SRV _minecraft._tcp.{subdomain} -> {target_host}:{port}",
                self.zone_id
            );
            return Ok(());
        }

        // Real integration path (only taken when a live token is configured).
        info!(
            "(Cloudflare) provisioning SRV _minecraft._tcp.{subdomain} -> {target_host}:{port} in zone {}",
            self.zone_id
        );
        Ok(())
    }
}
