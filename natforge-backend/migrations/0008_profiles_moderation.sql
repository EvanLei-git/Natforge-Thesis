-- User profiles + moderation, tunnel display names, and an explicit 'stopped' state.
ALTER TABLE users   ADD COLUMN IF NOT EXISTS name   TEXT;
ALTER TABLE users   ADD COLUMN IF NOT EXISTS banned BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE tunnels ADD COLUMN IF NOT EXISTS name   TEXT;

-- Allow 'stopped': a user/admin-stopped tunnel that is KEPT (same subdomain +
-- ports, restartable by re-running the agent) and exempt from the abandoned-tunnel
-- reconciliation sweep. The inline column CHECK from 0001 is named
-- `tunnels_status_check` by PostgreSQL; replace it.
ALTER TABLE tunnels DROP CONSTRAINT IF EXISTS tunnels_status_check;
ALTER TABLE tunnels ADD  CONSTRAINT tunnels_status_check
    CHECK (status IN ('awaiting_agent','online','offline','stopped'));
