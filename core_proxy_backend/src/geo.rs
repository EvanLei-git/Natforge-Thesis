//! IP → country resolution backed by a MaxMind GeoLite2-Country database.
//!
//! The database is optional: when `GEOIP_DB` is unset or the file is missing, the
//! reader degrades to "unknown country" for every IP, which disables geo-blocking
//! rather than failing the node. Lookups are infallible from the caller's side.

use std::net::IpAddr;
use std::sync::Arc;

use maxminddb::{geoip2, Reader};
use tracing::{info, warn};

#[derive(Clone, Default)]
pub struct GeoDb {
    reader: Option<Arc<Reader<Vec<u8>>>>,
}

impl GeoDb {
    /// Open the database at `path`. An empty path or an unreadable file yields a
    /// disabled reader (logged once), never an error.
    pub fn open(path: &str) -> Self {
        if path.trim().is_empty() {
            info!("no GEOIP_DB configured; country lookups + geo-blocking disabled");
            return GeoDb::default();
        }
        match Reader::open_readfile(path) {
            Ok(r) => {
                info!("GeoIP database loaded from {path}");
                GeoDb { reader: Some(Arc::new(r)) }
            }
            Err(e) => {
                warn!("GeoIP database at '{path}' unavailable ({e}); geo-blocking disabled");
                GeoDb::default()
            }
        }
    }

    /// ISO-3166 alpha-2 country code for `ip` (uppercased), or None when unknown
    /// (no database, private/loopback address, or IP not present in the database).
    pub fn country(&self, ip: IpAddr) -> Option<String> {
        let reader = self.reader.as_ref()?;
        let rec: geoip2::Country = reader.lookup(ip).ok()?;
        rec.country?.iso_code.map(|c| c.to_uppercase())
    }
}
