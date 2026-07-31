-- Allow udp routes. The inline column CHECK from 0001 is named
-- `routes_kind_check` by PostgreSQL; replace it to include 'udp'. A udp route
-- behaves like tcp (dedicated pooled port), so no other schema change is needed.
ALTER TABLE routes DROP CONSTRAINT IF EXISTS routes_kind_check;
ALTER TABLE routes ADD  CONSTRAINT routes_kind_check
    CHECK (kind IN ('http','https','tcp','udp'));
