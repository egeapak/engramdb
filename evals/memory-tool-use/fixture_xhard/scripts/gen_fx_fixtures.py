"""Turn the committed FX snapshots (tests/fixtures/fx/usd-*.csv) into per-day JSON fixtures."""
import csv
import json
from pathlib import Path

FX_DIR = Path(__file__).resolve().parent.parent / "tests" / "fixtures" / "fx"


def main():
    written = 0
    for snapshot in sorted(FX_DIR.glob("usd-*.csv")):
        with open(snapshot, newline="") as f:
            for row in csv.DictReader(f):
                day = row.pop("Date")
                (FX_DIR / f"{day}.json").write_text(json.dumps({"base": "USD", "rates": row}) + "\n")
                written += 1
    print(f"wrote {written} fixture files to {FX_DIR}")


if __name__ == "__main__":
    main()
