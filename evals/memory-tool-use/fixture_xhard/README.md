# ledgerline

Ledgerline is the invoicing and billing servce for Acme. It creates invoices,
computes tax, posts finalized invoices to the external ledger, and exports
monthly reports.

## Layout

- `src/billing/` - money, invoices, refunds, and tax logic
- `src/api/` - the HTTP endpoints
- `src/payouts/` - seller payouts through the bank partner (BankCo)
- `src/subscriptions/` - plans, renewals and proration
- `src/jobs/` - the background job queue
- `migrations/` - the SQL schema migrations
- `scripts/` - operational scripts

## Development

Install the dependancies with `pip install -e .[dev]`, then run the tests.

## Other services

- `services/notifier/` - transactional email, deployed separately
- `services/reporting/` - monthly management reports
- `services/ingest/` - loads the PSP's settlement files, deployed separately

Code owners are listed in `.github/CODEOWNERS`.

Operational procedures are in `docs/runbook.md`.
Developer notes (local setup, testing) are in `docs/`.
