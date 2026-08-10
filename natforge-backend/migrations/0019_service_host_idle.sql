-- Idle-based automatic port reclamation for device service hosts.
--
-- 1) Track when a service host last carried real traffic (bytes in or out increased),
--    so a 31-day idle timer can free its dedicated public ports while keeping the
--    service-host row (its name/organization) intact.
ALTER TABLE tunnels ADD COLUMN IF NOT EXISTS last_active_at TIMESTAMPTZ;

-- 2) A reclaimed service host has zero routes, so its route_sig becomes ''. Two empty
--    service hosts on one device would otherwise collide on the per-device uniqueness
--    index; make the index ignore empty signatures so 0-port service hosts are allowed.
DROP INDEX IF EXISTS tunnels_owner_device_route_uq;
CREATE UNIQUE INDEX tunnels_owner_device_route_uq
    ON tunnels (owner_id, COALESCE(device_id, 0), route_sig)
    WHERE route_sig <> '';
