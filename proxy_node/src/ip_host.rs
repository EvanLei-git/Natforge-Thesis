//! IP Host (Edge Node) mode.
//!
//! Registers this machine as a volunteer residential relay and runs a TCP
//! forwarder: every connection that arrives on the public listen port is relayed
//! out through *this host's* network to the configured upstream (typically the
//! core proxy's public port for a tunnel). Because egress happens from the edge
//! node, end users reach the service via the residential IP rather than the
//! datacenter — demonstrating Scenario B (verifiable public-IP change). Byte
//! volume is accounted against the configured bandwidth ceiling.
//!
//! Full UDP hole-punching / STUN P2P establishment is future work (thesis §7.3);
//! this provides the working relay tier the architecture falls back to.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

async fn set_active(control_plane: &str, session_token: &str, active: bool) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{control_plane}/api/ip_host/status"))
        .bearer_auth(session_token)
        .json(&serde_json::json!({ "active": active }))
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(anyhow!("status update failed: {}", resp.status()));
    }
    Ok(())
}

/// Best-effort public IP discovery (the "STUN-like" reflexive lookup).
async fn discover_public_ip() -> String {
    let client = reqwest::Client::new();
    match tokio::time::timeout(
        std::time::Duration::from_secs(4),
        client.get("https://api.ipify.org").send(),
    )
    .await
    {
        Ok(Ok(resp)) => resp.text().await.unwrap_or_else(|_| "unknown".into()),
        _ => "unknown (offline/simulated)".into(),
    }
}

pub async fn run(
    control_plane: &str,
    listen_port: u16,
    upstream: &str,
    max_bandwidth_mbps: u32,
    session_token: &str,
) -> Result<()> {
    set_active(control_plane, session_token, true).await?;
    let public_ip = discover_public_ip().await;

    info!("════════════════════════════════════════════════════");
    info!(" Edge Node ACTIVE");
    info!("   Public IP        : {public_ip}");
    info!("   Listening on     : 0.0.0.0:{listen_port}");
    info!("   Relaying to       : {upstream}");
    info!("   Bandwidth ceiling : {max_bandwidth_mbps} Mbps");
    info!("════════════════════════════════════════════════════");

    let listener = TcpListener::bind(("0.0.0.0", listen_port)).await?;
    let total = Arc::new(AtomicU64::new(0));
    let upstream = upstream.to_string();

    loop {
        let (inbound, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                warn!("accept error: {e}");
                continue;
            }
        };
        info!("Relaying connection from {peer} -> {upstream}");
        let up = upstream.clone();
        let counter = total.clone();
        tokio::spawn(async move {
            if let Err(e) = relay(inbound, &up, counter).await {
                warn!("relay closed: {e}");
            }
        });
    }
}

async fn relay(mut inbound: TcpStream, upstream: &str, counter: Arc<AtomicU64>) -> Result<()> {
    let mut out = TcpStream::connect(upstream).await?;
    let (a, b) = copy_bidirectional(&mut inbound, &mut out).await?;
    let moved = counter.fetch_add(a + b, Ordering::Relaxed) + a + b;
    info!("Connection done (+{} bytes, {} total relayed)", a + b, moved);
    Ok(())
}
