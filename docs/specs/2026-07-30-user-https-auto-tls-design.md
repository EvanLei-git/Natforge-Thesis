# Auto HTTPS for HTTP routes (user-subdomain TLS termination)

## Summary

Give every `http` route automatic HTTPS. The core terminates TLS for `http`-mode
subdomains using a `*.natforge.com` wildcard certificate and forwards plain HTTP to
the agent. `https` routes stay SNI passthrough (bring-your-own-cert / end-to-end);
`tcp` routes are unchanged.

## Motivation

A user who exposes a web app (e.g. a game's promo page on `localhost:8080`) via
`--route 8080:http` can reach it at `http://sub.natforge.com` today, but not over
HTTPS. The `:443` path is pure SNI passthrough, so HTTPS only works if the user's own
local service presents a valid certificate for `sub.natforge.com`, which a normal
localhost app does not have. Browsers flag plain HTTP as "Not Secure", so a user
hosting a public-facing page needs a real padlock without running their own CA.

## Current state (baseline)

`natforge-proto::RouteMode` has three modes:

- `Http`: matched by the `Host` header on the shared `:80` listener, forwarded to the agent as plain HTTP.
- `Https`: matched by TLS SNI on the shared `:443` listener, **passthrough, no termination** (the user's service terminates).
- `Tcp`: a dedicated public port from the `20000-20100` pool.

The `:443` handler (`core_proxy_backend/src/tunnel/shared.rs::serve_https`) parses the
ClientHello for SNI and splices the still-encrypted stream to the agent via
`route_and_splice(..)`, which currently takes a concrete `TcpStream`.

## Design

### Behaviour

- `http` route: served on BOTH `:80` and `:443`. On `:443` the core terminates TLS with
  the wildcard cert and forwards plain HTTP to the agent (the same forward as `:80`).
- `https` route: unchanged SNI passthrough.
- `tcp` route: unchanged.
- Precedence: if a subdomain has an `https` route it passes through; else if it has an
  `http` route and the wildcard acceptor is loaded, it terminates; else the connection closes.

The agent needs no change: for `http` routes it already receives and relays plain HTTP.

### Core changes (`core_proxy_backend`)

1. **Wildcard TLS acceptor.** Load `WILDCARD_CERT_PATH` + `WILDCARD_KEY_PATH` (PEM) into a
   `tokio_rustls::TlsAcceptor` at startup, held in `CoreState` behind an `ArcSwap` (or
   `RwLock<Option<TlsAcceptor>>`) so it can be hot-swapped. Absent or unreadable means
   `None`, i.e. the feature is disabled.
2. **`serve_https` dispatch.** After `parse_sni`, look up the subdomain: an `https_routes`
   hit passes through (current behaviour); otherwise an `http_routes` hit with the acceptor
   present calls `acceptor.accept()` and forwards the decrypted stream as HTTP; otherwise
   the connection closes.
3. **Generic forward.** Make `route_and_splice` (and its splice helper) generic over
   `S: AsyncRead + AsyncWrite + Unpin + Send` so it accepts a `TcpStream` (passthrough and
   `:80`) or a `TlsStream<TcpStream>` (terminated `:443`). Update the call sites.
4. **Hot reload.** A background task re-reads the cert files hourly (reload only on an mtime
   change) and swaps the acceptor in place, so a renewal never needs a restart and never
   drops live tunnels.

### Certificate (operations, on the VM)

- One-time: a Cloudflare API token scoped to DNS-edit for the zone.
- `certbot` with the `dns-cloudflare` plugin issues `*.natforge.com` via a DNS-01 challenge
  and installs its own renewal timer (auto-renews roughly every 60 days). The PEMs live at a
  fixed path that the core reads. No application code is involved in issuance.
- The `*.natforge.com` DNS record stays grey/DNS-only (unchanged). The certificate is used
  only for origin-side termination.

### Config

- New core env vars: `WILDCARD_CERT_PATH`, `WILDCARD_KEY_PATH`. Unset or missing means
  termination is disabled: `http` routes stay HTTP-only and everything else is unchanged, so
  the new core is safe to deploy before the certificate exists.

## Error handling

- Certificate missing at startup: log a warning and disable termination (graceful).
- A reload that reads a bad or partial file: keep the current acceptor and log the error.
- Client TLS handshake failure on a terminated subdomain: close the connection.
- SNI with no matching route: close (current behaviour).

## Testing

- Unit: the dispatch selection (a subdomain with an `http` route and an acceptor picks the
  terminate path; with an `https` route it picks passthrough).
- e2e (`scripts/e2e.sh`): generate a self-signed `*.natforge.com` certificate, point the core
  at it, then assert that an `http` tunnel answers over `https://sub...` (terminated, HTTP
  forwarded correctly), an `https` tunnel still passes through, and a `tcp` tunnel still works.
  `curl -k` skips validation of the self-signed CA in CI; the real Let's Encrypt certificate
  is validated on the VM.

## Rollout

- Ship the new core (the feature is off without the certificate). Issue the certificate on the
  VM with certbot, set the two env paths, restart the core. Prove it locally with a self-signed
  wildcard first.

## Non-goals (v1)

- No dashboard or UI changes (`http` routes gain HTTPS implicitly).
- No UDP support.
- No per-user or custom certificates (the shared wildcard covers every subdomain).

## Thesis note

This documents selective TLS termination in a reverse proxy (per-route-mode dispatch at
`:443`), a wildcard certificate obtained via ACME DNS-01, hot certificate reload, and
graceful degradation when no certificate is present. The passthrough mode is retained for
users who want end-to-end TLS with their own certificate.
