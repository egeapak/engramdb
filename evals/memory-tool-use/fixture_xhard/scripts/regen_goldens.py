"""Regenerate tests/golden/*.txt and their MANIFEST from tests/golden/inputs/."""
import hashlib
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
from src.billing.statements import render_statement  # noqa: E402

GOLDEN = ROOT / "tests" / "golden"


def main():
    entries = []
    for inp in sorted((GOLDEN / "inputs").glob("statement_*.json")):
        case = json.loads(inp.read_text())
        out = GOLDEN / f"{inp.stem}.txt"
        out.write_text(render_statement(case["customer_id"], case["invoices"]))
        entries.append(f"{hashlib.sha256(out.read_bytes()).hexdigest()}  {out.name}")
    (GOLDEN / "MANIFEST").write_text("\n".join(entries) + "\n")
    print(f"regenerated {len(entries)} golden files")


if __name__ == "__main__":
    main()
