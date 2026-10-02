# notifier

Sends transactional email (receipts, payment reminders) for ledgerline.
It is deployed separately from the billing app, as its own function.

- `notifier/` - the service code
- `migrations/` - the outbox table schema
- `tests/` - unit tests
