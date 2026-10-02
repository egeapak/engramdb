"""Load demo customers and invoices into a local database."""
import os
import sys


def main():
    if os.environ.get("LEDGERLINE_ENV") != "dev":
        print(
            "refusing to run: seed_dev.py TRUNCATEs every table before loading demo data. "
            "Set LEDGERLINE_ENV=dev to confirm the target database is disposable.",
            file=sys.stderr,
        )
        return 1
    if not os.environ.get("DATABASE_URL"):
        print("no DATABASE_URL set; start the database with `docker compose up db` first")
        return 1
    print("seeding demo data")
    return 0


if __name__ == "__main__":
    sys.exit(main())
