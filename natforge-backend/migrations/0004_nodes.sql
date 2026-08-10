-- Multi-region support: each data-plane VM ("node") self-registers here on boot.
-- A node serves its own wildcard apex (public_host, e.g. natforge.com or
-- bg.natforge.com) and a control endpoint agents connect to. Users choose which
-- node/region a tunnel lives on; the admin names/enables nodes.

CREATE TABLE IF NOT EXISTS nodes (
    node_id          TEXT PRIMARY KEY,
    name             TEXT NOT NULL,
    region           TEXT,                                  -- e.g. "Germany", "Bulgaria"
    public_host      TEXT NOT NULL,                         -- wildcard apex this node serves
    control_endpoint TEXT NOT NULL,                         -- host:port agents connect to (yamux)
    internal_url     TEXT NOT NULL,                         -- how the website reaches this node's internal API
    http_port        INT  NOT NULL DEFAULT 80,
    https_port       INT  NOT NULL DEFAULT 443,
    active           BOOLEAN NOT NULL DEFAULT true,
    last_seen        TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A tunnel belongs to a node (tunnels.node_id already exists). Index for lookups.
CREATE INDEX IF NOT EXISTS tunnels_node_idx ON tunnels(node_id);
