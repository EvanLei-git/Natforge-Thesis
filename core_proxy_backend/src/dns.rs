//! Cloudflare DNS provisioning (real, in Rust).
//!
//! For raw-TCP routes (e.g. a Minecraft server) the core provisions a per-tunnel
//! SRV record `_minecraft._tcp.<subdomain>` so players can enter just
//! `<subdomain>.<domain>` instead of `host:port`. HTTP/HTTPS routes need no
//! per-tunnel record - the wildcard `*.<domain>` A record (set up once, see thesis
//! Appendix D) already resolves them, and the core routes by Host/SNI.
//!
//! These call the Cloudflare v4 API for real when `CF_API_TOKEN` is a live token.
//! With the default `mock_token` (local dev) they log the intended action and
//! return `Ok`, so the platform runs end-to-end without Cloudflare credentials.

use serde_json::json;
use tracing::{info, warn};

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

    fn is_mock(&self) -> bool {
        self.api_token == "mock_token" || self.api_token.is_empty()
    }

    /// SRV record name, e.g. service="minecraft", proto="tcp" -> `_minecraft._tcp.<sub>.<domain>`.
    fn srv_name(service: &str, proto: &str, subdomain: &str, domain: &str) -> String {
        format!("_{service}._{proto}.{subdomain}.{domain}")
    }

    /// Provision an SRV record for a labelled tcp/udp route. `target`/host is
    /// `<subdomain>.<domain>` (covered by the wildcard A record), `port` is the allocated
    /// public port, and `service`/`proto` form the `_<service>._<proto>` prefix.
    pub async fn provision_srv(
        &self,
        http: &reqwest::Client,
        service: &str,
        proto: &str,
        subdomain: &str,
        domain: &str,
        port: u16,
    ) -> Result<(), String> {
        let fqdn = format!("{subdomain}.{domain}");
        let name = Self::srv_name(service, proto, subdomain, domain);
        if self.is_mock() {
            info!(
                "(Cloudflare mock) zone {}: SRV {name} -> {fqdn}:{port}",
                self.zone_id
            );
            return Ok(());
        }
        // Idempotent: drop any existing record of this name first (stale port, a prior
        // session, or a manually-created one), then create fresh.
        let _ = self
            .remove_srv(http, service, proto, subdomain, domain)
            .await;
        let url = format!(
            "https://api.cloudflare.com/client/v4/zones/{}/dns_records",
            self.zone_id
        );
        // Cloudflare wants the full record name at the top level and only the numeric
        // fields + target in `data`; the nested service/proto form is rejected (9000).
        let body = json!({
            "type": "SRV",
            "name": name,
            "ttl": 1, // 1 = automatic
            "data": {
                "priority": 0, "weight": 5, "port": port, "target": fqdn
            }
        });
        let v = self.send(http.post(&url).json(&body)).await?;
        if v["success"].as_bool() == Some(true) {
            info!("(Cloudflare) provisioned SRV {name} -> {fqdn}:{port}");
            Ok(())
        } else {
            Err(format!("cloudflare SRV create failed: {}", v["errors"]))
        }
    }

    /// Remove a tunnel's SRV record(s) on teardown (best-effort): find by name, delete.
    pub async fn remove_srv(
        &self,
        http: &reqwest::Client,
        service: &str,
        proto: &str,
        subdomain: &str,
        domain: &str,
    ) -> Result<(), String> {
        let name = Self::srv_name(service, proto, subdomain, domain);
        if self.is_mock() {
            info!("(Cloudflare mock) zone {}: delete SRV {name}", self.zone_id);
            return Ok(());
        }
        let list_url = format!(
            "https://api.cloudflare.com/client/v4/zones/{}/dns_records?type=SRV&name={}",
            self.zone_id, name
        );
        let v = self.send(http.get(&list_url)).await?;
        if let Some(records) = v["result"].as_array() {
            for rec in records {
                if let Some(id) = rec["id"].as_str() {
                    let del = format!(
                        "https://api.cloudflare.com/client/v4/zones/{}/dns_records/{}",
                        self.zone_id, id
                    );
                    if let Err(e) = self.send(http.delete(&del)).await {
                        warn!("cloudflare SRV delete {id} failed: {e}");
                    }
                }
            }
        }
        Ok(())
    }

    /// Attach the bearer token, send, and parse the JSON envelope.
    async fn send(&self, req: reqwest::RequestBuilder) -> Result<serde_json::Value, String> {
        let resp = req
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        resp.json::<serde_json::Value>()
            .await
            .map_err(|e| e.to_string())
    }
}
