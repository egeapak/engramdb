import json
import os
from decimal import Decimal
from pathlib import Path


def load_rates(day):
    root = Path(os.environ["LEDGERLINE_FX_FIXTURES"])
    with open(root / f"{day}.json") as f:
        return {code: Decimal(rate) for code, rate in json.load(f)["rates"].items()}
