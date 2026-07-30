# Monitoring and Alerting

Two layers: an always-on **off-VM uptime watcher** (GitHub Actions) and a
**self-hosted metrics stack** (Prometheus + Grafana) on the VM.

## Uptime alerting (implemented)

`.github/workflows/uptime.yml` runs on a schedule from GitHub's infrastructure (not
the VM, so it can report that the VM itself is down). It curls the public HTTPS
endpoints `https://natforge.com/` and `https://www.natforge.com/` (served through the
Cloudflare tunnel, see [HTTPS](https.md)); if either fails, the job fails and GitHub
emails you its built-in Actions-failure notification. That is the alert, with no
extra service, account, or secret.

**Cadence and cost.** Scheduled runs consume Actions minutes (billed at a 1-minute
minimum). On a private repo (2000 free min/month, shared with CI/CD) the default is
**hourly** (~720 min/month). To detect faster: make the repo public (unlimited
Actions) and lower the cron, or use a free external monitor (e.g. UptimeRobot,
5-minute checks) which uses no Actions minutes. Point it at `https://natforge.com/`.

## Metrics dashboards (implemented, self-hosted)

`monitoring/docker-compose.monitoring.yml` runs three containers on the VM:

- **node_exporter**, host CPU/memory/disk/network/load.
- **Prometheus** (30-day retention), scrapes node_exporter (`:9100`) and the control
  plane's `/metrics` (`127.0.0.1:9101`). Binds `127.0.0.1:9090`.
- **Grafana**, binds `127.0.0.1:3030` and is published at `https://grafana.natforge.com`
  through the tunnel (never on a public port). Datasource and dashboards are provisioned
  from `monitoring/grafana/provisioning/` and `monitoring/grafana/dashboards/`.

Two provisioned dashboards:

- **NatForge VM**, host metrics (CPU, memory, disk, network, load, uptime).
- **NatForge Platform**, the application metrics below.

### Application metrics

The control plane exposes Prometheus metrics on `127.0.0.1:9101/metrics`:

| Metric | Type | Source |
|---|---|---|
| `natforge_signins_total{method}` | counter | in-process, incremented on password + device-code login |
| `natforge_active_tunnels` | gauge | Postgres (`tunnels.status='online'`) |
| `natforge_users_total` | gauge | Postgres (`users`) |
| `natforge_tcp_ports_used` | gauge | Postgres (`port_pool.tunnel_id IS NOT NULL`) |
| `natforge_tcp_ports_total` | gauge | Postgres (`port_pool` capacity, 101 for the `20000-20100` range) |

All but signins are read from Postgres (the allocator of record) at scrape time, so
they are always current. The data plane needs no instrumentation.

### Deploy / operate (on the VM)

```sh
cd ~/monitoring
cp .env.example .env
# set a strong GRAFANA_ADMIN_PASSWORD in .env (compose refuses to start otherwise)
docker compose -f docker-compose.monitoring.yml up -d
```

Grafana is reached at `https://grafana.natforge.com`. To add another dashboard, drop a
JSON model into `monitoring/grafana/dashboards/` (the provider reloads every 30s). To
expose a new UI subdomain, see the tunnel steps in [HTTPS](https.md).

## Grafana Cloud (alternative, not used here)

The same node_exporter + Prometheus data could be remote-written to a free Grafana
Cloud stack instead of self-hosting Grafana, which offloads storage and the dashboard
off the VM. NatForge self-hosts because it wanted the dashboard on its own
`grafana.natforge.com` behind the tunnel and full control of the panels.
