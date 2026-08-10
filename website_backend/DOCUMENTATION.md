# Website Backend (Control Plane), Documentation

`website_backend` is the NatForge **control plane**: an Axum HTTP service (`:3000`) backed by **PostgreSQL** (durable) and **Redis** (ephemeral). It owns identity, the multi-route tunnel reservation + region registry, per-tunnel observability, geo-blocking policy, admin policy, the internal API the nodes report to, and serving of the static dashboard. It is *not* in the data path.

## Responsibilities
1. Auth: Argon2id hashing, HS256 JWT sessions, RFC 8628 device flow (codes in Redis with TTL). Login/registration are **geo-gated** when a GeoLite2 DB is configured.
2. **Multi-route** tunnel reservation (`http`/`https`/`tcp`/`udp`/`both`): pick a region/node, allocate a subdomain + dedicated TCP/UDP ports from that node's pool, persist `tunnels`+`routes`, mint one signed multi-route token. Idempotent per (owner, route-shape) so reconnects reuse the same subdomain/region/ports. Also handles user **custom domains** and per-route **DNS SRV**.
3. The `port_pool` allocator of record (`FOR UPDATE SKIP LOCKED`) + a reconciliation sweep for abandoned tunnels.
4. The **region registry** (nodes self-register; admin renames/enables/removes), per-tunnel **bandwidth + connection logs**, platform-wide and per-tunnel **geo-blocking**, admin region blocks, stats, all-tunnels, users.
5. Internal API for the nodes (secret-guarded). Serves the frontend (`ServeDir`).
6. **Persistent devices** (RFC 8628 enrollment + a server-authoritative per-service config the agent pulls and reconciles on reconnect), self-service **profiles/passwords**, admin **moderation** (ban/delete), **cross-region migration**, and a **failover** sweep that relocates a stale node's tunnels. Exposes a Prometheus `/metrics` endpoint on `127.0.0.1:9101`.

## Persistence
- **PostgreSQL** via `sqlx` **runtime** queries (no compile-time `query!` macros → build needs no DB). Migrations `0001..0022` applied at boot. Tables: `users, tunnels, routes, nodes, devices, port_pool, bandwidth_logs, connection_logs, region_blocks, tunnel_region_blocks, reserved_subdomains`.
- **Redis** via `ConnectionManager`: device codes (`nf:devcode:*`, 1-hour TTL) + the data plane's liveness mirror.
- `AppState::connect` fails fast if either store is down and opens the optional GeoLite2 DB. Each data-plane node seeds its own port range when it self-registers.

## Module layout
```
src/{config,jwt,geo,metrics,models,main}.rs # metrics.rs = Prometheus /metrics on 127.0.0.1:9101
src/db/{connection,queries}.rs              # connection.rs also holds the RFC 8628 device codes
src/models.rs                               # FromRow rows + API view structs
src/handlers/{auth,tunnels,devices,user,admin,internal}.rs
src/routes/mod.rs
migrations/0001..0022_*.sql
```

## Public REST API (selected)
| Method | Path | Auth | Purpose |
|---|---|---|---|
| POST | `/api/auth/register` \| `/login` | none | Argon2 + JWT (geo-gated) |
| POST | `/api/auth/device/start` \| `/device/token` | none | RFC 8628 issue / poll |
| POST | `/api/auth/device` | session | RFC 8628 approve |
| GET | `/api/tunnels` | session | List tunnels (nested routes, region) |
| POST | `/api/tunnels/request` | session | Reserve routes; mint token. Body: `{subdomain?, node_id?, routes:[{mode,local_port,label?}]}` |
| DELETE | `/api/tunnels/{id}` | session | Stop a tunnel |
| GET | `/api/tunnels/{id}/bandwidth` \| `/logs` | session | Per-tunnel bandwidth series / connection log |
| GET/PUT | `/api/tunnels/{id}/region_blocks` | session | Get/replace this tunnel's blocked countries |
| GET | `/api/regions` | session | Active regions (request dropdown) |
| GET/POST/DELETE | `/api/admin/region_blocks[/{cc}]` | admin | Policy |
| GET | `/api/admin/stats` \| `/admin/tunnels` \| `/admin/users` | admin | Network overview, all tunnels, per-user overview |
| GET | `/api/admin/nodes` ; PATCH/DELETE `/api/admin/nodes/{id}` | admin | List / rename-enable / remove regions |
| POST | `/api/tunnels/{id}/stop` \| `/start` ; PUT `/routes` ; POST `/migrate` | session | Stop/start, live add/remove ports, cross-region migrate |
| PUT/DELETE | `/api/tunnels/{id}/custom_domain` ; POST `/routes/{rid}/srv` | session | Custom domain; per-route DNS SRV |
| GET/POST | `/api/devices` ; `/devices/enroll/{start,approve,token}` ; GET `/devices/me/config` | mixed | Persistent devices: RFC 8628 enroll + server-authoritative config |
| GET/PUT | `/api/user/profile` ; PUT `/api/user/password` | session | Self-service profile + password |
| PATCH/DELETE | `/api/admin/users/{id}` | admin | Ban/unban / delete a user |

## Internal API (nodes only, `x-internal-secret`)
`POST /api/internal/node_register {…}` · `tunnel_up {tunnel_id,node_id,agent_ip?}` · `tunnel_down {tunnel_id}` · `bandwidth {…}` · `conn_log {…}` · `GET /api/internal/policy` (blocked regions + per-tunnel blocks).

## Config
`PORT, NATFORGE_DOMAIN, CORE_URL, FRONTEND_DIR, DATABASE_URL, REDIS_URL, GEOIP_DB, JWT_SECRET, INTERNAL_SECRET`. Dev defaults target the `docker compose` datastores. A startup WARN fires while dev secrets are in use.
