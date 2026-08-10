# natforge-node (Data Plane), Documentation

`natforge-node` is a NatForge **data-plane node** (one per region): a Tokio service that performs the byte relaying. Each agent's control connection is wrapped in **TLS** and becomes a **yamux** session carrying **multiple routes**; public traffic is routed to the right route by subdomain (HTTP `Host` / TLS SNI on shared ports) or by dedicated TCP port. On boot the node **self-registers** with the control plane (its host, port pool, and TLS cert fingerprint).

## Listeners
- **Agent control** `:4000`, agents connect over **TLS** (self-signed, fingerprint-pinned); length-prefixed JSON handshake, then yamux (core = client, agent = server).
- **Shared HTTP** `:80` (dev `:8080`), routes by `Host` header → subdomain.
- **Shared HTTPS** `:443` (dev `:8443`), routes by **TLS SNI**. `https` routes are layer-4 **passthrough** (never decrypted); a subdomain with only an `http` route, or a custom domain, is instead **TLS-terminated** here with a `*.<public_host>` wildcard cert or a per-domain ACME cert and forwarded as plain HTTP (auto-HTTPS).
- **Dedicated TCP/UDP**, one pooled public port per `tcp`, `udp`, or `both` route (UDP datagrams are relayed as framed messages over yamux).
- **Internal API** `:3001`, consumed by the control plane (secret-guarded).

## Data path (per public connection)
1. Resolve the route: subdomain in `http_routes`/`https_routes`, or port in `port_routes`.
2. Resolve country (GeoLite2) and apply platform-wide + per-tunnel geo-blocks (drop + log `blocked` if denied) → open one yamux stream via the route's `open_tx`.
3. Write the binary **preamble** (`natforge_proto::encode_preamble`: magic `NFS1`, route_id, peer, replay). For an `https`/SNI route the peeked ClientHello rides in `replay` byte-exact; for an `http` route the peeked request head is replayed with `X-Forwarded-For`/`-Proto`/`-Host` injected.
4. `copy_bidirectional` (zero-disk). Byte counts tracked per tunnel (reported every 5s); each closed/blocked connection reports a metadata-only `conn_log` row.

## Per-agent handshake (`tunnel/mod.rs`)
TLS-accept the socket; verify the multi-route token (shared `natforge_proto::TunnelClaims`); reconcile the agent's route bindings against the token; ownership-guard the subdomain (reject if live under a different tunnel id); bind each signed TCP port; register http/https by subdomain and tcp by port; run one **pipelined** yamux driver (`mux.rs`); report `tunnel_up` + Redis mirror. Teardown (agent disconnect or force-stop) removes registry entries it still owns and aborts listeners.

## Module layout
```
src/{config,jwt,state,reporter,tls,geo,main}.rs
src/tunnel/{mod,mux,shared}.rs # mod=TLS handler, mux=pipelined driver, shared=:80/:443 routers
src/{dns,acme,api}.rs # dns=Cloudflare SRV · acme=Let's Encrypt HTTP-01 · api=internal API
```

## Internal API (`x-internal-secret` required)
`GET /health` · `GET /internal/tunnels` · `POST /internal/tunnels/{subdomain}/stop`.

## Real vs. deferred
- **Real:** TLS-encrypted, fingerprint-pinned control channel (`tls.rs`); node self-registration; yamux multiplexing, multi-route, HTTP/Host + TLS/SNI subdomain routing (hand-rolled, bounds-checked, unit-tested parsers); the per-stream preamble; zero-disk relay; **GeoLite2 geo-blocking** (platform-wide + per-tunnel); per-connection logging; Redis liveness mirror; byte accounting; **Cloudflare SRV provisioning** (`dns.rs`, a real `reqwest` v4 client; live with `CF_API_TOKEN`, logs in dev); **UDP tunneling** (`udp`/`both` routes); and **custom domains** with SNI-passthrough or per-domain **ACME/Let's Encrypt** (`acme.rs`, HTTP-01) plus **auto-HTTPS** wildcard-cert termination for `http` routes.
- **Deferred:** direct UDP **hole punching (P2P)**. Geo-blocking requires a `GeoLite2-Country.mmdb` at `GEOIP_DB`; absent it, country resolution is "unknown" and blocking is a no-op.

## Config
`CORE_INTERNAL_PORT, CORE_CONTROL_PORT, HTTP_PORT, HTTPS_PORT, PUBLIC_HOST, NODE_ID, NODE_NAME, NODE_REGION, CONTROL_ENDPOINT, INTERNAL_URL, PUBLIC_PORT_MIN, PUBLIC_PORT_MAX, WEBSITE_URL, REDIS_URL, GEOIP_DB, JWT_SECRET, INTERNAL_SECRET, MAX_HEADER_BYTES, DASHBOARD_ADDR, CF_API_TOKEN, CF_ZONE_ID, WILDCARD_CERT_PATH, WILDCARD_KEY_PATH, ACME_ENABLED, ACME_EMAIL, ACME_DIR, ACME_STAGING`.
