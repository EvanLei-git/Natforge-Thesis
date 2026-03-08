# THESIS PROPOSAL

**Title:** Design and Implementation of a Hybrid Reverse Proxy and P2P Tunneling Platform with Decentralized Edge Nodes

**Author:** Evangelos Leivaditis

**Language/Tech Stack:** Rust, Tokio, HTML/Bootstrap, PostgreSQL, Redis

**Architecture:** Client-Server with P2P NAT-Traversal Extension

## 1. Abstract

This research proposes the development of a high-performance, hybrid reverse proxy and tunneling platform designed to expose local network services (e.g., game servers behind NAT/CGNAT) to the public internet without requiring port forwarding. Unlike traditional centralized solutions, this platform introduces a dual-architecture model:

- **A Centralized Core:** A high-throughput control plane written in Rust that handles authentication, signaling, and fallback routing.
- **A Decentralized "Edge" Layer & P2P Engine:** A system that establishes Direct Connections between users when possible, or routes traffic through volunteer residential IPs ("Edge Nodes") to bypass datacenter IP blacklists and offload centralized bandwidth.

Crucially, to protect the privacy of Edge Nodes and ensure high throughput, all relaying is done entirely in-memory via asynchronous buffers, mitigating passive data retention. The system features a centralized web management interface and a unified Rust-based agent, allowing users to seamlessly switch between hosting local services and providing network infrastructure.

## 2. Problem Statement & Motivation

**The Problem:** The exhaustion of IPv4 addresses has led ISPs to heavily rely on CGNAT (Carrier-Grade NAT). This architectural shift inherently breaks end-to-end connectivity, making it virtually impossible for standard home users, gamers, and self-hosters to securely expose local applications to the internet without renting a Virtual Private Server (VPS) or purchasing a static IP [1]. Furthermore, CGNAT architectures disrupt standard peer-to-peer protocols by sharing single public IPs across thousands of subscribers [2].

**The Proposed Solution:** A "Tunneling as a Service" platform that democratizes internet exposure. By allocating randomized subdomains and specific ports to users, the platform allows friends to connect to privately hosted game servers seamlessly. Furthermore, by implementing P2P NAT traversal and utilizing community-provided public IPs, the platform drastically reduces the bandwidth overhead on the central server and mitigates datacenter-level IP blocking.

## 3. Core Use Cases & Actor Roles

The system serves three distinct user roles, managed through a unified Bootstrap-based Web UI:

- **The Service Host (Standard User):** A user who wants to host a game server (e.g., Minecraft) from their home PC. They are assigned a random subdomain (e.g., `duck-main.test.com`) and up to 2 specific public ports.
- **The IP Host / Edge Node (Superuser):** A user with a public, non-CGNAT IP who volunteers to share their network. They act as a residential relay for other users, managing their bandwidth limits via the web dashboard.
- **The Administrator:** Oversees the entire network, monitors active tunnels, manages global bandwidth, and handles abuse/banning (including region blocking for specific geolocations for both hosts and IP addresses) via an Admin Panel.

## 4. Technical Architecture

### A. The Data Plane (The Rust Tunnel Engine)

### A. The Data Plane (The Rust Tunnel Engine)

The core high-throughput networking is isolated in the `core_proxy_backend` application, written entirely in Rust for deterministic memory safety and predictable latency [3]. 

- **WireGuard & Multiplexing Integrations:** Utilizes `boringtun` (WireGuard in userspace) combined with `yamux` to allow a high-speed, cryptographically verified UDP tunnel to carry multiple simultaneous data streams [10]. Each user is deterministically allocated **1 specific TCP and UDP port** mapped natively to the Global Anycast pool.
- **DDoS Mitigation (eBPF & Heuristics):** To protect volunteer nodes and the central infrastructure, the core engine tracks packet ingress. If volumetric floods (e.g., >10,000 packets/sec per IP) are detected, simulated eBPF firewall configurations seamlessly drop malicious payloads before they saturate the multiplexed buffer tunnels [11].
- **DNS SRV & Anycast Routing:** The `core_proxy` interacts securely with generalized DNS providers (e.g., Cloudflare APIs). Upon tunnel allocation, it dynamically provisions DNS SRV records (e.g., `_minecraft._tcp.duck-main`), mapping global gamers directly into the high-speed Node's designated TCP/UDP bounds, bypassing CGNAT transparently.
- **Bandwidth Tracking & Economics:** TCP/UDP byte lengths are tracked natively within the connection loops. This byte data is continually offloaded securely to the website server for accurate database accumulation and host compensation mapping.

### B. The User Platform (`website_backend` & `frontend`)

- **API Server & Authentication:** The `website_backend` isolates user data from the high-throughput network buffers. Built around Axum, it manages robust Web UI accounts utilizing the **Argon2** cryptographic hashing algorithms and JWT mappings. 
- **State Management & Persistence:** Redis for in-memory tracking; PostgreSQL for user accounts, logs, and billing/bandwidth quotas (mapped to the Core Proxy). The full API structures for account controls and region blocking are detailed in `/website_backend/DOCUMENTATION.md`.
- **Web Interface (UI):** Built with simple, lightweight vanilla HTML, JavaScript, and Bootstrap 5 to ensure a clean, responsive design. Refactored into a professional workspace separating `views`, `api`, and `assets`. Contains the Admin, Service Host, and IP Host Superuser panels dynamically interacting with the Axum REST endpoints. The structural mappings for the UI are detailed in `/frontend/DOCUMENTATION.md`.

### C. System Flow & Configuration (The Ubuntu Rust Script)

The client-side is driven by a single compiled Rust binary deployed via an Ubuntu script, serving both host types:

- **Initialization:** When run, the CLI asks the user to select their role: Service Host or IP Host.
- **Device Authorization Flow:** To securely link the headless CLI to the web account, the system utilizes the OAuth 2.0 Device Authorization Grant flow (RFC 8628) [7]. The CLI securely outputs: *"Please go to thesis.net/device and enter code XYZ."*
- **Execution (Service Host):** Once authenticated, the server allocates a subdomain (e.g., `duck-main`) and 2 ports. The client connects to the central server, and the daemon is registered to auto-start on PC boot via systemd.
- **Execution (IP Host):** The client registers its public IP with the central control plane. The user is upgraded to a "Superuser" in the web UI, where they can configure maximum allowed bandwidth and toggle their active status.

## 5. MoSCoW Analysis

### Must Have
- **Unified Rust CLI:** A single agent capable of running in both "Service Host" and "IP Host" modes, with auto-start daemon registration.
- **Device Authorization Flow:** Secure headless login mapping via a temporary string and web endpoint.
- **Dynamic Routing:** The central server correctly routes requests like `duck-main.test.com:25565` down the active Yamux tunnel.
- **Web Dashboards (Bootstrap):** Separate functional panels for Admin, User, and IP Host management.
- **Basic Security:** TLS termination at the web-server level for the UI and API.

### Should Have
- **P2P Direct Connection (Hole Punching):** Attempt UDP hole punching before falling back to server-relayed traffic.
- **Bandwidth Management:** IP Hosts must be able to set and enforce hard data limits via their web panel to prevent ISP overage charges.
- **Auto-Reconnection:** Client gracefully handles internet drops and reconnects without losing the allocated subdomain mapping.

### Could Have
- **Decentralized Rewards:** A database ledger crediting IP Hosts for relaying traffic, potentially redeemable for premium accounts.
- **Geo-Routing:** Allowing Service Hosts to dynamically request egress via an IP Host situated in a specific country.
- **Custom Domains:** Allowing users to CNAME their own domain (e.g., `play.mygame.com`) to the platform.

### Won't Have
- **Desktop GUI Application:** The local agent will remain a CLI/Background Daemon; all visual management is isolated to the centralized website.
- **Enterprise DDoS Mitigation:** Advanced Layer 7 attack scrubbing and WAF policies are strictly out of scope.

## 6. Demonstration Setup (Live Defense Requirements)

The thesis defense will feature a multi-device live demonstration:
- **Infrastructure:** 1 Cloud VM (Ubuntu 22.04) running the Axum API, Bootstrap Web UI, PostgreSQL, and Redis.
- **Scenario A (Direct/Relay Game Hosting):** Start a Minecraft server on Laptop A. Run the CLI in Service Host mode. Connect to the provided `duck-main.test.com` via Laptop B over a 4G hotspot.
- **Scenario B (P2P/Edge Node Routing):** Run the CLI in IP Host mode on Laptop C. Configure Laptop A to route through Laptop C. Verify the IP change using a terminal request to an IP-checking service (such as `curl ifconfig.me`), proving the decentralized relay functions correctly.

## 7. Security & Ethical Considerations

- **Traffic Encryption:** End-to-end encryption of the tunnel to prevent Man-in-the-Middle attacks between the proxy relay and the origin server.
- **Abuse Prevention & Liability Mitigation:** Because "IP Hosts" are allowing strangers to use their public IP, severe edge-case safeguards are required. The central server will explicitly block traffic on universally abused ports, including SMTP (Ports 25, 465) to prevent email spam. Strict adherence to legal compliance frameworks and implementing Terms of Service comparable to those governing Tor exit nodes drastically limits the abuse liability placed upon residential node operators [8].
- **Stateful Authentication:** JWT (JSON Web Tokens) generated during the device-login phase will be used continually to authorize tunnel creation and prevent hijacking, following RFC 7519 standards [9].

## 8. Project Structure and Documentation

This platform is split into three primary working directories, each requiring its own standalone `DOCUMENTATION.md` file to detail specific dependencies, endpoints, and CLI arguments:

1. **`/frontend`**: The centralized Web UI dashboards (Admin, Service User, and Superuser panels). This component will be built out first to establish a solid specification and user flow for the Rust networking agent.
2. **`/backend`**: The Rust Axum REST API, signaling server, and database state manager.
3. **`/proxy_node`**: The unified Rust daemon (CLI script) deployed on standard user machines and residential IP nodes.

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