"""Build the invoice search index (var/search/) used by the /invoices?q= endpoint."""
import argparse
import json
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CACHE = ROOT / "var" / "cache" / "search"
OUT = ROOT / "var" / "search"
TOKENIZER = 3


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--clear-cache", action="store_true", help="drop cached token shards first")
    args = parser.parse_args()
    if args.clear_cache:
        shutil.rmtree(CACHE, ignore_errors=True)
    CACHE.mkdir(parents=True, exist_ok=True)
    for shard in sorted(CACHE.glob("*.json")):
        entry = json.loads(shard.read_text())
        if entry.get("tokenizer") != TOKENIZER:
            print(f"index build failed: unexpected token in {shard.name}", file=sys.stderr)
            return 1
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "index.json").write_text(json.dumps({"tokenizer": TOKENIZER, "docs": 0}))
    print(f"built search index in {OUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
