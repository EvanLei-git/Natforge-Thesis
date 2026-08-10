-- Allow `both` routes: one dedicated pooled port exposed over TCP *and* UDP at once
-- (for game servers that use the same port for both). It is stored as a single `both`
-- route and expanded into a tcp claim + a udp claim (sharing the one public port) when
-- the control plane builds the agent's reservation, so no other schema change is needed.
ALTER TABLE routes DROP CONSTRAINT IF EXISTS routes_kind_check;
ALTER TABLE routes ADD  CONSTRAINT routes_kind_check
    CHECK (kind IN ('http','https','tcp','udp','both'));
