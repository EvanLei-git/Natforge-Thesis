-- Reserve infrastructure and region subdomain labels so a user cannot claim
-- them as a tunnel subdomain. Without this, a name like `swiss` (a region host /
-- agent control endpoint), `grafana`/`deploy`/`control` (infra origins), or a
-- future region label could be reserved by a user and collide with platform
-- naming. Idempotent and safe to re-run (extends the seed in 0001_init.sql).
INSERT INTO reserved_subdomains(name) VALUES
    ('swiss'),
    ('us'),
    ('eu'),
    ('asia'),
    ('grafana'),
    ('control'),
    ('deploy'),
    ('head')
ON CONFLICT DO NOTHING;
