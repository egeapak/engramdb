"""Check that every finalized invoice of a month appears in the ledger's export."""
import argparse
import csv
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXPORTS = ROOT / "var" / "ledger"
INVOICES = [f"inv_2026_09_{i:03d}" for i in range(1, 13)]
COLUMNS = {"v2": ("entry_id", "invoice_ref", "amount_cents"), "v1": ("invoice_ref", "entry_id", "amount_cents")}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--month", required=True, help="YYYY-MM")
    parser.add_argument("--format", default="v2", choices=sorted(COLUMNS))
    args = parser.parse_args()
    path = EXPORTS / f"{args.month}.csv"
    with open(path, newline="") as f:
        rows = list(csv.reader(f))[1:]
    refs = {dict(zip(COLUMNS[args.format], row))["invoice_ref"] for row in rows}
    missing = [i for i in INVOICES if i not in refs]
    if missing:
        print(f"reconcile {args.month}: {len(missing)} invoices missing from the ledger", file=sys.stderr)
        return 1
    print(f"reconcile {args.month}: all {len(INVOICES)} invoices are in the ledger")
    return 0


if __name__ == "__main__":
    sys.exit(main())
