# Ledgerline ops runbook

## Deploys

Deploys are driven by CI from release tags. Nothing in this runbook replaces
the release process; it only covers day-to-day operations.

## Monthly export

`scripts/export.py` writes the monthly invoice report to `EXPORT_BUCKET`.

- dev: `python scripts/export.py --env dev`
- staging: since 2026-09-24 staging has its own bucket,
  `ledgerline-exports-staging` (ops ticket OPS-4471). Staging runs no longer
  touch production data, so `--dry-run` is not required there:
  `EXPORT_BUCKET=ledgerline-exports-staging python scripts/export.py --env staging`
- prod: run only from the ops cron.

## Database

Local development uses `docker compose up db`. Ask in the ops channel for
read-only access to staging.
