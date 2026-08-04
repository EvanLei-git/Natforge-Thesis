# UDP tunneling design

Status: draft (pending approval)
Date: 2026-07-31

## Motivation

The data plane is TCP-only today. `RouteMode` is `Http | Https | Tcp`, every
listener is a `TcpListener`, and the relay is yamux (a reliable-stream
multiplexer). Minecraft Java (TCP 25565) works, but most real-time games run on
UDP and cannot be hosted:

- Valheim dedicated server: UDP 2456-2458
- Palworld dedicated server: UDP 8211
- Minecraft Bedrock: UDP 19132
- Most FPS and action games: UDP

This is the single highest-impact gap for "what a user can actually host". The
thesis already lists it honestly (7.2 "TCP-only data path", 7.3 "UDP tunneling").

## Goals

- Add a `udp` route type so an agent can expose a local UDP service.
- Reuse the existing single outbound TLS/yamux connection (NAT/CGNAT friendly).
- No change to the auth model, the reservation flow, or the routing preamble.
- Ship a working v1 that hosts real UDP games, with an honest latency caveat.

## Non-goals (v1)

- QUIC/unreliable-datagram transport (that is v2, see below).
- UDP hole punching / P2P (explicitly out of scope, relay only).
- Per-datagram abuse/amplification hardening.

## Design

### Wire protocol

`natforge-proto`: add `RouteMode::Udp`. The per-stream preamble is unchanged
(`magic`, version, `route_id`, `peer`, `replay`). The agent already knows each
`route_id`'s mode from its reservation, so it dispatches UDP framing locally with
no preamble change.

A UDP "flow" is carried on its own yamux stream. Because a yamux stream is a byte
stream, each datagram is framed as `u16 length` + payload (UDP payloads are
<= 65507 bytes, within `u16`). Message boundaries are therefore preserved exactly.

### Core (`core_proxy_backend`)

- Allocate a **UDP pool port** for the route. UDP and TCP port namespaces are
  independent, so the numeric pool (20000-20100) can be reused with a protocol
  tag on the `port_pool` row; a `udp` route binds `UdpSocket` on its allotted port.
- A new `run_udp` listener per active udp route binds the `UdpSocket` and keeps a
  **flow table**: `client_src_addr -> (yamux stream sender, last_seen)`.
  - First datagram from a new `client_src`: open a yamux stream to the agent,
    write `encode_preamble(route_id, Some(client_src), &[])`, register the flow.
  - Subsequent datagrams from that client: frame and write onto its stream.
  - Frames read back from the stream: `send_to(client_src)` on the `UdpSocket`.
  - Idle timeout (default 60s, tunable) evicts the flow and closes the stream.
- Geo-block checks apply per new flow
  (first datagram), mirroring how they apply per TCP connection today.
- Byte counters and the connection log record per flow, same as TCP.

### Agent (`natforge`)

- Parse `--route <port>:udp` into `RouteMode::Udp`.
- On accepting a new yamux stream whose `route_id` is a udp route: open a
  `UdpSocket` to `127.0.0.1:<localport>` for that flow, then relay:
  - length-prefixed frames from the core -> `send` to the local socket,
  - datagrams from the local socket -> length-prefixed frames back on the stream.
  - Idle timeout closes the local socket and the stream.

### Reservation / dashboard

- The reservation accepts `udp` as a route type and allocates a UDP pool port.
- The dashboard route-type selector gains `udp`. Endpoints render as
  `sub.natforge.com:<udp-port>`; an optional `_service._udp` SRV can be added
  later for clean addresses (Valheim/Palworld connect by host:port directly).

## Transport tradeoff (why v1, and what v2 is)

v1 carries UDP datagrams over a yamux stream, which is reliable and ordered.
Under packet loss that imposes retransmit + head-of-line delay, i.e. added jitter,
which is exactly what UDP games try to avoid. It is nonetheless correct and hosts
the games; for a hobby/thesis relay it is the right first step.

v2 replaces the agent<->core datagram path with **QUIC unreliable datagrams**
(the transport already flagged in thesis 2.4.3), giving true datagram semantics
with no head-of-line blocking. v2 is a transport swap under the same route model,
so v1 is not throwaway.

## Testing

1. Unit: `u16` datagram framing round-trip (empty, max-size, multi-datagram).
2. Local end-to-end: a UDP echo origin (`python`/`socat`), an agent `--route
   <p>:udp`, and a UDP client (`nc -u`) confirming datagrams round-trip through
   the tunnel; idle timeout evicts the flow.
3. Live: run a real Valheim or Palworld dedicated server behind the agent on
   `computer2` (CGNAT), connect a game client to `sub.natforge.com:<port>`, and
   verify a session holds. Record it as a live-deployment evaluation note, the
   same way the auto-HTTPS path was verified.

## Thesis impact

Moves "TCP-only data path" (7.2) from limitation toward resolved, with the honest
v1 jitter caveat and QUIC named as the v2 refinement. Add a functional-test row
and a short evaluation note once the live game test passes.

## Rollout

Behind the existing route model, so no migration. A node without any udp routes
binds no `UdpSocket`, so the change is inert until a udp route is reserved.
