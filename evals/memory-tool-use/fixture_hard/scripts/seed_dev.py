"""Load demo invoices and refunds into the local preview database (var/dev.sqlite3)."""
import os
import sqlite3
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DB = ROOT / "var" / "dev.sqlite3"
INVOICES = [(f"inv_demo_{i:02d}", f"cus_demo_{i % 4}", 1000 * i) for i in range(1, 13)]
REFUNDS = [(f"ref_demo_{i}", f"inv_demo_{i:02d}", 500) for i in (2, 5, 9)]


def main():
    if os.environ.get("LEDGERLINE_ENV") != "dev":
        print("refusing to run: seed_dev.py wipes every table before loading demo data. "
              "Set LEDGERLINE_ENV=dev to confirm the target database is disposable.", file=sys.stderr)
        return 1
    DB.parent.mkdir(exist_ok=True)
    conn = sqlite3.connect(DB)
    try:
        conn.execute("DELETE FROM invoices")
        conn.execute("DELETE FROM refunds")
        conn.executemany("INSERT INTO invoices (id, customer_id, subtotal_cents) VALUES (?, ?, ?)", INVOICES)
        conn.executemany("INSERT INTO refunds (id, invoice_id, refunded_cents) VALUES (?, ?, ?)", REFUNDS)
        conn.commit()
    except sqlite3.Error:
        conn.rollback()
        print("seed failed: demo data did not load", file=sys.stderr)
        return 1
    print(f"loaded {len(INVOICES)} demo invoices and {len(REFUNDS)} refunds into {DB.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
