-- Remove the globally-blocked-ports feature. In a reverse-tunnel model the exposed
-- service always lands on a random public port (the pool range, 20000+), and no
-- sending mail server will deliver to anything but port 25, so an SMTP-style local
-- port block never prevented the abuse it targeted. The feature is dropped end to end.
DROP TABLE IF EXISTS port_blocks;
