# Public HTTPS (Cloudflare Tunnel, apex-only)

The dashboard is served over HTTPS by a Cloudflare Tunnel scoped to the apex.
User tunnel subdomains are never proxied.

## What is set up

- `cloudflared` runs as a systemd service on the VM (`cloudflared.service`),
  tunnel name `natforge-apex`, config at `/etc/cloudflared/config.yml`.
- Ingress: `natforge.com` and `www.natforge.com` to `http://localhost:3000`
  (the dashboard); anything else returns HTTP 404.
- Cloudflare serves an auto-renewing Let's Encrypt certificate at the edge, so
  there is no certificate to install or renew on the host.

## DNS rules (do not break these)

| Record | Setting | Why |
|---|---|---|
| `natforge.com` | CNAME to the tunnel, **Proxied** | apex HTTPS |
| `www.natforge.com` | CNAME to the tunnel, **Proxied** | www HTTPS |
| `*.natforge.com` | A to the origin IP, **DNS only (grey)** | user tunnels: SNI passthrough and the raw TCP pool must reach the origin directly |

Never proxy `*.natforge.com`. Proxying it makes Cloudflare terminate TLS, which
breaks the data plane's `:443` SNI passthrough and cannot carry the `20000-20100`
TCP pool, so it breaks every user tunnel.

## Add a subdomain (e.g. Grafana)

On the VM, add an ingress rule above the `404` in `/etc/cloudflared/config.yml`:

```yaml
  - hostname: grafana.natforge.com
    service: http://localhost:3030
```

then create the route and reload:

```sh
cloudflared tunnel route dns natforge-apex grafana.natforge.com
sudo systemctl restart cloudflared
```

## User-subdomain HTTPS (auto-HTTPS for http routes)

`http`-mode user routes are served over HTTPS too, once the node holds a
`*.natforge.com` wildcard certificate. The core's `:443` router terminates TLS with
that certificate for any subdomain that has an `http` route but no `https` route, and
forwards plain HTTP to the agent, so a user's plain-HTTP service is reachable at
`https://<sub>.natforge.com` with a valid padlock. `https` routes still pass through
untouched (bring-your-own-cert); `tcp` is unaffected.

To enable it on the VM:

1. Issue the wildcard with certbot (DNS-01 via Cloudflare, needs a scoped API token):
   ```sh
   sudo certbot certonly --dns-cloudflare \
     --dns-cloudflare-credentials /etc/letsencrypt/cf.ini -d '*.natforge.com'
   ```
2. Point the core at it (in the core's env / `/etc/natforge/core.env`) and restart:
   ```sh
   WILDCARD_CERT_PATH=/etc/letsencrypt/live/natforge.com/fullchain.pem
   WILDCARD_KEY_PATH=/etc/letsencrypt/live/natforge.com/privkey.pem
   ```

Without these vars the feature is off and `http` routes stay HTTP-only. The core
re-reads the cert hourly, so certbot renewals apply with no restart. Keep the
`*.natforge.com` DNS record grey/DNS-only, this terminates at the origin, not Cloudflare.

## Operational note

Because the apex is proxied, `natforge.com` resolves to Cloudflare, not the VM.
Reach the origin directly (admin SSH, the CD deploy, direct `:3000` checks) via the
origin IP or any grey wildcard name (e.g. `deploy.natforge.com`), never via
`natforge.com:22`. The `DEPLOY_HOST` repo secret is set to such a grey name.

The **agent control plane** has the same constraint. The core's `CONTROL_ENDPOINT`
(in `/etc/natforge/core.env`) is what agents are told to connect to for the yamux
control channel on `:4000`, and Cloudflare does not carry `:4000`, so it must be a
grey origin name (`control.natforge.com:4000`), not the proxied apex. Agents learn
this endpoint from their tunnel reservation, so a wrong value silently breaks every
new or reconnecting tunnel.
