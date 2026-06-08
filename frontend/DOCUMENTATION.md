# Frontend (Dashboard) — Documentation

The NatForge web UI: vanilla HTML + Bootstrap 5 (CDN) + plain JavaScript, no build step. Served as static files by the control plane (`ServeDir` rooted at `frontend/`), so the dashboard and API share an origin.

## Layout
```
frontend/
├── views/        index.html (login/register), dashboard.html (Service Host),
│                 ipbase.html (IP Host), admin.html (Admin)
├── api/          client.js   — NatForgeAPI: fetch wrapper, JWT storage, one method/endpoint
└── assets/
    ├── js/app.js — requireAuth(adminOnly), logout, fmtBytes, toast, role visibility
    └── css/style.css
```

HTML under `views/` references siblings with `../api/...` and `../assets/...`. The control plane redirects `/` → `/views/index.html`.

## Pages & wiring (all calls go through `window.API`)
- **index.html** — Sign in (`API.login`) / Create account (`API.register`); stores the JWT and role in `localStorage`, redirects by role.
- **dashboard.html** — polls `API.getTunnels()` every 4s; "Request Tunnel" (`API.requestTunnel`) shows the exact CLI command with the tunnel token; per-row "Stop" (`API.stopTunnel`); a "Link a CLI Device" card calls `API.approveDevice` (RFC 8628 approval).
- **ipbase.html** — loads `API.getIpHostStatus`; toggle calls `API.setRelayStatus`; preferences form calls `API.updatePrefs`.
- **admin.html** — guarded by `requireAuth(true)`; loads stats, region blocks, port blocks, and all tunnels in parallel and re-renders every 5s; add/remove region and port blocks live.

## Auth model
`API` stores the JWT and attaches it as `Authorization: Bearer …`. `requireAuth()` redirects unauthenticated users to the login page; `requireAuth(true)` additionally enforces the admin role, and `.admin-only` nav elements are hidden for non-admins.
