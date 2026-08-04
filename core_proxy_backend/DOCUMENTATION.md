# Core Proxy Backend (Data Plane), Documentation

`core_proxy_backend` is a NatForge **data-plane node** (one per region): a Tokio service that performs the byte relaying. Each agent's control connection is wrapped in **TLS** and becomes a **yamux** session carrying **multiple routes**; public traffic is routed to the right route by subdomain (HTTP `Host` / TLS SNI on shared ports) or by dedicated TCP port. On boot the node **self-registers** with the control plane (its host, port pool, and TLS cert fingerprint).

## Listeners
- **Agent control** `:4000`, agents connect over **TLS** (self-signed, fingerprint-pinned); length-prefixed JSON handshake, then yamux (core = client, agent = server).
- **Shared HTTP** `:80` (dev `:8080`), routes by `Host` header → subdomain.
- **Shared HTTPS** `:443` (dev `:8443`), routes by **TLS SNI**, layer-4 passthrough (never decrypts).
- **Dedicated TCP**, one pooled public port per `tcp` route.
- **Internal API** `:3001`, consumed by the control plane (secret-guarded).

## Data path (per public connection)
1. Resolve the route: subdomain in `http_routes`/`https_routes`, or port in `port_routes`.
2. Resolve country (GeoLite2) and apply platform-wide + per-tunnel geo-blocks (drop + log `blocked` if denied) → open one yamux stream via the route's `open_tx`.
3. Write the binary **preamble** (`natforge_proto::encode_preamble`: magic `NFS1`, route_id, peer, replay), for HTTP/SNI the peeked bytes ride in `replay` so the origin sees a byte-exact request.
4. `copy_bidirectional` (zero-disk). Byte counts tracked per tunnel (reported every 5s); each closed/blocked connection reports a metadata-only `conn_log` row.

## Per-agent handshake (`tunnel/mod.rs`)
TLS-accept the socket; verify the multi-route token (shared `natforge_proto::TunnelClaims`); reconcile the agent's route bindings against the token; ownership-guard the subdomain (reject if live under a different tunnel id); bind each signed TCP port; register http/https by subdomain and tcp by port; run one **pipelined** yamux driver (`mux.rs`); report `tunnel_up` + Redis mirror. Teardown (agent disconnect or force-stop) removes registry entries it still owns and aborts listeners.

## Module layout
```
src/{config,jwt,state,reporter,tls,geo,main}.rs
src/tunnel/{mod,mux,shared}.rs # mod=TLS handler, mux=pipelined driver, shared=:80/:443 routers
src/{dns,api}.rs # Cloudflare SRV · internal API
```

## Internal API (`x-internal-secret` required)
`GET /health` · `GET /internal/tunnels` · `POST /internal/tunnels/{subdomain}/stop`.

## Real vs. deferred
- **Real:** TLS-encrypted, fingerprint-pinned control channel (`tls.rs`); node self-registration; yamux multiplexing, multi-route, HTTP/Host + TLS/SNI subdomain routing (hand-rolled, bounds-checked, unit-tested parsers); the per-stream preamble; zero-disk relay; **GeoLite2 geo-blocking** (platform-wide + per-tunnel); per-connection logging; Redis liveness mirror; byte accounting; and **Cloudflare SRV provisioning** (`dns.rs`, a real `reqwest` v4 client; live with `CF_API_TOKEN`, logs in dev).
- **Deferred:** UDP hole punching. Geo-blocking requires a `GeoLite2-Country.mmdb` at `GEOIP_DB`; absent it, country resolution is "unknown" and blocking is a no-op.

## Config
`CORE_INTERNAL_PORT, CORE_CONTROL_PORT, HTTP_PORT, HTTPS_PORT, PUBLIC_HOST, NODE_ID, NODE_NAME, NODE_REGION, CONTROL_ENDPOINT, INTERNAL_URL, PUBLIC_PORT_MIN, PUBLIC_PORT_MAX, WEBSITE_URL, REDIS_URL, GEOIP_DB, JWT_SECRET, INTERNAL_SECRET, MAX_HEADER_BYTES, CF_API_TOKEN, CF_ZONE_ID`.
