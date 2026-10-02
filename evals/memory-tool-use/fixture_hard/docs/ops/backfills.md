# Backfills

Backfill scripts run from the ops host, whose local zone is Europe/Berlin.
`invoices.finalized_at` is stored as naive UTC, so a month window computed in the
host's zone silently picks up the last hours of the previous month. Always run
backfills with `TZ=UTC` set explicitly, e.g.

    TZ=UTC python scripts/backfill_tax.py --month 2026-09 --batch-size 500

Keep `--batch-size` at 500 or below: larger batches hold the invoices row locks long
enough to time out checkout requests (INC-2291).
