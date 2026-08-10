# NatForge, Live Deployment Record & Troubleshooting

A concrete record of standing up the **head node** (control plane + region 1) on Azure,
plus **every difficulty hit and how it was resolved**. `docs/hosting.md` is the reference
guide ("how to host"); this file is "what we actually did, and what went wrong."

---

## 1. What is running

| Thing | Value |
|---|---|
| Cloud VM | Azure, hostname `head-node`, **Ubuntu 24.04 LTS**, 2 vCPU / 7.7 GB RAM / 29 GB disk |
| Public IP | `<VM_IP>` |
| Domain | `natforge.com` (DNS on Cloudflare) |
| Toolchain | Docker 29.6 + Compose v5.2, Rust 1.96, build-essential/pkg-config/libssl-dev |
| Control plane | `natforge-backend` on `:3000` (dashboard + API) |
| Data-plane node | `natforge-node`, registered as `head-1` / name **Main** / region **Primary**, `public_host=natforge.com`, ports `:80 :443 :4000 :3001` + TCP pool `20000-20100` |
| Datastores | PostgreSQL 16 + Redis 7 via `docker compose` |
| Agent | `natforge`, runs on **users' machines**, not the VM |
| Services | systemd: `natforge-backend`, `natforge-node` (env in `/etc/natforge/*.env`, chmod 600) |

DNS: `natforge.com` A → IP, and `*.natforge.com` A → IP, **both DNS-only (grey cloud)**.
NSG inbound open: `22, 80, 443, 3000, 4000, 20000-20100`. Internal only: `3001, 5432, 6379`.

---

## 2. Steps performed (reproducible outline)

1. **SSH key:** `ssh-keygen -t ed25519 -f ~/.ssh/natforge_azure -N ""`; paste the `.pub` into Azure; connect as `azureuser@<ip>`.
2. **NSG inbound rules:** `22` (SSH), then one rule for `80,443,3000,4000,20000-20100`.
3. **Install on VM:** Docker (`get.docker.com`), Rust (`rustup`), and `build-essential pkg-config libssl-dev`.
4. **Ship code:** `rsync` the local working tree to `~/natforge` (GitHub only had the first commits, see §3.7).
5. **Datastores + build:** `sudo docker compose up -d`; `cargo build --release` (~7.5 min first build).
6. **Configure + run:** write `/etc/natforge/website.env` and `core.env` (with **identical** `JWT_SECRET` + `INTERNAL_SECRET`), systemd units, `systemctl enable --now`.
7. **DNS:** Cloudflare apex `A` + wildcard `*` `A`, both grey.
8. **Clean URL:** taught the core to forward the apex/`www` on `:80` to the dashboard (so `http://natforge.com` serves it, not `:3000`) and added a landing page.

To run the core on `:80/:443` as a non-root user, the systemd unit uses
`AmbientCapabilities=CAP_NET_BIND_SERVICE`.

---

## 3. Difficulties hit & how they were resolved

### 3.1 SSH "nothing worked", it was the firewall, not the key
`ssh ... azure@<ip>` hung. Two issues: (a) the Azure-suggested command had a literal
`<private-key-file-path>` placeholder and the wrong user, the admin user is **`azureuser`**;
(b) more importantly, **port 22 wasn't open in the NSG**. A `/dev/tcp` probe showed
*connection timed out* (SYN dropped), which is a firewall block, not an auth failure.
**Fix:** add an NSG inbound rule for `22`. Auth/username only matters *after* TCP connects.

### 3.2 Azure rejected the SSH public key ("Check your key…")
The key was a valid `ssh-ed25519` line, but Azure refused the paste. Cause: the key got
**mangled on copy** (a terminal line-wrap inserted a break), not a format problem.
**Fix:** copy the key as a single clean line (e.g. via clipboard `xclip < key.pub`).
ed25519 is accepted by Azure; no need for RSA.

### 3.3 App ports unreachable after services started
Services were listening on the VM, but `80/443/3000/4000/20000-20100` were blocked.
**Fix:** one NSG inbound rule with `Destination port ranges = 80,443,3000,4000,20000-20100`,
TCP, Allow. (Azure forbids two rules sharing a priority, use one rule or distinct priorities.)
**Outbound is irrelevant here**, Azure allows all outbound by default, and these are
inbound services; adding outbound rules does nothing.

### 3.4 The `20000-20100` "BLOCKED" false alarm ⚠️ subtle
A plain TCP-connect test reported the pool blocked, but it was actually **open**. The core
only **binds** a pool port (e.g. `20000`) *when a tunnel reserves a raw-TCP route*; with no
active TCP tunnel, nothing listens there. The tell: the probe **refused in 0.1 s** (RST =
firewall open, no listener) versus a **timeout** (firewall blocking). A crude test can't
tell those apart. **Lesson:** distinguish *refused* (open, idle) from *timeout* (blocked);
prove the pool with a live TCP tunnel, not a bare port scan.

### 3.5 Node registration failed for 30 s, no node row ⚠️ root-caused
The core logged "waiting for the control plane to accept node registration…" for the full
30-second retry window, then started without registering (empty `nodes` table → no region
to pick). Root cause: **`core.env` was missing `JWT_SECRET`/`INTERNAL_SECRET`** (only
`website.env` had them), so the core fell back to the built-in **dev secrets** → its
`INTERNAL_SECRET` didn't match the website's → every `node_register` returned `401`.
**Fix:** both env files must carry the **same** secrets. Diagnosed without printing the
secrets (compared lengths + a match boolean only). After fixing, the node registered instantly:
`registered node 'head-1'`, and `nodes` showed `head-1 | Main | natforge.com | active | has_cert`.

### 3.6 DNS: apex resolved, subdomains didn't
`natforge.com` resolved but `*.natforge.com` returned nothing, the **wildcard `A` record
was missing**. Common trap: in Cloudflare's Name field type **just `*`** (it displays as
`*.natforge.com`); typing `*.natforge.com` creates a broken `*.natforge.com.natforge.com`.
The wildcard **must stay grey/DNS-only** (orange-proxy breaks SNI passthrough and won't
carry the TCP pool).

### 3.7 GitHub didn't have the current code
The remote only had the first one or two commits (the daily commit plan was mid-way), so
`git clone` would have deployed stale code. **Fix:** `rsync` the local working tree directly
(`--exclude target --exclude .git`), which carries the real, current source.

### 3.8 Dashboard on `:3000` instead of the apex
The core owns `:80`/`:443` for tunnel routing, so the dashboard initially lived on `:3000`
(`http://natforge.com:3000`). **Fix (Option A):** the core's `:80` router now forwards the
bare apex and `www` to the dashboard (`DASHBOARD_ADDR`, default `127.0.0.1:3000`), so
`http://natforge.com` serves it while `sub.natforge.com` stays a tunnel.

### 3.9 Building on a fresh VM
The release build needs a C toolchain + OpenSSL headers (`build-essential pkg-config
libssl-dev`) because `reqwest`'s default TLS pulls `native-tls`. First release build was
~7.5 min on 2 cores; incremental rebuilds ~3 min. 7.7 GB RAM was ample (no swap needed).

### 3.10 Admin assignment is manual (database only)
There is no auto-admin: every registration gets the `user` role. Admin is granted
explicitly in the database, which avoids any "first to register wins admin" land-grab
on a public URL and keeps the sign-up path simple:
```sh
docker exec natforge-postgres psql -U natforge -d natforge \
  -c "UPDATE users SET role='admin' WHERE email='you@example.com';"
```

Note: the pin only sets roles at *registration*, pre-existing rows must be
reconciled by hand (one `UPDATE`), which is why `admin@example.com` (registered
before the pin) was a `user` until promoted.

### 3.11 Website crash-looped after a VM reboot, datastores didn't auto-start ⚠️ root-caused
After the VM was stopped and started again, `natforge-backend` was flapping: the
journal showed `failed to connect to PostgreSQL … pool timed out` and systemd
restarting it every few seconds. The app was fine, **`docker ps` was empty**. The
`docker-compose.yml` had no `restart:` policy, so the Postgres/Redis containers did
**not** come back after the host reboot, and the control plane (which connects to
both at boot) had nothing to talk to. **Fix:** `docker compose up -d` to bring them
back, then restart the website (migrations applied, bound `:3000`). **Hardening so it
self-heals next time:** added `restart: unless-stopped` to both services in
`docker-compose.yml` and re-ran `docker compose up -d` to recreate them with the
policy (confirmed via `docker inspect … RestartPolicy`). The systemd units already
restart-on-failure, so once the datastores auto-start, the website reconnects on its
own. (Order matters at boot, the website tolerates a brief DB-not-ready window by
crash-restarting until Postgres accepts connections.)

---

## 4. Outstanding / optional

- **HTTPS padlock for the dashboard:** set the *apex* record to Cloudflare-proxied (orange)
 + SSL mode **Flexible** (origin is HTTP on `:80`). Keep the wildcard grey. (Tunnels are
 unaffected.) Not done yet, `http://natforge.com` works today.
- **Geo-blocking enforcement:** set `GEOIP_DB` to a MaxMind `GeoLite2-Country.mmdb` on the
 website and each node. Without it, country resolution is "unknown" and blocking is a no-op.
- **Hardening:** `docker-compose.yml` publishes Postgres/Redis on `0.0.0.0:5432/6379`. They
 are protected by the NSG (not in the allow-list), but binding them to `127.0.0.1` is better
 defense-in-depth.
- **Second region (test node):** deploy another `natforge-node` on a separate VM with a
 distinct `NODE_ID`/`PUBLIC_HOST` (e.g. `bg.natforge.com`) pointed at `WEBSITE_URL=https://natforge.com`;
 it self-registers and appears in the region dropdown.

---

## 5. Redeploying after a code change

The same `rsync` + rebuild loop ships any later change. The profiles / moderation /
stop-vs-delete feature set (migration `0008`) was deployed this way:

```bash
# from the local repo root, push the working tree to the VM
rsync -az --delete \
 --exclude target --exclude .git --exclude '*.env' \
 ./ azureuser@<VM_IP>:~/natforge/

# on the VM: rebuild the control plane (frontend is static, rsync alone updates it)
ssh azureuser@<VM_IP>
cd ~/natforge && source ~/.cargo/env
cargo build --release -p natforge-backend
sudo systemctl restart natforge-backend
```

- **DB migrations are automatic.** `natforge-backend` runs `sqlx::migrate!("./migrations")`
 at boot, so restarting the service applies any new append-only migration (here `0008`,
 which adds `users.name`/`users.banned`/`tunnels.name` and the `'stopped'` status) against
 the live Postgres. No manual `psql` step is needed; the migration is idempotent.
- **Frontend needs no rebuild.** It is served from disk with `Cache-Control: no-cache`, so the
 new `profile.html`, admin moderation controls, and the Stop/Delete split are live the moment
 `rsync` lands them.
- **Only rebuild the core** (`cargo build --release -p natforge-node` + `systemctl restart
 natforge-node`) when a change touches the data plane; the profiles feature did not.
- **The `natforge` agent runs on users' machines, not the VM.** The tunnel-edit feature changed
 the agent (it now re-reserves on every reconnect, so an admin/owner subdomain change re-routes a
 live tunnel within ~3s). The VM redeploy is still just website + static frontend; users pick up
 the new behaviour by rebuilding/redownloading the agent. The control plane edit endpoint and the
 Stop/Delete split work regardless of agent version, only the *live re-route* needs the new agent.

## 6. Container deployment (CD)

The `rsync` + build-on-VM loop of §5 is superseded by the containerised CD pipeline
(full reference: `docs/cd.md`). In short: CI builds the
`website` + `core` Docker images, pushes them to `ghcr.io`, Trivy-scans them, and
deploys them to the VM as `docker compose -f docker-compose.deploy.yml pull && up -d`
(the core runs with host networking + `NET_BIND_SERVICE`, replacing the systemd unit;
secrets stay in `/etc/natforge/*.env`). No more `cargo build` on the VM.

Key operator steps (once):
1. Repo secrets `DEPLOY_HOST` and `DEPLOY_SSH_KEY` (reuse `~/.ssh/natforge_azure`).
2. On the VM: `docker login ghcr.io` with a `read:packages` token so it can pull the
   private images.

Then deploy from Actions -> CD -> "Run workflow" (or automatically on merge to `main`).
Roll back with `NATFORGE_TAG=<previous-sha> docker compose -f docker-compose.deploy.yml up -d`.

**Staged cutover from systemd:** stop (do not remove) `natforge-backend`/`natforge-node`,
bring the container stack up, verify the dashboard + a tunnel; if anything misbehaves,
`docker compose ... down` and `systemctl start` the old units. Remove the units only
once the container path is proven over a few deploys.
