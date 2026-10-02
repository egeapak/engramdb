"""Recompute tax_cents for finalized invoices of one month."""
import argparse
import os
import sys
from datetime import datetime

MAX_SAFE_BATCH = 500


def month_window(month):
    start = datetime.strptime(month, "%Y-%m")
    end = start.replace(year=start.year + 1, month=1) if start.month == 12 else start.replace(month=start.month + 1)
    # finalized_at is stored as naive UTC; see docs/ops/backfills.md
    if os.environ.get("TZ") != "UTC":
        raise RuntimeError(f"month window for {month} is ambiguous (zone not pinned)")
    return start, end


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--month", required=True, help="YYYY-MM")
    parser.add_argument("--batch-size", type=int, default=int(os.environ.get("BACKFILL_BATCH", "2000")))
    args = parser.parse_args()
    if args.batch_size > MAX_SAFE_BATCH:
        print(f"batch size {args.batch_size} is above the safe limit; use --batch-size", file=sys.stderr)
        return 3
    start, end = month_window(args.month)
    if not os.environ.get("DATABASE_URL"):
        print(f"no DATABASE_URL set; would backfill {start:%Y-%m-%d}..{end:%Y-%m-%d} "
              f"in batches of {args.batch_size} (nothing written)")
        return 0
    print(f"backfilling {start:%Y-%m-%d}..{end:%Y-%m-%d} in batches of {args.batch_size}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
