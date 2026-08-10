-- Opt-in, per-route SRV provisioning. A route may carry an optional SRV service label
-- (e.g. 'minecraft', 'mindustry', 'ts3'); when set, the data plane provisions a
-- `_<service>._<proto>.<subdomain>` DNS record (proto from the route's transport) so
-- SRV-aware game clients can connect with just the hostname. Empty = no record (default),
-- which replaces the previous behaviour of stamping `_minecraft._tcp` on every tcp route.
ALTER TABLE routes ADD COLUMN IF NOT EXISTS srv_service TEXT;
