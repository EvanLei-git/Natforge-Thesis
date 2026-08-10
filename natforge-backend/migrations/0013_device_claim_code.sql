-- Dashboard-first enrollment: a device can be created from the browser (name only) and
-- connected later. It holds a one-time `claim_code` the agent redeems (`natforge enroll
-- --code`) to receive its token; redeeming clears the code, so it is single-use.
ALTER TABLE devices ADD COLUMN IF NOT EXISTS claim_code TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS devices_claim_code_uq
    ON devices(claim_code) WHERE claim_code IS NOT NULL;
