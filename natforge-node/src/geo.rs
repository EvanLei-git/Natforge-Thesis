//! IP -> country resolution backed by a MaxMind GeoLite2-Country database.
//!
//! The database is optional: when `GEOIP_DB` is unset or the file is missing, the
//! reader degrades to "unknown country" for every IP, which disables geo-blocking
//! rather than failing the node (the safe default). The reader is **hot-reloadable**:
//! a refreshed database (e.g. from `scripts/update-geoip.sh` on a cron) is picked up
//! without a restart. Lookups are infallible from the caller's side.

use std::net::IpAddr;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use maxminddb::{Reader, geoip2};
use tracing::{info, warn};

#[derive(Default)]
pub struct GeoDb {
    reader: RwLock<Option<Arc<Reader<Vec<u8>>>>>,
    path: String,
    mtime: RwLock<Option<SystemTime>>,
}

impl GeoDb {
    /// Open the database at `path`. An empty path or an unreadable file yields a
    /// disabled reader (logged once), never an error.
    pub fn open(path: &str) -> Self {
        let db = GeoDb {
            reader: RwLock::new(None),
            path: path.to_string(),
            mtime: RwLock::new(None),
        };
        if path.trim().is_empty() {
            info!("no GEOIP_DB configured; country lookups + geo-blocking disabled");
        } else {
            db.load();
        }
        db
    }

    fn file_mtime(path: &str) -> Option<SystemTime> {
        let modified = match std::fs::metadata(path) {
            Ok(m) => m.modified(),
            Err(e) => Err(e),
        };
        match modified {
            Ok(t) => Some(t),
            Err(_) => None,
        }
    }

    /// (Re)load the database from disk. Returns true if a reader is now active.
    fn load(&self) -> bool {
        match Reader::open_readfile(&self.path) {
            Ok(r) => {
                *self.reader.write().unwrap() = Some(Arc::new(r));
                *self.mtime.write().unwrap() = Self::file_mtime(&self.path);
                info!("GeoIP database loaded from {}", self.path);
                true
            }
            Err(e) => {
                warn!(
                    "GeoIP database at '{}' unavailable ({e}); geo-blocking disabled",
                    self.path
                );
                false
            }
        }
    }

    /// Reload if the file on disk changed since the last load (called on a timer),
    /// so an out-of-band refresh applies without restarting the node.
    pub fn reload_if_changed(&self) {
        if self.path.trim().is_empty() {
            return;
        }
        let current = Self::file_mtime(&self.path);
        let last = *self.mtime.read().unwrap();
        if current.is_some() && current != last && self.load() {
            info!("GeoIP database reloaded");
        }
    }

    /// ISO-3166 alpha-2 country code for `ip` (uppercased), or None when unknown
    /// (no database, private/loopback address, or IP not present in the database).
    pub fn country(&self, ip: IpAddr) -> Option<String> {
        let guard = self.reader.read().unwrap();
        let reader = guard.as_ref()?;
        let result = match reader.lookup(ip) {
            Ok(r) => r,
            Err(_) => return None,
        };
        let decoded = match result.decode() {
            Ok(v) => v,
            Err(_) => return None,
        };
        let rec: geoip2::Country = decoded?;
        match rec.country.iso_code {
            Some(c) => Some(c.to_uppercase()),
            None => None,
        }
    }
}
