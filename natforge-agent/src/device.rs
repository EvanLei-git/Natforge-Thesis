//! Persistent device identity. `natforge enroll` runs the agent-first device flow
//! (start -> user approves + names it in the dashboard -> poll) and stores the
//! long-lived device token under `~/.config/natforge/device.json` (0600 on Unix).
//! `natforge run` loads it; config-driven serving arrives in the next phase.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Serialize, Deserialize, Clone)]
pub struct StoredDevice {
    pub control_plane: String,
    pub device_id: i64,
    pub name: String,
    pub device_token: String,
}

#[derive(Deserialize)]
struct EnrollStart {
    device_code: String,
    user_code: String,
    interval: u32,
}

#[derive(Deserialize)]
struct EnrollPoll {
    status: String,
    device_token: Option<String>,
    device_id: Option<i64>,
    name: Option<String>,
}

fn config_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".config").join("natforge"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("device.json"))
}

pub fn load() -> Result<StoredDevice> {
    let path = config_path()?;
    let data = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "no enrolled device at {} - run `natforge enroll` first",
            path.display()
        )
    })?;
    Ok(serde_json::from_str(&data)?)
}

pub fn save(dev: &StoredDevice) -> Result<()> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = config_path()?;
    std::fs::write(&path, serde_json::to_string_pretty(dev)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Start the device flow, print the code for the user to approve and name in the
/// dashboard, and poll until a device token comes back.
pub async fn enroll(control_plane: &str) -> Result<StoredDevice> {
    let client = reqwest::Client::new();
    let start: EnrollStart = client
        .post(format!("{control_plane}/api/devices/enroll/start"))
        .send()
        .await?
        .json()
        .await?;

    println!();
    println!("  To add this device, open your NatForge dashboard:");
    println!("    {control_plane}/dashboard  ->  Add device");
    println!("    enter this code:  {}", start.user_code);
    println!("    and give the device a name.");
    println!();
    println!("  Waiting for approval...");

    let interval = Duration::from_secs(start.interval.max(1) as u64);
    loop {
        tokio::time::sleep(interval).await;
        let poll: EnrollPoll = client
            .post(format!("{control_plane}/api/devices/enroll/token"))
            .json(&serde_json::json!({ "device_code": start.device_code }))
            .send()
            .await?
            .json()
            .await?;
        match poll.status.as_str() {
            "approved" => {
                let device_token = poll
                    .device_token
                    .ok_or_else(|| anyhow!("approved without a token"))?;
                let device_id = poll
                    .device_id
                    .ok_or_else(|| anyhow!("approved without a device id"))?;
                let name = poll.name.unwrap_or_else(|| "device".to_string());
                info!("device '{name}' (#{device_id}) enrolled");
                return Ok(StoredDevice {
                    control_plane: control_plane.to_string(),
                    device_id,
                    name,
                    device_token,
                });
            }
            "authorization_pending" => continue,
            other => return Err(anyhow!("enrollment failed: {other}")),
        }
    }
}
