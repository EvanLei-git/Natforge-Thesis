# NatForge

[![CI](https://github.com/EvanLei-git/Thesis-reverse-proxy/actions/workflows/ci.yml/badge.svg)](https://github.com/EvanLei-git/Thesis-reverse-proxy/actions/workflows/ci.yml)

**Title:** Design and Implementation of a Multi-Region Reverse-Proxy and Tunneling Platform for NAT/CGNAT Traversal

**Platform name:** NatForge (`natforge.com`)

> **Implementation status:** the complete platform is implemented and tested end-to-end (see `Thesis.md`, Chapter 5, `scripts/e2e.sh`, 27 assertions, all passing, run automatically in CI on every push). Working: **subdomain routing** so friends connect via `sub.natforge.com`, HTTP `Host` routing and HTTPS **TLS-SNI passthrough** on shared ports, plus dedicated TCP ports for raw protocols; **multiple routes per tunnel** over one yamux session (per-stream binary preamble); a **multi-region** data plane (self-registering nodes; users pick a region per tunnel); **per-tunnel observability** (bandwidth series + a connection log); **geo-blocking** (MaxMind GeoLite2; platform-wide + per-tunnel); a **real TLS** agent↔core channel with self-signed certs pinned by fingerprint; Argon2+JWT auth and the RFC 8628 device flow; **PostgreSQL + Redis persistence** with idempotent reservation, a per-region port-pool allocator, and a reconciliation sweep; role-aware dashboards; and admin abuse controls. Users get a custom-or-random subdomain and optional per-route labels for any TCP server; admins manage regions and get a network overview plus a **Users** page (per-user tunnels, agent IP, traffic). Verified scenarios include two users sharing one port by subdomain and full state surviving a both-planes restart. Run the datastores with `docker compose up -d`. Direct UDP hole punching is deferred as documented; kernel eBPF/XDP mitigation is noted as a production enhancement (the implemented guard is userspace).

**Author:** Evangelos Leivaditis

**Language/Tech Stack:** Rust, Tokio, Axum, yamux, rustls, HTML/CSS/JS, PostgreSQL, Redis

**Architecture:** Centralized control plane + distributed multi-region data plane (direct-P2P extension as future work)

## 1. Abstract

This research develops a high-performance, multi-region reverse proxy and tunneling platform designed to expose local network services (e.g., game servers behind NAT/CGNAT) to the public internet without requiring port forwarding. The platform separates concerns into:

- **A Centralized Control Plane:** A Rust/Axum service that handles authentication, signaling, tunnel reservation, the region (node) registry, per-tunnel observability, geo-blocking policy, and the management UI.
- **A Distributed, Multi-Region Data Plane:** One or more Rust relay nodes, each in a different region, that multiplex each tunnel over a single TLS-encrypted connection to the agent. Nodes self-register; users choose which region hosts their tunnel; direct peer-to-peer establishment is identified as future work.

Crucially, to ensure high throughput and minimize data retention, all relaying is done entirely in-memory via asynchronous buffers, only connection *metadata* (not payload) is logged. The system features a centralized web management interface and a unified Rust-based agent for hosting local services.

## 2. Problem Statement & Motivation

**The Problem:** The exhaustion of IPv4 addresses has led ISPs to heavily rely on CGNAT (Carrier-Grade NAT). This architectural shift inherently breaks end-to-end connectivity, making it virtually impossible for standard home users, gamers, and self-hosters to securely expose local applications to the internet without renting a Virtual Private Server (VPS) or purchasing a static IP [1]. Furthermore, CGNAT architectures disrupt standard peer-to-peer protocols by sharing single public IPs across thousands of subscribers [2].

**The Proposed Solution:** A "Tunneling as a Service" platform that democratizes internet exposure. By allocating randomized subdomains and specific ports to users, the platform allows friends to connect to privately hosted game servers seamlessly. Furthermore, by implementing P2P NAT traversal and utilizing community-provided public IPs, the platform drastically reduces the bandwidth overhead on the central server and mitigates datacenter-level IP blocking.

## 3. Core Use Cases & Actor Roles

The system serves two user roles, managed through a unified custom Web UI:

- **The Service Host (Standard User):** A user hosting any TCP server from their home PC, a game server (Minecraft, Terraria, GTA-MTA, …), a website, an API, SSH. They pick a subdomain (e.g. `duck-main.natforge.com`) or get a random one, choose a **region**, add per-port routes with optional labels, and reach HTTP/HTTPS by subdomain or raw TCP on a dedicated port. A per-tunnel detail panel shows the tunnel's **location, logging** (bandwidth + recent connections), and **blocking** (per-tunnel country blocks). (UDP-only games are not yet supported.)
- **The Administrator:** Oversees the network, monitors active tunnels and users, manages the **regions (nodes)** (rename/enable/disable/remove self-registered nodes), and handles abuse via globally blocked ports and platform-wide **geo-blocking** by country.

## 4. Technical Architecture

### A. The Data Plane (The Rust Tunnel Engine)

### A. The Data Plane (The Rust Tunnel Engine)

The core high-throughput networking is isolated in the `core_proxy_backend` application, written entirely in Rust for deterministic memory safety and predictable latency [3].

- **TLS + Multiplexing:** Each agent's single outbound connection is wrapped in **real TLS** (`tokio-rustls`, self-signed cert pinned by SHA-256 fingerprint) and upgraded to a **`yamux`** session carrying many simultaneous streams. HTTP/HTTPS routes share `:80`/`:443` (Host/SNI routing); each raw-TCP route gets a dedicated public port from that node's pool.
- **Multi-region nodes:** Each node self-registers with the control plane (its host, port range, and cert fingerprint), serves its own wildcard apex, and owns its own port pool. Users pick a region; a tunnel records its `node_id` so reconnection stays on the same node.
- **Abuse mitigation (userspace):** A per-IP connection-rate guard blackholes sources that exceed a threshold (time-bounded), shedding load before it reaches the multiplexer. This is honest userspace mitigation; a kernel eBPF/XDP drop path is noted as a production enhancement [11], not implemented.
- **Geo-blocking:** A MaxMind GeoLite2 lookup resolves each connection's country; platform-wide (admin) and per-tunnel (owner) block lists are enforced at the node and recorded in the connection log.
- **DNS SRV provisioning:** Upon a tcp route coming up, the `core_proxy` provisions a per-tunnel `_minecraft._tcp.<sub>` SRV record via the Cloudflare v4 API (live with `CF_API_TOKEN`, logged in dev) and removes it on teardown.
- **Per-tunnel observability:** Byte counts are tracked with atomics and reported every 5s; each closed (or geo-blocked) connection contributes a metadata-only row (source, country, bytes, duration) to the per-tunnel connection log.

### B. The User Platform (`website_backend` & `frontend`)

- **API Server & Authentication:** The `website_backend` isolates user data from the high-throughput network buffers. Built around Axum, it manages robust Web UI accounts utilizing the **Argon2** cryptographic hashing algorithms and JWT mappings. 
- **State Management & Persistence:** Redis for ephemeral state (device codes, liveness mirror); PostgreSQL for users, tunnels/routes, the region registry, bandwidth, connection logs, geo-block lists, and the per-region port allocator. The full API is detailed in `/website_backend/DOCUMENTATION.md`.
- **Web Interface (UI):** Vanilla HTML/JavaScript with a **custom, framework-free design system**. A dark, layered **Discord-style** palette, the **brand teal (`#40b8c0`) sampled from the `natforge_flake` logo** as the accent, and **Azure-style components** (Segoe UI, low-radius rectangular controls, flat fills, crisp focus rings). **No emoji**, all icons are inline stroke-SVGs matching the hexagonal logo. The workspace separates `views`, `api`, and `assets`; the Service Host (region-aware request builder + per-tunnel detail panel) and Admin (regions, blocks, users) panels interact with the Axum REST endpoints. Details in `/frontend/DOCUMENTATION.md`.

### C. System Flow & Configuration

The client-side is a single compiled Rust binary deployed via the install script:

- **Device Authorization Flow:** To securely link the headless CLI to the web account, the system uses the OAuth 2.0 Device Authorization Grant flow (RFC 8628) [7]. The CLI outputs: *"Please go to natforge.com/device and enter code XYZ."* The code is valid for **30 minutes** and is **single-use** (consumed the moment the agent retrieves its token). (A pre-issued `--token` or `--email/--password` also work for non-interactive use.)
- **Execution (Service Host):** Once authenticated, the user reserves a subdomain, a region, and a set of routes; the agent connects to the chosen region's node over a pinned-TLS channel, and the daemon can be registered to auto-start on boot via systemd.
- **Adding a region:** An operator deploys another `core_proxy_backend` with a distinct `NODE_ID`/`PUBLIC_HOST` pointed at the same control plane; it self-registers and appears in the admin panel and every user's region dropdown.

## 5. MoSCoW Analysis

### Must Have *(done)*
- **Unified Rust CLI:** A single Service-Host agent, with auto-start daemon registration.
- **Device Authorization Flow:** Secure headless login via a temporary code and web endpoint.
- **Dynamic Routing:** The node correctly routes `duck-main.natforge.com` (Host/SNI) and dedicated TCP ports down the active yamux tunnel.
- **Web Dashboards:** Functional Service-Host and Admin panels wired to the REST API.
- **Persistence:** PostgreSQL + Redis with idempotent reservation; full state survives a restart.

### Should Have *(done)*
- **Multi-region data plane:** Self-registering nodes; users pick a region per tunnel.
- **Per-tunnel observability:** Bandwidth series + a per-connection log in the dashboard.
- **Geo-blocking:** MaxMind GeoLite2; platform-wide (admin, also gates login) and per-tunnel (owner).
- **Encrypted control channel:** TLS with self-signed certs pinned by fingerprint.
- **Auto-Reconnection:** Client reconnects without losing its subdomain, region, or ports.
- **Tunnel lifecycle:** Separate **Stop** (pause but keep the subdomain + ports, restartable) and **Delete** (remove and free ports). Tunnels are fully **editable** by owner or admin, subdomain (the public address), display name, and per-route labels; changing the subdomain of a *live* tunnel re-routes it onto the new host within a few seconds (the agent re-reserves on reconnect).
- **Self-service profiles:** Users change their own display name, email, and password from the dashboard.
- **Moderation:** Admins can rename/delete any tunnel, and ban/unban or delete a user (banning drops their live tunnels and blocks login).

### Could Have *(future)*
- **P2P Direct Connection (Hole Punching):** Attempt UDP hole punching before falling back to a regional relay.
- **Cross-region tunnel migration:** Move a live tunnel between regions without losing its subdomain.
- **Custom Domains:** Allow users to CNAME their own domain (e.g., `play.mygame.com`) to a tunnel.

### Won't Have
- **Desktop GUI Application:** The local agent remains a CLI/Background Daemon; all visual management is on the website.
- **Enterprise DDoS Mitigation:** Advanced Layer-7 attack scrubbing and WAF policies are out of scope.

## 6. Demonstration Setup (Live Defense Requirements)

The thesis defense will feature a multi-device live demonstration:
- **Infrastructure:** A Cloud VM (Ubuntu 22.04) running the Axum API, the Web UI, PostgreSQL, and Redis, plus a data-plane node; optionally a **second VM in another region** running a second node.
- **Scenario A (Relay Game Hosting):** Start a Minecraft server on Laptop A. Run the CLI in Service Host mode. Connect to the provided `duck-main.natforge.com` via Laptop B over a 4G hotspot.
- **Scenario B (Region choice + observability + geo-block):** Reserve a tunnel in the second region and connect to it; watch the per-tunnel **bandwidth and connection log** populate in the dashboard; add a country to the tunnel's **block list** and confirm connections from it are refused (and logged as `blocked`). Confirm the control channel is real TLS (`openssl s_client` to `:4000` presents the pinned cert).

## 7. Security & Ethical Considerations

- **Traffic Encryption:** The agent↔core control channel runs over real TLS (self-signed cert pinned by fingerprint), so every multiplexed stream is authenticated and confidential between the agent and the node; HTTPS routes additionally use SNI **passthrough**, so the node never decrypts origin traffic.
- **Abuse Prevention:** The control plane blocks universally abused ports by default (SMTP 25/465/587) to prevent spam, and supports **geo-blocking** by country, platform-wide (also gating login/registration) and per-tunnel. A userspace connection-rate guard sheds volumetric floods.
- **Privacy:** Relaying is in-memory only; no payload is ever written to disk. Only connection *metadata* (source, country, bytes, duration) is logged, visible solely to the tunnel's owner and admins. A Terms-of-Service "mere conduit" posture suits this no-inspection, no-retention design [8].
- **Stateful Authentication:** JWTs from the device-login phase authorize tunnel creation and prevent hijacking, following RFC 7519 [9]; scoped tunnel tokens bind a single subdomain. Banned accounts are refused at login and cannot reserve tunnels.
- **Output Encoding (XSS):** The dashboards render user-controlled strings (emails, tunnel names, region labels) only through context-aware encoders, `escapeHtml` for element text and `escapeAttr` (`JSON.stringify` + HTML-encode) for inline event-handler attributes, closing a stored-XSS vector where a crafted tunnel name could otherwise execute in an admin's session.
- **CI & automated security scanning:** GitHub Actions runs lint (`fmt` + `clippy -D warnings`), unit tests, a release build, and the full 27-assertion e2e on every push/PR, plus a security layer, **cargo-audit** (RustSec CVEs, triaged in `.cargo/audit.toml`), **gitleaks** (secret scanning), and **Dependabot** (weekly dependency updates). See `docs/ci.md` and `Thesis.md` §5.5.
- **Continuous deployment & monitoring:** on merge to `main`, the server is built into Docker images (pushed to `ghcr.io`), **Trivy**-scanned, and deployed to the VM (`docker compose pull && up -d`, with a reachability gate, a post-deploy health check, and a manual "Deploy" button). The agent is published as `x86_64` **glibc + static-musl** binaries on a rolling `latest` release; the static-musl build runs on **any** Linux distro (verified on Alpine, which has no glibc). An off-VM GitHub Actions **uptime** watcher emails you on downtime. See `docs/cd.md`, `docs/monitoring.md`, and `Thesis.md` §5.6, §5.7. Install the agent:
  ```sh
  curl -L https://github.com/EvanLei-git/Thesis-reverse-proxy/releases/latest/download/natforge-x86_64-linux-musl -o natforge && chmod +x natforge
  ```

## 8. Project Structure and Documentation

This platform is split into three primary working directories, each requiring its own standalone `DOCUMENTATION.md` file to detail specific dependencies, endpoints, and CLI arguments:

1. **`/frontend`**: The Web UI dashboards (Service Host and Admin panels).
2. **`/website_backend`**: The Rust Axum control plane, REST API, auth/signaling, region registry, and database state manager.
3. **`/core_proxy_backend`**: The Rust data-plane node, TLS+yamux relay, shared Host/SNI routers, dedicated TCP ports, geo-blocking.
4. **`/natforge`**: The unified Rust Service-Host agent (CLI/daemon) deployed on user machines.

---

## 8.5 Deploying the Domain with Cloudflare

NatForge's routing (one shared `:80`/`:443` demultiplexed by subdomain, plus dedicated TCP ports) fits Cloudflare DNS perfectly: **one wildcard record serves unlimited tunnel subdomains**, no per-subdomain record and no practical limit.

1. **Delegate** `natforge.com` to Cloudflare (set the registrar nameservers to the two Cloudflare provides).
2. **Records** (the core proxy runs on a cloud VM with a static public IP, it is the public endpoint and cannot itself be behind CGNAT):
 - `A natforge.com → <VM IP>`, the dashboard/apex (may be Cloudflare-proxied/orange).
 - `A *.natforge.com → <VM IP>`, **DNS-only (grey cloud)**; this single record resolves every tunnel subdomain, which the core then routes by `Host`/SNI.
 - Open the VM firewall for `80`, `443`, `4000` (agent control), and `20000–20100` (TCP pool).
3. **Keep the wildcard grey-cloud (DNS-only).** Cloudflare's orange-cloud proxy *terminates TLS* (breaking NatForge's SNI passthrough) and does **not** carry arbitrary TCP ports (e.g. `25565`) without **Spectrum** (paid). Grey-cloud passes both straight to the origin. The platform does its own routing/relay/DDoS, so it does not need Cloudflare's proxy; orange-cloud only the apex if you want CDN/WAF on the dashboard.
4. **Clean game addresses (optional):** for tcp routes the core's DNS module (a real Rust Cloudflare-v4 client) provisions a per-tunnel `_minecraft._tcp.<sub>` **SRV** record on tunnel-up and removes it on teardown, so players can enter just `<sub>.natforge.com`. This runs the live API when `CF_API_TOKEN` is set and logs in local dev; without SRV, players use `<sub>.natforge.com:<port>`.

**Bottom line:** Cloudflare handles all the subdomains with no issue, a single grey-cloud wildcard is the ideal fit for this architecture. The only constraints are deliberate: keep the wildcard DNS-only so SNI passthrough and non-standard TCP ports work, and use Spectrum if you ever want proxied raw-TCP. Full detail in `Thesis.md`, Appendix D.

---

## 9. References & Further Reading

[1] R. Bush, "The IPv4 Address Exhaustion," *IEEE Internet Computing*, vol. 15, no. 6, pp. 65-68, Nov.-Dec. 2011. Details the implications of IPv4 address depletion and the forced adoption of carrier-side network abstractions. 
[2] M. Bagnulo, P. Matthews, and I. van Beijnum, "NAT64: Network Address and Protocol Translation from IPv6 Clients to IPv4 Servers," *RFC 6146*, Apr. 2011. Discusses widespread deployment of complex NAT scenarios like CGNAT and its restrictive impact on inbound peer-to-peer relationships. 
[3] S. Klabnik and C. Nichols, *The Rust Programming Language*. No Starch Press, 2018. Outlines fundamental Rust design paradigms such as strict data ownership, borrowing mechanisms, and guarantees against data races in concurrent networking. 
[4] W. R. Stevens and S. A. Rago, *Advanced Programming in the UNIX Environment*, 3rd ed. Addison-Wesley Professional, 2013. Offers detailed breakdowns of asynchronous, event-driven I/O modeling compared to blocking processes, demonstrating why in-memory routing buffer arrays achieve significantly higher throughput than traditional persistence-to-disk proxies. 
[5] J. Rosenberg, R. Mahy, P. Matthews, and D. Wing, "Session Traversal Utilities for NAT (STUN)," *RFC 5389*, Oct. 2008. The standard authoritative documentation on how local endpoints determine mapping rules established by edge NAT/Firewalls. 
[6] B. Ford, P. Srisuresh, and D. Kegel, "Peer-to-peer communication across network address translators," in *Proceedings of the USENIX Annual Technical Conference (ATC)*, 2005. A seminal evaluation of traversal success rates across diverse NAT hardware, explicitly detailing the inefficiencies of TCP Simultaneous Open implementations versus more permissible UDP stateless hole punching logic. 
[7] W. Denniss, J. Bradley, M. Jones, and H. Tschofenig, "OAuth 2.0 Device Authorization Grant," *RFC 8628*, Aug. 2019. Details the specifications for the code-flow model necessary to securely provision interactive token grants for browserless agents running in headless environments. 
[8] R. Dingledine, N. Mathewson, and P. Syverson, "Tor: The Second-Generation Onion Router," in *Proceedings of the 13th USENIX Security Symposium*, 2004. Explores the liability models inherent in deploying decentralized routing proxies and the port-restriction mechanisms necessary to counteract protocol abuse on volunteer nodes. 
[9] M. Jones, J. Bradley, and N. Sakimura, "JSON Web Token (JWT)," *RFC 7519*, May 2015. Outlines the token format utilized in stateless web infrastructures to securely transport user and session identity metadata.
[10] J. Donenfeld, "WireGuard: Next Generation Kernel Network Tunnel," in *Proceedings of the Network and Distributed System Security Symposium (NDSS)*, 2017. Explores the cryptographic structures enabling massive global throughput over unrestrictive UDP tunneling primitives.
[11] Cloudflare Inc., "Understanding Volumetric DDoS Mitigation via eBPF," *Network Security Engineering Review*, 2021. Reviews the methodology behind analyzing raw packet ingestion rates using heuristics to deploy dynamic drops before multiplexed layer-saturation occurs.