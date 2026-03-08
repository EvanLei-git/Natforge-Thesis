# Frontend Web UI Documentation

This document outlines the setup, features, and precise API specifications required by the centralized web dashboard. 

## Technology Stack
- **Framework:** Vanilla HTML/JS
- **Styling:** Bootstrap 5 (CDN)
- **API Client:** `/api/client.js` handles all JS `fetch()` requests and JWT authorization headers.
- **Routing:** Direct file links located in `/views/` (`index.html`, `dashboard.html`, `ipbase.html`, `admin.html`)
- **Assets:** CSS and vendor scripts located in `/assets/`

## Page Breakdown & Required API Endpoints

The frontend requires the `website_backend` Axum server (Port 3000) to implement the following REST routes to function completely:

### 1. Login Authentication (`index.html`)
- **Device Authorization (OAuth 2.0 Flow):**
  - **`POST /api/auth/device`**
  - Payload: `{ "code": "XYZ-ABC" }`
  - Action: Validates the CLI code and issues a JWT session token to the browser.
- **Web Login:**
  - **`POST /api/auth/login`**
  - Payload: `{ "email": "...", "password": "..." }`
  - Action: Standard user login returning a JWT.

### 2. Service Host Dashboard (`dashboard.html`)
- **Get Active Tunnels:**
  - **`GET /api/tunnels`**
  - Action: Returns an array of the user's active subdomains, local ports, and routing mode (Direct vs Relay).
- **Request New Tunnel:**
  - **`POST /api/tunnels/request`**
  - Action: Allocates a new random `duck-xxx.test.com` subdomain and awaits the CLI connection.
- **Stop Tunnel:**
  - **`DELETE /api/tunnels/{subdomain}`**
  - Action: Forcibly closes the specific Yamux tunnel.

### 3. IP Host Dashboard (`ipbase.html`)
- **Toggle Relay Active Status:**
  - **`POST /api/ip_host/status`**
  - Payload: `{ "active": true/false }`
  - Action: Advertises or removes the node from the active public relay pool.
- **Update Bandwidth & Preferences:**
  - **`PUT /api/user/preferences`**
  - Payload: `{ "max_bandwidth_mbps": 100, "geo_pref_only": true }`
  - Action: Updates the PostgreSQL database limits for this specific IP Host.

### 4. Admin Panel & Region Blocking (`admin.html`)
- **Global Port Bans:**
  - **`POST /api/admin/port_blocks`**
  - Payload: `{ "port": 25 }`
  - Action: Adds a globally banned TCP/UDP port across the entire network.
  - **`DELETE /api/admin/port_blocks/{port}`**
- **Region and Geographic Blocking (Crucial):**
  - **`GET /api/admin/region_blocks`**
  - Action: Returns an array of blocked ISO Country Codes (e.g. `['RU', 'CN']`).
  - **`POST /api/admin/region_blocks`**
  - Payload: `{ "country_code": "CN" }`
  - Action: Adds an entire country to the blocklist. Any `proxy_node` or `Service_Host` originating from IPs geo-located in these countries will be instantly rejected by the Control Plane.
  - **`DELETE /api/admin/region_blocks/{country_code}`**
  - Action: Removes a country from the blocklist.
