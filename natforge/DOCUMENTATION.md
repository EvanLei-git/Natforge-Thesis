# Proxy Node (Agent) — Documentation

`natforge` is the NatForge **Service-Host agent** — one binary. It authenticates against the control plane, reserves a tunnel (in a chosen region), then connects to that region's data-plane node over TLS.

## Authentication
Priority order (`auth::obtain_token`): `--token <JWT>` → `--email`+`--password` → RFC 8628 device flow (prints a code to approve in the dashboard, polls until approved).

## Service Host (multi-route)
Expose one or more local services through a single tunnel:

```bash
natforge service-host \
  --route 8000:http --route 9443:https --route 25565:tcp \
  --region <node_id> \           # optional: pick a region (default region otherwise)
  --email you@example.com --password ...
# legacy: --local-port 25565  (sugar for one tcp route)
# dev only: --tunnel-server 127.0.0.1:4000  (override the connect address)
```

Flow: `POST /api/tunnels/request {routes:[{mode,local_port}], node_id?}` → reservation `{tunnel_id, subdomain, tunnel_token, control_endpoint, region, control_cert_fingerprint, routes:[{route_id,mode,local_port,public_endpoint}]}`. TCP-connect to `control_endpoint` (the node the reservation names) and **wrap it in TLS**, pinning `control_cert_fingerprint` via a custom `rustls` verifier (`tls.rs`). Send the handshake with the per-route `{route_id, local_port}` bindings; on `Ok`, build a `route_id → local_port` map and run a **yamux server**. For each inbound stream, read the **preamble** (`natforge_proto::read_preamble`) to get the `route_id`, dial `127.0.0.1:<local_port>`, write any replay bytes, then `copy_bidirectional`. Auto-reconnects on drop; idempotent reservation preserves the subdomain, region, and ports.

## Modules
`main.rs` (CLI) · `auth.rs` (token) · `service_host.rs` (reserve + serve; also the wire-frame helpers, with the handshake/preamble contract re-used from the shared `natforge-proto` crate) · `tls.rs` (pinned-cert TLS client).

## Notes
- The wire contract (handshake, claims, preamble codec) lives in the shared `natforge-proto` crate, so agent and core cannot drift.
- `tokio_util::compat` bridges Tokio sockets ↔ yamux's futures-io streams; relaying is `copy_bidirectional` (zero-disk).
- Future work: UDP hole punching (STUN via the control plane) to remove the relay leg for non-symmetric NATs.
