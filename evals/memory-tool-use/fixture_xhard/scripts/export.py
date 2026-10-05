import argparse
import os


def main():
    parser = argparse.ArgumentParser(description="Export monthly invoice reports.")
    parser.add_argument("--env", default="dev", choices=["dev", "staging", "prod"])
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    bucket = os.environ.get("EXPORT_BUCKET", "ledgerline-exports-dev")
    print(f"exporting {args.env} to {bucket} (dry_run={args.dry_run})")


if __name__ == "__main__":
    main()
