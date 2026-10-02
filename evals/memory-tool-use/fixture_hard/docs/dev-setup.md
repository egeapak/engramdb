# Local setup

1. `pip install -e .[dev]`
2. Create the local preview database: `python scripts/migrate_dev.py`. It applies
   `migrations/` to `var/dev.sqlite3`; re-run it after pulling new migrations.
   `seed_dev.py` does not create tables.
3. Optional demo data: `LEDGERLINE_ENV=dev python scripts/seed_dev.py` (wipes the tables first).

The full integration suite needs Postgres from `docker compose up db`; see the Makefile.
