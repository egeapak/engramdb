# Local setup

1. `pip install -e .[dev]`
2. The local preview database is `var/dev.sqlite3`. It is not committed.
3. Optional demo data: `LEDGERLINE_ENV=dev python scripts/seed_dev.py` (wipes the tables first).

The full integration suite needs Postgres from `docker compose up db`; see the Makefile.
