#!/usr/bin/env bash
# Download the latest MaxMind GeoLite2-Country database used for geo-blocking.
# GeoLite2 is free but needs an account + licence key:
#   https://www.maxmind.com/en/geolite2/signup
#
# Usage:
#   MAXMIND_LICENSE_KEY=xxxx GEOIP_DB=/etc/natforge/GeoLite2-Country.mmdb \
#     bash scripts/update-geoip.sh
#
# The core watches GEOIP_DB and hot-reloads a refreshed file within the hour, so
# running this on a cron keeps geo-blocking current with no restart.
set -euo pipefail
: "${MAXMIND_LICENSE_KEY:?set MAXMIND_LICENSE_KEY (free key at maxmind.com/en/geolite2/signup)}"
DEST="${GEOIP_DB:-/etc/natforge/GeoLite2-Country.mmdb}"
EDITION="${MAXMIND_EDITION:-GeoLite2-Country}"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
URL="https://download.maxmind.com/app/geoip_download?edition_id=${EDITION}&license_key=${MAXMIND_LICENSE_KEY}&suffix=tar.gz"

echo "Downloading ${EDITION}..."
curl -fsSL "$URL" -o "$TMP/db.tar.gz"
tar -xzf "$TMP/db.tar.gz" -C "$TMP"
MMDB="$(find "$TMP" -name '*.mmdb' | head -1)"
[ -n "$MMDB" ] || { echo "no .mmdb in the archive (bad or unauthorized licence key?)" >&2; exit 1; }

mkdir -p "$(dirname "$DEST")"
install -m 0644 "$MMDB" "$DEST"
echo "Installed $DEST ($(stat -c%s "$DEST" 2>/dev/null || wc -c <"$DEST") bytes)."
echo "The core hot-reloads it within the hour; no restart needed."
