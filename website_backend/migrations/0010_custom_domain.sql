-- A tunnel may front an optional custom hostname (e.g. play.mygame.com) in
-- addition to its <subdomain>.natforge.com address. Unique when set, so two
-- tunnels cannot claim the same hostname.
ALTER TABLE tunnels ADD COLUMN IF NOT EXISTS custom_domain TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS tunnels_custom_domain_uq
    ON tunnels(custom_domain) WHERE custom_domain IS NOT NULL;
