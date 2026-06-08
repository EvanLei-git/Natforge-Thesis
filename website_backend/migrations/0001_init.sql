-- NatForge initial schema (PostgreSQL 16).
-- Applied automatically at boot via sqlx::migrate!("./migrations").
-- TEXT + CHECK is used instead of native ENUM types to keep runtime sqlx
-- (query/query_as, no compile-time macros) free of custom type derives.

-- ----------------------------------------------------------------- users
-- id stays INT to match the JWT `sub: i32` claim used across the platform.
CREATE TABLE IF NOT EXISTS users (
    id            INT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    email         TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,                          -- argon2id PHC string
    role          TEXT NOT NULL DEFAULT 'user' CHECK (role IN ('user','admin')),
    max_tunnels   INT  NOT NULL DEFAULT 2,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- --------------------------------------------------------------- tunnels
-- subdomain is the live routing key (UNIQUE); id is the durable PK embedded in
-- the tunnel token so reconnects keep the same subdomain. route_sig makes
-- reservation idempotent per (owner, route shape) so a reconnecting agent reuses
-- the same subdomain + ports instead of leaking the pool.
CREATE TABLE IF NOT EXISTS tunnels (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    subdomain   TEXT NOT NULL,
    owner_id    INT  NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    route_sig   TEXT NOT NULL,                            -- e.g. "http:8000,tcp:25565"
    status      TEXT NOT NULL DEFAULT 'awaiting_agent'
                  CHECK (status IN ('awaiting_agent','online','offline')),
    public_host TEXT NOT NULL,
    node_id     TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen   TIMESTAMPTZ,
    CONSTRAINT tunnels_subdomain_uq  UNIQUE (subdomain),
    CONSTRAINT tunnels_owner_route_uq UNIQUE (owner_id, route_sig)
);
CREATE INDEX IF NOT EXISTS tunnels_owner_idx ON tunnels(owner_id);

-- ---------------------------------------------------------------- routes
-- http/https => public_port NULL (shared :80/:443, matched by subdomain).
-- tcp        => public_port set (dedicated pooled port, matched by port).
CREATE TABLE IF NOT EXISTS routes (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tunnel_id   BIGINT   NOT NULL REFERENCES tunnels(id) ON DELETE CASCADE,
    route_id    SMALLINT NOT NULL,                        -- dense id, unique within tunnel
    kind        TEXT     NOT NULL CHECK (kind IN ('http','https','tcp')),
    local_port  INT      NOT NULL,
    public_port INT,
    UNIQUE (tunnel_id, route_id)
);
CREATE INDEX IF NOT EXISTS routes_tunnel_idx ON routes(tunnel_id);
-- At most one http (and one https) route per subdomain (Host/SNI can't disambiguate).
CREATE UNIQUE INDEX IF NOT EXISTS routes_one_host_kind
    ON routes(tunnel_id, kind) WHERE kind IN ('http','https');

-- ------------------------------------------------------------- port pool
-- Allocator of record for dedicated TCP ports. Seeded per node at website boot.
CREATE TABLE IF NOT EXISTS port_pool (
    node_id   TEXT   NOT NULL,
    port      INT    NOT NULL,
    tunnel_id BIGINT REFERENCES tunnels(id) ON DELETE SET NULL,  -- NULL = free
    route_id  SMALLINT,
    PRIMARY KEY (node_id, port)
);
CREATE INDEX IF NOT EXISTS port_pool_free_idx ON port_pool(node_id) WHERE tunnel_id IS NULL;

-- --------------------------------------------------------- bandwidth logs
-- Periodic cumulative snapshot rows; the dashboard reads the latest per tunnel.
CREATE TABLE IF NOT EXISTS bandwidth_logs (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tunnel_id   BIGINT NOT NULL REFERENCES tunnels(id) ON DELETE CASCADE,
    owner_id    INT    NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    bytes_in    BIGINT NOT NULL DEFAULT 0,
    bytes_out   BIGINT NOT NULL DEFAULT 0,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS bw_tunnel_time_idx ON bandwidth_logs(tunnel_id, recorded_at DESC);

-- ------------------------------------------------------------- ip hosts
CREATE TABLE IF NOT EXISTS ip_hosts (
    user_id            INT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    active             BOOLEAN NOT NULL DEFAULT false,
    max_bandwidth_mbps INT     NOT NULL DEFAULT 100,
    geo_pref_only      BOOLEAN NOT NULL DEFAULT false,
    bytes_relayed      BIGINT  NOT NULL DEFAULT 0,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- --------------------------------------------------------- admin policy
CREATE TABLE IF NOT EXISTS region_blocks (
    country_code CHAR(2) PRIMARY KEY,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS port_blocks (
    port       INT PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- --------------------------------------------- reserved subdomains (anti-shadow)
CREATE TABLE IF NOT EXISTS reserved_subdomains ( name TEXT PRIMARY KEY );

-- --------------------------------------------------------- idempotent seeds
INSERT INTO port_blocks(port) VALUES (25),(465),(587) ON CONFLICT DO NOTHING;
INSERT INTO region_blocks(country_code) VALUES ('RU') ON CONFLICT DO NOTHING;
INSERT INTO reserved_subdomains(name)
  VALUES ('www'),('api'),('admin'),('device'),('app'),('edge'),('mail') ON CONFLICT DO NOTHING;
