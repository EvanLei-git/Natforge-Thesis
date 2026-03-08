# Core Proxy Backend Documentation

## Dedicated Tunneling Engine (TCP/UDP)
Unlike the `website_backend` which handles authentication and billing logic, this repository governs the actual **Data Plane**. It is responsible for bridging users behind NATs with the broader, unrestrictive web.

### Structural Separation
- `/src/api/` -> Extremely lightweight internal routes. Used purely for the Web Backend to securely signal down that a new Wireguard/Yamux connection is approved.
- `/src/tunnel/` -> Replaces standard proxy implementations with `boringtun` logic. Allocates explicitly **1 TCP Port** and **1 UDP Port** per connected `end_user_id` mapped dynamically.
- `/src/ddos/` -> Volumetric heuristics mapping IP structs. Scans bytes flowing into the Core Proxy. If packets exceed 10K/second on a single IP footprint, an eBPF drop instruction is triggered protecting the overarching proxy array.
- `/src/dns/` -> Mock logic designed to connect directly to the Cloudflare API (`POST /client/v4/zones/...`). Whenever an Anycast proxy spins up, this engine automatically generates a randomized SRV (`_minecraft._tcp`) or generic CNAME record directing global traffic specifically to the allocated `tcp_port`.

### Bandwidth Tracking
Wireguard/Yamux byte length is mapped into the `PeerTunnel` struct internally. These values are batched and fired securely to the `website_backend` for SQL persistence to track economics/margins accurately for volunteer Edge Nodes!
