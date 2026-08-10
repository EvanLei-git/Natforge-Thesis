-- Remove the dashboard-issued pairing-code columns (0013/0014). The connect flow is now
-- agent-first only: the machine prints a code the user enters in the dashboard, so no
-- server-generated claim code is stored.
ALTER TABLE devices DROP COLUMN IF EXISTS claim_code;
ALTER TABLE devices DROP COLUMN IF EXISTS claim_expires_at;
