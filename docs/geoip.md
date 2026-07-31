# GeoIP country blocking

Geo-blocking (platform-wide blocks set by an admin, and per-tunnel country blocks set
by an owner) resolves each public connection's source IP to a country with a MaxMind
**GeoLite2-Country** database. Enforcement is always compiled in; it only takes effect
once the database is present. Without it, every IP resolves to "unknown" and is never
blocked, which is the safe default (we never block on missing data).

## Provision the database

GeoLite2 is free but needs a MaxMind account + licence key:
<https://www.maxmind.com/en/geolite2/signup>. Then, on the node:

```sh
MAXMIND_LICENSE_KEY=your_key \
GEOIP_DB=/etc/natforge/GeoLite2-Country.mmdb \
  bash scripts/update-geoip.sh
```

Point the core (and website) at it with `GEOIP_DB=/etc/natforge/GeoLite2-Country.mmdb`.

## Keep it fresh

MaxMind refreshes GeoLite2 a few times a week. Run the script on a cron:

```cron
0 4 * * 1  MAXMIND_LICENSE_KEY=your_key GEOIP_DB=/etc/natforge/GeoLite2-Country.mmdb bash /path/to/scripts/update-geoip.sh
```

The core watches the file and **hot-reloads** a refreshed database within the hour, so
no restart is needed.
