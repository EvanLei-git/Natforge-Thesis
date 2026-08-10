#!/bin/bash
# ==============================================================================
# NatForge - dedicated data-plane node tuning
# ==============================================================================
# Run this ONCE on a machine that runs ONLY the core (a relay-only regional
# node, e.g. a US/Asia VM), NOT the head-node that also hosts Postgres, Redis,
# the website and monitoring. It reclaims almost the whole port range for the
# dedicated TCP/UDP route pool by:
#   1. narrowing the kernel's ephemeral (outbound source) port range so it no
#      longer overlaps the pool, and letting outbound connections reuse
#      TIME_WAIT ports, and
#   2. opening the firewall for the public listeners and the pool.
#
# It deliberately does NOT change the head-node's conservative 20000-20100
# default; that box must keep a small pool to avoid its co-located services.
#
# Pool floor is 10000 so the node's own ports (3001, 4000) and a local
# monitoring stack on its defaults (Grafana 3030, Prometheus 9090,
# node_exporter 9100) all sit BELOW the pool and never collide with it.
#
# After running this, the installer (or you) must set in the node env
# (/etc/natforge/natforge-node.env):
#     PUBLIC_PORT_MIN=10000
#     PUBLIC_PORT_MAX=60999
# and restart the core, so it seeds the larger pool on re-registration:
#     sudo systemctl restart natforge-node
# ==============================================================================

set -euo pipefail

# Overridable via env (the installer passes POOL_MIN/POOL_MAX through).
POOL_MIN="${POOL_MIN:-10000}"
POOL_MAX="${POOL_MAX:-60999}"
EPHEMERAL_LOW="${EPHEMERAL_LOW:-61000}"
EPHEMERAL_HIGH="${EPHEMERAL_HIGH:-65535}"

log() { echo -e "\e[1;36m[node-tune]\e[0m $1"; }
err() { echo -e "\e[1;31m[ERROR]\e[0m $1" >&2; exit 1; }

[[ "$(uname -s)" == "Linux" ]] || err "this tuning is Linux-specific (ip_local_port_range)."
[[ "${EUID}" -eq 0 ]] || err "run as root: sudo bash scripts/dedicated-node.sh"

# The pool must sit entirely below the (new) ephemeral floor, or a pooled
# listener could collide with a kernel-chosen outbound source port.
if (( POOL_MAX >= EPHEMERAL_LOW )); then
    err "POOL_MAX ($POOL_MAX) must be below the ephemeral floor ($EPHEMERAL_LOW)."
fi
if (( POOL_MIN < 1024 )); then
    err "POOL_MIN ($POOL_MIN) must be >= 1024 (stay out of the privileged range)."
fi

# ---------------------------------------------------------------------------
# 1. Kernel: shrink the outbound ephemeral range to a small high band and let
#    outbound connections recycle TIME_WAIT ports. Safe because a relay node
#    makes only a handful of long-lived/pooled outbound connections (Redis and
#    the control-plane API), so a few thousand ephemeral ports is ample.
# ---------------------------------------------------------------------------
SYSCTL_FILE=/etc/sysctl.d/99-natforge-node.conf
log "writing ${SYSCTL_FILE} (ephemeral ${EPHEMERAL_LOW}-${EPHEMERAL_HIGH}, tcp_tw_reuse=1)"
cat > "${SYSCTL_FILE}" <<EOF
# NatForge dedicated data-plane node. Keep the kernel's outbound ephemeral
# ports in a small high band so the rest of the range is free for the public
# route pool. A relay makes few outbound connections, so this reserve is ample;
# tcp_tw_reuse lets those outbound connections recycle TIME_WAIT ports.
net.ipv4.ip_local_port_range = ${EPHEMERAL_LOW} ${EPHEMERAL_HIGH}
net.ipv4.tcp_tw_reuse = 1
EOF
sysctl --system >/dev/null
log "applied: ip_local_port_range = $(cat /proc/sys/net/ipv4/ip_local_port_range)"

# ---------------------------------------------------------------------------
# 2. Firewall: only ADD allow rules (never enable/deny), so this can never lock
#    out SSH. If ufw is not the firewall in use, print the rules to apply.
# ---------------------------------------------------------------------------
if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q "Status: active"; then
    log "ufw is active: adding allow rules for the listeners and the pool"
    ufw allow 80/tcp   >/dev/null
    ufw allow 443/tcp  >/dev/null
    ufw allow 4000/tcp >/dev/null
    ufw allow "${POOL_MIN}:${POOL_MAX}/tcp" >/dev/null
    ufw allow "${POOL_MIN}:${POOL_MAX}/udp" >/dev/null
    log "opened tcp 80,443,4000 and tcp/udp ${POOL_MIN}-${POOL_MAX}"
    log "NOTE: :3001 (internal API) is NOT opened publicly. Allow it only from the"
    log "      control-plane host, e.g.: ufw allow from <HEAD_IP> to any port 3001 proto tcp"
else
    log "ufw is not active. Open these on the host firewall AND the cloud firewall/NSG:"
    echo "        tcp: 80, 443, 4000, ${POOL_MIN}-${POOL_MAX}"
    echo "        udp: ${POOL_MIN}-${POOL_MAX}"
    echo "        :3001 (internal API): only from the control-plane host, never public."
fi

log "cloud reminder: the same ports must also be open in your provider's firewall / NSG."
log "next: ensure PUBLIC_PORT_MIN=${POOL_MIN} PUBLIC_PORT_MAX=${POOL_MAX} in the node env, then:"
log "      sudo systemctl restart natforge-node"
log "done. this node can now host up to $((POOL_MAX - POOL_MIN + 1)) dedicated TCP/UDP pool ports"
log "      (plus unlimited HTTP/HTTPS subdomains on the shared :80/:443)."
