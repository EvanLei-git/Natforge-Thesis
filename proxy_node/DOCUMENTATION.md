# Proxy Node (Rust CLI) Documentation

This document outlines the architecture, setup, and usage of the unified Rust daemon used by both standard game hosts and edge node IP providers.

## Overview

The daemon relies heavily on:
1. **Tokio Async I/O:** `tokio::io::copy_bidirectional` is utilized to establish zero-disk memory buffering. This maps the incoming traffic directly into the user's local instance (e.g., Minecraft on Port 25565).
2. **WireGuard & Yamux Multiplexing:** `boringtun` establishes a high-speed secure UDP tunnel. `yamux` multiplexes the inner stream to handle thousands of sub-connections mapped to **1 Dedicated TCP** and **1 Dedicated UDP** port on the Global Anycast `core_proxy_backend`.
3. **STUN Traversal:** Custom UDP encapsulation for direct P2P NAT Traversal, falling back to WireGuard/Yamux over TCP if symmetric blocking occurs.

## CLI Commands

### 1. Act as a Service Host
```bash
proxy_node service-host --local-port 25565 --subdomain duck-main --control-plane http://api.thesis.net
```
**Action:** Allocates `duck-main.thesis.net` and automatically pushes traffic down the active tunnel into `127.0.0.1:25565`.

### 2. Act as an IP Edge Node
```bash
proxy_node ip-host --max-bandwidth 100 --control-plane http://api.thesis.net
```
**Action:** Registers the host's public IP as an available relay in the P2P network, capping throughput at 100 Mbps. Uses STUN to verify symmetric/asymmetric NAT types before listing in the Control Plane database.
