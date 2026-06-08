# Proxy Node (Agent) — Documentation

`proxy_node` is the unified NatForge **agent**: a single Rust binary that runs on the end-user's machine in one of two roles. It authenticates against the control plane, then connects to the data plane.

## Authentication (all modes)
Resolved in priority order by `auth::obtain_token`:
1. `--token <JWT>` — use a pre-issued session token (non-interactive).
2. `--email` + `--password` — log in directly (non-interactive).
3. Otherwise — the **RFC 8628 device flow**: the CLI prints a code and verification URL; the user approves it from the dashboard; the CLI polls until approved.

## Mode 1 — Service Host
Expose a local service through a reverse tunnel.

```bash
proxy_node service-host \
  --local-port 25565 \
  --control-plane http://127.0.0.1:3000 \
  --tunnel-server 127.0.0.1:4000
```

Flow: reserve a subdomain (`POST /api/tunnels/request`) → connect to the core control port → length-prefixed handshake with the tunnel token → on `Ok`, print the live public endpoint → run a **yamux server** loop, accepting one inbound stream per public connection and bridging each to `127.0.0.1:<local-port>` with `copy_bidirectional`. Reconnects automatically on disconnect, preserving the subdomain via reservation reuse.

## Mode 2 — IP Host (Edge Node)
Volunteer this machine as a residential relay.

```bash
proxy_node ip-host \
  --listen-port 30000 \
  --upstream <core-host>:<public-port> \
  --max-bandwidth 100 \
  --control-plane http://127.0.0.1:3000
```

Flow: register as active (`POST /api/ip_host/status`) → best-effort reflexive public-IP discovery (STUN-like) → run a TCP forwarder from `--listen-port` to `--upstream`, relaying in-memory and accounting bytes. Because egress occurs from this machine, end users reach the service via this residential IP (Scenario B).

## Implementation notes
- **Multiplexing:** `yamux` over a single outbound TCP connection (one NAT mapping, full concurrency).
- **Compat:** `tokio_util::compat` bridges Tokio sockets ↔ yamux's futures-io streams.
- **Zero-disk:** all relaying is `tokio::io::copy_bidirectional` over transient memory buffers.
- **Future work:** direct UDP hole punching (STUN candidate exchange via the control plane) and WireGuard encapsulation are designed but not yet implemented; the relay/edge tiers are the working data paths today.
