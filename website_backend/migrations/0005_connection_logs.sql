-- Per-tunnel connection logging. The core reports each closed public connection
-- (and each geo-blocked attempt) so the dashboard can show who connected, from
-- where, how much they transferred, and for how long.
CREATE TABLE IF NOT EXISTS connection_logs (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tunnel_id   BIGINT NOT NULL REFERENCES tunnels(id) ON DELETE CASCADE,
    owner_id    INT    NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
    route_id    SMALLINT NOT NULL,
    kind        TEXT   NOT NULL,                          -- http | https | tcp
    peer_ip     TEXT   NOT NULL,
    country     TEXT,                                     -- ISO-3166 alpha-2, if known
    bytes_in    BIGINT NOT NULL DEFAULT 0,
    bytes_out   BIGINT NOT NULL DEFAULT 0,
    duration_ms BIGINT NOT NULL DEFAULT 0,
    blocked     BOOLEAN NOT NULL DEFAULT false,           -- true = refused by a geo rule
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()        -- connection close time
);
CREATE INDEX IF NOT EXISTS conn_logs_tunnel_idx ON connection_logs(tunnel_id, id DESC);
