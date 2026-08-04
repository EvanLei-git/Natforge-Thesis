-- Bound the lifetime of a dashboard-issued pairing code: a device that is created but
-- never connected must not keep a redeemable code forever. The code itself is
-- high-entropy (unguessable); this caps the exposure window if one ever leaks.
ALTER TABLE devices ADD COLUMN IF NOT EXISTS claim_expires_at TIMESTAMPTZ;
-- Give any code already issued (dashboard-first shipped moments ago) a bounded window.
UPDATE devices SET claim_expires_at = now() + interval '7 days'
    WHERE claim_code IS NOT NULL AND claim_expires_at IS NULL;
