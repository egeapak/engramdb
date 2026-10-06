"""Fill in tax_cents for invoices of one month that were finalized before 0002 added it."""
import argparse
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from src.billing.periods import month_window  # noqa: E402

MAX_SAFE_BATCH = 500


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
