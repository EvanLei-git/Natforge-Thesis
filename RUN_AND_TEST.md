# Run & Test NatForge locally

What to run, in what order, and **why**, to bring the whole platform up on your machine and prove it works. For production hosting see `Hosting.md`.

---

## Prerequisites

- **Rust** ≥ 1.85, `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && source "$HOME/.cargo/env"`
- **Docker + Docker Compose** (for PostgreSQL + Redis)
- `jq`: `curl`, `python3`, `openssl`, `nc` (used by the test script)

Always run commands **from the repo root**, `FRONTEND_DIR` and the migrations path are relative to it.

---

## A. One-command test (recommended first)

```bash
bash scripts/e2e.sh
```

**Why:** this is the fastest proof the whole build works. It starts the datastores and both planes, spins up three stand-in origin services (an HTTP server, a TLS server, and a raw-TCP responder), reserves a **3-route tunnel**, runs the agent, and asserts every core claim, printing `PASS`/`FAIL`:

| Check | What it proves |
|---|---|
| HTTP via subdomain (`:8080`) | a friend reaches your service by `sub.natforge.com` (Host routing) |
| unknown subdomain → 404 | routing rejects unknown hosts |
| HTTPS via SNI passthrough (`:8443`) | TLS reaches the origin's own cert, the relay never decrypts |
| raw TCP via dedicated port | hostname-less protocols (Minecraft) work on their own port |
| two users share `:8080` | unlimited users multiplex one port by subdomain |
| multi-route over one session | http + tcp run over a single yamux connection |
| same subdomain+port after restart | PostgreSQL persistence + idempotent reservation + auto-reconnect |

Expected last line: `### RESULT: 27 passed, 0 failed` (the suite has grown to cover profiles, moderation, tunnel edit + live re-route, and the device flow).

```bash
cargo test # also run the 10 unit tests (preamble codec, JWT claims, SNI/Host parsers)
```

> **In CI:** this exact `scripts/e2e.sh` run, the unit tests, `fmt`/`clippy`, a release build, and the
> security scanners (cargo-audit, gitleaks, CodeQL) run automatically on every push and pull request
> via GitHub Actions, see `docs/ci.md`. Locally you need Docker running; CI provides it on `ubuntu-latest`.

---

## B. Run it by hand (to actually click around the dashboard)

Open four terminals **at the repo root**.

**1, datastores.** *Why: durable state (Postgres) + ephemeral state / device codes (Redis).*
```bash
docker compose up -d
```

**2, build once.** *Why: compile all four Rust crates.*
```bash
cargo build
```

**3, control plane (dashboard + API on :3000).** *Why: auth, reservation, admin, serves the website.*
```bash
./target/debug/website_backend
```

**4, data plane.** *Why: the actual relay, agent control `:4000`, shared HTTP `:8080`, shared HTTPS/SNI `:8443`, TCP pool `20000–20100`.*
```bash
PUBLIC_HOST=natforge.com HTTP_PORT=8080 HTTPS_PORT=8443 ./target/debug/core_proxy_backend
```

**5, a service to expose + the agent.** *Why: stand-in for your real app, then the agent that tunnels it.*
```bash
python3 -m http.server 8000 # something to expose
./target/debug/natforge service-host --route 8000:http # interactive device-flow login
# or skip the prompt: --email you@x.com --password yourpass
# or a raw game port: --route 25565:tcp
```

Now open **http://127.0.0.1:3000** → **Create account** (the first account is admin) → **Request tunnel** (build routes, copy the command) → the agent prints the live endpoints.

**A "friend" connects** (no real DNS needed locally, fake the hostname):
```bash
# HTTP route, by subdomain, on the shared port:
curl -H "Host: <subdomain>.natforge.com" http://127.0.0.1:8080/
# raw TCP route, on its dedicated port:
nc 127.0.0.1 <public-tcp-port>
# HTTPS route (SNI), if you exposed one:
curl -k --resolve "<subdomain>.natforge.com:8443:127.0.0.1" https://<subdomain>.natforge.com:8443/
```

---

## C. Troubleshooting

**"The login still shows emojis / the dashboard has no CSS."**
This is your **browser cache** showing the *old* page/stylesheet, the files on disk are the redesigned, emoji-free ones. Fix:
- **Hard refresh:** `Ctrl+Shift+R` (or open a private window).
- The server now sends `Cache-Control: no-cache`, so after one hard refresh it won't happen again.
- **Frontend changes need no rebuild**, `website_backend` serves `frontend/` live from disk. Only **Rust** changes require `cargo build`. (So "re-running the binary" doesn't change anything if you only need a browser refresh.)

**"Address already in use" / a page won't load.**
A previous run is still holding a port. Kill strays (the bracket avoids killing this command itself):
```bash
pkill -9 -f '[t]arget/debug/website_backend'
pkill -9 -f '[t]arget/debug/core_proxy_backend'
pkill -9 -f '[t]arget/debug/natforge'
```

**"failed to connect to PostgreSQL/Redis".** Run `docker compose up -d` and check `docker compose ps` shows both healthy.

**Dashboard redirects to login immediately.** That's the auth guard, you're not signed in (no token). Create an account / sign in first.

**Run location.** If assets 404 or the dashboard is blank, you started a backend from the wrong directory, `cd` to the repo root (or set `FRONTEND_DIR` to an absolute path).

---

## D. What's mocked vs. real (so tests don't mislead you)

Fully real and exercised by the tests above: the tunnel (HTTP/SNI/TCP), multi-route, auth + device flow, and Postgres+Redis persistence. **Region/geo blocking is managed in the UI but not enforced**; WireGuard encryption, UDP hole punching, and multi-node forwarding are not implemented. Cloudflare SRV provisioning is real but only fires with a configured `CF_API_TOKEN` (it logs locally). See `Hosting.md` §8 for the full list.
