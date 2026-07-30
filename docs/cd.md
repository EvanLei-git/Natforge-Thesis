# Continuous Deployment

CD builds the server as Docker images, pushes them to ghcr.io, scans them, and
deploys them to the VM. It also publishes the client agent as downloadable binaries.

## Workflows

- **`.github/workflows/cd.yml`**
  - Builds `website` + `core` images (multi-stage; the frontend is baked into the
    website image). On a **pull request** it only builds + scans; on a **push to
    `main`** it also pushes and deploys.
  - **Trivy** scans each image and fails on fixable HIGH/CRITICAL CVEs, reading
    `.trivyignore` (triaged, accountable, like `.cargo/audit.toml`).
  - **Deploy** job: a reachability gate (fails loudly if the VM is down), then
    `docker compose -f docker-compose.deploy.yml pull && up -d` over SSH, then a
    post-deploy health check.
  - Trigger it **manually** any time from Actions -> CD -> "Run workflow" (optionally
    pinning an image tag), useful right after starting the VM.
- **`.github/workflows/agent-release.yml`**, builds the `natforge` agent for
  `x86_64-linux-gnu` and static `x86_64-linux-musl` (via `cross` + `Cross.toml`) and
  publishes both to a rolling **`latest`** GitHub pre-release.

## Images

`ghcr.io/evanlei-git/natforge-website` and `ghcr.io/evanlei-git/natforge-core`,
tagged with the commit SHA and `latest`. A deploy pins a SHA, so rollback is just
re-pinning the previous one.

Each image also carries OCI labels (`title`, `description`, `source`) that render on
its ghcr page, and BuildKit attaches a SLSA provenance attestation (the
`unknown/unknown` manifest is that signed build record, not a runnable image).
Whether the VM needs a ghcr login (one-time setup step 2) depends on package
visibility: private images require it, public images need no credential anywhere.

## One-time setup (operator)

1. **Repo secrets** (Settings -> Secrets and variables -> Actions):
   - `DEPLOY_HOST`, a grey (DNS-only) name or IP that reaches the origin VM directly
     (e.g. `deploy.natforge.com` via the wildcard, or the raw IP), **not** the
     Cloudflare-proxied apex `natforge.com`. See [HTTPS](https.md).
   - `DEPLOY_SSH_KEY`, the private SSH key for `azureuser` (reuse `~/.ssh/natforge_azure`).
2. **On the VM**, log Docker in to ghcr so it can pull the private images:
   ```sh
   echo <GHCR_READ_TOKEN> | docker login ghcr.io -u EvanLei-git --password-stdin
   ```
   (`GHCR_READ_TOKEN` = a classic PAT with `read:packages`, or a fine-grained token
   with package read.) The compose files are copied to `~/natforge` by the deploy.
3. **Env + data on the VM** stay where they are: `/etc/natforge/website.env`,
   `/etc/natforge/core.env`, and (optional) the GeoIP db under `/etc/natforge/`.

## Deploying manually

```sh
# from your machine (needs gh + the deploy secrets set):
gh workflow run cd.yml                    # deploys latest main build
# or on the VM directly:
NATFORGE_TAG=<sha|latest> docker compose -f docker-compose.deploy.yml pull
NATFORGE_TAG=<sha|latest> docker compose -f docker-compose.deploy.yml up -d
```

## Rollback

```sh
NATFORGE_TAG=<previous-sha> docker compose -f docker-compose.deploy.yml up -d
```

## Cutover from systemd (automated in the deploy)

The container stack replaces the native `systemd` services. The deploy job performs the
handoff automatically: before `docker compose up -d` it runs
`sudo systemctl disable --now natforge-website natforge-core` (stop + disable, so the
containers can bind the shared ports and a reboot does not race the units against the
containers' `restart: unless-stopped`). The unit files stay installed, so **rollback** is
immediate:

```sh
docker compose -f docker-compose.deploy.yml down
sudo systemctl start natforge-website natforge-core
```

Prove the stack locally first (the smoke test in thesis §5.6.1). Once the container path is
proven over a few deploys, the systemd unit files can be removed.

## Installing the agent (clients)

```sh
# static build, runs on any x86_64 Linux distro:
curl -L https://github.com/EvanLei-git/Thesis-reverse-proxy/releases/latest/download/natforge-x86_64-linux-musl -o natforge
chmod +x natforge
./natforge service-host --route 8080:http --control-plane http://natforge.com:3000
```
