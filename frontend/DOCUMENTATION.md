# Frontend (Dashboard), Documentation

Vanilla HTML + a **custom CSS design system** (no framework) + plain JavaScript, no build step. Served as static files by the control plane (`ServeDir` rooted at `frontend/`), so the dashboard and API share an origin.

## Design system
A dark, layered UI: **Discord-style surfaces** (`#1a1b1e` / `#2b2d31` / `#313338`), the **brand teal `#40b8c0`** taken from the logo (`natforge_flake`) as the accent, and **Azure-style components**, Segoe UI, low-radius rectangular buttons/inputs, flat fills, crisp focus rings. All defined as CSS variables at the top of `assets/css/style.css` (`--brand`, surfaces, `--radius-sm`, etc.). **No external CSS/JS framework and no emoji**, icons are inline stroke-SVGs (line style matching the hexagon logo) defined in `assets/js/app.js` (`NF_ICONS`) and injected into `[data-icon]` placeholders. Components: `.btn` (primary/secondary/ghost/danger), `.card`, `.stat`, `.table`, `.badge` (status + route-mode), `.switch`, `.input`, `.tabs`, `.modal-overlay`, `.toast`, all custom and themeable from the variables.

The logo lives at `assets/img/natforge_flake.{png,ico}` (transparent teal flake), used as the sidebar/auth mark and the favicon.

## Layout
```
frontend/
├── views/       landing.html (/), signin.html (/signin), dashboard.html (/dashboard, Service Host),
│                admin.html (/admin/network), users.html (/admin/users),
│                tunnels.html (/admin/tunnels), profile.html (/profile)
├── api/client.js         NatForgeAPI: fetch wrapper, JWT storage, one method per endpoint (~40)
└── assets/
    ├── css/style.css      design tokens + components (custom, no framework)
    ├── js/app.js          icons + injector, auth guard, modals/tabs/toasts, output encoders, formatters
    ├── js/sidebar-nav.js  read-only device -> service-host tree for the non-dashboard pages
    └── img/natforge_flake.{png,ico}
```
The control plane serves **clean, extensionless routes** (`/`, `/signin`, `/dashboard`, `/admin/network|users|tunnels|profile`); pages load scripts and assets by absolute path (`/client.js`, `/assets/...`).

## Pages & wiring (all calls via `window.API`)
- **landing.html** (`/`), the public entry page.
- **signin.html** (`/signin`), sign in / create account; stores the JWT + role and redirects by role.
- **dashboard.html** (`/dashboard`), the **Service Host** view: a **device-tree sidebar** (each enrolled device expands to its service hosts; standalone service hosts group separately) driving a per-tunnel **detail panel** with **location** (region), **logging** (bandwidth summary + a recent-connections table), and **blocking** (a per-tunnel country editor). From here a user requests tunnels (a region-aware route builder + optional custom subdomain), **adds/removes ports** on a live service (`setServiceRoutes`), sets a **per-route SRV** (`setRouteSrv`), attaches a **custom domain** (`setCustomDomain`/`clearCustomDomain`), **migrates** a tunnel to another region (`migrateTunnel`), and **stops/starts/deletes** it. An **Add device** modal enrolls a machine by pasting the code printed by `natforge enroll` (valid 1 hour).
- **admin.html** (`/admin/network`), `requireAuth(true)`, stats + platform-wide region blocks + the **regions (nodes)** table (rename/enable/disable/remove).
- **users.html** (`/admin/users`), per-user table (role, tunnel count, traffic, last seen) expanding to each user's tunnels + agent IP, with ban/unban/delete.
- **tunnels.html** (`/admin/tunnels`), every tunnel across the platform (owner, region, public port, routes, traffic).
- **profile.html** (`/profile`), self-service display name / email / password.

## Auth model
`API` stores the JWT and sends `Authorization: Bearer …`. `requireAuth()` redirects unauthenticated users; `requireAuth(true)` enforces the admin role. `applyRoleVisibility` hides `.admin-only` nav for non-admins **and** `.user-only` items for admins. Every server-supplied string is rendered through `escapeHtml` (element text) or `escapeAttr` (inline event-handler attributes) to prevent stored XSS.
