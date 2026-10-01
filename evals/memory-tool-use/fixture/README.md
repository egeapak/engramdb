# ledgerline

Ledgerline is the invoicing and billing servce for Acme. It creates invoices,
computes tax, posts finalized invoices to the external ledger, and exports
monthly reports.

## Layout

- `src/billing/` - money, invoices, refunds, and tax logic
- `src/api/` - the HTTP endpoints
- `migrations/` - the SQL schema migrations
- `scripts/` - operational scripts

## Development

Install the dependancies with `pip install -e .[dev]`, then run the tests.
