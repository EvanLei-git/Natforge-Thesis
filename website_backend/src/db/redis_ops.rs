//! Redis-backed ephemeral state: the RFC 8628 device-authorization codes, which
//! are naturally short-lived and a perfect fit for Redis key TTLs.

use anyhow::Context;
use redis::aio::ConnectionManager;
use redis::AsyncCommands;

const TTL_SECS: i64 = 600; // 10 minutes

fn user_key(user_code: &str) -> String {
    format!("nf:devcode:{user_code}")
}
fn device_index_key(device_code: &str) -> String {
    format!("nf:devcode:dc:{device_code}")
}

#[derive(Debug)]
pub struct DeviceRecord {
    pub device_code: String,
    pub approved_user: Option<i32>,
}

/// Store a fresh device/user code pair (both expire together after 10 minutes).
pub async fn devcode_create(redis: &ConnectionManager, user_code: &str, device_code: &str) -> anyhow::Result<()> {
    let mut conn = redis.clone();
    let uk = user_key(user_code);
    let _: () = conn
        .hset_multiple(&uk, &[("device_code", device_code), ("approved_user", "")])
        .await
        .context("redis devcode hset")?;
    let _: () = conn.expire(&uk, TTL_SECS).await?;
    let _: () = conn.set_ex(device_index_key(device_code), user_code, TTL_SECS as u64).await?;
    Ok(())
}

/// Mark a user code approved by `uid`. Returns false if the code does not exist
/// (expired or never issued).
pub async fn devcode_approve(redis: &ConnectionManager, user_code: &str, uid: i32) -> anyhow::Result<bool> {
    let mut conn = redis.clone();
    let uk = user_key(user_code);
    let exists: bool = conn.exists(&uk).await?;
    if !exists {
        return Ok(false);
    }
    let _: () = conn.hset(&uk, "approved_user", uid.to_string()).await?;
    Ok(true)
}

/// Resolve a device code to its approval state. `None` => unknown/expired.
pub async fn devcode_status(redis: &ConnectionManager, device_code: &str) -> anyhow::Result<Option<DeviceRecord>> {
    let mut conn = redis.clone();
    let user_code: Option<String> = conn.get(device_index_key(device_code)).await?;
    let Some(user_code) = user_code else {
        return Ok(None);
    };
    let uk = user_key(&user_code);
    let fields: Option<(String, String)> = conn
        .hget(&uk, &["device_code", "approved_user"])
        .await?;
    let Some((stored_device, approved)) = fields else {
        return Ok(None);
    };
    if stored_device != device_code {
        return Ok(None);
    }
    let approved_user = if approved.is_empty() {
        None
    } else {
        approved.parse::<i32>().ok()
    };
    Ok(Some(DeviceRecord { device_code: stored_device, approved_user }))
}
