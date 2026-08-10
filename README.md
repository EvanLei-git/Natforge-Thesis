[![CI](https://github.com/EvanLei-git/Thesis-reverse-proxy/actions/workflows/ci.yml/badge.svg)](https://github.com/EvanLei-git/Thesis-reverse-proxy/actions/workflows/ci.yml)

# <img width="50" alt="natforge_flake" src="https://github.com/user-attachments/assets/a8279cf8-234f-4cb0-9d57-f9e6fddeed85" />  NatForge - Thesis

# Development of a High-Performance Distributed Reverse Proxy in Rust

<div align="center">
  <img width="843" height="199" alt="HUA-Logo-White-Transparent-RGB" src="https://github.com/user-attachments/assets/2a323c33-10e4-4249-9496-d78a6076252a" />
</div>

**NatForge makes a service running on a machine behind NAT/CGNAT reachable from the public internet, with no port forwarding.** Run a game server, website, API, or SSH box at home; NatForge gives it a public address (`sub.natforge.com`, or your own domain) that anyone can connect to.

It is a self-hostable, multi-region reverse-proxy and tunneling platform written in Rust (Tokio, Axum, yamux, rustls) with a PostgreSQL + Redis control plane, a framework-free web dashboard, and a single-binary agent.

## Why

IPv4 exhaustion pushed ISPs onto Carrier-Grade NAT (CGNAT), where thousands of subscribers share one public IP and no subscriber can open an inbound port. Hosting anything from home then means renting a VPS or buying a static IP. NatForge restores inbound reachability the other way around: your machine dials *out* to a relay node, and the relay accepts public connections and forwards them back down that outbound tunnel.

## How it works
<img width="1349" height="507" src="https://github.com/user-attachments/assets/8908152d-ea89-4793-8a89-ee2a9f889bce" />


1. You sign in to the dashboard, reserve a tunnel (pick a **region** and one or more **routes**), and receive a signed token.
2. The **agent** on your machine dials *outbound* to that region's **node**, wraps the connection in real TLS (a self-signed cert pinned by SHA-256 fingerprint), and upgrades it to a **yamux** session that multiplexes every route over one connection.
3. A **friend** connects to the public address. The node routes **HTTP by `Host`** and **HTTPS by TLS SNI** on shared `:80`/`:443`, and gives each raw **TCP/UDP** route its own dedicated public port. It opens one stream back to your agent, which relays to your local service. Relaying is **in-memory only** (never written to disk); only connection *metadata* is logged.

## Features

**Tunneling**
- Friends reach your service by **subdomain** (`sub.natforge.com`) over shared `:80`/`:443` (Host + SNI-passthrough routing), or via a **dedicated public port** for raw protocols.
- **Many routes per tunnel** over one multiplexed TLS connection: `http`, `https`, `tcp`, `udp`, or `both`.
- **HTTPS SNI passthrough** so the node never decrypts your traffic, plus optional **auto-HTTPS** for plain-HTTP routes using a wildcard certificate.
- **Custom domains:** front a tunnel with your own hostname (`play.mygame.com`) via bring-your-own-cert passthrough or automatic per-domain Let's Encrypt (ACME HTTP-01).
- **UDP tunneling** for UDP games/services.
- **DNS SRV** provisioning (opt-in per route) so SRV-aware clients (Minecraft, Mindustry) connect with no port.

**Multi-region**
- Nodes **self-register**; users pick which **region** hosts each tunnel.
- **Automatic failover:** a reconciliation sweep relocates a down node's tunnels onto a healthy node, and the agent re-homes on reconnect.
- **Cross-region migration:** move a live tunnel to another region without losing its subdomain.

**Agent and devices**
- A single static agent binary (glibc + musl). Use ad-hoc `service-host` mode, or the **persistent device** model: enroll a machine once (RFC 8628 device flow), then manage its services entirely from the dashboard, add/remove exposed ports live, one device serving many services at once.
- **Resilient reconnect:** TCP keepalive plus idempotent reservation keep the same subdomain and ports across drops and interface/IP changes.

**Control plane and dashboard**
- Argon2 password hashing + JWT sessions, and the RFC 8628 device-authorization grant for headless login.
- **Per-tunnel observability:** a bandwidth series and a per-connection log (source, country, bytes, duration).
- **Geo-blocking** (MaxMind GeoLite2), platform-wide and per-tunnel, enforced at the node (and gating login).
- **Self-service** subdomain / route-label / profile / password editing; **admin** panels for regions and nodes, all tunnels, users (ban/delete), and platform policy.
- **Prometheus metrics** (`/metrics`) with a self-hosted Prometheus + Grafana stack.

**Privacy and security**
- In-memory relaying only, no payload ever touches disk; only metadata is logged, visible to the tunnel owner and admins.
- The agent-to-core channel is always real TLS with fingerprint pinning; scoped, short-lived tunnel tokens bind a single subdomain.
- Context-aware output encoding on the dashboard (`escapeHtml` / `escapeAttr`) closes stored-XSS vectors.

## Quickstart (local)

```bash
docker compose up -d                     # PostgreSQL + Redis
cargo run -p natforge-backend             # control plane + dashboard on :3000
PUBLIC_HOST=natforge.com CONTROL_ENDPOINT=127.0.0.1:4000 \
  cargo run -p natforge-node        # a data-plane node (self-registers)
cargo run -p natforge-agent -- service-host \
  --email you@example.com --password '...' --route 8000:http
```

Open http://127.0.0.1:3000. The full local walkthrough and a one-command end-to-end test (`bash scripts/e2e.sh`, 29 assertions) are in **[docs/running.md](docs/running.md)**.

## Architecture

Four Rust artifacts around a **control-plane / data-plane split**:

| Component | Role |
|---|---|
| `natforge-backend` | Control plane: Axum REST API + dashboard, auth, tunnel reservation, region registry, policy, PostgreSQL + Redis. |
| `natforge-node` | Data plane (one per region): the TLS + yamux relay, shared `:80`/`:443` Host/SNI routers, dedicated TCP/UDP port pool. Self-registers with the control plane. |
| `natforge-agent` | The single-binary agent (Service Host / persistent device); installed command `natforge`. |
| `natforge-frontend` | Static, framework-free dashboard (Service Host + Admin). |
| `natforge-proto` | The shared wire protocol (tokens, handshake, per-stream preamble). |

Each of `natforge-frontend`, `natforge-backend`, `natforge-node`, and `natforge-agent` carries its own `DOCUMENTATION.md`.

## CI/CD and operations

- **CI** (GitHub Actions): `cargo fmt` + `clippy -D warnings`, unit tests, a release build, and the full **29-assertion** `scripts/e2e.sh` on every push/PR. See [docs/ci.md](docs/ci.md).
- **Security:** `cargo-audit` (RustSec advisories, triaged in `.cargo/audit.toml`), `gitleaks` secret scanning, and weekly **Dependabot** updates.
- **Continuous deployment:** on push to `main`, the control plane and data plane are built into Docker images, **Trivy**-scanned, pushed to `ghcr.io`, and deployed to the VM (`docker compose pull && up -d`) behind a reachability gate and a post-deploy health check. See [docs/cd.md](docs/cd.md).
- **Agent releases:** published as `x86_64` **glibc + static-musl** binaries on a rolling `latest` release; the static-musl build runs on any Linux distro (verified on Alpine, which ships no glibc).
- **Monitoring:** a self-hosted Prometheus + Grafana + node_exporter stack, plus an off-VM GitHub Actions **uptime** watcher. See [docs/monitoring.md](docs/monitoring.md).

Install the prebuilt agent:
```sh
curl -L https://github.com/EvanLei-git/Thesis-reverse-proxy/releases/latest/download/natforge-x86_64-linux-musl -o natforge && chmod +x natforge
```

## Deployment and hosting

Two paths: **containers** (the CD pipeline, `docker-compose.deploy.yml`) or **bare-metal systemd** via `sudo ./install.sh --component <website|core|node>`. Operator guides: **[docs/hosting.md](docs/hosting.md)** (production reference), **[docs/deployment-log.md](docs/deployment-log.md)** (a real setup log + troubleshooting), and DNS/TLS in **[docs/https.md](docs/https.md)**.

In short: point a Cloudflare **grey-cloud wildcard** `A *.natforge.com` at the node's IP (one record serves every tunnel subdomain), keep it **DNS-only** so SNI passthrough and raw TCP/UDP ports work, and open `80/443/4000` plus the port pool on the firewall.

## Status

NatForge began as a thesis project and is a working, tested platform. The main planned directions are **direct peer-to-peer UDP hole punching** (to offload traffic from the relay) and a **QUIC-datagram** transport for lower-jitter UDP relaying; both are currently deferred.
