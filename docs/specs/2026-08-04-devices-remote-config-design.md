# Devices and remote-configured agents

**Status:** design approved 2026-08-04. Build in phases (see end).

## Goal

Replace the "copy a `natforge service-host --route ...` command and run it" model
with a **device** the user manages entirely from the dashboard:

- Enroll one agent per machine (**agent-first**, extending the RFC 8628 device
  flow to a long-lived *device token*).
- A device hosts **multiple services**; each service is a subdomain + its routes.
  One agent connection serves all of them.
- The dashboard is the **source of truth** for routes. Adding/removing/changing a
  port in the UI updates a running agent within a few seconds, with no CLI.

Hierarchy: **Device -> Service (a tunnel) -> Routes (ports)**.

## Data model

New table `devices`:

| column | notes |
|---|---|
| `id` | BIGINT identity PK |
| `owner_id` | FK users(id) ON DELETE CASCADE |
| `name` | TEXT, user-chosen ("device 1" default) |
| `token_fp` | TEXT, SHA-256 of the issued device token (for revoke/verify), nullable until enrolled |
| `status` | TEXT CHECK (pending, online, offline) |
| `agent_ip` | TEXT nullable |
| `last_seen` | TIMESTAMPTZ nullable |
| `created_at` | TIMESTAMPTZ default now() |

Reuse `tunnels`/`routes`. `tunnels` gains `device_id BIGINT REFERENCES devices(id) ON DELETE CASCADE`
(nullable during migration; a tunnel with a `device_id` is a *service* of that device).

**Port uniqueness:** a device's services may not reuse a local port. Partial unique
index on `(device_id, local_port)` in `routes` via a device-scoped check at
reservation/edit time (routes has no device_id, so enforce in the query by joining
to tunnels; or denormalize device_id onto routes for a DB constraint - decide in P3).

## Enrollment (agent-first)

1. `natforge enroll --control-plane https://natforge.com` -> `POST /api/devices/enroll/start`
   returns `{device_code, user_code, verification_uri, interval}` (same shape as the
   existing device flow, new endpoint).
2. Agent prints the code and polls `POST /api/devices/enroll/token {device_code}`.
3. In the dashboard the user enters the `user_code`, **names** the device, and approves
   (`POST /api/devices/enroll/approve {user_code, name}`, session-authed). This creates
   the `devices` row and marks the enroll code approved.
4. The agent's next poll returns `{device_token, device_id}`. It stores the token
   (e.g. `~/.config/natforge/device.json`, 0600) and switches to run mode.

**Device token:** a JWT with `{ purpose: "device", device_id, sub: owner_id }`, long-lived
(1 year) and revocable by clearing `devices.token_fp` (verified against the stored fp on
each use, so revoke is immediate). Never a session token.

## Run mode + remote config

`natforge run` (or the enroll flow continuing) uses the stored device token:

1. `GET /api/devices/me/config` (device-token authed) -> the device's services and routes:
   `[{ subdomain, region, custom_domain, routes: [{route_id, mode, local_port, label}] }]`.
2. The agent reserves/connects for each service and serves them (Phase 3: over one
   connection; Phase 2: the agent can start with one service to prove the pull path).
3. On a config change the control plane **signals the agent to reconnect** (the existing
   `signal_node_stop`-style mechanism, generalized to a device). On reconnect the agent
   re-fetches `/config` and applies it. "Live" = a few seconds; no CLI, no manual restart.

The route config is **server-authoritative**: no `--route` flags in run mode. The
reservation's route-shape idempotency key is replaced by the device's stored config.

## Core: one agent, many subdomains

Today one agent connection registers one subdomain in the core route registry. For a
device the core registers **each of the device's services** (subdomains + pooled ports)
against that one agent connection's `open_tx`. The per-stream preamble already carries
`route_id`; the agent's route map gains entries for every service. The control plane's
reservation issues a device-scoped token authorizing all the device's subdomains/ports.

## API surface (new/changed)

- `POST /api/devices/enroll/start` (none) -> device/user codes.
- `POST /api/devices/enroll/token` (none) -> `{status}` or `{device_token, device_id}`.
- `POST /api/devices/enroll/approve` (session) `{user_code, name}` -> create device, approve code.
- `GET /api/devices` (session) -> the caller's devices (+ nested services).
- `PATCH /api/devices/{id}` (session) -> rename.
- `DELETE /api/devices/{id}` (session) -> remove device + its services (frees ports), revoke token.
- `GET /api/devices/me/config` (device token) -> this device's services + routes.
- `POST /api/devices/{id}/services` (session) -> add a service (subdomain + routes) to the device.
- `POST /api/devices/{id}/services/{tid}/routes` (session) -> add a route to a service (P2/P3).
- Route/service edits reuse the existing tunnel edit/delete endpoints, scoped to the device.

## UI

- **Sidebar:** the nav's "Service Host" entry becomes **Devices**. A cyan **Add device**
  button at the top opens the enroll modal (run `natforge enroll`, enter the `user_code`,
  name it). Below it, the device list; each device expands to show its service-hosts
  indented beneath it. **Profile moves to the bottom**, above Sign out.
- **Right pane:** the selected device's service-hosts are stacked (host 1, host 2, ...,
  no dropdown) in the scrollable content pane. A **scroll-spy** highlights the service-host
  currently scrolled into view in the sidebar (and clicking one in the sidebar scrolls to
  it). Each service-host shows its subdomain, routes/ports (inline add/remove), custom
  domain, region, and logging. Per-device port-uniqueness check on add.

## Migration of existing tunnels

Existing tunnels have `device_id = NULL` and keep working through the current
flag-based `service-host` flow. Devices are additive. (Optionally, later, offer to
"attach" an existing tunnel to a device.)

## Security

- Device token is long-lived but fingerprint-checked against `devices.token_fp`; deleting
  the device clears the fp, revoking instantly.
- A device may only claim subdomains/ports it owns (owner_id scoping, same as tunnels).
- Enroll codes keep the existing 30-min single-use TTL.

## Non-goals

- True zero-downtime hot config reload (reconnect-based is enough).
- Auto-discovery of local services (the user still declares ports).

## Phasing

- **Phase 1 - device entity + enrollment + sidebar.** `devices` migration; enroll
  endpoints + device token; agent `enroll`/`run`; dashboard Add-device modal + device
  list; Profile to the bottom. Deliverable: enroll and name a device, see it listed
  online.
- **Phase 2 - server-authoritative routes.** Move route config to the DB per device;
  agent pulls `/config` and serves one service from it; live update via reconnect.
  Deliverable: add/remove a port in the UI, the agent applies it.
- **Phase 3 - multi-service per device + full UI.** Core multi-subdomain routing so one
  agent serves several services; the Device->ServiceHost right-pane UI (host 1/2/3);
  per-device port uniqueness constraint.
