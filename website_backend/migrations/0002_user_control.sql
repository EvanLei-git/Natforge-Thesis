-- More user control + richer admin visibility.
-- Optional per-route label (e.g. "GTA server", "web") and the agent's source IP
-- (the user's machine address, captured by the core on the control connection).

ALTER TABLE routes  ADD COLUMN IF NOT EXISTS label    TEXT;
ALTER TABLE tunnels ADD COLUMN IF NOT EXISTS agent_ip TEXT;
