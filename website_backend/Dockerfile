# syntax=docker/dockerfile:1
# Multi-stage build for the NatForge control plane (website_backend).
# Build from the repo root:  docker build -f website_backend/Dockerfile -t natforge-website .

FROM rust:1.96-slim-bookworm AS builder
# aws-lc-sys (via rustls) needs cmake + perl + a C toolchain.
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake perl pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# Cache the cargo registry + target dir across builds (and across the two images)
# so the dependency compile happens once. Copy the binary out of the cache mount.
RUN --mount=type=cache,target=/src/target,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo build --release -p website_backend \
    && cp target/release/website_backend /website_backend

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
LABEL org.opencontainers.image.title="natforge-website" \
      org.opencontainers.image.description="NatForge control plane. Serves the web dashboard and REST API, handles user authentication and RFC 8628 device-code login, and tracks users, tunnels and agents in Postgres and Redis. The static frontend is baked into the image." \
      org.opencontainers.image.source="https://github.com/EvanLei-git/Thesis-reverse-proxy"
COPY --from=builder /website_backend /usr/local/bin/website_backend
# The frontend is served from disk by ServeDir; bake it into the image.
COPY --from=builder /src/frontend /app/frontend
ENV FRONTEND_DIR=/app/frontend
EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/website_backend"]
