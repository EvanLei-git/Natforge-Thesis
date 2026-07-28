# Frontend (Dashboard), Documentation

Vanilla HTML + a **custom CSS design system** (no framework) + plain JavaScript, no build step. Served as static files by the control plane (`ServeDir` rooted at `frontend/`), so the dashboard and API share an origin.

## Design system
A dark, layered UI: **Discord-style surfaces** (`#1a1b1e` / `#2b2d31` / `#313338`), the **brand teal `#40b8c0`** taken from the logo (`natforge_flake`) as the accent, and **Azure-style components**, Segoe UI, low-radius rectangular buttons/inputs, flat fills, crisp focus rings. All defined as CSS variables at the top of `assets/css/style.css` (`--brand`, surfaces, `--radius-sm`, etc.). **No external CSS/JS framework and no emoji**, icons are inline stroke-SVGs (line style matching the hexagon logo) defined in `assets/js/app.js` (`NF_ICONS`) and injected into `[data-icon]` placeholders. Components: `.btn` (primary/secondary/ghost/danger), `.card`, `.stat`, `.table`, `.badge` (status + route-mode), `.switch`, `.input`, `.tabs`, `.modal-overlay`, `.toast`, all custom and themeable from the variables.

The logo lives at `assets/img/natforge_flake.{png,ico}` (transparent teal flake), used as the sidebar/auth mark and the favicon.

## Layout
```
frontend/
├── views/ index.html (login/register), dashboard.html (Service Host),
│ admin.html (Admin), users.html (Admin → Users)
├── api/client.js NatForgeAPI: fetch wrapper, JWT storage, one method/endpoint
└── assets/
 ├── css/style.css design tokens + components (custom, no framework)
 ├── js/app.js NF_ICONS + injector, requireAuth/logout, escapeHtml, tabs, modal, toast, fmtBytes
 └── img/natforge_flake.{png,ico}
```
HTML under `views/` references siblings with `../api/...` and `../assets/...`; `/` redirects to `/views/index.html`.

## Pages & wiring (all calls via `window.API`)
- **index.html**, sign in / create account; stores JWT + role; redirects by role.
- **dashboard.html**, the **Service Host** view: a **tunnel selector** (dropdown) driving a per-tunnel **detail panel** that shows the tunnel's **location** (region), **logging** (bandwidth summary + a recent-connections table from `getTunnelBandwidth`/`getTunnelLogs`), and **blocking** (a per-tunnel country-block editor via `getTunnelRegionBlocks`/`setTunnelRegionBlocks`). A **route builder** modal with an optional **custom subdomain**, a **region** dropdown (`getRegions`), and per-route **label** inputs submits `requestTunnel(routes, subdomain, nodeId)` and prints the exact agent command. A card approves a CLI device code (RFC 8628).
- **admin.html**, `requireAuth(true)`; stats + region/port blocks + the **regions (nodes)** table (`getNodes`, with rename/enable/disable/remove) + all-tunnels (with agent IP), refreshed on an interval.
- **users.html**, `requireAuth(true)`; `getUsers()` + `getAllTunnels()`; per-user table (role, tunnel count, traffic, last seen) expanding to that user's tunnels with **agent IP** and route badges.

## Auth model
`API` stores the JWT and sends `Authorization: Bearer …`. `requireAuth()` redirects unauthenticated users; `requireAuth(true)` enforces the admin role; `.admin-only` nav is hidden for non-admins.
