-- The control connection (agent ↔ core) is now real TLS. Each node advertises the
-- SHA-256 fingerprint of its self-signed control certificate; agents pin it.
ALTER TABLE nodes ADD COLUMN IF NOT EXISTS control_cert_fp TEXT;
