-- A device is a persistent, enrolled agent (one per machine). It owns one or more
-- services (tunnels). A device is enrolled via the RFC 8628 device flow; the agent
-- then holds a long-lived device token carrying a nonce that must match `token_fp`
-- here, so the token is revoked by deleting the device (which clears the nonce).
CREATE TABLE IF NOT EXISTS devices (
    id         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    owner_id   INT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    token_fp   TEXT,                         -- nonce of the issued device token; NULL revokes it
    status     TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','online','offline')),
    agent_ip   TEXT,
    last_seen  TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS devices_owner_idx ON devices(owner_id);

-- A tunnel may belong to a device; then it is a "service" of that device.
ALTER TABLE tunnels ADD COLUMN IF NOT EXISTS device_id BIGINT REFERENCES devices(id) ON DELETE CASCADE;
