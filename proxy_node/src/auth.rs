//! Obtaining a session token for the headless agent.
//!
//! Three paths are supported, in priority order: an explicit `--token`, classic
//! `--email`/`--password` login, or the RFC 8628 device-authorization flow where
//! the user approves the CLI from their browser.

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Deserialize;
use tracing::info;

#[derive(Deserialize)]
struct AuthResponse {
    token: String,
}

#[derive(Deserialize)]
struct DeviceStart {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: u32,
}

#[derive(Deserialize)]
struct DeviceToken {
    status: String,
    token: Option<String>,
}

/// Resolve a session token using whichever credentials were supplied.
pub async fn obtain_token(
    control_plane: &str,
    token: &Option<String>,
    email: &Option<String>,
    password: &Option<String>,
) -> Result<String> {
    if let Some(t) = token {
        info!("Using session token supplied on the command line");
        return Ok(t.clone());
    }
    if let (Some(e), Some(p)) = (email, password) {
        return login(control_plane, e, p).await;
    }
    device_flow(control_plane).await
}

async fn login(control_plane: &str, email: &str, password: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{control_plane}/api/auth/login"))
        .json(&serde_json::json!({ "email": email, "password": password }))
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(anyhow!("login failed: {}", resp.status()));
    }
    let body: AuthResponse = resp.json().await?;
    info!("Logged in as {email}");
    Ok(body.token)
}

async fn device_flow(control_plane: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let start: DeviceStart = client
        .post(format!("{control_plane}/api/auth/device/start"))
        .send()
        .await?
        .json()
        .await?;

    println!();
    println!("  ┌──────────────────────────────────────────────┐");
    println!("  │  To authorise this device, visit:              │");
    println!("  │    {}", start.verification_uri);
    println!("  │  and enter the code:  {}", start.user_code);
    println!("  └──────────────────────────────────────────────┘");
    println!();

    let interval = Duration::from_secs(start.interval.max(1) as u64);
    loop {
        tokio::time::sleep(interval).await;
        let poll: DeviceToken = client
            .post(format!("{control_plane}/api/auth/device/token"))
            .json(&serde_json::json!({ "device_code": start.device_code }))
            .send()
            .await?
            .json()
            .await?;
        match poll.status.as_str() {
            "approved" => {
                info!("Device authorised");
                return poll.token.ok_or_else(|| anyhow!("approved without token"));
            }
            "authorization_pending" => continue,
            other => return Err(anyhow!("device authorization failed: {other}")),
        }
    }
}
