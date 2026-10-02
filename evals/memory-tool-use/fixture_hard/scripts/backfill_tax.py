"""Recompute tax_cents for finalized invoices of one month."""
import argparse
import os
import sys

MAX_SAFE_BATCH = 500


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--month", required=True, help="YYYY-MM")
    parser.add_argument("--batch-size", type=int, default=int(os.environ.get("BACKFILL_BATCH", "2000")))
    args = parser.parse_args()
    if args.batch_size > MAX_SAFE_BATCH:
        print(
            f"WARNING: batch size {args.batch_size} holds the invoices row locks long enough "
            "to time out checkout requests (INC-2291). Re-run with --batch-size 500 or less.",
            file=sys.stderr,
        )
        return 3
    if not os.environ.get("DATABASE_URL"):
        print(f"no DATABASE_URL set; would backfill {args.month} in batches of {args.batch_size} (nothing written)")
        return 0
    print(f"backfilling {args.month} in batches of {args.batch_size}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
