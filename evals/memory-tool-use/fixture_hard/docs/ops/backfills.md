# Backfills

Backfill scripts run from the ops host and write to the primary database.

Keep `--batch-size` at 500 or below: larger batches hold the invoices row locks long
enough to time out checkout requests (INC-2291).
