# Hosting NatForge on a VM

Everything **you** need to do to run NatForge in production without issues, secrets/API keys, what to change in each file, IPs/URLs, the database, DNS, ports, TLS, and an honest list of what works vs. what needs config vs. what is not implemented.

> **No source-code changes are required to host.** Everything is configured through environment variables. The only files you edit are the generated env files (`/etc/natforge/*.env`), `docker-compose.yml` (DB password), and your Cloudflare DNS.

---

## 0. What runs where

| Process | Where | Notes |
|---|---|---|
| `website_backend` | the VM | control plane + dashboard, talks to PostgreSQL + Redis (port 3000, internal) |
| `core_proxy_backend` | the VM (**static public IP**) | data plane: agent control `:4000`, shared HTTP `:80`, shared HTTPS/SNI `:443`, TCP pool `20000–20100`, internal API `:3001` |
| PostgreSQL + Redis | the VM (or managed) | durable + ephemeral state |
| `natforge` | **your users' machines** (not the VM) | the agent; can be behind CGNAT |

The VM is the public endpoint, so it **must have a static public IP** and cannot itself be behind CGNAT.

---

## 1. Prerequisites on the VM

- Ubuntu 22.04+ (any modern Linux).
- Rust ≥ 1.85 to build (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y`), **or** copy pre-built release binaries.
- Docker + Docker Compose **or** native PostgreSQL 16 + Redis 7.
- Your domain (`natforge.com`) added to a Cloudflare account.

---

## 2. Secrets & API keys you must set

| Variable | How to obtain / generate | Required? | Used by |
|---|---|---|---|
| `JWT_SECRET` | `openssl rand -hex 32`, **must be identical** on both planes | **Yes** | website + core |
| `INTERNAL_SECRET` | `openssl rand -hex 32`, **must be identical** on both planes | **Yes** | website + core |
| PostgreSQL password (in `DATABASE_URL`) | choose a strong one; set it in `docker-compose.yml` **and** `DATABASE_URL` | **Yes** | website |
| `CF_API_TOKEN` | Cloudflare → **My Profile → API Tokens → Create Token → "Edit zone DNS"** template, scoped to the `natforge.com` zone | Optional (only for clean game/SRV addresses) | core |
| `CF_ZONE_ID` | Cloudflare → select `natforge.com` → **Overview → API → Zone ID** | Optional (with `CF_API_TOKEN`) | core |
| TLS certificate | Let's Encrypt (e.g. via Caddy) or a Cloudflare Origin CA cert | **Yes for production HTTPS dashboard** | reverse proxy (see §7) |
| MaxMind license key + `GeoLite2-Country.mmdb` | maxmind.com (free signup) | Only if you want geo-blocking enforced (set `GEOIP_DB`, see §8) | website + each core |

> The dev defaults (`natforge-dev-secret-change-me`, `natforge-internal-dev-secret`) are **insecure**, both planes log a warning while they're in use. Tunnel tokens are forgeable until you change them.

---

## 3. Environment per component (what to change, and where)

`sudo ./install.sh --component website` and `--component core` generate `/etc/natforge/natforge-website.env` and `/etc/natforge/natforge-core.env` with `CHANGE_ME` placeholders. Edit those:

> These are the **systemd** env files (their names match the `natforge-website`/`natforge-core` units). The container deploy of `docs/cd.md` reads `/etc/natforge/website.env` and `core.env` instead, same keys, different filenames.

**`/etc/natforge/natforge-website.env`**
```ini
PORT=3000
NATFORGE_DOMAIN=natforge.com
CORE_URL=http://127.0.0.1:3001 # fallback only; nodes carry their own internal_url
FRONTEND_DIR=/usr/local/share/natforge/frontend
DATABASE_URL=postgres://natforge:<STRONG_DB_PASSWORD>@127.0.0.1:5432/natforge
REDIS_URL=redis://127.0.0.1:6379
GEOIP_DB=/etc/natforge/GeoLite2-Country.mmdb # optional; geo-blocking is a no-op without it
JWT_SECRET=<paste the 64-hex secret>
INTERNAL_SECRET=<paste the other 64-hex secret>
```

**`/etc/natforge/natforge-core.env`** (one per region, change the NODE_* / PUBLIC_HOST / CONTROL_ENDPOINT / INTERNAL_URL per VM)
```ini
CORE_INTERNAL_PORT=3001
CORE_CONTROL_PORT=4000
HTTP_PORT=80
HTTPS_PORT=443
PUBLIC_HOST=natforge.com # wildcard apex this node serves
NODE_ID=edge-1 # unique per node
NODE_NAME=Primary
NODE_REGION=Default
CONTROL_ENDPOINT=natforge.com:4000 # host:port agents connect to
INTERNAL_URL=http://127.0.0.1:3001 # how the website reaches THIS node
PUBLIC_PORT_MIN=20000
PUBLIC_PORT_MAX=20100
GEOIP_DB=/etc/natforge/GeoLite2-Country.mmdb # optional
WEBSITE_URL=http://127.0.0.1:3000
REDIS_URL=redis://127.0.0.1:6379
JWT_SECRET=<same secret as the website>
INTERNAL_SECRET=<same secret as the website>
CF_API_TOKEN=<cloudflare token, or leave 'mock_token' to disable SRV>
CF_ZONE_ID=<cloudflare zone id, or 'mock_zone'>
```

**Adding a second region (one command).** On a new VM that runs *only* the core:
```bash
sudo ./install.sh --component core --dedicated \
  --node-id us-1 --public-host us.natforge.com --head-host <head-ip> \
  --jwt-secret <same-as-head> --internal-secret <same-as-head>
```
This widens the pool to `10000–60999`, applies the kernel/firewall tuning (`scripts/dedicated-node.sh`, §6), points `WEBSITE_URL`/`REDIS_URL` at the head, and enables + starts the service (omit any flag to be prompted; secrets are hidden). The node keeps no database of its own. Give its `PUBLIC_HOST` a grey `*.us.natforge.com` wildcard pointing at the VM, ensure the head's `:3000` (control-plane API) and `:6379` (Redis) are reachable from the node (private network recommended) and the node's `:3001` from the head, then enable it in the admin panel and it appears in every user's region dropdown.

**Your users' agents** (`natforge` on their machines) point at your control plane; the node to connect to comes from the reservation, so no `--tunnel-server` is needed:
```bash
natforge service-host --route 25565:tcp \
 --control-plane https://natforge.com \
 --region <node_id> # optional: pick a region
```
(The `install.sh --component node` template already uses `https://natforge.com`.)

---

## 4. Database & Redis

- **Docker (simplest):** edit `POSTGRES_PASSWORD` in `docker-compose.yml` to match your `DATABASE_URL`, then `docker compose up -d`. Migrations run automatically when `website_backend` starts; each node seeds its own TCP port pool when it self-registers on boot.
- **Managed/native:** create a `natforge` database and user, set `DATABASE_URL`, point `REDIS_URL` at your Redis. Nothing else, the schema is applied by `sqlx::migrate!` at boot.

---

## 5. DNS on Cloudflare

After delegating `natforge.com` to Cloudflare (set the two nameservers Cloudflare gives you at your registrar):

| Type | Name | Content | Proxy status |
|---|---|---|---|
| A | `natforge.com` | `<VM public IP>` | DNS-only **or** Proxied (apex/dashboard) |
| A | `*.natforge.com` | `<VM public IP>` | **DNS-only (grey cloud)** ← all tunnels |
| A | `app.natforge.com` | `<VM public IP>` | Proxied (recommended for the dashboard, see §7) |

- The **wildcard is the whole DNS story for tunnels**: every `duck-xxxx.natforge.com` already resolves; the core routes by Host/SNI. There is no per-subdomain record and no limit.
- **Keep the wildcard grey (DNS-only)**: Cloudflare's orange-cloud proxy terminates TLS (breaks SNI passthrough) and won't carry the raw TCP/UDP pool (`20000–20100` by default) without paid **Spectrum**.
- Per-tunnel `_minecraft._tcp.<sub>` **SRV** records are created/removed automatically by the core when `CF_API_TOKEN`/`CF_ZONE_ID` are set (so players type just `<sub>.natforge.com`). Without them, players use `<sub>.natforge.com:<port>`.

Full DNS/TLS rationale: `docs/https.md`.

---

## 6. Ports / firewall (VM)

| Port(s) | Expose publicly? | Purpose |
|---|---|---|
| `80`, `443` | **Yes** | core shared HTTP / HTTPS-SNI routers |
| `4000` | **Yes** | agent control plane (yamux) |
| `20000–20100` (default) | **Yes** | dedicated TCP/UDP route pool (per node; widened on a dedicated node, below) |
| `3000` | No (localhost / behind reverse proxy) | dashboard + API |
| `3001` | **No** on the head (localhost); on a remote node, reachable only from the control plane | core internal API (secret-guarded) |
| `5432`, `6379` | No (localhost only) | PostgreSQL / Redis |

> **Dedicated (relay-only) node:** a VM that runs *only* the core has far fewer occupied ports, so it can host a much larger pool. `sudo ./install.sh --component core --dedicated` (or `sudo bash scripts/dedicated-node.sh`) widens the pool to **`10000–60999`** and narrows the kernel's outbound ephemeral range to `61000–65535`; then open `tcp/udp 10000–60999` on the host firewall **and** the cloud NSG, and keep `:3001` reachable only from the control-plane host. The full recipe is `scripts/dedicated-node.sh`.

---

## 7. The dashboard & TLS (read this, there's a real nuance)

The core's `:443` router does **SNI passthrough** (it never decrypts), so it **cannot also serve the TLS-terminated dashboard** on the same port. Pick one:

- **Recommended (simplest):** give the dashboard its own hostname `app.natforge.com`, **Cloudflare-proxied (orange)**, with Cloudflare terminating TLS and forwarding to your origin (run `website_backend` on `:3000`, or behind Caddy). Keep `*.natforge.com` grey for tunnels. No port conflict because the dashboard host is distinct from tunnel hosts.
- **Self-managed TLS:** run a one-line **Caddy** (or nginx) reverse proxy that terminates TLS for `app.natforge.com` → `127.0.0.1:3000`. Caddy auto-obtains Let's Encrypt certs.
- **Single-IP advanced:** front everything with a proxy on `:443` that terminates TLS for the dashboard host and TCP-passes tunnel hosts to the core by SNI. Only needed if you refuse a separate dashboard subdomain.

For **unencrypted-origin HTTP tunnels** (a user exposing plain HTTP), NatForge can add TLS *at the public edge* itself: `http` subdomain routes are terminated at the core with a `*.natforge.com` wildcard cert (`WILDCARD_CERT_PATH`, certbot DNS-01), and user **custom domains** get a per-domain Let's Encrypt cert automatically over ACME HTTP-01 (`ACME_ENABLED=1`). `https` routes stay pure SNI passthrough (the relay never decrypts; the origin's own cert is served). All of this is separate from the **agent↔core** leg, which is always TLS-encrypted (self-signed, fingerprint-pinned) regardless of the inner protocol.

---

## 8. Feature status, what works, what needs config, what is NOT implemented

**Works out of the box:** the reverse tunnel (HTTP-by-subdomain, HTTPS-by-SNI, **raw TCP, and raw UDP**), multiple routes per tunnel, **user-owned custom domains** (a tunnel can be fronted by `play.mygame.com` alongside its `<sub>.natforge.com`), **cross-region migration** (move a live tunnel to another region from the dashboard), the **multi-region** data plane (self-registering nodes, per-tunnel region choice), **per-tunnel observability** (bandwidth series + connection log), the **TLS-encrypted, fingerprint-pinned** agent↔core channel, Argon2+JWT auth, RFC 8628 device flow, the dashboards, and full PostgreSQL+Redis persistence with idempotent reservation.

**Works once you enable it / add the key or file:**
- **Automatic HTTPS for custom domains** (`ACME_ENABLED=1`, on in the production compose): the core obtains a per-domain Let's Encrypt certificate over ACME HTTP-01 on `:80`, so a user's `play.mygame.com` gets edge TLS with no cert management. A custom domain can alternatively bring its own cert via SNI passthrough.
- **Automatic HTTPS for `http` subdomain routes** via a `*.natforge.com` **wildcard** cert: set `WILDCARD_CERT_PATH`/`WILDCARD_KEY_PATH` (certbot DNS-01). Absent the file the core serves those routes over plain HTTP (graceful-off).
- Cloudflare **SRV** game-address provisioning (`CF_API_TOKEN` + `CF_ZONE_ID`): lets Minecraft-Java players type just `<sub>.natforge.com`. Without it, players use `<sub>.natforge.com:<port>`.
- **Geo-blocking** (platform-wide + per-tunnel): set `GEOIP_DB` to a MaxMind `GeoLite2-Country.mmdb` on the website **and** each node. Enforcement is fully implemented (login/registration gating on the website; public-connection drops on the nodes); it only needs the database. Without it, country resolution returns "unknown" and blocking is a no-op (it never blocks blindly).

**NOT implemented, do not rely on these:**
- **Direct UDP hole punching (P2P)**: future work; today every tunnel relays through a regional node. This is distinct from **UDP tunneling**, which *is* implemented, UDP datagrams are relayed over the node exactly like TCP.

---

## 9. Production deploy (systemd)

> The **primary** production path is the container CD pipeline (`docs/cd.md`). The systemd install below is the bare-metal alternative.

```bash
# 1. Build
cargo build --release

# 2. Install binaries + frontend
sudo install -m755 target/release/{website_backend,core_proxy_backend,natforge} /usr/local/bin/
sudo mkdir -p /usr/local/share/natforge && sudo cp -r frontend /usr/local/share/natforge/

# 3. Datastores
# edit POSTGRES_PASSWORD in docker-compose.yml first
docker compose up -d

# 4. Generate + edit service env (see §3), then enable
sudo ./install.sh --component website
sudo ./install.sh --component core
sudo nano /etc/natforge/natforge-website.env # paste real secrets/URLs
sudo nano /etc/natforge/natforge-core.env
sudo systemctl daemon-reload
sudo systemctl enable --now natforge-website natforge-core
```

> The core binds `:80`/`:443`; the generated unit runs as root so it can. If you prefer a non-root user, grant `CAP_NET_BIND_SERVICE` or front it with a proxy.

---

## 10. Verify the deployment

```bash
# dashboard reachable (via your reverse proxy / Cloudflare)
curl -I https://app.natforge.com/

# register the admin account in the browser, reserve a tunnel, then on a SEPARATE machine:
natforge service-host --route 25565:tcp --control-plane https://natforge.com
# -> connects (over TLS) to the node named in the reservation; note the public endpoint (natforge.com:200xx)

# a friend connects to the dedicated port, or to <sub>.natforge.com for HTTP/SNI routes
```

If something is unreachable, re-check §6 (firewall) and that `*.natforge.com` is **grey-cloud**.
