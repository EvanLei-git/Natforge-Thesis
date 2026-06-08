# Website Backend (Control Plane) — Documentation

`website_backend` is the NatForge **control plane**: an Axum HTTP service (default `:3000`) that owns user identity, tunnel reservation, edge-node configuration, administrative policy, and serving of the static dashboard. It is intentionally *out* of the high-throughput data path.

## Responsibilities
1. Authentication & authorization — Argon2id password hashing, HS256 JWT sessions, RFC 8628 device flow.
2. Tunnel reservation and lifecycle (subdomain allocation, scoped tunnel tokens).
3. IP-host (edge node) configuration and accounting.
4. Administrative panels: region blocking, global port bans, network overview.
5. The secret-guarded internal API the core proxy reports to.
6. Serving the static frontend (`ServeDir`).

## Module layout
```
src/
├── main.rs            router assembly, static serving, default policy seeding
├── config.rs          environment configuration
├── jwt.rs             session/tunnel token mint+verify; AuthUser extractor
├── db/connection.rs   AppState (in-memory store shaped like the SQL schema)
├── models/user.rs     User, TunnelInfo, DeviceCode, IpHostConfig
├── handlers/          auth, tunnels, iphost, admin, internal
└── routes/mod.rs      REST route table
```

## State
In-memory `RwLock`-guarded maps (`AppState`) standing in for PostgreSQL (durable) + Redis (live). Field shapes match the SQL schema in the thesis Appendix A, so migration is mechanical. The first registered account becomes `admin`.

## Public REST API (selected)
| Method | Path | Auth | Purpose |
|---|---|---|---|
| POST | `/api/auth/register` | none | Create account, return JWT |
| POST | `/api/auth/login` | none | Verify Argon2, return JWT |
| POST | `/api/auth/device/start` | none | RFC 8628: issue device+user code |
| POST | `/api/auth/device/token` | none | RFC 8628: poll for approval |
| POST | `/api/auth/device` | session | RFC 8628: approve a user code |
| GET | `/api/tunnels` | session | List caller's tunnels |
| POST | `/api/tunnels/request` | session | Reserve subdomain + tunnel token |
| DELETE | `/api/tunnels/{subdomain}` | session | Stop a tunnel |
| GET/POST | `/api/ip_host/status` | session | Get/set edge-node active status |
| PUT | `/api/user/preferences` | session | Bandwidth/geo preferences |
| GET/POST | `/api/admin/region_blocks` | admin | List/add blocked countries |
| DELETE | `/api/admin/region_blocks/{cc}` | admin | Unblock a country |
| GET/POST | `/api/admin/port_blocks` | admin | List/add blocked ports |
| DELETE | `/api/admin/port_blocks/{port}` | admin | Unblock a port |
| GET | `/api/admin/stats` | admin | Network overview |
| GET | `/api/admin/tunnels` | admin | All active tunnels |

## Internal API (core proxy only — `x-internal-secret` header required)
| Method | Path | Purpose |
|---|---|---|
| POST | `/api/internal/tunnel_up` | Agent connected; mark tunnel online |
| POST | `/api/internal/tunnel_down` | Agent gone; remove tunnel |
| POST | `/api/internal/bandwidth` | Update byte counters |
| GET | `/api/internal/policy` | Return blocked ports/regions |

## Configuration (env)
`PORT` (3000), `NATFORGE_DOMAIN`, `CORE_URL`, `FRONTEND_DIR`, `JWT_SECRET`, `INTERNAL_SECRET`. Dev defaults run on loopback with no external services.
