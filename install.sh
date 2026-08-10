#!/bin/bash

# ==============================================================================
# NatForge - Deployment & Daemonization Script
# ==============================================================================
# Automates deployment of the distributed NatForge platform.
# Components available for configuration:
#   1. natforge-backend (control plane): auth, tunnels, region registry, admin, dashboard.
#   2. natforge-node (data plane, one per region): TLS+yamux relay, per-region
#      public-port pool, geo-blocking. Self-registers on boot.
#   3. natforge-agent (agent): the Service Host agent on end-user machines.
#
# Usage:
#   sudo ./install.sh --component <website | core | node>
#
#   --dedicated (core only): set up a relay-only REGIONAL node in ONE command.
#   It widens the public pool to 10000-60999, applies the kernel + firewall
#   tuning (scripts/dedicated-node.sh), and enables + starts the service. Pass
#   the node's settings as flags, or you will be prompted for the required ones:
#     --node-id <id>           unique node id, e.g. us-1               (required)
#     --public-host <host>     this node's wildcard host, e.g. us.natforge.com (required)
#     --head-host <host|ip>    head-node address for WEBSITE_URL + REDIS_URL   (required)
#     --jwt-secret <hex>       MUST match the head-node                (required)
#     --internal-secret <hex>  MUST match the head-node                (required)
#     --node-name <name>       display name        (default: node id)
#     --node-region <region>   region label        (default: node id)
#     --internal-url <url>     how the head reaches this node's :3001
#                              (default: http://<this host's IP>:3001)
#     --cf-token <t> --cf-zone <z>   Cloudflare SRV credentials (default: mock)
#   Without --dedicated the core installs with the safe shared default pool
#   (20000-20100) and head-local values, and does NOT auto-start.
# ==============================================================================

set -e

INSTALL_DIR="/usr/local/bin"
SYSTEMD_DIR="/etc/systemd/system"
ENV_DIR="/etc/natforge"

log() { echo -e "\e[1;36m[INSTALLER]\e[0m $1"; }
err() { echo -e "\e[1;31m[ERROR]\e[0m $1"; exit 1; }

# Parse arguments
COMPONENT=""
DEDICATED=""
NODE_ID=""; NODE_NAME=""; NODE_REGION=""; PUBLIC_HOST=""; HEAD_HOST=""
INTERNAL_URL=""; JWT_SECRET=""; INTERNAL_SECRET=""; CF_TOKEN=""; CF_ZONE=""

while [[ "$#" -gt 0 ]]; do
    case $1 in
        --component) COMPONENT="$2"; shift ;;
        --dedicated) DEDICATED="1" ;;
        --node-id) NODE_ID="$2"; shift ;;
        --node-name) NODE_NAME="$2"; shift ;;
        --node-region) NODE_REGION="$2"; shift ;;
        --public-host) PUBLIC_HOST="$2"; shift ;;
        --head-host) HEAD_HOST="$2"; shift ;;
        --internal-url) INTERNAL_URL="$2"; shift ;;
        --jwt-secret) JWT_SECRET="$2"; shift ;;
        --internal-secret) INTERNAL_SECRET="$2"; shift ;;
        --cf-token) CF_TOKEN="$2"; shift ;;
        --cf-zone) CF_ZONE="$2"; shift ;;
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

    # Write environment variables. EXTRA_ENV uses "\n" separators; expand them
    # to real newlines so systemd's EnvironmentFile parses one KEY=VALUE per line.
    {
        echo "# Auto-Generated Environment for $SERVICE_NAME"
        printf '%b\n' "$EXTRA_ENV"
    } > "$ENV_DIR/$SERVICE_NAME.env"

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

# Prompt for a required value if it was not supplied as a flag (secrets hidden).
require_or_prompt() {
    local __var="$1" __msg="$2" __secret="${3:-}"
    [[ -n "${!__var:-}" ]] && return 0
    local __val=""
    if [[ "$__secret" == "secret" ]]; then
        read -r -s -p "  $__msg: " __val || true; echo
    else
        read -r -p "  $__msg: " __val || true
    fi
    [[ -n "$__val" ]] || err "$__var is required (pass the flag or answer the prompt)."
    printf -v "$__var" '%s' "$__val"
}

# Enable on boot, and start now if the binary is already installed.
enable_and_start() {
    local svc="$1" bin="$2"
    systemctl daemon-reload
    systemctl enable "$svc.service" >/dev/null 2>&1 || true
    if [[ -x "$bin" ]]; then
        systemctl restart "$svc.service"
        log "$svc enabled on boot and started. Status: systemctl status $svc"
    else
        log "$svc enabled on boot. Install the binary at $bin, then: systemctl start $svc"
    fi
}

case $COMPONENT in
    "website")
        configure_service "natforge-backend" "natforge-backend" "$INSTALL_DIR/natforge-backend" \
            "PORT=3000\nNATFORGE_DOMAIN=natforge.com\nCORE_URL=http://127.0.0.1:3001\nFRONTEND_DIR=/usr/local/share/natforge/natforge-frontend\nDATABASE_URL=postgres://natforge:natforge@127.0.0.1:5432/natforge\nREDIS_URL=redis://127.0.0.1:6379\nGEOIP_DB=/etc/natforge/GeoLite2-Country.mmdb\nJWT_SECRET=CHANGE_ME\nINTERNAL_SECRET=CHANGE_ME"
        ;;
    "core")
        if [[ -z "$DEDICATED" ]]; then
            # Head-node / shared box: conservative pool (20000-20100, safe alongside
            # the co-hosted Postgres/Redis/website/monitoring) and head-local defaults.
            # Not auto-started here; the operator enables it, or the container deploy runs.
            configure_service "natforge-node" "natforge-node" "$INSTALL_DIR/natforge-node" \
                "CORE_INTERNAL_PORT=3001\nCORE_CONTROL_PORT=4000\nHTTP_PORT=80\nHTTPS_PORT=443\nPUBLIC_HOST=natforge.com\nNODE_ID=edge-1\nNODE_NAME=Primary\nNODE_REGION=Default\nCONTROL_ENDPOINT=natforge.com:4000\nINTERNAL_URL=http://127.0.0.1:3001\nPUBLIC_PORT_MIN=20000\nPUBLIC_PORT_MAX=20100\nGEOIP_DB=/etc/natforge/GeoLite2-Country.mmdb\nWEBSITE_URL=http://127.0.0.1:3000\nREDIS_URL=redis://127.0.0.1:6379\nJWT_SECRET=CHANGE_ME\nINTERNAL_SECRET=CHANGE_ME\nCF_API_TOKEN=mock_token"
        else
            # One-command relay-only REGIONAL node: collect the region settings (flags
            # or prompt), widen the pool, tune the OS, and enable + start the service.
            POOL_MIN=10000; POOL_MAX=60999
            require_or_prompt NODE_ID         "Node id (unique, e.g. us-1)"
            require_or_prompt PUBLIC_HOST     "This node's public wildcard host (e.g. us.natforge.com)"
            require_or_prompt HEAD_HOST       "Head-node host/IP (for WEBSITE_URL + REDIS_URL)"
            require_or_prompt JWT_SECRET      "JWT_SECRET (MUST match the head-node)" secret
            require_or_prompt INTERNAL_SECRET "INTERNAL_SECRET (MUST match the head-node)" secret
            NODE_NAME="${NODE_NAME:-$NODE_ID}"
            NODE_REGION="${NODE_REGION:-$NODE_ID}"
            CONTROL_ENDPOINT="$PUBLIC_HOST:4000"
            if [[ -z "$INTERNAL_URL" ]]; then
                NODE_IP="$(hostname -I 2>/dev/null | awk '{print $1}')"
                if [[ -n "$NODE_IP" ]]; then INTERNAL_URL="http://$NODE_IP:3001"
                else require_or_prompt INTERNAL_URL "How the head reaches this node's :3001 (e.g. http://10.0.0.5:3001)"; fi
            fi
            WEBSITE_URL="http://$HEAD_HOST:3000"
            REDIS_URL="redis://$HEAD_HOST:6379"
            CF_API_TOKEN="${CF_TOKEN:-mock_token}"
            CF_ZONE_ID="${CF_ZONE:-mock_zone}"
            log "Node $NODE_ID ($NODE_NAME / $NODE_REGION) @ $PUBLIC_HOST, pool $POOL_MIN-$POOL_MAX"
            log "Head  WEBSITE_URL=$WEBSITE_URL  REDIS_URL=$REDIS_URL  (must be reachable from this node)"
            log "Node  CONTROL_ENDPOINT=$CONTROL_ENDPOINT  INTERNAL_URL=$INTERNAL_URL  (keep :3001 private)"
            configure_service "natforge-node" "natforge-node" "$INSTALL_DIR/natforge-node" \
                "CORE_INTERNAL_PORT=3001\nCORE_CONTROL_PORT=4000\nHTTP_PORT=80\nHTTPS_PORT=443\nPUBLIC_HOST=$PUBLIC_HOST\nNODE_ID=$NODE_ID\nNODE_NAME=$NODE_NAME\nNODE_REGION=$NODE_REGION\nCONTROL_ENDPOINT=$CONTROL_ENDPOINT\nINTERNAL_URL=$INTERNAL_URL\nPUBLIC_PORT_MIN=$POOL_MIN\nPUBLIC_PORT_MAX=$POOL_MAX\nGEOIP_DB=/etc/natforge/GeoLite2-Country.mmdb\nWEBSITE_URL=$WEBSITE_URL\nREDIS_URL=$REDIS_URL\nJWT_SECRET=$JWT_SECRET\nINTERNAL_SECRET=$INTERNAL_SECRET\nCF_API_TOKEN=$CF_API_TOKEN\nCF_ZONE_ID=$CF_ZONE_ID"
            DIR="$(cd "$(dirname "$0")" && pwd)"
            if [[ -f "$DIR/scripts/dedicated-node.sh" ]]; then
                POOL_MIN="$POOL_MIN" POOL_MAX="$POOL_MAX" bash "$DIR/scripts/dedicated-node.sh"
            else
                log "run scripts/dedicated-node.sh on this VM to apply the kernel + firewall tuning."
            fi
            enable_and_start "natforge-node" "$INSTALL_DIR/natforge-node"
        fi
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
