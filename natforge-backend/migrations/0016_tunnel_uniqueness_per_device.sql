-- Scope tunnel route-set uniqueness to the device, so the same local ports can be
-- exposed from two DIFFERENT devices (each machine has its own local port space). The
-- original `(owner_id, route_sig)` constraint wrongly blocked a second device that
-- reused, say, tcp:8080. Device-less CLI tunnels (device_id IS NULL) keep their old
-- per-owner uniqueness via COALESCE(device_id, 0), preserving reconnection reuse.
ALTER TABLE tunnels DROP CONSTRAINT IF EXISTS tunnels_owner_route_uq;
CREATE UNIQUE INDEX IF NOT EXISTS tunnels_owner_device_route_uq
    ON tunnels (owner_id, COALESCE(device_id, 0), route_sig);
