# CI & required checks

Continuous integration runs on every push to `main` and every pull request.

## Workflows

- **`.github/workflows/ci.yml`**
 - `lint`: `cargo fmt --all --check` + `cargo clippy --workspace --all-targets -- -D warnings`
 - `unit`: `cargo test --workspace`
 - `build`: `cargo build --release --workspace`
 - `e2e`: `scripts/e2e.sh` (27 assertions; brings up Postgres+Redis via `docker compose`,
 runs both planes + the agent, tests HTTP/SNI/TCP routing, restart survival, profiles,
 moderation, tunnel edit + live re-route, and the device flow). Uploads `/tmp/nf_*.log`
 as an artifact on failure. **No secrets required**: the apps use matching dev defaults
 when `JWT_SECRET`/`INTERNAL_SECRET` are unset.
- **`.github/workflows/security.yml`**
 - `cargo-audit`: installs cargo-audit (prebuilt via `taiki-e/install-action`) and runs
 `cargo audit`, which reads **`.cargo/audit.toml`** (the triaged ignore-list; every real,
 applicable advisory is fixed by keeping deps current, so only non-applicable advisories
 are ignored, each with a written justification).
 - `gitleaks`: secret scanning over the full history.
 - Also runs weekly (Monday cron) to catch newly-published advisories without a push.
- **CodeQL SAST is not enabled.** Code scanning on a *private* repository requires GitHub
 Advanced Security, which this repo's tier does not include (the SARIF upload is rejected
 with "Code scanning is not enabled for this repository"), so the CodeQL workflow was
 removed. It can be turned on later by making the repository public (free code scanning) or
 adding Advanced Security.
- **`.github/dependabot.yml`**, weekly update PRs for **cargo** and **github-actions**
 (minor/patch grouped). Each PR re-runs the CI above.

## Enable required checks (one-time, GitHub Settings)

Settings → Branches → Add branch protection rule for `main`:

- Require a pull request before merging.
- Require status checks to pass, selecting: **lint**, **unit**, **build**, **e2e**,
 **cargo-audit**, **gitleaks**.
- Leave **Dependabot** advisory (not required).

## Updating the audit ignore-list

When a new advisory appears:

1. Prefer a fix: `cargo update` (or bump the owning dependency) and re-run `cargo audit`.
2. Only if it is genuinely non-applicable to NatForge's usage, add its `RUSTSEC-…` id to
 `.cargo/audit.toml` with a one-line justification. Never ignore a real, fixable,
 applicable advisory.
