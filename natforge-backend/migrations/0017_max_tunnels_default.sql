-- Raise the per-user service-host limit default to 10 and wire it up (the code used to
-- ignore this column and hardcode 2). Bump existing accounts still sitting on the old
-- default of 2 so they get the new headroom; the admin can set a higher value per user.
ALTER TABLE users ALTER COLUMN max_tunnels SET DEFAULT 10;
UPDATE users SET max_tunnels = 10 WHERE max_tunnels = 2;
