# syntax=docker/dockerfile:1
# Multi-stage build for the NatForge data plane (core_proxy_backend).
# Build from the repo root:  docker build -f core_proxy_backend/Dockerfile -t natforge-core .
# Run with host networking so :80/:443/:4000/:3001 and the 20000-20100 TCP pool
# bind on the host exactly as the native service did (see docker-compose.yml).

FROM rust:1.96-slim-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake perl pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/src/target,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo build --release -p core_proxy_backend \
    && cp target/release/core_proxy_backend /core_proxy_backend

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
LABEL org.opencontainers.image.title="natforge-core" \
      org.opencontainers.image.description="NatForge data plane. Terminates public HTTP, HTTPS and raw TCP traffic and relays it over multiplexed yamux tunnels to agents running behind NAT or CGNAT, making services on private networks reachable from the internet." \
      org.opencontainers.image.source="https://github.com/EvanLei-git/Thesis-reverse-proxy"
COPY --from=builder /core_proxy_backend /usr/local/bin/core_proxy_backend
ENTRYPOINT ["/usr/local/bin/core_proxy_backend"]
