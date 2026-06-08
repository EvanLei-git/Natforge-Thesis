# Core Proxy Backend (Data Plane) — Documentation

`core_proxy_backend` is the NatForge **data plane**: a Tokio service that performs the actual byte relaying. It accepts a single TCP control connection per agent, upgrades it to a **yamux** multiplexed session, binds a dedicated public port per tunnel, and bridges every inbound public connection to a fresh multiplexed stream the agent forwards to the local service.

## Listeners
- **Agent control plane** — default `:4000`. Agents connect here; after a length-prefixed JSON handshake the socket becomes a yamux session (core = client, agent = server).
- **Public port pool** — default `20000`–`20100`. One port allocated per tunnel; end users connect here.
- **Internal API** — default `:3001`. Consumed by the website control plane.

## Data path (per agent)
1. Read length-prefixed `AgentHello { tunnel_token, local_port, role }` (exact `read_exact`, no over-read).
2. Verify the tunnel token (HS256, shared secret) — no database access (stateless).
3. Refuse globally blocked local ports.
4. Allocate a public port; bind its listener.
5. Reply `CoreReply::Ok { public_host, public_port, subdomain }`, then upgrade to yamux.
6. Per public connection: DDoS check → open outbound yamux stream → `copy_bidirectional` (in-memory, zero-disk) with byte accounting.
7. Report `tunnel_up`, provision DNS SRV (mock), report bandwidth every 5s.
8. On agent disconnect: abort listener, free port, report `tunnel_down`.

## Module layout
```
src/
├── main.rs            boots internal API, control plane, periodic policy refresh
├── config.rs          ports, public host, port pool, secrets
├── jwt.rs             tunnel-token verification (shared secret)
├── state.rs           CoreState: tunnels, free-port pool, ddos, blocked ports
├── tunnel/
│   ├── mod.rs         control listener + per-agent handler (the core data path)
│   ├── mux.rs         single-borrow yamux client driver (poll_fn state machine)
│   └── wireguard.rs   simulated WireGuard peer / bandwidth accounting
├── ddos/filter.rs     per-IP sliding-window connection-rate guard
├── dns/cloudflare.rs  SRV-record provisioning (mock unless CF_API_TOKEN set)
├── api/routes.rs      internal API: health, list/close tunnels
└── reporter.rs        outbound reporting to the control plane
```

## Internal API
| Method | Path | Purpose |
|---|---|---|
| GET | `/health` | Liveness |
| GET | `/internal/tunnels` | Snapshot of live tunnels + byte counters |
| POST | `/internal/tunnels/{subdomain}` | Force-close a tunnel (dashboard "Stop") |

## Simulated vs. real
- **Real:** yamux multiplexing, the full TCP relay, byte accounting, the connection-rate DDoS heuristic, port-policy enforcement, the internal reporting channel.
- **Simulated (clearly labeled):** WireGuard encapsulation (`wireguard.rs`), kernel eBPF/XDP drop (the decision is real, the kernel action logged), and the live Cloudflare API call (logged by default; real if `CF_API_TOKEN` is set).

## Configuration (env)
`CORE_INTERNAL_PORT` (3001), `CORE_CONTROL_PORT` (4000), `PUBLIC_HOST`, `PUBLIC_PORT_MIN`/`MAX`, `WEBSITE_URL`, `JWT_SECRET`, `INTERNAL_SECRET`, `CF_API_TOKEN`, `CF_ZONE_ID`.
