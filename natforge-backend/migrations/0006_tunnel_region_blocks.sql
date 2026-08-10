-- Per-tunnel geo-blocking: a tunnel owner can refuse connections from chosen
-- countries (in addition to the platform-wide region_blocks the admin controls).
CREATE TABLE IF NOT EXISTS tunnel_region_blocks (
    tunnel_id    BIGINT  NOT NULL REFERENCES tunnels(id) ON DELETE CASCADE,
    country_code CHAR(2) NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tunnel_id, country_code)
);
CREATE INDEX IF NOT EXISTS tunnel_region_blocks_idx ON tunnel_region_blocks(tunnel_id);
