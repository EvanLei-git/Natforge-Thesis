-- Marks a tunnel that the failover sweep relocated off a node that went down,
-- storing the human region label it was moved FROM. The dashboard shows a badge so
-- the user understands their public address changed because that region is down.
-- Cleared on any (re)placement of the tunnel (see migrate_tunnel), so a subsequent
-- manual move or a fresh reservation removes the notice.
ALTER TABLE tunnels ADD COLUMN IF NOT EXISTS failover_from TEXT;
