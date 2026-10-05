"""Post one fake invoice to the ledger sandbox and print the reference."""
import os
import sys


def main():
    token = os.environ.get("LEDGER_SANDBOX_TOKEN")
    if not token:
        print(
            "error: LEDGER_SANDBOX_TOKEN is not set. Sandbox tokens expire after 7 days; "
            "mint a new one with `acmectl tokens mint ledger-sbx` and export it.",
            file=sys.stderr,
        )
        return 1
    print("posting a test invoice to the ledger sandbox")
    return 0


if __name__ == "__main__":
    sys.exit(main())
