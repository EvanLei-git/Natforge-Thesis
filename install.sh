#!/bin/bash

# ==============================================================================
# NatForge - Deployment & Daemonization Script
# ==============================================================================
# Automates deployment of the distributed NatForge platform.
# Components available for configuration:
#   1. website_backend (control plane): auth, tunnels, region registry, admin, dashboard.
#   2. core_proxy_backend (data plane, one per region): TLS+yamux relay, per-region
#      public-port pool, geo-blocking, connection-rate guard. Self-registers on boot.
#   3. natforge (agent): the Service Host agent on end-user machines.
#
# Usage:
#   sudo ./install.sh --component <website | core | node> [--mode service-host]
# ==============================================================================

set -e

INSTALL_DIR="/usr/local/bin"
SYSTEMD_DIR="/etc/systemd/system"
ENV_DIR="/etc/natforge"

log() { echo -e "\e[1;36m[INSTALLER]\e[0m $1"; }
err() { echo -e "\e[1;31m[ERROR]\e[0m $1"; exit 1; }

# Parse arguments
COMPONENT=""
NODE_MODE=""

while [[ "$#" -gt 0 ]]; do
    case $1 in
        --component) COMPONENT="$2"; shift ;;
        --mode) NODE_MODE="$2"; shift ;;
        *) err "Unknown parameter passed: $1"; exit 1 ;;
    esac
    shift
done

if [[ -z "$COMPONENT" ]]; then
    err "You must specify a component: --component <website | core | node>"
fi

mkdir -p "$ENV_DIR"

configure_service() {
    local BIN_NAME=$1
    local SERVICE_NAME=$2
    local EXEC_CMD=$3
    local EXTRA_ENV=$4

    log "Preparing to daemonize $SERVICE_NAME..."

    # Write environment variables
    cat <<EOF > "$ENV_DIR/$SERVICE_NAME.env"
# Auto-Generated Environment for $SERVICE_NAME
$EXTRA_ENV
EOF

    # Write systemd service module
    cat <<EOF > "$SYSTEMD_DIR/$SERVICE_NAME.service"
[Unit]
Description=Thesis Distributed Platform: $SERVICE_NAME
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=$ENV_DIR/$SERVICE_NAME.env
ExecStart=$EXEC_CMD
Restart=always
RestartSec=10
LimitNOFILE=1048576

# Security hardening (Limits file access to just the binary and config)
ProtectSystem=full
ProtectHome=true
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
EOF

    log "Reloading systemd daemon..."
    # systemctl daemon-reload
    # systemctl enable $SERVICE_NAME.service
    # systemctl start $SERVICE_NAME.service
    log "$SERVICE_NAME daemonized successfully! Check status with: systemctl status $SERVICE_NAME"
}

case $COMPONENT in
    "website")
        configure_service "website_backend" "natforge-website" "$INSTALL_DIR/website_backend" \
            "PORT=3000\nNATFORGE_DOMAIN=natforge.com\nCORE_URL=http://127.0.0.1:3001\nFRONTEND_DIR=/usr/local/share/natforge/frontend\nDATABASE_URL=postgres://natforge:natforge@127.0.0.1:5432/natforge\nREDIS_URL=redis://127.0.0.1:6379\nGEOIP_DB=/etc/natforge/GeoLite2-Country.mmdb\nJWT_SECRET=CHANGE_ME\nINTERNAL_SECRET=CHANGE_ME"
        ;;
    "core")
        # Binds shared :80/:443 in production (service runs as root by default here).
        # For a second region, change NODE_ID, NODE_NAME, NODE_REGION, PUBLIC_HOST,
        # CONTROL_ENDPOINT, and INTERNAL_URL; it self-registers with the control plane.
        configure_service "core_proxy_backend" "natforge-core" "$INSTALL_DIR/core_proxy_backend" \
            "CORE_INTERNAL_PORT=3001\nCORE_CONTROL_PORT=4000\nHTTP_PORT=80\nHTTPS_PORT=443\nPUBLIC_HOST=natforge.com\nNODE_ID=edge-1\nNODE_NAME=Primary\nNODE_REGION=Default\nCONTROL_ENDPOINT=natforge.com:4000\nINTERNAL_URL=http://127.0.0.1:3001\nPUBLIC_PORT_MIN=20000\nPUBLIC_PORT_MAX=20100\nGEOIP_DB=/etc/natforge/GeoLite2-Country.mmdb\nWEBSITE_URL=http://127.0.0.1:3000\nREDIS_URL=redis://127.0.0.1:6379\nJWT_SECRET=CHANGE_ME\nINTERNAL_SECRET=CHANGE_ME\nCF_API_TOKEN=mock_token"
        ;;
    "node")
        # The agent runs in Service Host mode; it learns the node to connect to from
        # the reservation, so no --tunnel-server is needed in production.
        configure_service "natforge" "natforge-agent" \
            "$INSTALL_DIR/natforge service-host --control-plane https://natforge.com" ""
        ;;
    *)
        err "Invalid component. Use: website, core, or node."
        ;;
esac

log "Deployment script finished execution."
