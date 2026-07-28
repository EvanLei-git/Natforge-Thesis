//! IP → country resolution for the control plane, backed by an optional MaxMind
//! GeoLite2-Country database. Used to enforce admin region blocks at login/
//! registration. Degrades to "unknown country" (allow) when no database is set.

use std::net::IpAddr;
use std::sync::Arc;

use axum::http::HeaderMap;
use maxminddb::{Reader, geoip2};
use tracing::{info, warn};

#[derive(Clone, Default)]
pub struct GeoDb {
    reader: Option<Arc<Reader<Vec<u8>>>>,
}

impl GeoDb {
    pub fn open(path: &str) -> Self {
        if path.trim().is_empty() {
            info!("no GEOIP_DB configured; login/registration geo-blocking disabled");
            return GeoDb::default();
        }
        match Reader::open_readfile(path) {
            Ok(r) => {
                info!("GeoIP database loaded from {path}");
                GeoDb {
                    reader: Some(Arc::new(r)),
                }
            }
            Err(e) => {
                warn!("GeoIP database at '{path}' unavailable ({e}); geo-blocking disabled");
                GeoDb::default()
            }
        }
    }

    /// ISO-3166 alpha-2 country (uppercased) for an IP, or None when unknown.
    pub fn country(&self, ip: IpAddr) -> Option<String> {
        let reader = self.reader.as_ref()?;
        let rec: geoip2::Country = reader.lookup(ip).ok()?;
        rec.country?.iso_code.map(|c| c.to_uppercase())
    }

    /// Resolve the caller's country from forwarded-IP headers (set by the edge /
    /// CDN). Looks at `CF-Connecting-IP` then the first hop of `X-Forwarded-For`.
    /// Direct connections (no header) resolve to None and are never geo-blocked.
    pub fn country_from_headers(&self, headers: &HeaderMap) -> Option<String> {
        let ip = client_ip(headers)?;
        self.country(ip)
    }
}

/// Best-effort client IP from proxy headers. None for direct/local connections.
pub fn client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    if let Some(v) = headers
        .get("cf-connecting-ip")
        .and_then(|h| h.to_str().ok())
        && let Ok(ip) = v.trim().parse()
    {
        return Some(ip);
    }
    if let Some(v) = headers.get("x-forwarded-for").and_then(|h| h.to_str().ok())
        && let Some(first) = v.split(',').next()
        && let Ok(ip) = first.trim().parse()
    {
        return Some(ip);
    }
    None
}
