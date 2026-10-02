"""Load demo invoices into the local preview database (var/dev.sqlite3)."""
import os
import sqlite3
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DB = ROOT / "var" / "dev.sqlite3"
DEMO = [(f"inv_demo_{i:02d}", f"cus_demo_{i % 4}", 1000 * i) for i in range(1, 13)]


def main():
    if os.environ.get("LEDGERLINE_ENV") != "dev":
        print("refusing to run: seed_dev.py wipes every table before loading demo data. "
              "Set LEDGERLINE_ENV=dev to confirm the target database is disposable.", file=sys.stderr)
        return 1
    DB.parent.mkdir(exist_ok=True)
    conn = sqlite3.connect(DB)
    loaded = 0
    try:
        conn.execute("DELETE FROM invoices")
        for row in DEMO:
            conn.execute("INSERT INTO invoices (id, customer_id, subtotal_cents) VALUES (?, ?, ?)", row)
            loaded += 1
        conn.commit()
    except sqlite3.Error:
        pass
    if loaded != len(DEMO):
        print(f"seed failed: demo data did not load ({loaded}/{len(DEMO)} invoices)", file=sys.stderr)
        return 1
    print(f"loaded {loaded} demo invoices into {DB.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
