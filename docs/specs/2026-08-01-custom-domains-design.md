# Custom domains design

Status: draft (pending approval)
Date: 2026-08-01

## Motivation

Users want their own domain (e.g. `play.mygame.com`) to front a tunnel instead of
the assigned `sub.natforge.com`, so a promo page or service keeps their brand. This
is purely DNS delegation + hostname routing + TLS; no ASN/anycast is involved. It is
already listed as future work (thesis 7.3).

## Scope and phasing

Custom domains split into two cleanly shippable phases:

- **Phase 1 (this spec): routing + registration + bring-your-own-cert HTTPS.** A
  tunnel registers a custom hostname; the core routes it by full Host/SNI; HTTP
  works, and HTTPS works via SNI passthrough (the user's origin serves its own
  cert). Fully local-testable, no new dependencies, its own PR.
- **Phase 2 (separate PR): automatic HTTPS via ACME.** For an `http` route (user
  runs plain HTTP), issue a per-domain Let's Encrypt certificate (HTTP-01) so
  `https://<customdomain>` works with zero cert work by the user, mirroring the
  `*.natforge.com` auto-HTTPS. Full validation needs a real domain pointed at the
  node, so it is deploy-verified and kept separate.

Rationale for the split: Phase 1 is fully verifiable on the local stack (like the UDP
work) and useful on its own; Phase 2's real-cert path can only be proven on the
deployed VM with a domain the user controls, so bundling it would make the whole
feature un-testable locally.

## Phase 1 design

### Registration and storage
- A tunnel gains one optional **custom domain**. Migration adds
  `tunnels.custom_domain TEXT` with a partial unique index (unique when not null),
  so two tunnels cannot claim the same hostname.
- API (owner only): `PUT /api/tunnels/{id}/custom_domain {domain}` validates the
  hostname (a real FQDN, not under `natforge.com`, not an apex we serve), lowercases
  and stores it; `DELETE` clears it. The dashboard shows the CNAME target to set
  (`edge.natforge.com`, a stable grey name) and the tunnel's routes under it.
- The custom domain is carried in the tunnel token (`TunnelClaims.custom_domain:
  Option<String>`) so the data plane learns it at handshake, and in the reservation
  response so the agent/log can show it.

### Routing (core)
- Two new registries, `custom_http` and `custom_https` (`HashMap<hostname,
  RouteHandle>`), mirroring the subdomain registries.
- `serve_http` and `serve_https`: after reading the Host / SNI, look up the **full
  hostname** in the custom map first; on a hit, route to that tunnel's http/https
  handle. On a miss, fall back to the existing `subdomain_of` (`*.natforge.com`)
  path. The apex/www dashboard special-case still takes precedence.
- Register a tunnel's custom entries at handshake (only for the modes it actually
  has: http -> `custom_http`, https -> `custom_https`), and remove them on teardown
  (same generation-guarded pattern as the subdomain registries).

### Verification (Phase 1)
- A custom domain only carries traffic once its owner points it at the node (a CNAME
  to `edge.natforge.com`, or an A record to the node IP). Because a hostname can only
  resolve to us if whoever controls its DNS sets that record, "it reaches us" is
  itself the control gate; the unique index stops collisions/squatting. This is
  documented as the Phase 1 guarantee.
- Phase 2's ACME HTTP-01 adds cryptographic proof of control; an explicit TXT
  challenge is a possible later hardening, deferred.

### TLS (Phase 1)
- **HTTP** (`:80`): routed by Host, no certificate needed. Works.
- **HTTPS** (`:443`): if the tunnel has an `https` route, SNI passthrough by the
  custom hostname (the origin presents its own cert); zero cert work for us. Works.
- An `http`-only tunnel's custom domain is **HTTP-only in Phase 1** (no cert for a
  non-natforge hostname yet); Phase 2 (ACME) fills this gap. Stated plainly so the
  limitation is honest, and it is symmetric with how the wildcard cert was added to
  `*.natforge.com` after the fact.

### Testing (Phase 1)
- `scripts/e2e.sh`: reserve a tunnel with http + https routes, `PUT` a custom domain,
  then assert (a) `http://<custom>/` routes by Host header to the origin, and (b)
  `https://<custom>/` passes through to the origin's own cert (SNI, `--resolve` to the
  node). Adds two assertions.
- Unit: hostname validation (reject apex, reject `*.natforge.com`, reject malformed).

## Phase 2 design (outline, separate PR)

- Add `instant-acme` (async ACME) with a persisted account key.
- On a custom domain for an `http` route: place an LE order and answer **HTTP-01** at
  `/.well-known/acme-challenge/<token>` intercepted in the `:80` router; store the
  issued cert + key per domain.
- `:443` terminates TLS for custom `http` domains via a rustls `ResolvesServerCert`
  that picks the per-domain cert by SNI (generalizing today's single wildcard
  acceptor into a resolver keyed by hostname, with the wildcard as the default).
- Renewal on a timer (re-order before expiry), hot-loaded like the wildcard cert.
- Verified on the VM with a real domain, as the wildcard cert was.

## Thesis impact

New implementation subsection (custom-domain routing) once Phase 1 lands, with the
honest Phase-1 HTTP-only-for-http-routes note; "Custom domains" moves out of 7.3
future work when Phase 2 (ACME) completes. Functional-test rows for the new
assertions.

## Rollout

Additive: a tunnel with no custom domain behaves exactly as today. The migration only
adds a nullable column. No change to existing reservations.
